//! The map widget: draws the tree as nested rectangles and lets you fly around it.
//!
//! Everything is a quad drawn by one tiny pixel shader. Makepad batches
//! all quads of the same shader into one GPU draw call, which is why drawing
//! a hundred thousand of them per frame is fine.

use crate::history::{self, History};
use crate::call_colors::{self, CallHighlights, CallMark};
use crate::filter::Filter;
use crate::evidence::{EvidenceFilter, EvidenceKind};
use crate::evidence_input::{self, EvidenceInput};
use crate::relationships::{supports_path, CallEdge, Direction, Graph, Reachability, SourceFile, SymbolRef};
use crate::syntax::HighlightedText;
use crate::source_cache::{self, SourceCache};
use crate::model::{code_inner, frame, ColorMode, Kind, Tree, CHAR_W, COL_CHARS, R};
use crate::scan::{self, Entry};
use makepad_widgets::*;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, mpsc::{channel, sync_channel, Receiver, Sender, SyncSender}},
    time::Instant,
};

/// Below this many screen pixels per line, a file is just a tinted box.
const STRIPS_FROM_PX: f64 = 0.6;
/// From this many pixels per line, draw the real text instead of strips.
const TEXT_FROM_PX: f64 = 9.0;
/// Hard cap on quads per frame, a safety net for huge projects.
const QUAD_BUDGET: usize = 400_000;
const LABEL_BUDGET: usize = 500;
pub const DEFAULT_SHOW_IGNORED: bool = false;
pub const DEFAULT_SYNTAX_COLORS: bool = true;

#[derive(Clone, Copy, PartialEq)]
enum ViewUpdate {
    Fit,
    Preserve,
}

#[derive(Clone, Copy)]
struct SourceAnchor {
    index: usize,
    line: usize,
    x: f64,
    y: f64,
    screen: DVec2,
    line_px: f64,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // The shader for every box, line strip and label pill. `self.pos` is the
    // position inside the quad (0..1) and `self.rect_size` its size in pixels,
    // just like UV coordinates and a resolution uniform in a Three.js shader.
    set_type_default() do #(DrawBlock::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: #x202630
        edge: #x000000
        border: 0.0
        hatch: 0.0
        pixel: fn() {
            let p = self.pos * self.rect_size
            let d = min(min(p.x, p.y), min(self.rect_size.x - p.x, self.rect_size.y - p.y))
            // diagonal stripes mark ignored things
            let stripe = step(0.5, fract((p.x + p.y) / 10.0)) * self.hatch
            let base = vec4(self.color.rgb * (1.0 + 0.6 * stripe), self.color.w)
            let cov = clamp(self.border - d + 0.5, 0.0, 1.0) * step(0.01, self.border)
            let c = mix(base, self.edge, cov)
            return vec4(c.rgb * c.w, c.w)
        }
    }

    mod.widgets.CodeMapBase = #(CodeMap::register_widget(vm))
    mod.widgets.CodeMap = set_type_default() do mod.widgets.CodeMapBase{
        width: Fill
        height: Fill
        draw_bg +: {color: #x07090c}
        draw_block +: {}
        draw_label +: {
            color: #xe6ebf0
            text_style: theme.font_regular{font_size: 8.0}
        }
        draw_code +: {
            color: #xc9d1d9
            text_style: theme.font_code{font_size: 6.0}
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBlock {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    edge: Vec4f,
    #[live]
    border: f32,
    #[live]
    hatch: f32,
}

/// What the app shows in the inspector panel.
#[derive(Clone, Debug, Default)]
pub struct NodeInfo {
    pub title: String,
    pub path: String,
    pub details: String,
}

#[derive(Clone, Debug, Default)]
pub enum CodeMapAction {
    #[default]
    None,
    Selected(NodeInfo),
    Status(String),
    Root(String, usize),
    EvidenceStatus(String),
}

enum ScanMsg {
    Project(Result<scan::ProjectScan, String>, f64),
    Expanded(String, Vec<Entry>),
    History(Result<History, String>),
    Graph(GraphUpdate),
    Text(u64, usize, HighlightedText),
}

struct GraphUpdate {
    generation: u64,
    graph: Graph,
    read_errors: usize,
    status: String,
    view: ViewUpdate,
    entries: Vec<Entry>,
    source_texts: HashMap<String, Arc<String>>,
}

type TextRequest = (u64, usize, PathBuf, String, Option<Arc<String>>);

struct Label {
    rect: Rect,
    text: String,
    dim: bool,
}

struct CallLink {
    rect: Rect,
    target: CallTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CallTarget {
    definition: SymbolRef,
    inference: Option<String>,
    evidence: EvidenceKind,
}

#[derive(Clone, Copy)]
struct Drag {
    start: DVec2,
    start_off: DVec2,
    moved: bool,
    taps: u32,
    follow_link: bool,
}

#[derive(Script, ScriptHook, Widget)]
pub struct CodeMap {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[redraw]
    #[area]
    area: Area,
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_block: DrawBlock,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_code: DrawText,

    #[rust]
    root: PathBuf,
    #[rust]
    tree: Tree,
    #[rust(DEFAULT_SHOW_IGNORED)]
    show_ignored: bool,
    #[rust]
    needs_layout: bool,
    #[rust]
    needs_fit: bool,
    #[rust]
    source_anchor: Option<SourceAnchor>,
    /// The map's rectangle on screen, from the last draw.
    #[rust]
    view: Rect,
    /// World size of the whole map.
    #[rust]
    world: R,
    /// Camera: screen = view.pos + (world - cam_off) * cam_scale
    #[rust]
    cam_scale: f64,
    #[rust]
    cam_off: DVec2,
    /// Where a double click is flying the camera to: (world center, scale).
    #[rust]
    flight: Option<(DVec2, f64)>,
    #[rust]
    drag: Option<Drag>,
    #[rust]
    hover: Option<usize>,
    #[rust]
    selected: Option<usize>,
    #[rust]
    definition_focus: Option<CallTarget>,
    #[rust]
    call_links: Vec<CallLink>,
    #[rust]
    label_rects: Vec<Rect>,
    #[rust]
    color_mode: ColorMode,
    #[rust(DEFAULT_SYNTAX_COLORS)]
    syntax_colors: bool,
    #[rust]
    call_highlights: CallHighlights,
    #[rust]
    history: Option<History>,
    #[rust]
    search: String,
    #[rust]
    filter: Filter,
    #[rust]
    filter_error: Option<String>,
    #[rust]
    hide_tests: bool,
    #[rust]
    relationship_mode: usize,
    #[rust(1)]
    relationship_depth: usize,
    #[rust]
    relationship_root: Option<String>,
    #[rust]
    graph: Option<Graph>,
    #[rust]
    graph_loading: bool,
    #[rust]
    graph_read_errors: usize,
    #[rust]
    graph_generation: u64,
    #[rust]
    evidence_input: EvidenceInput,
    #[rust]
    evidence_mode: usize,
    #[rust]
    evidence_status: String,
    #[rust]
    scan_ready: bool,
    #[rust]
    reachable: Option<Reachability>,
    #[rust]
    matches: Vec<usize>,
    #[rust]
    match_cursor: usize,
    #[rust]
    tx: Option<Sender<ScanMsg>>,
    #[rust]
    rx: Option<Receiver<ScanMsg>>,
    #[rust]
    pending: usize,
    #[rust]
    frame: NextFrame,
    /// File contents for the fully zoomed-in view, loaded on demand.
    #[rust]
    content: SourceCache,
    #[rust]
    text_worker: Option<SyncSender<TextRequest>>,
    #[rust]
    message: String,
    #[rust]
    labels: Vec<Label>,
}

impl CodeMap {
    pub fn open(&mut self, cx: &mut Cx, root: PathBuf) {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
        self.tree = Tree::new(&name);
        self.root = root.clone();
        self.selected = None;
        self.definition_focus = None;
        self.call_links.clear();
        self.source_anchor = None;
        self.hover = None;
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
        cx.widget_action(self.uid, CodeMapAction::EvidenceStatus(self.evidence_status.clone()));
        self.set_status(cx, format!("Scanning {} ...", root.display()));
        // Scanning runs on a background thread so the window stays responsive.
        // `move` hands ownership of `root` and the sender to that thread.
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = scan::scan_project(&root);
            let _ = worker_tx.send(ScanMsg::Project(result, started.elapsed().as_secs_f64()));
        });
        self.frame = cx.new_next_frame();
        self.redraw(cx);
    }

    fn set_status(&mut self, cx: &mut Cx, message: String) {
        self.message = message;
        cx.widget_action(self.uid, CodeMapAction::Status(self.message.clone()));
    }

    pub fn set_show_ignored(&mut self, cx: &mut Cx, show: bool) {
        self.show_ignored = show;
        self.apply_selection(cx, ViewUpdate::Fit);
    }

    pub fn set_filter(&mut self, cx: &mut Cx, query: &str) {
        match Filter::parse(query) {
            Ok(filter) => {
                self.filter = filter;
                self.filter_error = None;
                self.apply_selection(cx, ViewUpdate::Fit);
            }
            Err(error) => {
                self.filter_error = Some(error.clone());
                self.set_status(cx, format!("Filter error: {error}. Showing the last valid filter."));
            }
        }
    }

    pub fn set_hide_tests(&mut self, cx: &mut Cx, hide: bool) {
        self.hide_tests = hide;
        self.apply_selection(cx, ViewUpdate::Fit);
    }

    pub fn set_syntax_colors(&mut self, cx: &mut Cx, on: bool) {
        self.syntax_colors = on;
        self.redraw(cx);
    }

    pub fn set_evidence_mode(&mut self, cx: &mut Cx, mode: usize) {
        self.evidence_mode = mode.min(4);
        let filter = evidence_filter(self.evidence_mode);
        if self.definition_focus.as_ref().is_some_and(|target| !filter.allows(target.evidence)) { self.definition_focus = None; }
        self.refresh_relationships(cx, ViewUpdate::Preserve);
    }

    pub fn set_evidence_input(&mut self, cx: &mut Cx, input: EvidenceInput) {
        self.evidence_input = input;
        self.definition_focus = None;
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.evidence_status = match &self.evidence_input {
            EvidenceInput::Static => "Using static analysis; reindexing without app data…".into(),
            EvidenceInput::File(_) => "Loading optional snapshot; existing information remains available…".into(),
            EvidenceInput::Rails => "Capturing optional Rails metadata (45s limit); existing information remains available…".into(),
        };
        cx.widget_action(self.uid, CodeMapAction::EvidenceStatus(self.evidence_status.clone()));
        self.refresh_relationships(cx, ViewUpdate::Preserve);
        self.start_graph(cx);
    }

    pub fn set_relationship_mode(&mut self, cx: &mut Cx, mode: usize) {
        self.relationship_mode = mode.min(2);
        if mode > 0 && self.relationship_root.is_none() {
            self.trace_selected_in(cx, self.relationship_mode);
        } else {
            self.refresh_relationships(cx, ViewUpdate::Fit);
        }
    }

    pub fn set_relationship_depth(&mut self, cx: &mut Cx, depth: usize) {
        self.relationship_depth = depth.clamp(1, 8);
        if self.relationship_mode > 0 && self.graph.is_some() {
            self.refresh_relationships(cx, ViewUpdate::Preserve);
        }
    }

    pub fn trace_selected(&mut self, cx: &mut Cx) {
        self.trace_selected_in(cx, 1);
    }

    fn trace_selected_in(&mut self, cx: &mut Cx, mode: usize) {
        let Some(index) = self.selected.filter(|&i| self.tree.nodes[i].kind == Kind::Text) else {
            self.set_status(cx, "Select a Ruby, JavaScript or TypeScript file, then Trace selected file.".into());
            return;
        };
        let path = self.tree.nodes[index].path.clone();
        if !supports_path(&path) {
            self.set_status(cx, "Call analysis supports Ruby, JavaScript and TypeScript source files.".into());
            return;
        }
        if self.tree.nodes[index].ignored {
            self.set_status(cx, "Ignored files are outside the call index. Select a non-ignored source file.".into());
            return;
        }
        self.relationship_root = Some(path.clone());
        self.relationship_mode = mode;
        cx.widget_action(self.uid, CodeMapAction::Root(path, mode));
        if self.graph.is_none() && !self.graph_loading {
            self.start_graph(cx);
        }
        self.refresh_relationships(cx, ViewUpdate::Fit);
    }

    fn start_graph(&mut self, cx: &mut Cx) {
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
        self.frame = cx.new_next_frame();
    }

    fn refresh_relationships(&mut self, cx: &mut Cx, view: ViewUpdate) {
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
        self.apply_selection(cx, view);
    }

    fn apply_selection(&mut self, cx: &mut Cx, view: ViewUpdate) {
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
                }
            }
        }
        if self.definition_focus.as_ref().is_some_and(|focus| self.tree.find(&focus.definition.file).is_none_or(|i| !self.tree.is_visible(i))) {
            self.definition_focus = None;
        }
        if let Some(index) = self.selected.filter(|&i| self.tree.is_visible(i)) {
            cx.widget_action(self.uid, CodeMapAction::Selected(self.info(index)));
        } else if self.selected.is_some() {
            self.selected = None;
            cx.widget_action(self.uid, CodeMapAction::Selected(NodeInfo::default()));
        }
        self.set_status(cx, self.summary());
        self.redraw(cx);
    }

    pub fn set_color_mode(&mut self, cx: &mut Cx, mode: ColorMode) {
        self.color_mode = mode;
        self.tree.compute_heat(mode);
        if mode != ColorMode::FileType && self.history.is_none() {
            self.set_status(cx, "Reading git history ...".to_string());
        }
        self.redraw(cx);
    }

    pub fn set_search(&mut self, cx: &mut Cx, query: &str) {
        self.search = query.trim().to_string();
        self.matches = self.tree.search(&self.search);
        self.match_cursor = 0;
        if self.search.is_empty() {
            self.set_status(cx, self.summary());
        } else {
            let text = format!("{} matches for \"{}\". Press Enter to fly to the next one.", fmt_num(self.matches.len() as u64), self.search);
            self.set_status(cx, text);
        }
        self.redraw(cx);
    }

    pub fn next_match(&mut self, cx: &mut Cx) {
        if self.matches.is_empty() {
            return;
        }
        let index = self.matches[self.match_cursor % self.matches.len()];
        self.match_cursor += 1;
        self.select(cx, index);
        self.fly_to_node(cx, index);
    }

    pub fn fit(&mut self, cx: &mut Cx) {
        let world = self.world;
        self.fly_to(cx, world, 1.0);
    }

    fn fly_to(&mut self, cx: &mut Cx, r: R, fill: f64) {
        self.source_anchor = None;
        if self.view.size.x <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let scale = (self.view.size.x / r.w).min(self.view.size.y / r.h) * fill;
        self.flight = Some((dvec2(r.x + r.w * 0.5, r.y + r.h * 0.5), scale));
        self.frame = cx.new_next_frame();
    }

    fn fly_to_node(&mut self, cx: &mut Cx, index: usize) {
        let r = self.tree.nodes[index].rect;
        self.fly_to(cx, r, 0.95);
    }

    fn select(&mut self, cx: &mut Cx, index: usize) {
        if self.definition_focus.take().is_some() {
            self.set_status(cx, self.summary());
        }
        self.selected = Some(index);
        cx.widget_action(self.uid, CodeMapAction::Selected(self.info(index)));
        self.redraw(cx);
    }

    fn definition_line_rect(&self, index: usize, line: usize) -> Option<R> {
        let node = self.tree.nodes.get(index)?;
        if !self.tree.is_visible(index) { return None; }
        node.code_line_rect(line)
    }

    fn jump_to_definition(&mut self, cx: &mut Cx, target: CallTarget) {
        self.source_anchor = None;
        let definition = &target.definition;
        let Some(index) = self.tree.find(&definition.file) else { return };
        let Some(line) = self.definition_line_rect(index, definition.line) else { return };
        self.flight = None;
        let source = self.tree.nodes[index].lines[definition.line - 1];
        self.flight = Some(definition_camera(line, source.indent as usize, source.len as usize, self.view.size));
        self.frame = cx.new_next_frame();
        self.definition_focus = Some(target.clone());
        self.selected = Some(index);
        self.ensure_text(cx, index);
        cx.widget_action(self.uid, CodeMapAction::Selected(self.info(index)));
        self.set_status(cx, format!("{} · {}", self.summary(), target_description(&target)));
        self.redraw(cx);
    }

    fn call_at(&self, abs: DVec2) -> Option<CallTarget> {
        if !self.view.contains(abs) || self.label_rects.iter().any(|r| r.contains(abs)) { return None; }
        self.call_links.iter().find(|link| link.rect.contains(abs)).map(|link| link.target.clone())
    }

    fn expand(&mut self, cx: &mut Cx, index: usize) {
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
        self.frame = cx.new_next_frame();
        self.redraw(cx);
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

    /// Pick up finished background work.
    fn drain(&mut self, cx: &mut Cx) {
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
                    self.tree.update_weights(self.show_ignored);
                    self.tree.apply_filter(&self.filter, self.hide_tests, None);
                    let text = format!(
                        "{}, scanned in {:.2}s ({})",
                        self.summary(),
                        secs,
                        if used_git { "ignore rules from git" } else { "no git: read .gitignore files" }
                    );
                    self.set_status(cx, text);
                    self.needs_layout = true;
                    self.needs_fit = true;
                    if !matches!(self.evidence_input, EvidenceInput::Static) { self.start_graph(cx); }
                    if used_git {
                        // git history is slower than the scan, so load it separately
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
                    self.set_status(cx, format!("Could not scan: {err}"));
                }
                ScanMsg::Expanded(path, entries) => {
                    if let Some(index) = self.tree.find(&path) {
                        let count = entries.len();
                        self.tree.graft(index, entries);
                        self.after_tree_change();
                        self.selected = Some(index);
                        self.set_status(cx, format!("Read ignored {path}: {} files", fmt_num(count as u64)));
                    }
                }
                ScanMsg::History(Ok(history)) => {
                    self.tree.apply_history(&history);
                    self.history = Some(history);
                    self.tree.compute_heat(self.color_mode);
                    self.set_status(cx, self.summary());
                }
                ScanMsg::History(Err(err)) => {
                    self.set_status(cx, format!("No git history: {err}"));
                }
                ScanMsg::Graph(update) => {
                    self.graph_loading = false;
                    if update.generation != self.graph_generation { self.start_graph(cx); continue; }
                    self.source_anchor = if update.view == ViewUpdate::Preserve { capture_source_anchor(&self.tree, self.view, self.cam_off, self.cam_scale) } else { None };
                    for entry in update.entries { self.tree.insert(entry); }
                    self.content.replace(update.source_texts);
                    self.definition_focus = None;
                    self.call_links.clear();
                    self.needs_layout = true;
                    self.graph = Some(update.graph);
                    self.graph_read_errors = update.read_errors;
                    self.evidence_status = update.status;
                    cx.widget_action(self.uid, CodeMapAction::EvidenceStatus(self.evidence_status.clone()));
                    self.refresh_relationships(cx, update.view);
                }
                ScanMsg::Text(generation, index, text) => {
                    self.content.accept(generation, index, text);
                }
            }
            self.redraw(cx);
        }
    }

    fn after_tree_change(&mut self) {
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
            // the world is the size of the first view, so zoom 1.0 is "fit"
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
        if self.tree.is_empty() || self.cam_scale <= 0.0 {
            return None;
        }
        let p = self.to_world(abs);
        self.tree.hit(p.x, p.y, 3.0 / self.cam_scale)
    }

    fn step_flight(&mut self, cx: &mut Cx) {
        let Some((center, scale)) = self.flight else { return };
        let half = self.view.size * 0.5;
        let now_center = self.cam_off + half / self.cam_scale;
        let k = 0.2;
        // interpolate zoom in log space so zooming in and out feel the same
        let new_scale = (self.cam_scale.ln() + (scale.ln() - self.cam_scale.ln()) * k).exp();
        let new_center = now_center + (center - now_center) * k;
        self.cam_scale = new_scale;
        self.cam_off = new_center - half / new_scale;
        if (new_scale / scale - 1.0).abs() < 0.002 && (new_center - center).length() * new_scale < 0.5 {
            self.cam_scale = scale;
            self.cam_off = center - half / scale;
            self.flight = None;
        } else {
            self.frame = cx.new_next_frame();
        }
        self.redraw(cx);
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

    /// Base color of a file for the current color mode.
    fn file_color(&self, index: usize) -> Vec4f {
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

    fn draw_map(&mut self, cx: &mut Cx2d) {
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
            // search: things off the path to a match fade into the background
            let fade = if searching && self.reachable.is_none() && !node.on_path { 0.22 } else { 1.0 };
            let small = r.size.x.min(r.size.y);
            let border = if small > 8.0 { 1.0 } else { 0.0 };
            match node.kind {
                Kind::Dir => {
                    let shade = 0.07 + 0.03 * (node.depth.min(5) as f32);
                    let color = if node.ignored { vec4(shade * 1.3, shade, shade * 0.8, 1.0) } else { vec4(shade * 0.85, shade, shade * 1.25, 1.0) };
                    block(&mut self.draw_block, cx, r, scale_rgb(color, fade), vec4(0.0, 0.0, 0.0, 0.6), border, if node.ignored { 0.4 } else { 0.0 });
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
                    block(&mut self.draw_block, cx, r, bg, edge, border, if node.ignored { 0.3 } else { 0.0 });
                    quads += 1;
                    quads += self.draw_code_lines(cx, index, fade);
                }
                Kind::Binary => {
                    let base = self.file_color(index);
                    block(&mut self.draw_block, cx, r, scale_rgb(base, 0.35 * fade), scale_rgb(base, 0.6 * fade), border, 0.5);
                    quads += 1;
                    if r.size.x > 60.0 && r.size.y > 18.0 {
                        self.labels.push(Label { rect: r, text: node.name.clone(), dim: true });
                    }
                }
                Kind::Ghost { loading, .. } => {
                    let color = if loading { vec4(0.20, 0.17, 0.08, 1.0) } else { vec4(0.10, 0.09, 0.08, 1.0) };
                    block(&mut self.draw_block, cx, r, scale_rgb(color, fade), vec4(0.45, 0.38, 0.25, fade), border, 1.0);
                    quads += 1;
                    if r.size.x > 60.0 && r.size.y > 18.0 {
                        let text = if loading { format!("{} (reading...)", node.name) } else { format!("{} (ignored)", node.name) };
                        self.labels.push(Label { rect: r, text, dim: true });
                    }
                }
            }
        }
        // outline search matches so they pop out even when small
        if searching {
            for k in 0..self.matches.len().min(5000) {
                let index = self.matches[k];
                let r = self.to_screen(self.tree.nodes[index].rect);
                if r.size.x >= 2.0 && self.tree.nodes[index].weight > 0.0 {
                    block(&mut self.draw_block, cx, r, vec4(0.0, 0.0, 0.0, 0.0), vec4(1.0, 0.85, 0.2, 1.0), 2.0, 0.0);
                }
            }
        }
    }

    /// Returns how many quads were drawn.
    fn draw_code_lines(&mut self, cx: &mut Cx2d, index: usize, fade: f32) -> usize {
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
            self.ensure_text(cx.cx, index);
        }
        let node = &self.tree.nodes[index];
        let code_color = vec4(0.49 * fade, 0.52 * fade, 0.56 * fade, 1.0);
        let comment_color = if self.syntax_colors { vec4(0.30 * fade, 0.40 * fade, 0.30 * fade, 1.0) } else { code_color };
        let mut drawn = 0;
        // only visit the columns and rows that are actually on screen
        let first_col = ((view.pos.x - inner.pos.x) / col_px).floor().max(0.0) as usize;
        let last_col = ((((view.pos.x + view.size.x) - inner.pos.x) / col_px).ceil().max(0.0) as usize).min(cols);
        let first_row = ((view.pos.y - inner.pos.y) / line_px).floor().max(0.0) as usize;
        let last_row = ((((view.pos.y + view.size.y) - inner.pos.y) / line_px).ceil().max(0.0) as usize).min(rows);
        if text_mode {
            self.draw_code.text_style.font_size = (line_px * 0.6) as f32;
        }
        for col in first_col..last_col {
            for row in first_row..last_row {
                let k = col * rows + row;
                let Some(line) = node.lines.get(k) else { break };
                let source_rect = node.code_line_rect(k + 1).unwrap();
                let line_rect = self.to_screen(source_rect);
                let (x, y) = (line_rect.pos.x, line_rect.pos.y);
                if self.definition_focus.as_ref().is_some_and(|target| target.definition.file == node.path && target.definition.line == k + 1) {
                    block(&mut self.draw_block, cx, line_rect, vec4(0.95, 0.73, 0.24, 0.12), vec4(0.95, 0.73, 0.24, 0.9), 1.5, 0.0);
                }
                if text_mode {
                    if let Some(line) = self.content.files.get(&index).and_then(|text| text.lines.get(k)) {
                        self.call_links.extend(draw_source_line(&mut self.draw_code, cx, line, self.call_highlights.line(&node.path, k + 1), self.relationship_root.as_deref(), self.syntax_colors, dvec2(x, y), line_px, fade));
                    }
                } else if line.len > 0 {
                    let color = if line.comment { comment_color } else { code_color };
                    let w = (line.len as f64 * char_px).min(col_px - line.indent as f64 * char_px).max(0.5);
                    let rect = Rect {
                        pos: dvec2(x + line.indent as f64 * char_px, y + line_px * 0.18),
                        size: dvec2(w, (line_px * 0.64).max(0.5)),
                    };
                    block(&mut self.draw_block, cx, rect, color, vec4(0.0, 0.0, 0.0, 0.0), 0.0, 0.0);
                    drawn += 1;
                    drawn += draw_call_strips(&mut self.draw_block, cx, self.call_highlights.line(&node.path, k + 1), self.relationship_root.as_deref(), dvec2(x, y), char_px, line_px, fade);
                }
            }
        }
        if wants_label && self.labels.len() < LABEL_BUDGET {
            self.labels.push(Label { rect: r_screen, text: node.name.clone(), dim: false });
        }
        drawn
    }

    /// Load a file's text for the fully zoomed-in view (cached).
    fn ensure_text(&mut self, cx: &mut Cx, index: usize) {
        if self.content.files.contains_key(&index) || self.content.pending.contains(&index) { return; }
        let relative = self.tree.nodes[index].path.clone();
        let source = self.content.originals.get(&relative).cloned();
        if self.text_worker.as_ref().is_some_and(|tx| tx.try_send((self.content.generation, index, self.root.join(&relative), relative, source)).is_ok()) {
            self.content.pending.insert(index);
            self.pending += 1;
            self.frame = cx.new_next_frame();
        }
    }

    fn outline(&mut self, cx: &mut Cx2d, index: usize, color: Vec4f, width: f32) {
        let r = self.to_screen(self.tree.nodes[index].rect);
        block(&mut self.draw_block, cx, r, vec4(0.0, 0.0, 0.0, 0.0), color, width, 0.0);
    }

    fn draw_labels(&mut self, cx: &mut Cx2d) {
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
            let w = (text.chars().count() as f64 * 5.6 + 8.0).min(room + 4.0);
            let label_rect = Rect { pos: pos - dvec2(3.0, 1.0), size: dvec2(w, 14.0) };
            self.label_rects.push(label_rect);
            block(&mut self.draw_block, cx, label_rect, vec4(0.0, 0.0, 0.0, 0.72), vec4(0.0, 0.0, 0.0, 0.0), 0.0, 0.0);
            self.draw_label.color = if label.dim { vec4(0.75, 0.68, 0.55, 1.0) } else { vec4(0.92, 0.94, 0.96, 1.0) };
            self.draw_label.draw_abs(cx, pos, &text);
        }
        self.labels = labels;
        self.labels.clear();
    }

    fn handle_input(&mut self, cx: &mut Cx, hit: Hit) {
        match hit {
            Hit::FingerMove(e) => {
                if let Some(mut drag) = self.drag {
                    if (e.abs - drag.start).length() > 4.0 {
                        drag.moved = true;
                    }
                    if drag.moved {
                        cx.set_cursor(MouseCursor::Grabbing);
                        self.cam_off = drag.start_off - (e.abs - drag.start) / self.cam_scale;
                        self.redraw(cx);
                    }
                    self.drag = Some(drag);
                }
            }
            Hit::FingerScroll(e) => {
                self.source_anchor = None;
                self.flight = None;
                // zoom around the point under the mouse
                let factor = (-e.scroll.y * 0.01).exp();
                let anchor = self.to_world(e.abs);
                let min_scale = (self.view.size.x / self.world.w).min(self.view.size.y / self.world.h) * 0.5;
                self.cam_scale = (self.cam_scale * factor).clamp(min_scale, min_scale * 200_000.0);
                self.cam_off = anchor - (e.abs - self.view.pos) / self.cam_scale;
                self.hover = self.hit(e.abs);
                self.redraw(cx);
            }
            _ => {}
        }
    }
}

impl Widget for CodeMap {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.call_links.clear();
        self.label_rects.clear();
        // begin_turtle clips everything we draw to the widget's own rectangle
        cx.begin_turtle(walk, self.layout);
        let rect = cx.turtle().rect();
        self.draw_bg.draw_abs(cx, rect);
        self.view = rect;

        if self.tree.is_empty() {
            self.draw_label.color = vec4(0.7, 0.7, 0.7, 1.0);
            let message = if self.message.is_empty() { "No folder opened".to_string() } else { self.message.clone() };
            self.draw_label.draw_abs(cx, rect.pos + dvec2(16.0, 16.0), &message);
            cx.end_turtle_with_area(&mut self.area);
            return DrawStep::done();
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
            self.draw_label.color = vec4(0.7, 0.7, 0.7, 1.0);
            self.draw_label.draw_abs(cx, rect.pos + dvec2(16.0, 16.0), "No files match this selection. Clear the filter or choose All files.");
            cx.end_turtle_with_area(&mut self.area);
            return DrawStep::done();
        }
        if self.needs_fit && rect.size.x > 0.0 {
            self.cam_scale = (rect.size.x / self.world.w).min(rect.size.y / self.world.h);
            let half = rect.size * 0.5;
            self.cam_off = dvec2(self.world.w * 0.5, self.world.h * 0.5) - half / self.cam_scale;
            self.needs_fit = false;
        }

        self.labels.clear();
        self.draw_map(cx);
        if let Some(h) = self.hover {
            if self.tree.is_visible(h) {
                self.outline(cx, h, vec4(1.0, 1.0, 1.0, 0.5), 1.5);
            }
        }
        if let Some(s) = self.selected {
            if self.tree.is_visible(s) {
                self.outline(cx, s, vec4(0.35, 0.85, 1.0, 1.0), 2.5);
            }
        }
        self.draw_labels(cx);
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.frame.is_event(event).is_some() {
            self.drain(cx);
            self.step_flight(cx);
            if self.pending > 0 {
                self.frame = cx.new_next_frame();
            }
        }
        let hit = event.hits(cx, self.area);
        match &hit {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                cx.set_cursor(if self.call_at(e.abs).is_some() { MouseCursor::Hand } else { MouseCursor::Default });
                let hover = self.hit(e.abs);
                if hover != self.hover {
                    self.hover = hover;
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                cx.set_cursor(MouseCursor::Default);
                if self.hover.take().is_some() {
                    self.redraw(cx);
                }
            }
            Hit::FingerDown(e) => {
                self.source_anchor = None;
                self.flight = None;
                self.drag = Some(Drag {
                    start: e.abs,
                    start_off: self.cam_off,
                    moved: false,
                    taps: e.tap_count,
                    follow_link: e.device.is_primary_hit() && !e.modifiers.shift,
                });
            }
            Hit::FingerUp(e) => {
                cx.set_cursor(MouseCursor::Default);
                if let Some(drag) = self.drag.take() {
                    if !drag.moved {
                        if drag.follow_link {
                            if let Some(target) = self.call_at(drag.start).filter(|target| self.call_at(e.abs).as_ref() == Some(target)) {
                                self.jump_to_definition(cx, target);
                                return;
                            }
                        }
                        if let Some(index) = self.hit(e.abs) {
                            self.select(cx, index);
                            if matches!(self.tree.nodes[index].kind, Kind::Ghost { .. }) {
                                self.expand(cx, index);
                            } else if drag.taps >= 2 {
                                self.fly_to_node(cx, index);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if self.cam_scale <= 0.0 {
            return;
        }
        self.handle_input(cx, hit);
    }
}

// `CodeMapRef` is generated by `#[derive(Widget)]`: a handle the app uses to
// reach this widget from outside.
impl CodeMapRef {
    pub fn set_evidence_input(&self, cx: &mut Cx, input: EvidenceInput) {
        if let Some(mut inner) = self.borrow_mut() { inner.set_evidence_input(cx, input); }
    }
    pub fn set_evidence_mode(&self, cx: &mut Cx, mode: usize) {
        if let Some(mut inner) = self.borrow_mut() { inner.set_evidence_mode(cx, mode); }
    }
    pub fn is_loading(&self) -> bool {
        self.borrow().is_some_and(|inner| inner.pending > 0)
    }
    pub fn set_syntax_colors(&self, cx: &mut Cx, on: bool) {
        if let Some(mut inner) = self.borrow_mut() { inner.set_syntax_colors(cx, on); }
    }
    pub fn set_filter(&self, cx: &mut Cx, query: &str) {
        if let Some(mut inner) = self.borrow_mut() { inner.set_filter(cx, query); }
    }
    pub fn set_hide_tests(&self, cx: &mut Cx, hide: bool) {
        if let Some(mut inner) = self.borrow_mut() { inner.set_hide_tests(cx, hide); }
    }
    pub fn set_relationship_mode(&self, cx: &mut Cx, mode: usize) {
        if let Some(mut inner) = self.borrow_mut() { inner.set_relationship_mode(cx, mode); }
    }
    pub fn set_relationship_depth(&self, cx: &mut Cx, depth: usize) {
        if let Some(mut inner) = self.borrow_mut() { inner.set_relationship_depth(cx, depth); }
    }
    pub fn trace_selected(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() { inner.trace_selected(cx); }
    }
    pub fn open(&self, cx: &mut Cx, root: PathBuf) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.open(cx, root);
        }
    }
    pub fn set_show_ignored(&self, cx: &mut Cx, show: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_show_ignored(cx, show);
        }
    }
    pub fn fit(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.fit(cx);
        }
    }
    pub fn set_color_mode(&self, cx: &mut Cx, mode: ColorMode) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_color_mode(cx, mode);
        }
    }
    pub fn set_search(&self, cx: &mut Cx, query: &str) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_search(cx, query);
        }
    }
    pub fn next_match(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.next_match(cx);
        }
    }
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

fn draw_source_line(draw: &mut DrawText, cx: &mut Cx2d, line: &crate::syntax::HighlightedLine, marks: &[CallMark], root: Option<&str>, syntax: bool, pos: DVec2, line_height: f64, fade: f32) -> Vec<CallLink> {
    let ranges = call_colors::link_ranges(marks, line.text.chars().count().min(COL_CHARS as usize));
    let mut links = Vec::new();
    let mut x = pos.x;
    let mut column = 0;
    for run in call_colors::paint_line(line, marks, root, syntax, COL_CHARS as usize) {
        let [r, g, b] = run.color;
        draw.color = vec4(r * fade, g * fade, b * fade, 1.0);
        let width = source_text_width(draw, cx.cx, run.text);
        let end = column + run.text.chars().count();
        for range in &ranges {
            let (start, stop) = (range.start.max(column), range.end.min(end));
            if stop <= start { continue; }
            let prefix = run.text.char_indices().nth(start - column).map_or(run.text.len(), |(i, _)| i);
            let suffix = run.text.char_indices().nth(stop - column).map_or(run.text.len(), |(i, _)| i);
            let left = source_text_width(draw, cx.cx, &run.text[..prefix]);
            let right = source_text_width(draw, cx.cx, &run.text[..suffix]);
            links.push(CallLink {
                rect: Rect { pos: dvec2(x + left, pos.y), size: dvec2(right - left, line_height) },
                target: CallTarget { definition: range.definition.clone(), inference: range.inference.map(str::to_owned), evidence: range.evidence },
            });
        }
        draw.draw_abs(cx, dvec2(x, pos.y), run.text);
        x += width;
        column = end;
    }
    links
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

fn source_text_width(draw: &DrawText, cx: &mut Cx, text: &str) -> f64 {
    if text.is_empty() { return 0.0; }
    draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text)
        .rows.first().map_or(0.0, |row| row.width_in_lpxs as f64)
}

fn draw_call_strips(draw: &mut DrawBlock, cx: &mut Cx2d, marks: &[CallMark], root: Option<&str>, pos: DVec2, char_width: f64, line_height: f64, fade: f32) -> usize {
    let mut count = 0;
    for mark in marks {
        let start = mark.start.min(COL_CHARS as usize);
        let end = mark.end.min(COL_CHARS as usize);
        if end <= start { continue; }
        let [r, g, b] = call_colors::file_rgb(&mark.target, root);
        block(draw, cx, Rect { pos: pos + dvec2(start as f64 * char_width, line_height * 0.18), size: dvec2((end - start) as f64 * char_width, (line_height * 0.64).max(0.5)) }, vec4(r * fade, g * fade, b * fade, 1.0), vec4(0.0, 0.0, 0.0, 0.0), 0.0, 0.0);
        count += 1;
    }
    count
}

fn block(draw: &mut DrawBlock, cx: &mut Cx2d, rect: Rect, color: Vec4f, edge: Vec4f, border: f32, hatch: f32) {
    draw.color = color;
    draw.edge = edge;
    draw.border = border;
    draw.hatch = hatch;
    draw.draw_abs(cx, rect);
}

/// A stable color per file extension, with hand-picked ones for common types.
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

/// Heatmap ramp: cold blue, through magenta and orange, to hot yellow.
/// Grey when a file has no git history.
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
    vec4(c.x * k, c.y * k, c.z * k, c.w)
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
