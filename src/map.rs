use crate::{
    canvas::Canvas,
    math::{vec2 as dvec2, Vec2 as DVec2, Rect, intersection},
    history::{self, History},
    call_colors::{self, CallHighlights, CallMark},
    filter::Filter,
    evidence::{EvidenceFilter, EvidenceKind},
    evidence_input::{self, EvidenceInput},
    relationships::{supports_path, CallEdge, Direction, Graph, Reachability, SourceFile, SymbolRef},
    syntax::HighlightedText,
    source_cache::{self, SourceCache},
    model::{code_inner, frame, ColorMode, Kind, Tree, CHAR_W, COL_CHARS, R},
    scan::{self, Entry},
};
use std::{collections::HashMap, path::PathBuf, sync::{Arc, mpsc::{channel, sync_channel, Receiver, Sender, SyncSender}}, time::Instant};

const STRIPS_FROM_PX: f64 = 0.6;
const TEXT_FROM_PX: f64 = 9.0;
const QUAD_BUDGET: usize = 400_000;
const LABEL_BUDGET: usize = 500;
pub const DEFAULT_SHOW_IGNORED: bool = false;
pub const DEFAULT_SYNTAX_COLORS: bool = true;
type Vec4f = [f32; 4];
fn vec4(r: f32, g: f32, b: f32, a: f32) -> Vec4f { [r, g, b, a] }

#[derive(Clone, Copy, PartialEq)]
enum ViewUpdate { Fit, Preserve }

#[derive(Clone, Copy)]
struct SourceAnchor { index: usize, line: usize, x: f64, y: f64, screen: DVec2, line_px: f64 }

#[derive(Clone, Debug, Default)]
pub struct NodeInfo { pub title: String, pub path: String, pub details: String }

#[derive(Clone, Debug)]
pub struct CallLink { pub rect: R, pub target: CallTarget }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallTarget { pub definition: SymbolRef, pub inference: Option<String>, pub evidence: EvidenceKind }

struct Label { rect: Rect, text: String, dim: bool }
#[derive(Clone, Copy)]
struct Drag { start: DVec2, start_off: DVec2, moved: bool, taps: u32, follow_link: bool, button: u32 }

enum ScanMsg {
    Project(Result<scan::ProjectScan, String>, f64),
    Expanded(String, Vec<Entry>),
    History(Result<History, String>),
    Graph(GraphUpdate),
    Text(u64, usize, HighlightedText),
}
struct GraphUpdate {
    generation: u64, graph: Graph, read_errors: usize, status: String, view: ViewUpdate,
    entries: Vec<Entry>, source_texts: HashMap<String, Arc<String>>,
}
type TextRequest = (u64, usize, PathBuf, String, Option<Arc<String>>);

pub struct Map {
    root: PathBuf,
    tree: Tree,
    tree_revision: u64,
    selection_revision: u64,
    pub show_ignored: bool,
    needs_layout: bool,
    needs_fit: bool,
    source_anchor: Option<SourceAnchor>,
    view: Rect,
    world: R,
    cam_scale: f64,
    cam_off: DVec2,
    flight: Option<(DVec2, f64)>,
    flight_tick: Instant,
    drag: Option<Drag>,
    hover: Option<usize>,
    selected: Option<usize>,
    definition_focus: Option<CallTarget>,
    call_links: Vec<CallLink>,
    label_rects: Vec<Rect>,
    pub color_mode: ColorMode,
    pub syntax_colors: bool,
    call_highlights: CallHighlights,
    history: Option<History>,
    search: String,
    filter: Filter,
    filter_error: Option<String>,
    pub hide_tests: bool,
    pub relationship_mode: usize,
    pub relationship_depth: usize,
    relationship_root: Option<String>,
    graph: Option<Graph>,
    graph_loading: bool,
    graph_read_errors: usize,
    graph_generation: u64,
    evidence_input: EvidenceInput,
    pub evidence_mode: usize,
    evidence_status: String,
    scan_ready: bool,
    reachable: Option<Reachability>,
    matches: Vec<usize>,
    match_cursor: usize,
    tx: Option<Sender<ScanMsg>>,
    rx: Option<Receiver<ScanMsg>>,
    pending: usize,
    content: SourceCache,
    text_worker: Option<SyncSender<TextRequest>>,
    message: String,
    labels: Vec<Label>,
    dirty: bool,
}

impl Map {
    pub fn new(root: PathBuf) -> Self {
        let mut map = Self::empty();
        map.open(root);
        map
    }

    fn empty() -> Self {
        Self {
            root: PathBuf::new(), tree: Tree::new("project"), tree_revision: 0, selection_revision: 0, show_ignored: DEFAULT_SHOW_IGNORED,
            needs_layout: true, needs_fit: true, source_anchor: None, view: Rect::default(), world: R::default(),
            cam_scale: 1.0, cam_off: DVec2::default(), flight: None, flight_tick: Instant::now(), drag: None,
            hover: None, selected: None, definition_focus: None, call_links: Vec::new(), label_rects: Vec::new(),
            color_mode: ColorMode::FileType, syntax_colors: DEFAULT_SYNTAX_COLORS, call_highlights: CallHighlights::default(),
            history: None, search: String::new(), filter: Filter::default(), filter_error: None, hide_tests: false,
            relationship_mode: 0, relationship_depth: 1, relationship_root: None, graph: None, graph_loading: false,
            graph_read_errors: 0, graph_generation: 0, evidence_input: EvidenceInput::Static, evidence_mode: 0,
            evidence_status: String::new(), scan_ready: false, reachable: None, matches: Vec::new(), match_cursor: 0,
            tx: None, rx: None, pending: 0, content: SourceCache::default(), text_worker: None,
            message: String::new(), labels: Vec::new(), dirty: true,
        }
    }

    pub fn tick(&mut self) -> bool {
        self.drain();
        self.step_flight();
        std::mem::take(&mut self.dirty)
    }
    pub fn needs_frame(&self) -> bool { self.dirty || self.pending > 0 || self.flight.is_some() }
    pub fn loading(&self) -> bool { self.pending > 0 }
    pub fn status(&self) -> &str { &self.message }
    pub fn evidence_status(&self) -> &str { &self.evidence_status }
    pub fn relationship_root(&self) -> Option<&str> { self.relationship_root.as_deref() }
    pub fn selection_info(&self) -> Option<NodeInfo> { self.selected.map(|index| self.info(index)) }
    pub fn tree(&self) -> &Tree { &self.tree }
    pub fn tree_revision(&self) -> u64 { self.tree_revision }
    pub fn selection_revision(&self) -> u64 { self.selection_revision }
    pub fn selected_node(&self) -> Option<usize> { self.selected }
    pub fn select_node(&mut self, index: usize, fly: bool) {
        if !self.tree.is_visible(index) { return; }
        self.select(index);
        if fly { self.fly_to_node(index); }
    }
    pub fn read_ignored_node(&mut self, index: usize) { if self.tree.is_visible(index) { self.expand(index); } }
    pub fn links(&self) -> &[CallLink] { &self.call_links }
    pub fn has_link_at(&self, x: f64, y: f64) -> bool { self.call_at(dvec2(x, y)).is_some() }
    pub fn debug_snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "root": self.root, "selected": self.selected.map(|index| &self.tree.nodes[index].path),
            "definition": self.definition_focus.as_ref().map(|target| serde_json::json!({"name":target.definition.name,"file":target.definition.file,"line":target.definition.line,"evidence":target.evidence.label(),"reason":target.inference})),
            "camera": {"x":self.cam_off.x,"y":self.cam_off.y,"scale":self.cam_scale},
            "view":[self.view.pos.x,self.view.pos.y,self.view.size.x,self.view.size.y],
            "loading":self.loading(), "pending":self.pending,
            "counts":{"nodes":self.tree.nodes.len(),"files":self.tree.nodes[0].total_files,"lines":self.tree.nodes[0].total_lines,"hotlinks":self.call_links.len()},
            "links":self.call_links.iter().map(|link|serde_json::json!({"rect":[link.rect.x,link.rect.y,link.rect.w,link.rect.h],"name":link.target.definition.name,"file":link.target.definition.file,"line":link.target.definition.line,"evidence":link.target.evidence.label(),"reason":link.target.inference})).collect::<Vec<_>>(),
        })
    }

    pub fn open(&mut self, root: PathBuf) {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
        self.tree = Tree::new(&name);
        self.tree_revision = self.tree_revision.wrapping_add(1);
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.root = root.clone();
        self.selected = None;
        self.definition_focus = None;
        self.call_links.clear();
        self.source_anchor = None;
        self.hover = None;
        self.drag = None;
        self.flight = None;
        self.world = R::default();
        self.needs_layout = true;
        self.needs_fit = true;
        self.cam_scale = 1.0;
        self.cam_off = DVec2::default();
        self.matches.clear();
        self.match_cursor = 0;
        self.label_rects.clear();
        self.history = None;
        self.graph = None;
        self.graph_loading = false;
        self.graph_generation = 0;
        self.evidence_input = EvidenceInput::Static;
        self.evidence_status = "Static analysis; optional app data is off".into();
        self.scan_ready = false;
        self.relationship_root = None;
        self.reachable = None;
        self.call_highlights = CallHighlights::default();
        self.content.replace(HashMap::new());
        let (tx, rx) = channel();
        let worker_tx = tx.clone();
        let (text_tx, text_rx) = sync_channel::<TextRequest>(8);
        let result_tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok((generation, index, path, relative, source)) = text_rx.recv() {
                let text = if let Some(source) = source { HighlightedText::new(&relative, &source) }
                    else { HighlightedText::from_bytes(&relative, &std::fs::read(path).unwrap_or_default()) };
                if result_tx.send(ScanMsg::Text(generation, index, text)).is_err() { break; }
            }
        });
        self.text_worker = Some(text_tx);
        self.tx = Some(tx);
        self.rx = Some(rx);
        self.pending = 1;
        self.set_status(format!("Scanning {} ...", root.display()));
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = scan::scan_project(&root);
            let _ = worker_tx.send(ScanMsg::Project(result, started.elapsed().as_secs_f64()));
        });
        self.dirty = true;
    }

    fn set_status(&mut self, message: String) {
        self.message = message;
        self.dirty = true;
    }

    pub fn set_show_ignored(&mut self, show: bool) {
        self.show_ignored = show;
        self.apply_selection(ViewUpdate::Fit);
    }

    pub fn set_filter(&mut self, query: &str) {
        match Filter::parse(query) {
            Ok(filter) => {
                self.filter = filter;
                self.filter_error = None;
                self.apply_selection(ViewUpdate::Fit);
            }
            Err(error) => {
                self.filter_error = Some(error.clone());
                self.set_status(format!("Filter error: {error}. Showing the last valid filter."));
            }
        }
    }

    pub fn set_hide_tests(&mut self, hide: bool) {
        self.hide_tests = hide;
        self.apply_selection(ViewUpdate::Fit);
    }

    pub fn set_syntax_colors(&mut self, on: bool) {
        self.syntax_colors = on;
        self.dirty = true;
    }

    pub fn set_evidence_mode(&mut self, mode: usize) {
        self.evidence_mode = mode.min(4);
        let filter = evidence_filter(self.evidence_mode);
        if self.definition_focus.as_ref().is_some_and(|target| !filter.allows(target.evidence)) { self.definition_focus = None; }
        self.refresh_relationships(ViewUpdate::Preserve);
    }

    pub fn set_evidence_input(&mut self, input: EvidenceInput) {
        self.evidence_input = input;
        self.definition_focus = None;
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.evidence_status = match &self.evidence_input {
            EvidenceInput::Static => "Using static analysis; reindexing without app data…".into(),
            EvidenceInput::File(_) => "Loading optional snapshot; existing information remains available…".into(),
            EvidenceInput::Rails => "Capturing optional Rails metadata (45s limit); existing information remains available…".into(),
        };
        self.refresh_relationships(ViewUpdate::Preserve);
        self.start_graph();
    }

    pub fn set_relationship_mode(&mut self, mode: usize) {
        self.relationship_mode = mode.min(2);
        if mode > 0 && self.relationship_root.is_none() {
            self.trace_selected_in(self.relationship_mode);
        } else {
            self.refresh_relationships(ViewUpdate::Fit);
        }
    }

    pub fn set_relationship_depth(&mut self, depth: usize) {
        self.relationship_depth = depth.clamp(1, 8);
        if self.relationship_mode > 0 && self.graph.is_some() {
            self.refresh_relationships(ViewUpdate::Preserve);
        }
    }

    pub fn trace_selected(&mut self) {
        self.trace_selected_in(1);
    }

    fn trace_selected_in(&mut self, mode: usize) {
        let Some(index) = self.selected.filter(|&i| self.tree.nodes[i].kind == Kind::Text) else {
            self.set_status("Select a Ruby, JavaScript or TypeScript file, then Trace selected file.".into());
            return;
        };
        let path = self.tree.nodes[index].path.clone();
        if !supports_path(&path) {
            self.set_status("Call analysis supports Ruby, JavaScript and TypeScript source files.".into());
            return;
        }
        if self.tree.nodes[index].ignored {
            self.set_status("Ignored files are outside the call index. Select a non-ignored source file.".into());
            return;
        }
        self.relationship_root = Some(path.clone());
        self.relationship_mode = mode;
        if self.graph.is_none() && !self.graph_loading {
            self.start_graph();
        }
        self.refresh_relationships(ViewUpdate::Fit);
    }

    fn start_graph(&mut self) {
        if self.graph_loading || !self.scan_ready { return; }
        let Some(tx) = self.tx.clone() else { return };
        let paths: Vec<String> = self.tree.nodes.iter()
            .filter(|node| node.kind == Kind::Text && !node.ignored && supports_path(&node.path))
            .map(|node| node.path.clone()).collect();
        let root = self.root.clone();
        let input = self.evidence_input.clone();
        let generation = self.graph_generation;
        let view = if self.graph.is_some() { ViewUpdate::Preserve } else { ViewUpdate::Fit };
        self.graph_loading = true;
        self.pending += 1;
        std::thread::spawn(move || {
            let mut read_errors = 0;
            let sources: Vec<SourceFile> = paths.into_iter().filter_map(|path| {
                match std::fs::read_to_string(root.join(&path)) {
                    Ok(text) => Some(SourceFile { path, text }),
                    Err(_) => { read_errors += 1; None }
                }
            }).collect();
            let base_url = std::fs::read_to_string(root.join("tsconfig.json")).ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| value.pointer("/compilerOptions/baseUrl").and_then(|v| v.as_str()).map(str::to_owned));
            let evidence = evidence_input::load(&root, &sources, &input);
            let graph = match evidence.snapshot.as_ref() {
                Some(snapshot) => Graph::build_with_snapshot(&sources, base_url.as_deref(), Some(snapshot)),
                None => Graph::build(&sources, base_url.as_deref()),
            };
            let (entries, source_texts) = source_cache::prepare(sources);
            let _ = tx.send(ScanMsg::Graph(GraphUpdate { generation, graph, read_errors, status: evidence.status, view, entries, source_texts }));
        });
    }

    fn refresh_relationships(&mut self, view: ViewUpdate) {
        self.reachable = if self.relationship_mode > 0 {
            self.graph.as_ref().zip(self.relationship_root.as_deref()).map(|(graph, root)| {
                let direction = if self.relationship_mode == 2 { Direction::Callers } else { Direction::Callees };
                if self.evidence_mode == 0 && !matches!(self.evidence_input, EvidenceInput::Static) { graph.related(root, direction, self.relationship_depth) }
                else {
                    let mut filter = evidence_filter(self.evidence_mode);
                    if matches!(self.evidence_input, EvidenceInput::Static) { filter.loaded = false; filter.observed = false; }
                    graph.related_filtered(root, direction, self.relationship_depth, filter)
                }
            })
        } else { None };
        self.apply_selection(view);
    }

    fn apply_selection(&mut self, view: ViewUpdate) {
        self.tree_revision = self.tree_revision.wrapping_add(1);
        let weights: Vec<_> = if view == ViewUpdate::Preserve {
            self.tree.nodes.iter().map(|node| node.weight).collect()
        } else { Vec::new() };
        let anchor = if view == ViewUpdate::Preserve && !self.needs_layout && !self.needs_fit {
            capture_source_anchor(&self.tree, self.view, self.cam_off, self.cam_scale)
        } else { self.source_anchor };
        if view == ViewUpdate::Fit { self.definition_focus = None; }
        self.tree.update_weights(self.show_ignored);
        self.tree.apply_filter(&self.filter, self.hide_tests, self.reachable.as_ref().map(|r| &r.paths));
        let visible = self.tree.nodes.iter().enumerate()
            .filter(|(index, node)| node.is_file() && self.tree.is_visible(*index))
            .map(|(_, node)| node.path.clone()).collect();
        self.call_highlights = CallHighlights::new(self.reachable.as_ref().map_or(&[], |r| r.call_sites.as_slice()), &visible);
        self.matches = self.tree.search(&self.search);
        self.match_cursor = 0;
        self.hover = None;
        self.flight = None;
        if view == ViewUpdate::Fit {
            self.needs_layout = true;
            self.needs_fit = true;
            self.source_anchor = None;
        } else if self.tree.nodes.iter().map(|node| node.weight).ne(weights) {
            self.needs_layout = true;
            self.source_anchor = anchor;
            if let Some(anchor) = self.source_anchor.as_mut().filter(|anchor| !self.tree.is_visible(anchor.index)) {
                if let Some(index) = self.relationship_root.as_ref().and_then(|root| self.tree.find(root)).filter(|&i| self.tree.is_visible(i)) {
                    anchor.index = index;
                    anchor.line = 1;
                    anchor.x = 0.1;
                    anchor.y = 0.5;
                    anchor.screen = self.view.size * 0.5;
                    self.selected = Some(index);
                    self.selection_revision = self.selection_revision.wrapping_add(1);
                }
            }
        }
        if self.definition_focus.as_ref().is_some_and(|focus| self.tree.find(&focus.definition.file).is_none_or(|i| !self.tree.is_visible(i))) {
            self.definition_focus = None;
        }
        if self.selected.is_some_and(|index| !self.tree.is_visible(index)) {
            self.selected = None;
        }
        self.set_status(self.summary());
        self.dirty = true;
    }

    pub fn set_color_mode(&mut self, mode: ColorMode) {
        self.color_mode = mode;
        self.tree.compute_heat(mode);
        if mode != ColorMode::FileType && self.history.is_none() {
            self.set_status("Reading git history ...".to_string());
        }
        self.dirty = true;
    }

    pub fn set_search(&mut self, query: &str) {
        self.search = query.trim().to_string();
        self.matches = self.tree.search(&self.search);
        self.match_cursor = 0;
        if self.search.is_empty() {
            self.set_status(self.summary());
        } else {
            let text = format!("{} matches for \"{}\". Press Enter to fly to the next one.", fmt_num(self.matches.len() as u64), self.search);
            self.set_status(text);
        }
        self.dirty = true;
    }

    pub fn next_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        let index = self.matches[self.match_cursor % self.matches.len()];
        self.match_cursor += 1;
        self.select(index);
        self.fly_to_node(index);
    }

    pub fn fit(&mut self) {
        let world = self.world;
        self.fly_to(world, 1.0);
    }

    fn fly_to(&mut self, r: R, fill: f64) {
        self.source_anchor = None;
        if self.view.size.x <= 0.0 || self.view.size.y <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let scale = (self.view.size.x / r.w).min(self.view.size.y / r.h) * fill;
        if !scale.is_finite() || scale <= 0.0 { return; }
        self.flight = Some((dvec2(r.x + r.w * 0.5, r.y + r.h * 0.5), scale));
    }

    fn fly_to_node(&mut self, index: usize) {
        let r = self.tree.nodes[index].rect;
        self.fly_to(r, 0.95);
    }

    fn select(&mut self, index: usize) {
        if self.definition_focus.take().is_some() {
            self.set_status(self.summary());
        }
        self.selected = Some(index);
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.dirty = true;
    }

    fn definition_line_rect(&self, index: usize, line: usize) -> Option<R> {
        let node = self.tree.nodes.get(index)?;
        if !self.tree.is_visible(index) { return None; }
        node.code_line_rect(line)
    }

    fn jump_to_definition(&mut self, target: CallTarget) {
        self.source_anchor = None;
        let definition = &target.definition;
        let Some(index) = self.tree.find(&definition.file) else { return };
        let Some(line) = self.definition_line_rect(index, definition.line) else { return };
        self.flight = None;
        let source = self.tree.nodes[index].lines[definition.line - 1];
        self.flight = Some(definition_camera(line, source.indent as usize, source.len as usize, self.view.size));
        self.definition_focus = Some(target.clone());
        self.selected = Some(index);
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.ensure_text(index);
        self.set_status(format!("{} · {}", self.summary(), target_description(&target)));
        self.dirty = true;
    }

    fn call_at(&self, abs: DVec2) -> Option<CallTarget> {
        if !self.view.contains(abs) || self.label_rects.iter().any(|r| r.contains(abs)) { return None; }
        self.call_links.iter().find(|link| link.rect.contains(abs.x, abs.y)).map(|link| link.target.clone())
    }

    fn expand(&mut self, index: usize) {
        let Kind::Ghost { dir, loading: false } = self.tree.nodes[index].kind else { return };
        let Some(tx) = self.tx.clone() else { return };
        self.tree.nodes[index].kind = Kind::Ghost { dir, loading: true };
        let root = self.root.clone();
        let path = self.tree.nodes[index].path.clone();
        self.pending += 1;
        std::thread::spawn(move || {
            let entries = scan::scan_ignored(&root, &path);
            let _ = tx.send(ScanMsg::Expanded(path, entries));
        });
        self.dirty = true;
    }

    fn summary(&self) -> String {
        let root = &self.tree.nodes[0];
        let mut text = format!("{} files, {} lines", fmt_num(root.total_files), fmt_num(root.total_lines));
        if let Some(h) = &self.history {
            text.push_str(&format!(", {} commits of history", fmt_num(h.commits_read as u64)));
        }
        if let Some(error) = &self.filter_error {
            return format!("Filter error: {error}. Showing the last valid filter.");
        }
        if self.graph_loading { text.push_str(" · Indexing calls…"); }
        if self.relationship_mode > 0 {
            if let Some(reachable) = &self.reachable {
                let visible = |path: &&String| self.tree.find(path).is_some_and(|index| self.tree.is_visible(index));
                text.push_str(&format!(" · {} direct / {} indirect files · depth {}: {} reached methods / {} colored sites ({} inferred, {} loaded, {} observed)",
                    reachable.direct.iter().filter(visible).count(), reachable.indirect.iter().filter(visible).count(), self.relationship_depth,
                    reachable.reached_symbols, self.call_highlights.site_count(), self.call_highlights.inferred_site_count(), self.call_highlights.evidence_site_count(EvidenceKind::Loaded), self.call_highlights.evidence_site_count(EvidenceKind::Observed)));
            }
            if let Some(graph) = &self.graph {
                text.push_str(&format!(" · {} unresolved static sites / {} parse errors / {} unread files",
                    graph.unresolved_sites, graph.parse_errors, self.graph_read_errors));
            }
        }
        text
    }

    fn drain(&mut self) {
        let Some(rx) = &self.rx else { return };
        let msgs: Vec<ScanMsg> = rx.try_iter().collect();
        for msg in msgs {
            self.pending = self.pending.saturating_sub(1);
            match msg {
                ScanMsg::Project(Ok(project), secs) => {
                    self.scan_ready = true;
                    let used_git = project.used_git;
                    for entry in project.entries {
                        self.tree.insert(entry);
                    }
                    self.tree_revision = self.tree_revision.wrapping_add(1);
                    self.tree.update_weights(self.show_ignored);
                    self.tree.apply_filter(&self.filter, self.hide_tests, None);
                    let text = format!(
                        "{}, scanned in {:.2}s ({})",
                        self.summary(),
                        secs,
                        if used_git { "ignore rules from git" } else { "no git: read .gitignore files" }
                    );
                    self.set_status(text);
                    self.needs_layout = true;
                    self.needs_fit = true;
                    if !matches!(self.evidence_input, EvidenceInput::Static) { self.start_graph(); }
                    if used_git {
                        if let Some(tx) = self.tx.clone() {
                            let root = self.root.clone();
                            self.pending += 1;
                            std::thread::spawn(move || {
                                let _ = tx.send(ScanMsg::History(history::load(&root)));
                            });
                        }
                    }
                }
                ScanMsg::Project(Err(err), _) => {
                    self.set_status(format!("Could not scan: {err}"));
                }
                ScanMsg::Expanded(path, entries) => {
                    if let Some(index) = self.tree.find(&path) {
                        let count = entries.len();
                        self.tree.graft(index, entries);
                        self.after_tree_change();
                        self.selected = Some(index);
                        self.selection_revision = self.selection_revision.wrapping_add(1);
                        self.set_status(format!("Read ignored {path}: {} files", fmt_num(count as u64)));
                    }
                }
                ScanMsg::History(Ok(history)) => {
                    self.tree.apply_history(&history);
                    self.history = Some(history);
                    self.tree.compute_heat(self.color_mode);
                    self.set_status(self.summary());
                }
                ScanMsg::History(Err(err)) => {
                    self.set_status(format!("No git history: {err}"));
                }
                ScanMsg::Graph(update) => {
                    self.graph_loading = false;
                    if update.generation != self.graph_generation { self.start_graph(); continue; }
                    self.source_anchor = if update.view == ViewUpdate::Preserve { capture_source_anchor(&self.tree, self.view, self.cam_off, self.cam_scale) } else { None };
                    for entry in update.entries { self.tree.insert(entry); }
                    self.content.replace(update.source_texts);
                    self.definition_focus = None;
                    self.call_links.clear();
                    self.needs_layout = true;
                    self.graph = Some(update.graph);
                    self.graph_read_errors = update.read_errors;
                    self.evidence_status = update.status;
                    self.refresh_relationships(update.view);
                }
                ScanMsg::Text(generation, index, text) => {
                    self.content.accept(generation, index, text);
                }
            }
            self.dirty = true;
        }
    }

    fn after_tree_change(&mut self) {
        self.tree_revision = self.tree_revision.wrapping_add(1);
        self.needs_layout = true;
        if let Some(history) = &self.history {
            self.tree.apply_history(history);
        }
        self.tree.compute_heat(self.color_mode);
        if !self.search.is_empty() {
            self.matches = self.tree.search(&self.search);
        }
    }

    fn relayout(&mut self) {
        self.tree.update_weights(self.show_ignored);
        if self.world.w <= 0.0 {
            self.world = R { x: 0.0, y: 0.0, w: self.view.size.x.max(100.0), h: self.view.size.y.max(100.0) };
        }
        self.tree.layout(self.world);
        self.needs_layout = false;
    }

    fn to_screen(&self, r: R) -> Rect {
        Rect {
            pos: self.view.pos + (dvec2(r.x, r.y) - self.cam_off) * self.cam_scale,
            size: dvec2(r.w, r.h) * self.cam_scale,
        }
    }

    fn to_world(&self, abs: DVec2) -> DVec2 {
        self.cam_off + (abs - self.view.pos) / self.cam_scale
    }

    fn hit(&self, abs: DVec2) -> Option<usize> {
        if self.tree.is_empty() || self.cam_scale <= 0.0 || !self.view.contains(abs) {
            return None;
        }
        let p = self.to_world(abs);
        self.tree.hit(p.x, p.y, 3.0 / self.cam_scale)
    }

    fn step_flight(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.flight_tick).as_secs_f64().min(0.05);
        self.flight_tick = now;
        let Some((center, scale)) = self.flight else { return };
        if self.cam_scale <= 0.0 || self.view.size.x <= 0.0 { return; }
        let half = self.view.size * 0.5;
        let now_center = self.cam_off + half / self.cam_scale;
        let k = 1.0 - (-14.0 * dt).exp();
        let new_scale = (self.cam_scale.ln() + (scale.ln() - self.cam_scale.ln()) * k).exp();
        let new_center = now_center + (center - now_center) * k;
        self.cam_scale = new_scale;
        self.cam_off = new_center - half / new_scale;
        if (new_scale / scale - 1.0).abs() < 0.002 && (new_center - center).length() * new_scale < 0.5 {
            self.cam_scale = scale;
            self.cam_off = center - half / scale;
            self.flight = None;
        }
        self.dirty = true;
    }

    fn info(&self, index: usize) -> NodeInfo {
        let n = &self.tree.nodes[index];
        let mut details = match n.kind {
            Kind::Dir => format!(
                "Folder\n{} files\n{} lines\n{}\n{} direct children",
                fmt_num(n.total_files),
                fmt_num(n.total_lines),
                fmt_bytes(n.bytes),
                n.children.len()
            ),
            Kind::Text => {
                let comments = n.lines.iter().filter(|l| l.comment).count();
                format!(
                    "Text file\n{} lines ({} comment lines)\n{}",
                    fmt_num(n.lines.len() as u64),
                    fmt_num(comments as u64),
                    fmt_bytes(n.bytes)
                )
            }
            Kind::Binary => format!("Binary or very large file\n{}", fmt_bytes(n.bytes)),
            Kind::Ghost { dir, loading } => format!(
                "Ignored {}\n{}",
                if dir { "folder" } else { "file" },
                if loading { "Reading it now ..." } else { "Not read yet. Click it to read it." }
            ),
        };
        if self.history.is_some() && !matches!(n.kind, Kind::Ghost { .. }) {
            if n.commits > 0 {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                details.push_str(&format!(
                    "\n\nGit: {} commits\nlast changed {}",
                    fmt_num(n.commits as u64),
                    fmt_age(now - n.last_change)
                ));
            } else {
                details.push_str("\n\nGit: no commits in history");
            }
        }
        if n.ignored && !matches!(n.kind, Kind::Ghost { .. }) {
            details.push_str("\n\nMatched by .gitignore");
        }
        if !matches!(self.evidence_input, EvidenceInput::Static) {
            details.push_str(&format!("\n\nOptional evidence\n{}", self.evidence_status));
        }
        if self.relationship_mode > 0 && n.is_file() {
            if self.relationship_root.as_deref() == Some(n.path.as_str()) {
                details.push_str("\n\nCall root: all methods in this file.\nDepth counts method-call hops;\ndeeper methods can stay in the\nsame files.");
            } else if let Some(chain) = self.reachable.as_ref().and_then(|r| r.witnesses.get(&n.path)) {
                details.push_str("\n\n");
                details.push_str(witness_label(chain));
                for edge in chain {
                    details.push_str(&format!("\n\n{}\n{}:{}\n  calls {}\n{}:{}",
                        short_label(&edge.caller.name, 40), short_label(&edge.caller.file, 42), edge.line,
                        short_label(&edge.callee.name, 38), short_label(&edge.callee.file, 42), edge.callee.line));
                    if let Some(reason) = &edge.inference {
                        details.push_str(&format!("\n{}: {reason}", evidence_title(edge.evidence)));
                    }
                    details.push_str(&format!("\nEvidence: {}", edge.evidence.label()));
                }
                details.push_str("\n\nObserved calls apply to their capture.\nOther links describe possible execution.\nInferred targets are candidates.");
            }
            let connections = self.call_highlights.connections(&n.path);
            if !connections.is_empty() {
                details.push_str("\n\nColored calls in this file:");
                for (line, mark) in connections.iter().take(16) {
                    details.push_str(&format!("\nL{}:{} → {}", line, mark.start + 1, short_label(&mark.target, 31)));
                    if let Some(reason) = &mark.inference {
                        details.push_str(&format!("\n{}: {reason}", evidence_title(mark.evidence)));
                    }
                }
                if connections.len() > 16 { details.push_str(&format!("\n…and {} more call sites", connections.len() - 16)); }
            } else if self.relationship_mode == 2 {
                details.push_str("\n\nIncoming call sites are in the\ncaller files. Select one to see\nits colored calls to this file.");
            }
        }
        if let Some(target) = self.definition_focus.as_ref().filter(|target| target.definition.file == n.path) {
            details.push_str(&format!("\n\n{}\nLine {} (highlighted)", target_description(target), target.definition.line));
        }
        NodeInfo {
            title: n.name.clone(),
            path: if n.path.is_empty() { self.root.display().to_string() } else { n.path.clone() },
            details,
        }
    }

    fn search_active(&self) -> bool {
        !self.search.is_empty()
    }

    pub fn file_color(&self, index: usize) -> Vec4f {
        let node = &self.tree.nodes[index];
        if self.reachable.is_some() && node.is_file() {
            let [r, g, b] = call_colors::file_rgb(&node.path, self.relationship_root.as_deref());
            return vec4(r, g, b, 1.0);
        }
        match self.color_mode {
            ColorMode::FileType => match node.kind {
                Kind::Binary => vec4(0.45, 0.45, 0.47, 1.0),
                _ => hsv(ext_hue(&node.name), 0.5, 0.8),
            },
            _ => heat_color(node.heat),
        }
    }

    pub fn draw(&mut self, canvas: &mut Canvas, view: R) {
        self.call_links.clear();
        self.label_rects.clear();
        self.view = view.into();
        if [view.x, view.y, view.w, view.h].iter().any(|value| !value.is_finite()) || view.w <= 0.0 || view.h <= 0.0 { return; }
        canvas.rect(view, vec4(0.025, 0.035, 0.045, 1.0), [0.0; 4], 0.0, 0.0, view);
        if self.tree.is_empty() {
            let message = if self.message.is_empty() { "No folder opened" } else { &self.message };
            canvas.text(message, view.x + 16.0, view.y + 16.0, 12.0, vec4(0.7, 0.7, 0.7, 1.0), view, false);
            return;
        }
        if self.needs_layout {
            self.relayout();
            if let Some(anchor) = self.source_anchor.take() {
                if let Some((offset, scale)) = restore_source_anchor(&self.tree, anchor) {
                    self.cam_off = offset;
                    self.cam_scale = scale;
                }
            }
        }
        if self.tree.nodes[0].weight <= 0.0 {
            canvas.text("No files match this selection. Clear the filter or choose All files.", view.x + 16.0, view.y + 16.0, 12.0, vec4(0.7, 0.7, 0.7, 1.0), view, false);
            return;
        }
        if self.needs_fit {
            self.cam_scale = (view.w / self.world.w).min(view.h / self.world.h);
            let half = self.view.size * 0.5;
            self.cam_off = dvec2(self.world.w * 0.5, self.world.h * 0.5) - half / self.cam_scale;
            self.needs_fit = false;
        }
        self.labels.clear();
        self.draw_map(canvas);
        if let Some(index) = self.hover.filter(|&index| self.tree.is_visible(index)) {
            self.outline(canvas, index, vec4(1.0, 1.0, 1.0, 0.5), 1.5);
        }
        if let Some(index) = self.selected.filter(|&index| self.tree.is_visible(index)) {
            self.outline(canvas, index, vec4(0.35, 0.85, 1.0, 1.0), 2.5);
        }
        self.draw_labels(canvas);
    }

    pub fn pointer_move(&mut self, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() { return; }
        let abs = dvec2(x, y);
        if let Some(mut drag) = self.drag {
            if (abs - drag.start).length() > 4.0 { drag.moved = true; }
            if drag.moved && self.cam_scale > 0.0 {
                self.cam_off = drag.start_off - (abs - drag.start) / self.cam_scale;
                self.dirty = true;
            }
            self.drag = Some(drag);
        }
        let hover = self.hit(abs);
        if hover != self.hover { self.hover = hover; self.dirty = true; }
    }

    #[cfg(test)]
    pub fn pointer_down(&mut self, x: f64, y: f64, button: u32, clicks: u32) {
        self.pointer_down_with_modifiers(x, y, button, clicks, 0);
    }

    pub fn pointer_down_with_modifiers(&mut self, x: f64, y: f64, button: u32, clicks: u32, modifiers: u32) {
        let abs = dvec2(x, y);
        if !x.is_finite() || !y.is_finite() || !self.view.contains(abs) { return; }
        self.source_anchor = None;
        self.flight = None;
        self.drag = Some(Drag { start: abs, start_off: self.cam_off, moved: false, taps: clicks, follow_link: button == 0 && modifiers & crate::platform::SHIFT == 0, button });
    }

    pub fn pointer_up(&mut self, x: f64, y: f64, button: u32) {
        let Some(drag) = self.drag.take() else { return };
        if drag.button != button { self.drag = Some(drag); return; }
        let abs = dvec2(x, y);
        if !x.is_finite() || !y.is_finite() || drag.moved || !self.view.contains(abs) { return; }
        if drag.follow_link {
            if let Some(target) = self.call_at(drag.start).filter(|target| self.call_at(abs).as_ref() == Some(target)) {
                self.jump_to_definition(target);
                return;
            }
        }
        if let Some(index) = self.hit(abs) {
            self.select(index);
            if matches!(self.tree.nodes[index].kind, Kind::Ghost { .. }) { self.expand(index); }
            else if drag.taps >= 2 { self.fly_to_node(index); }
        }
    }

    pub fn scroll(&mut self, x: f64, y: f64, dy: f64) {
        let abs = dvec2(x, y);
        if !x.is_finite() || !y.is_finite() || !dy.is_finite() || !self.view.contains(abs) || self.cam_scale <= 0.0 || self.world.w <= 0.0 || self.world.h <= 0.0 { return; }
        self.source_anchor = None;
        self.flight = None;
        let factor = (-dy * 0.01).clamp(-20.0, 20.0).exp();
        let anchor = self.to_world(abs);
        let min_scale = (self.view.size.x / self.world.w).min(self.view.size.y / self.world.h) * 0.5;
        self.cam_scale = (self.cam_scale * factor).clamp(min_scale, min_scale * 200_000.0);
        self.cam_off = anchor - (abs - self.view.pos) / self.cam_scale;
        self.hover = self.hit(abs);
        self.dirty = true;
    }

    fn draw_map(&mut self, canvas: &mut Canvas) {
        let view = self.view;
        let searching = self.search_active();
        let mut quads = 0usize;
        let mut stack = vec![0usize];
        while let Some(index) = stack.pop() {
            let node = &self.tree.nodes[index];
            if node.weight <= 0.0 {
                continue;
            }
            let r = self.to_screen(node.rect);
            if r.pos.x > view.pos.x + view.size.x
                || r.pos.y > view.pos.y + view.size.y
                || r.pos.x + r.size.x < view.pos.x
                || r.pos.y + r.size.y < view.pos.y
                || (r.size.x < 0.5 && r.size.y < 0.5)
            {
                continue;
            }
            if quads > QUAD_BUDGET {
                break;
            }
            let fade = if searching && self.reachable.is_none() && !node.on_path { 0.22 } else { 1.0 };
            let small = r.size.x.min(r.size.y);
            let border = if small > 8.0 { 1.0 } else { 0.0 };
            match node.kind {
                Kind::Dir => {
                    let shade = 0.07 + 0.03 * (node.depth.min(5) as f32);
                    let color = if node.ignored { vec4(shade * 1.3, shade, shade * 0.8, 1.0) } else { vec4(shade * 0.85, shade, shade * 1.25, 1.0) };
                    block(canvas, self.view, r, scale_rgb(color, fade), vec4(0.0, 0.0, 0.0, 0.6), border, if node.ignored { 0.4 } else { 0.0 });
                    quads += 1;
                    if small > 4.0 {
                        stack.extend(node.children.iter().rev());
                        let (_, head) = frame(node.rect);
                        if head * self.cam_scale >= 13.0 && r.size.x > 50.0 && (!searching || node.on_path) {
                            self.labels.push(Label { rect: r, text: node.name.clone(), dim: node.ignored });
                        }
                    }
                }
                Kind::Text => {
                    let base = self.file_color(index);
                    let dark = if node.ignored { 0.12 } else { 0.17 };
                    let bg = scale_rgb(base, dark * fade);
                    let edge = scale_rgb(base, 0.35 * fade);
                    block(canvas, self.view, r, bg, edge, border, if node.ignored { 0.3 } else { 0.0 });
                    quads += 1;
                    quads += self.draw_code_lines(canvas, index, fade);
                }
                Kind::Binary => {
                    let base = self.file_color(index);
                    block(canvas, self.view, r, scale_rgb(base, 0.35 * fade), scale_rgb(base, 0.6 * fade), border, 0.5);
                    quads += 1;
                    if r.size.x > 60.0 && r.size.y > 18.0 {
                        self.labels.push(Label { rect: r, text: node.name.clone(), dim: true });
                    }
                }
                Kind::Ghost { loading, .. } => {
                    let color = if loading { vec4(0.20, 0.17, 0.08, 1.0) } else { vec4(0.10, 0.09, 0.08, 1.0) };
                    block(canvas, self.view, r, scale_rgb(color, fade), vec4(0.45, 0.38, 0.25, fade), border, 1.0);
                    quads += 1;
                    if r.size.x > 60.0 && r.size.y > 18.0 {
                        let text = if loading { format!("{} (reading...)", node.name) } else { format!("{} (ignored)", node.name) };
                        self.labels.push(Label { rect: r, text, dim: true });
                    }
                }
            }
        }
        if searching {
            for k in 0..self.matches.len().min(5000) {
                let index = self.matches[k];
                let r = self.to_screen(self.tree.nodes[index].rect);
                if r.size.x >= 2.0 && self.tree.nodes[index].weight > 0.0 {
                    block(canvas, self.view, r, vec4(0.0, 0.0, 0.0, 0.0), vec4(1.0, 0.85, 0.2, 1.0), 2.0, 0.0);
                }
            }
        }
    }

    fn draw_code_lines(&mut self, canvas: &mut Canvas, index: usize, fade: f32) -> usize {
        let node = &self.tree.nodes[index];
        let line_px = node.line_h * self.cam_scale;
        let r_screen = self.to_screen(node.rect);
        let searching = self.search_active() && self.reachable.is_none();
        let wants_label = r_screen.size.x > 60.0 && r_screen.size.y > 18.0 && (!searching || node.on_path);
        if line_px < STRIPS_FROM_PX || node.lines.is_empty() {
            if wants_label && self.labels.len() < LABEL_BUDGET {
                self.labels.push(Label { rect: r_screen, text: node.name.clone(), dim: false });
            }
            return 0;
        }
        let inner = self.to_screen(code_inner(node.rect));
        let cols = node.cols.max(1) as usize;
        let rows = node.lines.len().div_ceil(cols);
        let char_px = line_px * CHAR_W;
        let col_px = COL_CHARS * char_px;
        let view = self.view;
        let text_mode = line_px >= TEXT_FROM_PX;
        if text_mode {
            self.ensure_text(index);
        }
        let node = &self.tree.nodes[index];
        let code_color = vec4(0.49 * fade, 0.52 * fade, 0.56 * fade, 1.0);
        let comment_color = if self.syntax_colors { vec4(0.30 * fade, 0.40 * fade, 0.30 * fade, 1.0) } else { code_color };
        let mut drawn = 0;
        let first_col = ((view.pos.x - inner.pos.x) / col_px).floor().max(0.0) as usize;
        let last_col = ((((view.pos.x + view.size.x) - inner.pos.x) / col_px).ceil().max(0.0) as usize).min(cols);
        let first_row = ((view.pos.y - inner.pos.y) / line_px).floor().max(0.0) as usize;
        let last_row = ((((view.pos.y + view.size.y) - inner.pos.y) / line_px).ceil().max(0.0) as usize).min(rows);
        for col in first_col..last_col {
            for row in first_row..last_row {
                let k = col * rows + row;
                let Some(line) = node.lines.get(k) else { break };
                let source_rect = node.code_line_rect(k + 1).unwrap();
                let line_rect = self.to_screen(source_rect);
                let (x, y) = (line_rect.pos.x, line_rect.pos.y);
                if self.definition_focus.as_ref().is_some_and(|target| target.definition.file == node.path && target.definition.line == k + 1) {
                    block(canvas, self.view, line_rect, vec4(0.95, 0.73, 0.24, 0.12), vec4(0.95, 0.73, 0.24, 0.9), 1.5, 0.0);
                }
                if text_mode {
                    if let Some(line) = self.content.files.get(&index).and_then(|text| text.lines.get(k)) {
                        self.call_links.extend(draw_source_line(canvas, self.view, line, self.call_highlights.line(&node.path, k + 1), self.relationship_root.as_deref(), self.syntax_colors, dvec2(x, y), line_px, fade));
                    }
                } else if line.len > 0 {
                    let color = if line.comment { comment_color } else { code_color };
                    let w = (line.len as f64 * char_px).min(col_px - line.indent as f64 * char_px).max(0.5);
                    let rect = Rect {
                        pos: dvec2(x + line.indent as f64 * char_px, y + line_px * 0.18),
                        size: dvec2(w, (line_px * 0.64).max(0.5)),
                    };
                    block(canvas, self.view, rect, color, vec4(0.0, 0.0, 0.0, 0.0), 0.0, 0.0);
                    drawn += 1;
                    drawn += draw_call_strips(canvas, self.view, self.call_highlights.line(&node.path, k + 1), self.relationship_root.as_deref(), dvec2(x, y), char_px, line_px, fade);
                }
            }
        }
        if wants_label && self.labels.len() < LABEL_BUDGET {
            self.labels.push(Label { rect: r_screen, text: node.name.clone(), dim: false });
        }
        drawn
    }

    fn ensure_text(&mut self, index: usize) {
        if self.content.files.contains_key(&index) || self.content.pending.contains(&index) { return; }
        let relative = self.tree.nodes[index].path.clone();
        let source = self.content.originals.get(&relative).cloned();
        if self.text_worker.as_ref().is_some_and(|tx| tx.try_send((self.content.generation, index, self.root.join(&relative), relative, source)).is_ok()) {
            self.content.pending.insert(index);
            self.pending += 1;
            self.dirty = true;
        }
    }

    fn outline(&mut self, canvas: &mut Canvas, index: usize, color: Vec4f, width: f32) {
        let r = self.to_screen(self.tree.nodes[index].rect);
        block(canvas, self.view, r, vec4(0.0, 0.0, 0.0, 0.0), color, width, 0.0);
    }

    fn draw_labels(&mut self, canvas: &mut Canvas) {
        let labels = std::mem::take(&mut self.labels);
        for label in labels.iter().take(LABEL_BUDGET) {
            let pos = dvec2(label.rect.pos.x.max(self.view.pos.x) + 4.0, label.rect.pos.y.max(self.view.pos.y) + 3.0);
            let room = label.rect.pos.x + label.rect.size.x - pos.x - 8.0;
            if room < 20.0 {
                continue;
            }
            let max_chars = (room / 6.0) as usize;
            let text: String = if label.text.chars().count() > max_chars {
                label.text.chars().take(max_chars.saturating_sub(1)).chain(['…']).collect()
            } else {
                label.text.clone()
            };
            let w = (canvas.text_width(&text, 11.0, false) + 8.0).min(room + 4.0);
            let label_rect = Rect { pos: pos - dvec2(3.0, 1.0), size: dvec2(w, 14.0) };
            self.label_rects.push(label_rect);
            block(canvas, self.view, label_rect, vec4(0.0, 0.0, 0.0, 0.72), vec4(0.0, 0.0, 0.0, 0.0), 0.0, 0.0);
            let color = if label.dim { vec4(0.75, 0.68, 0.55, 1.0) } else { vec4(0.92, 0.94, 0.96, 1.0) };
            canvas.text(&text, pos.x, pos.y, 11.0, color, self.view.into(), false);
        }
        self.labels = labels;
        self.labels.clear();
    }

}

fn draw_source_line(canvas: &mut Canvas, view: Rect, line: &crate::syntax::HighlightedLine, marks: &[CallMark], root: Option<&str>, syntax: bool, pos: DVec2, line_height: f64, fade: f32) -> Vec<CallLink> {
    let ranges = call_colors::link_ranges(marks, line.text.chars().count().min(COL_CHARS as usize));
    let mut links = Vec::new();
    let mut x = pos.x;
    let mut column = 0;
    let size = line_height * 0.8;
    for run in call_colors::paint_line(line, marks, root, syntax, COL_CHARS as usize) {
        let [r, g, b] = run.color;
        let width = canvas.text_width(run.text, size, true);
        let end = column + run.text.chars().count();
        for range in &ranges {
            let (start, stop) = (range.start.max(column), range.end.min(end));
            if stop <= start { continue; }
            let prefix = run.text.char_indices().nth(start - column).map_or(run.text.len(), |(i, _)| i);
            let suffix = run.text.char_indices().nth(stop - column).map_or(run.text.len(), |(i, _)| i);
            let left = canvas.text_width(&run.text[..prefix], size, true);
            let right = canvas.text_width(&run.text[..suffix], size, true);
            if let Some(rect) = intersection(R { x: x + left, y: pos.y, w: right - left, h: line_height }, view.into()) {
                links.push(CallLink { rect, target: CallTarget { definition: range.definition.clone(), inference: range.inference.map(str::to_owned), evidence: range.evidence } });
            }
        }
        canvas.text(run.text, x, pos.y, size, vec4(r * fade, g * fade, b * fade, 1.0), view.into(), true);
        x += width;
        column = end;
    }
    links
}

fn draw_call_strips(canvas: &mut Canvas, view: Rect, marks: &[CallMark], root: Option<&str>, pos: DVec2, char_width: f64, line_height: f64, fade: f32) -> usize {
    let mut count = 0;
    for mark in marks {
        let start = mark.start.min(COL_CHARS as usize);
        let end = mark.end.min(COL_CHARS as usize);
        if end <= start { continue; }
        let [r, g, b] = call_colors::file_rgb(&mark.target, root);
        block(canvas, view, Rect { pos: pos + dvec2(start as f64 * char_width, line_height * 0.18), size: dvec2((end - start) as f64 * char_width, (line_height * 0.64).max(0.5)) }, vec4(r * fade, g * fade, b * fade, 1.0), [0.0; 4], 0.0, 0.0);
        count += 1;
    }
    count
}

fn block(canvas: &mut Canvas, view: Rect, rect: Rect, color: Vec4f, edge: Vec4f, border: f32, hatch: f32) {
    canvas.rect(rect.into(), color, edge, border, hatch, view.into());
}

fn short_label(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_owned() }
    else { text.chars().take(max.saturating_sub(1)).chain(['…']).collect() }
}

fn witness_label(chain: &[CallEdge]) -> &'static str {
    let observed = chain.iter().any(|edge| edge.evidence == EvidenceKind::Observed);
    if observed && chain.iter().any(|edge| edge.evidence != EvidenceKind::Observed) { return "Mixed evidence call path"; }
    match (chain.iter().map(|edge| edge.evidence).max().unwrap_or(EvidenceKind::Source), chain.len() == 1) {
        (EvidenceKind::Observed, true) => "Observed direct call",
        (EvidenceKind::Observed, false) => "Observed call path",
        (EvidenceKind::Loaded, true) => "Loaded-app-assisted call",
        (EvidenceKind::Loaded, false) => "Loaded-app-assisted call path",
        (EvidenceKind::Inferred, true) => "Inferred direct call",
        (EvidenceKind::Inferred, false) => "Inferred call path",
        (EvidenceKind::Source, true) => "Direct static call",
        (EvidenceKind::Source, false) => "Indirect static call path",
    }
}

fn evidence_filter(mode: usize) -> EvidenceFilter {
    EvidenceFilter { source: mode <= 2, inferred: mode == 0 || mode == 2, loaded: mode == 0 || mode == 3, observed: mode == 0 || mode == 4 }
}

fn evidence_title(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Source => "Source target",
        EvidenceKind::Inferred => "Inferred target",
        EvidenceKind::Loaded => "Loaded-app-assisted target",
        EvidenceKind::Observed => "Observed target",
    }
}

fn target_description(target: &CallTarget) -> String {
    let definition = &target.definition;
    let location = format!("{} at {}:{}", definition.name, definition.file, definition.line);
    match &target.inference {
        Some(reason) => format!("{}: {reason} · {location}", evidence_title(target.evidence)),
        None => format!("Definition: {location}"),
    }
}

fn capture_source_anchor(tree: &Tree, view: Rect, offset: DVec2, scale: f64) -> Option<SourceAnchor> {
    if scale <= 0.0 || !scale.is_finite() { return None; }
    let center = offset + view.size * 0.5 / scale;
    let index = tree.hit(center.x, center.y, 0.0)?;
    let node = &tree.nodes[index];
    if node.kind != Kind::Text || node.lines.is_empty() || node.line_h * scale < STRIPS_FROM_PX { return None; }
    let inner = code_inner(node.rect);
    let rows = node.lines.len().div_ceil(node.cols.max(1) as usize);
    let col = ((center.x - inner.x) / (COL_CHARS * CHAR_W * node.line_h)).floor().max(0.0) as usize;
    let row = ((center.y - inner.y) / node.line_h).floor().max(0.0) as usize;
    let line = (col.min(node.cols.max(1) as usize - 1) * rows + row.min(rows - 1)).min(node.lines.len() - 1) + 1;
    let rect = node.code_line_rect(line)?;
    let x = ((center.x - rect.x) / rect.w).clamp(0.0, 1.0);
    let y = ((center.y - rect.y) / rect.h).clamp(0.0, 1.0);
    let point = dvec2(rect.x + rect.w * x, rect.y + rect.h * y);
    Some(SourceAnchor { index, line, x, y, screen: (point - offset) * scale, line_px: rect.h * scale })
}

fn restore_source_anchor(tree: &Tree, anchor: SourceAnchor) -> Option<(DVec2, f64)> {
    if !tree.is_visible(anchor.index) { return None; }
    let rect = tree.nodes.get(anchor.index)?.code_line_rect(anchor.line)?;
    let scale = anchor.line_px / rect.h;
    if scale <= 0.0 || !scale.is_finite() { return None; }
    let point = dvec2(rect.x + rect.w * anchor.x, rect.y + rect.h * anchor.y);
    Some((point - anchor.screen / scale, scale))
}

fn definition_camera(line: R, indent: usize, length: usize, view: DVec2) -> (DVec2, f64) {
    let scale = (view.x / line.w * 0.9).max(18.0 / line.h);
    let start = line.x + indent.min(COL_CHARS as usize) as f64 * CHAR_W * line.h;
    let width = length.max(1).min((COL_CHARS as usize).saturating_sub(indent)) as f64 * CHAR_W * line.h;
    (dvec2(start + width.min(view.x / scale * 0.8) * 0.5, line.y + line.h * 0.5), scale)
}

fn ext_hue(name: &str) -> f32 {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "rs" => 0.07,
        "clj" | "cljc" | "edn" => 0.33,
        "cljs" => 0.45,
        "js" | "mjs" | "jsx" => 0.14,
        "ts" | "tsx" => 0.58,
        "css" | "scss" => 0.78,
        "html" => 0.02,
        "json" | "toml" | "yaml" | "yml" => 0.5,
        "md" | "txt" => 0.62,
        _ => {
            let h = ext.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
            (h % 1000) as f32 / 1000.0
        }
    }
}

fn heat_color(heat: f32) -> Vec4f {
    if heat < 0.0 {
        return vec4(0.30, 0.30, 0.32, 1.0);
    }
    let stops = [(0.10, 0.22, 0.60), (0.62, 0.22, 0.62), (0.98, 0.45, 0.18), (1.0, 0.92, 0.35)];
    let t = heat.clamp(0.0, 1.0) * 3.0;
    let i = (t.floor() as usize).min(2);
    let f = t - i as f32;
    let (a, b) = (stops[i], stops[i + 1]);
    vec4(a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f, 1.0)
}

fn hsv(h: f32, s: f32, v: f32) -> Vec4f {
    let f = |n: f32| {
        let k = (n + h * 6.0) % 6.0;
        v - v * s * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    vec4(f(5.0), f(3.0), f(1.0), 1.0)
}

fn scale_rgb(c: Vec4f, k: f32) -> Vec4f {
    vec4(c[0] * k, c[1] * k, c[2] * k, c[3])
}

pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn fmt_bytes(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} bytes"),
    }
}

fn fmt_age(secs: i64) -> String {
    let days = secs / 86_400;
    match days {
        d if d < 1 => "today".to_string(),
        1 => "yesterday".to_string(),
        d if d < 60 => format!("{d} days ago"),
        d if d < 730 => format!("{} months ago", d / 30),
        d => format!("{} years ago", d / 365),
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn inferred_witnesses_and_clicked_targets_name_the_reason_and_location() {
        let source = SymbolRef { name: "A#run".into(), file: "a.rb".into(), line: 2 };
        let definition = SymbolRef { name: "B#run".into(), file: "b.rb".into(), line: 8 };
        let exact = CallEdge { caller: source, callee: definition.clone(), line: 3, start_column: 0, end_column: 3, inference: None, evidence: EvidenceKind::Source };
        let mut inferred = exact.clone();
        inferred.inference = Some("Rails belongs_to association".into());
        inferred.evidence = EvidenceKind::Inferred;
        assert_eq!(witness_label(&[exact.clone()]), "Direct static call");
        assert_eq!(witness_label(&[inferred.clone()]), "Inferred direct call");
        assert_eq!(witness_label(&[exact.clone(), inferred.clone()]), "Inferred call path");
        assert_eq!(witness_label(&[inferred.clone(), exact]), "Inferred call path");
        let target = CallTarget { definition, inference: inferred.inference.clone(), evidence: EvidenceKind::Inferred };
        assert_eq!(target_description(&target), "Inferred target: Rails belongs_to association · B#run at b.rb:8");
        let mut observed = inferred.clone();
        observed.evidence = EvidenceKind::Observed;
        observed.inference = Some("request example · 3 calls".into());
        assert_eq!(witness_label(&[observed.clone()]), "Observed direct call");
        assert_eq!(witness_label(&[observed.clone(), observed.clone()]), "Observed call path");
        assert_eq!(witness_label(&[observed.clone(), inferred]), "Mixed evidence call path");
        let target = CallTarget { definition: observed.callee, inference: observed.inference, evidence: EvidenceKind::Observed };
        assert!(target_description(&target).starts_with("Observed target: request example"));
    }

    #[test]
    fn source_position_and_readable_zoom_survive_a_trace_layout_change() {
        let mut tree = Tree::new("project");
        for (path, lines) in [("a.rb", 40), ("b.rb", 1800), ("c.rb", 50_000)] {
            tree.insert(Entry { path: path.into(), kind: crate::scan::EntryKind::Text {
                bytes: lines as u64, lines: vec![crate::scan::Line::default(); lines],
            }, ignored: false });
        }
        tree.update_weights(false);
        let world = R { x: 0.0, y: 0.0, w: 1060.0, h: 726.0 };
        let visible = HashSet::from(["a.rb".into(), "b.rb".into()]);
        tree.apply_filter(&Filter::default(), false, Some(&visible));
        tree.layout(world);
        let index = tree.find("b.rb").unwrap();
        let columns = tree.nodes[index].cols;
        let line = tree.nodes[index].code_line_rect(800).unwrap();
        let view = Rect { pos: dvec2(0.0, 148.0), size: dvec2(1060.0, 726.0) };
        let scale = 20.0 / line.h;
        let offset = dvec2(line.x + line.w * 0.3, line.y + line.h * 0.4) - view.size * 0.5 / scale;
        let anchor = capture_source_anchor(&tree, view, offset, scale).unwrap();
        assert_eq!(anchor.line, 800);
        let (same_offset, same_scale) = restore_source_anchor(&tree, anchor).unwrap();
        assert!((same_offset - offset).length() < 1e-8);
        assert!((same_scale - scale).abs() < 1e-8);

        tree.apply_filter(&Filter::default(), false, None);
        tree.layout(world);
        assert_ne!(tree.nodes[index].cols, columns);
        let (new_offset, new_scale) = restore_source_anchor(&tree, anchor).unwrap();
        let line = tree.nodes[index].code_line_rect(800).unwrap();
        let point = dvec2(line.x + line.w * anchor.x, line.y + line.h * anchor.y);
        assert!(((point - new_offset) * new_scale - anchor.screen).length() < 1e-7);
        assert!((line.h * new_scale - 20.0).abs() < 1e-8);

        tree.apply_filter(&Filter::parse("a.rb").unwrap(), false, None);
        tree.layout(world);
        assert!(restore_source_anchor(&tree, anchor).is_none());
        assert!(capture_source_anchor(&tree, view, offset, 0.0).is_none());
    }

    fn controller() -> Map {
        let mut map = Map::empty();
        for path in ["a.rb", "b.rb"] {
            map.tree.insert(Entry { path: path.into(), kind: crate::scan::EntryKind::Text {
                bytes: 20, lines: vec![crate::scan::Line { indent: 0, len: 8, comment: false }; 20],
            }, ignored: false });
        }
        map.view = R { x: 40.0, y: 100.0, w: 800.0, h: 600.0 }.into();
        map.relayout();
        map.needs_fit = false;
        map.dirty = false;
        map
    }

    #[test]
    fn pointer_links_follow_only_primary_unmodified_clicks_and_stay_inside_the_map() {
        let mut map = controller();
        let target = CallTarget { definition: SymbolRef { file: "b.rb".into(), name: "B#run".into(), line: 4 }, inference: None, evidence: EvidenceKind::Source };
        let rect = R { x: 70.0, y: 140.0, w: 60.0, h: 20.0 };
        map.call_links.push(CallLink { rect, target: target.clone() });
        assert!(map.has_link_at(80.0, 150.0));
        assert!(!map.has_link_at(80.0, 99.0));
        map.pointer_down(80.0, 150.0, 1, 1);
        map.pointer_up(80.0, 150.0, 1);
        assert!(map.definition_focus.is_none());
        map.pointer_down_with_modifiers(80.0, 150.0, 0, 1, crate::platform::SHIFT);
        map.pointer_up(80.0, 150.0, 0);
        assert!(map.definition_focus.is_none());
        map.pointer_down(80.0, 150.0, 0, 1);
        map.pointer_up(80.0, 150.0, 0);
        assert_eq!(map.definition_focus, Some(target));
        assert_eq!(map.selected, map.tree.find("b.rb"));
        assert!(map.flight.is_some());
    }

    #[test]
    fn dragging_cancels_a_link_click_and_zoom_preserves_the_pointer_anchor() {
        let mut map = controller();
        let start = map.cam_off;
        map.pointer_down(80.0, 150.0, 0, 1);
        map.pointer_move(100.0, 180.0);
        map.pointer_up(100.0, 180.0, 0);
        assert_eq!(map.cam_off, start - dvec2(20.0, 30.0));
        assert!(map.selected.is_none());
        let point = dvec2(180.0, 220.0);
        let anchor = map.to_world(point);
        let scale = map.cam_scale;
        map.scroll(point.x, point.y, -80.0);
        assert!(map.cam_scale > scale);
        assert!((map.to_world(point) - anchor).length() < 1e-8);
        let zoomed = map.cam_scale;
        map.scroll(10.0, 10.0, -80.0);
        map.scroll(point.x, point.y, f64::NAN);
        assert_eq!(map.cam_scale, zoomed);
    }

    #[test]
    fn invalid_filter_retains_the_last_valid_selection_and_reports_the_draft_error() {
        let mut map = controller();
        map.set_filter("a.rb");
        assert!(map.tree.is_visible(map.tree.find("a.rb").unwrap()));
        assert!(!map.tree.is_visible(map.tree.find("b.rb").unwrap()));
        map.set_filter("(");
        assert!(map.status().starts_with("Filter error:"));
        assert!(map.tree.is_visible(map.tree.find("a.rb").unwrap()));
        assert!(!map.tree.is_visible(map.tree.find("b.rb").unwrap()));
    }

    #[test]
    fn definition_flights_keep_the_declaration_visible_in_narrow_maps() {
        let line = R { x: 150.0, y: 290.0, w: 100.0, h: 2.0 };
        for width in [320.0, 600.0, 1060.0] {
            for indent in [0, 4, 24] {
                let (center, scale) = definition_camera(line, indent, 18, dvec2(width, 700.0));
                let declaration_start = line.x + indent as f64 * CHAR_W * line.h;
                let left = (declaration_start - center.x) * scale + width * 0.5;
                assert!(left >= 0.0 && left < width);
                assert!(line.h * scale >= 18.0);
                assert_eq!(center.y, line.y + line.h * 0.5);
            }
        }
    }
}
