use crate::{canvas::Canvas, editor::Editor, evidence_input::EvidenceInput, map::Map, model::{ColorMode, R}, platform::{self, Accessibility, AccessibilityRole, Backend, Cursor, Event, Quad}};
use crate::file_tree::{FileTree, ROW_HEIGHT, is_folder};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Instant};

const FONT: f64 = 12.0;
const TEXT: [f32; 4] = [0.86, 0.89, 0.92, 1.0];
const MUTED: [f32; 4] = [0.58, 0.64, 0.69, 1.0];
const EDGE: [f32; 4] = [0.25, 0.30, 0.35, 1.0];
const ACCENT: [f32; 4] = [0.30, 0.72, 0.90, 1.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field { Search, Filter, Evidence }
impl Field {
    fn index(self) -> usize { match self { Self::Search => 0, Self::Filter => 1, Self::Evidence => 2 } }
    fn id(self) -> &'static str { match self { Self::Search => "search", Self::Filter => "filter_query", Self::Evidence => "evidence_path" } }
}

#[derive(Clone, Debug)]
enum Action { Field(Field), Button(&'static str), Toggle(&'static str), Drop(&'static str), Choose(&'static str, usize), TreePane, TreeRow(usize), TreeToggle(usize), TreeScrollbar(bool), None }

#[derive(Clone)]
struct Widget { id: String, ty: &'static str, rect: R, text: String, value: Option<String>, checked: Option<bool>, action: Action }

pub struct App {
    pub map: Map,
    pub running: bool,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    editors: [Editor; 3],
    widgets: Vec<Widget>,
    focus: Option<String>,
    pressed: Option<String>,
    editing_drag: bool,
    pressed_clicks: u32,
    tree_shown: bool,
    file_tree: FileTree,
    tree_rect: R,
    tree_body: R,
    tree_scroll_drag: Option<(bool, f64, f64)>,
    pointer: (f64, f64),
    tooltip_hover: Option<(String, Instant)>,
    tooltip_visible: bool,
    popup: Option<(&'static str, usize)>,
    map_rect: R,
    inspector: R,
    inspector_scroll: f64,
    inspector_limit: f64,
    detail_cache: Option<(String, f64, Vec<String>)>,
    buffer: Option<Vec<Quad>>,
    pub local_status: Option<String>,
    started: Instant,
}

impl App {
    pub fn new(root: PathBuf, evidence: Option<PathBuf>) -> Result<Self, String> {
        let mut editors = [Editor::default(), Editor::default(), Editor::default()];
        let mut map = Map::new(root);
        if let Some(path) = evidence {
            editors[2] = Editor::new(path.to_string_lossy().into_owned())?;
            map.set_evidence_input(EvidenceInput::File(path));
        }
        Ok(Self { map, running: true, width: 1400.0, height: 900.0, scale: 1.0, editors,
            widgets: Vec::new(), focus: None, pressed: None, editing_drag: false, pressed_clicks: 1, tree_shown: false, file_tree: FileTree::default(), tree_rect: R::default(), tree_body: R::default(), tree_scroll_drag: None, pointer: (0.0, 0.0), tooltip_hover: None, tooltip_visible: false,
            popup: None, map_rect: R::default(), inspector: R::default(), inspector_scroll: 0.0,
            inspector_limit: 0.0, detail_cache: None, buffer: None, local_status: None, started: Instant::now() })
    }

    pub fn tick(&mut self) -> bool {
        let dirty = self.map.tick();
        if !self.tooltip_visible && self.tooltip_hover.as_ref().is_some_and(|(_, since)| since.elapsed().as_millis() >= 500) {
            self.tooltip_visible = true;
            return true;
        }
        dirty
    }

    fn dismiss_tooltip(&mut self) {
        self.tooltip_hover = None;
        self.tooltip_visible = false;
    }

    fn update_tooltip_hover(&mut self) {
        let target = if self.popup.is_none() && self.pressed.is_none() && !self.editing_drag && self.tree_scroll_drag.is_none() {
            self.widgets.iter().rev().find(|widget| widget.rect.contains(self.pointer.0, self.pointer.1) && control_help(&widget.action).is_some()).map(|widget| widget.id.as_str())
        } else { None };
        if target != self.tooltip_hover.as_ref().map(|(id, _)| id.as_str()) {
            self.tooltip_hover = target.map(|id| (id.to_owned(), Instant::now()));
            self.tooltip_visible = false;
        }
    }

    fn add(&mut self, id: &str, ty: &'static str, rect: R, text: impl Into<String>, value: Option<String>, checked: Option<bool>, action: Action) {
        self.widgets.push(Widget { id: id.into(), ty, rect, text: text.into(), value, checked, action });
    }

    fn layout(&mut self) {
        self.widgets.clear();
        let groups: &[&[(&str, f64)]] = &[
            &[("search", 250.0), ("fit_button", 44.0), ("show_ignored", 126.0), ("syntax_colors", 130.0), ("color_mode", 160.0), ("hide_tests", 142.0), ("file_tree_toggle", 112.0)],
            &[("filter_query", 300.0), ("relationship_mode", 174.0), ("trace_selected", 136.0), ("relationship_depth", 90.0), ("evidence_mode", 180.0)],
            &[("evidence_path", 340.0), ("evidence_load", 114.0), ("evidence_capture", 114.0), ("evidence_clear", 92.0)],
        ];
        let mut y = 8.0;
        let mut last_x = 8.0;
        for group in groups {
            let mut x = 8.0;
            for &(id, requested) in *group {
                let w = requested.min((self.width - 16.0).max(1.0));
                if x > 8.0 && x + w > self.width - 8.0 { x = 8.0; y += 34.0; }
                let rect = R { x, y, w, h: 26.0 };
                match id {
                    "search" => self.field(Field::Search, rect, "Search files · Enter to fly"),
                    "filter_query" => self.field(Field::Filter, rect, "Filter: app/ | webpack/ -spec/"),
                    "evidence_path" => self.field(Field::Evidence, rect, "Optional evidence snapshot.json"),
                    "fit_button" => self.add(id, "Button", rect, "Fit", None, None, Action::Button("fit_button")),
                    "trace_selected" => self.add(id, "Button", rect, "Trace selected", None, None, Action::Button("trace_selected")),
                    "evidence_load" => self.add(id, "Button", rect, "Load snapshot", None, None, Action::Button("evidence_load")),
                    "evidence_capture" => self.add(id, "Button", rect, "Capture Rails", None, None, Action::Button("evidence_capture")),
                    "evidence_clear" => self.add(id, "Button", rect, "Static only", None, None, Action::Button("evidence_clear")),
                    "file_tree_toggle" => self.toggle(id, rect, "File tree", self.tree_shown),
                    "show_ignored" => self.toggle(id, rect, "Show ignored", self.map.show_ignored),
                    "syntax_colors" => self.toggle(id, rect, "Syntax colors", self.map.syntax_colors),
                    "hide_tests" => self.toggle(id, rect, "Hide specs/tests", self.map.hide_tests),
                    "color_mode" => self.drop(id, rect, match self.map.color_mode { ColorMode::FileType => 0, ColorMode::Recent => 1, ColorMode::Churn => 2 }),
                    "relationship_mode" => self.drop(id, rect, self.map.relationship_mode),
                    "relationship_depth" => self.drop(id, rect, [1,2,3,5,8].iter().position(|&n| n == self.map.relationship_depth).unwrap_or(0)),
                    "evidence_mode" => self.drop(id, rect, self.map.evidence_mode),
                    _ => {}
                }
                x += w + 8.0;
            }
            last_x = x;
            y += 34.0;
        }
        if self.width - last_x > 180.0 {
            self.add("evidence_status", "Label", R { x: last_x, y: y - 32.0, w: self.width - last_x - 8.0, h: 24.0 }, self.map.evidence_status().to_owned(), None, None, Action::None);
        } else {
            self.add("evidence_status", "Label", R { x: 8.0, y, w: (self.width - 16.0).max(1.0), h: 20.0 }, self.map.evidence_status().to_owned(), None, None, Action::None);
            y += 24.0;
        }
        let root = self.map.relationship_root().unwrap_or("Select a source file, then trace its calls").to_owned();
        self.add("relationship_root", "Label", R { x: 8.0, y, w: (self.width - 16.0).max(1.0), h: 18.0 }, root, None, None, Action::None);
        let top = y + 22.0;
        let inspector_w = if self.width >= 900.0 { 340.0 } else { 260.0_f64.min(self.width * 0.45) };
        let tree_w = if self.tree_shown { (self.width * 0.24).clamp(150.0, 300.0) } else { 0.0 };
        self.tree_rect = R { x: 0.0, y: top, w: tree_w, h: (self.height - top - 28.0).max(1.0) };
        self.tree_body = R { x: 0.0, y: top+28.0, w: (tree_w-10.0).max(1.0), h: (self.tree_rect.h-38.0).max(1.0) };
        self.map_rect = R { x: tree_w, y: top, w: (self.width - inspector_w - tree_w).max(1.0), h: self.tree_rect.h };
        self.inspector = R { x: self.map_rect.x+self.map_rect.w, y: top, w: inspector_w, h: self.map_rect.h };
    }

    fn field(&mut self, field: Field, rect: R, placeholder: &str) {
        self.add(field.id(), "TextInput", rect, placeholder, Some(self.editors[field.index()].text.clone()), None, Action::Field(field));
    }
    fn toggle(&mut self, id: &'static str, rect: R, label: &str, checked: bool) {
        self.add(id, "CheckBox", rect, label, None, Some(checked), Action::Toggle(id));
    }
    fn drop(&mut self, id: &'static str, rect: R, index: usize) {
        let text = options(id).get(index).unwrap_or(&"?").to_string();
        self.add(id, "DropDown", rect, text.clone(), Some(text), None, Action::Drop(id));
    }

    pub fn draw(&mut self, backend: &mut Backend) -> usize {
        (self.width, self.height, self.scale) = backend.size();
        self.layout();
        let screen = R { x: 0.0, y: 0.0, w: self.width, h: self.height };
        let info = self.map.selection_info();
        let mut canvas = match self.buffer.take() { Some(buffer) => Canvas::with_buffer(backend, buffer), None => Canvas::new(backend) };
        canvas.rect(screen, [0.035,0.045,0.055,1.0], [0.0;4], 0.0, 0.0, screen);
        canvas.rect(R { x: 0.0, y: 0.0, w: self.width, h: self.map_rect.y }, [0.08,0.10,0.12,1.0], [0.0;4], 0.0, 0.0, screen);
        self.map.draw(&mut canvas, self.map_rect);
        canvas.rect(self.inspector, [0.075,0.09,0.11,1.0], [0.0;4], 0.0, 0.0, screen);
        for widget in &self.widgets {
            let focus = self.focus.as_deref() == Some(&widget.id);
            let hovered = widget.rect.contains(self.pointer.0, self.pointer.1);
            if let Action::Field(field) = widget.action {
                let editor = &mut self.editors[field.index()];
                canvas.rect(widget.rect, [0.025,0.035,0.045,1.0], if focus { ACCENT } else { EDGE }, 1.0, 0.0, screen);
                let inset = inset(widget.rect, 6.0, 4.0);
                let display = editor.display_text().into_owned();
                let caret = editor.display_caret_byte().min(display.len());
                let advance = canvas.text_width(&display[..caret], FONT, true);
                if focus {
                    if advance - editor.scroll_x > inset.w - 2.0 { editor.scroll_x = (advance - inset.w + 2.0).max(0.0); }
                    if advance - editor.scroll_x < 0.0 { editor.scroll_x = advance; }
                }
                let x = inset.x - editor.scroll_x;
                if focus && !editor.is_composing() {
                    let selection = editor.selection_bytes();
                    let left = canvas.text_width(&display[..selection.start], FONT, true);
                    let right = canvas.text_width(&display[..selection.end], FONT, true);
                    canvas.rect(R { x: x + left, y: inset.y, w: right - left, h: inset.h }, [0.16,0.34,0.49,1.0], [0.0;4], 0.0, 0.0, inset);
                }
                if display.is_empty() { canvas.text(&widget.text, inset.x, inset.y, FONT, MUTED, inset, true); }
                else { canvas.text(&display, x, inset.y, FONT, TEXT, inset, true); }
                if let Some(range) = editor.composition_bytes() {
                    let left = canvas.text_width(&display[..range.start], FONT, true);
                    let right = canvas.text_width(&display[..range.end], FONT, true);
                    canvas.rect(R { x: x+left, y: inset.y+inset.h-1.0, w: right-left, h: 1.0 }, ACCENT, [0.0;4], 0.0, 0.0, inset);
                }
                if focus { canvas.rect(R { x: x+advance, y: inset.y, w: 1.0, h: inset.h }, TEXT, [0.0;4], 0.0, 0.0, inset); }
            } else if widget.ty == "Label" {
                canvas.text(&widget.text, widget.rect.x, widget.rect.y + 2.0, 11.0, MUTED, widget.rect, true);
            } else {
                let fill = if self.pressed.as_deref() == Some(&widget.id) { [0.18,0.25,0.30,1.0] } else if hovered { [0.14,0.18,0.22,1.0] } else { [0.10,0.13,0.16,1.0] };
                canvas.rect(widget.rect, fill, if focus { ACCENT } else { EDGE }, 1.0, 0.0, screen);
                let prefix = if let Some(checked) = widget.checked { if checked { "[x] " } else { "[ ] " } } else { "" };
                let suffix = if widget.ty == "DropDown" { " v" } else { "" };
                canvas.text(&format!("{prefix}{}{suffix}", widget.text), widget.rect.x+6.0, widget.rect.y+5.0, FONT, TEXT, inset(widget.rect, 4.0, 2.0), true);
            }
        }
        let title = info.as_ref().map_or("INSPECTOR", |i| i.title.as_str());
        let path = info.as_ref().map_or("Click a file to inspect it", |i| i.path.as_str());
        let title_rect = R { x: self.inspector.x+12.0, y: self.inspector.y+12.0, w: self.inspector.w-24.0, h: 20.0 };
        let path_rect = R { y: title_rect.y+24.0, ..title_rect };
        canvas.text(title, title_rect.x, title_rect.y, 14.0, TEXT, title_rect, true);
        canvas.text(path, path_rect.x, path_rect.y, 10.0, MUTED, path_rect, true);
        let details = info.as_ref().map_or("Scroll to zoom. Drag to pan.\nDouble-click a file to fly to it.\nColored calls jump to their target.\n\nTab moves between controls.\nCommand-F focuses search.\nCommand-Shift-F focuses filters.\n\nOptional app evidence is off by default.", |i| i.details.as_str());
        let details_rect = R { x: title_rect.x, y: path_rect.y+28.0, w: title_rect.w, h: (self.inspector.h-88.0).max(1.0) };
        if self.detail_cache.as_ref().is_none_or(|(old, width, _)| old != details || *width != details_rect.w) {
            self.detail_cache = Some((details.into(), details_rect.w, wrap(&canvas, details, details_rect.w, FONT)));
            self.inspector_scroll = 0.0;
        }
        let lines = &self.detail_cache.as_ref().unwrap().2;
        self.inspector_limit = (lines.len() as f64 * 17.0 - details_rect.h).max(0.0);
        self.inspector_scroll = self.inspector_scroll.clamp(0.0, self.inspector_limit);
        for (i, line) in lines.iter().enumerate() {
            let y = details_rect.y + i as f64 * 17.0 - self.inspector_scroll;
            if y+17.0 >= details_rect.y && y < details_rect.y+details_rect.h { canvas.text(line, details_rect.x, y, FONT, MUTED, details_rect, true); }
        }
        let status_rect = R { x: 8.0, y: self.height-23.0, w: (self.width-48.0).max(1.0), h: 20.0 };
        let status = self.local_status.as_deref().unwrap_or_else(|| self.map.status()).to_owned();
        canvas.text(&status, status_rect.x, status_rect.y, 11.0, MUTED, status_rect, true);
        if self.map.loading() {
            let x = self.width-31.0;
            let y = self.height-25.0;
            let alpha = (0.65+0.35*(self.started.elapsed().as_secs_f64()*std::f64::consts::TAU/1.5).sin()) as f32;
            let gold = [1.0,0.78,0.22,alpha];
            for rect in [R{x:x+6.0,y,w:12.0,h:11.0}, R{x,y:y+11.0,w:12.0,h:11.0}, R{x:x+12.0,y:y+11.0,w:12.0,h:11.0}] { canvas.triangle(rect, gold, screen); }
        }
        let drops = canvas.dropped_quads;
        self.buffer = Some(canvas.into_buffer());
        self.add("map", "CodeMap", self.map_rect, "Code map", None, None, Action::None);
        self.add("info_title", "Label", title_rect, title, None, None, Action::None);
        self.add("info_path", "Label", path_rect, path, None, None, Action::None);
        self.add("info_details", "Label", details_rect, details, None, None, Action::None);
        self.add("status", "Label", status_rect, status, None, None, Action::None);
        for (i, link) in self.map.links().iter().enumerate() {
            if !self.map.has_link_at(link.rect.x+link.rect.w*0.5, link.rect.y+link.rect.h*0.5) { continue; }
            self.widgets.push(Widget { id: format!("call_{i}"), ty: "CallLink", rect: link.rect, text: format!("{} at {}:{} · {}", link.target.definition.name, link.target.definition.file, link.target.definition.line, link.target.evidence.label()), value: None, checked: None, action: Action::None });
        }
        self.draw_file_tree(backend, screen);
        self.draw_popup(backend, screen);
        self.draw_tooltip(backend, screen);
        self.sync_input(backend);
        let elements: Vec<_> = self.widgets.iter().filter(|widget| !matches!(widget.action, Action::None)).map(|widget| Accessibility {
            id: id_hash(&widget.id), role: match widget.ty { "TextInput" => AccessibilityRole::Text, "CheckBox" => AccessibilityRole::CheckBox, "DropDown" => AccessibilityRole::Popup, _ => AccessibilityRole::Button },
            enabled: true, focused: self.focus.as_deref() == Some(&widget.id) || (self.focus.as_deref() == Some("file_tree") && matches!(widget.action, Action::TreeRow(i) if Some(i) == self.file_tree.focus)), rect: [widget.rect.x,widget.rect.y,widget.rect.w,widget.rect.h], label: &widget.text,
            value: widget.value.as_deref().unwrap_or(if widget.checked == Some(true) { "1" } else { "0" }),
        }).collect();
        backend.accessibility(&elements);
        drops
    }

    fn tree_content_width(&self) -> f64 { self.file_tree.content_width }

    fn browse_select(&mut self, index: usize, double: bool) {
        self.file_tree.focus = Some(index);
        if double && is_folder(self.map.tree().nodes[index].kind) { self.file_tree.toggle(self.map.tree(), index); }
        self.map.select_node(index, double && !is_folder(self.map.tree().nodes[index].kind));
        self.map.read_ignored_node(index);
    }

    fn tree_key(&mut self, code: u32) {
        let current = self.file_tree.focus.or(self.file_tree.rows.first().copied());
        let target = match code {
            platform::KEY_DOWN => self.file_tree.adjacent(1),
            platform::KEY_UP => self.file_tree.adjacent(-1),
            platform::KEY_HOME => self.file_tree.rows.first().copied(),
            platform::KEY_END => self.file_tree.rows.last().copied(),
            platform::KEY_LEFT => current.and_then(|i| {
                if is_folder(self.map.tree().nodes[i].kind) && self.file_tree.is_expanded(self.map.tree(),i) { self.file_tree.toggle(self.map.tree(),i); None }
                else { self.map.tree().nodes[i].parent }
            }),
            platform::KEY_RIGHT => current.and_then(|i| {
                if !is_folder(self.map.tree().nodes[i].kind) { return None; }
                if !self.file_tree.is_expanded(self.map.tree(),i) {
                    self.map.read_ignored_node(i);
                    self.file_tree.toggle(self.map.tree(),i);
                    None
                } else { self.file_tree.rows.iter().position(|&index| index == i).and_then(|row| self.file_tree.rows.get(row+1)).copied().filter(|&child| self.map.tree().nodes[child].parent == Some(i)) }
            }),
            platform::KEY_RETURN => { if let Some(i)=current { self.map.select_node(i,true); self.map.read_ignored_node(i); } None },
            _ => None,
        };
        if let Some(index)=target { self.map.select_node(index,false); self.file_tree.focus=Some(index); self.file_tree.reveal_pending=true; }
    }

    fn draw_file_tree(&mut self, backend: &mut Backend, screen: R) {
        if !self.tree_shown { return; }
        self.file_tree.sync(self.map.tree(), self.map.tree_revision(), self.map.selected_node(), self.map.selection_revision());
        let range = self.file_tree.visible_range(self.map.tree(), self.tree_body.w, self.tree_body.h);
        let max_x = (self.tree_content_width()-self.tree_body.w).max(0.0);
        self.file_tree.scroll_x = self.file_tree.scroll_x.clamp(0.0,max_x);
        let mut widgets = Vec::new();
        let mut canvas = Canvas::with_buffer(backend,self.buffer.take().unwrap_or_default());
        canvas.rect(self.tree_rect,[0.06,0.075,0.09,1.0],EDGE,1.0,0.0,screen);
        canvas.text("FILES",8.0,self.tree_rect.y+7.0,FONT,TEXT,self.tree_rect,true);
        self.widgets.push(Widget { id:"file_tree".into(), ty:"FileTree", rect:self.tree_body, text:"Files".into(),value:None,checked:None,action:Action::TreePane });
        for row in range {
            let index = self.file_tree.rows[row];
            let node = &self.map.tree().nodes[index];
            let y = self.tree_body.y + row as f64*ROW_HEIGHT-self.file_tree.scroll_y;
            let row_rect=R{x:0.0,y,w:self.tree_body.w,h:ROW_HEIGHT};
            let Some(hit) = crate::math::intersection(row_rect,self.tree_body) else { continue };
            let selected = self.map.selected_node()==Some(index);
            let focused = self.focus.as_deref()==Some("file_tree") && self.file_tree.focus==Some(index);
            if selected || focused { canvas.rect(row_rect,if selected {[0.12,0.27,0.36,1.0]}else{[0.11,0.15,0.18,1.0]},if focused{ACCENT}else{[0.0;4]},if focused{1.0}else{0.0},0.0,self.tree_body); }
            let x=8.0+node.depth as f64*16.0-self.file_tree.scroll_x;
            let folder=is_folder(node.kind);
            widgets.push(Widget{id:format!("tree_row_{index}"),ty:"TreeRow",rect:hit,text:node.name.clone(),value:Some(node.path.clone()),checked:Some(selected),action:Action::TreeRow(index)});
            if folder {
                let disclosure=R{x,y,w:18.0,h:ROW_HEIGHT};
                canvas.text(if self.file_tree.is_expanded(self.map.tree(),index){"v"}else{">"},x+2.0,y+4.0,FONT,MUTED,self.tree_body,true);
                if let Some(rect)=crate::math::intersection(disclosure,self.tree_body) { widgets.push(Widget{id:format!("tree_toggle_{index}"),ty:"TreeDisclosure",rect,text:format!("Toggle {}",node.name),value:Some(node.path.clone()),checked:Some(self.file_tree.is_expanded(self.map.tree(),index)),action:Action::TreeToggle(index)}); }
            } else { canvas.rect(R{x:x+5.0,y:y+8.0,w:6.0,h:7.0},self.map.file_color(index),[0.0;4],0.0,0.0,self.tree_body); }
            let suffix=if matches!(node.kind,crate::model::Kind::Ghost{loading:true,..}){" [loading]"}else if node.ignored{" [ignored]"}else{""};
            let color=if node.ignored { MUTED } else if !self.editors[0].text.is_empty() && node.lit { [1.0,0.8,0.32,1.0] } else {TEXT};
            canvas.text(&format!("{}{suffix}",node.name),x+21.0,y+4.0,FONT,color,self.tree_body,true);
        }
        if self.file_tree.rows.is_empty() {canvas.text(if self.map.loading(){"Scanning…"}else{"No matching files"},8.0,self.tree_body.y+8.0,FONT,MUTED,self.tree_body,true);}
        for horizontal in [false,true] {
            let (extent,viewport)=if horizontal {(self.tree_content_width(),self.tree_body.w)}else{(self.file_tree.rows.len() as f64*ROW_HEIGHT,self.tree_body.h)};
            if extent<=viewport {continue;}
            let offset=if horizontal {self.file_tree.scroll_x}else{self.file_tree.scroll_y};
            let thumb=(viewport*viewport/extent).max(20.0).min(viewport);
            let position=offset/(extent-viewport)*(viewport-thumb);
            let rect=if horizontal{R{x:position,y:self.tree_body.y+self.tree_body.h+2.0,w:thumb,h:6.0}}else{R{x:self.tree_body.w+2.0,y:self.tree_body.y+position,w:6.0,h:thumb}};
            canvas.rect(rect,[0.35,0.43,0.49,1.0],[0.0;4],0.0,0.0,self.tree_rect);
            widgets.push(Widget{id:format!("tree_scroll_{}",if horizontal{"x"}else{"y"}),ty:"ScrollBar",rect,text:"Scroll files".into(),value:None,checked:None,action:Action::TreeScrollbar(horizontal)});
        }
        self.buffer=Some(canvas.into_buffer());
        self.widgets.extend(widgets);
    }

    fn draw_popup(&mut self, backend: &mut Backend, screen: R) {
        let Some((id, selected)) = self.popup else { return };
        let Some(anchor) = self.widgets.iter().find(|widget| widget.id == id).map(|widget| widget.rect) else { self.popup = None; return };
        let choices = options(id);
        let h = choices.len() as f64 * 27.0;
        let y = if anchor.y+anchor.h+h+2.0 < self.height { anchor.y+anchor.h+2.0 } else { (anchor.y-h-2.0).max(0.0) };
        let mut canvas = Canvas::with_buffer(backend, self.buffer.take().unwrap_or_default());
        for (index, text) in choices.iter().enumerate() {
            let rect = R { x: anchor.x, y: y+index as f64*27.0, w: anchor.w.max(175.0).min(self.width-anchor.x), h: 27.0 };
            canvas.rect(rect, if index == selected { [0.14,0.29,0.39,1.0] } else { [0.10,0.13,0.16,1.0] }, EDGE, 1.0, 0.0, screen);
            canvas.text(text, rect.x+6.0, rect.y+5.0, FONT, TEXT, inset(rect,4.0,2.0), true);
            self.widgets.push(Widget { id: format!("{id}.item.{index}"), ty: "PopupMenuItem", rect, text: (*text).into(), value: None, checked: Some(index == selected), action: Action::Choose(id,index) });
        }
        self.buffer = Some(canvas.into_buffer());
    }

    fn draw_tooltip(&mut self, backend: &mut Backend, screen: R) {
        if !self.tooltip_visible { return; }
        let target = self.tooltip_hover.as_ref().and_then(|(id, _)| self.widgets.iter().find(|widget| widget.id == *id && widget.rect.contains(self.pointer.0, self.pointer.1)));
        let Some((anchor, help)) = target.and_then(|widget| control_help(&widget.action).map(|help| (widget.rect, help))) else { self.dismiss_tooltip(); return; };
        let mut canvas = Canvas::with_buffer(backend, self.buffer.take().unwrap_or_default());
        let width = 420.0_f64.min(screen.w - 16.0);
        let lines = wrap(&canvas, help, width - 20.0, FONT);
        let height = 16.0 + lines.len() as f64 * 17.0;
        let below = anchor.y + anchor.h + 8.0;
        let y = if below + height <= screen.h - 8.0 { below } else { (anchor.y - height - 8.0).max(8.0) };
        let rect = R { x: anchor.x.clamp(8.0, (screen.w - width - 8.0).max(8.0)), y, w: width, h: height };
        canvas.rect(R { x: rect.x+3.0, y: rect.y+3.0, ..rect }, [0.0,0.0,0.0,0.55], [0.0;4], 0.0, 0.0, screen);
        canvas.rect(rect, [0.12,0.16,0.19,1.0], ACCENT, 1.0, 0.0, screen);
        for (index, line) in lines.iter().enumerate() {
            canvas.text(line, rect.x+10.0, rect.y+8.0+index as f64*17.0, FONT, TEXT, inset(rect, 8.0, 6.0), true);
        }
        self.buffer = Some(canvas.into_buffer());
        self.add("tooltip", "Tooltip", rect, help, None, None, Action::None);
    }

    fn focused_field(&self) -> Option<Field> {
        [Field::Search,Field::Filter,Field::Evidence].into_iter().find(|field| self.focus.as_deref() == Some(field.id()))
    }

    fn apply_field(&mut self, field: Field) {
        self.local_status = None;
        match field { Field::Search => self.map.set_search(&self.editors[0].text), Field::Filter => self.map.set_filter(&self.editors[1].text), Field::Evidence => {} }
    }

    fn set_focus(&mut self, id: Option<String>, backend: &mut Backend) {
        if self.focus == id { return; }
        if let Some(field) = self.focused_field() {
            match self.editors[field.index()].commit_composition() {
                Ok(true) => self.apply_field(field), Err(error) => self.local_status = Some(error), _ => {}
            }
        }
        backend.cancel_composition();
        self.focus = id;
        self.editing_drag = false;
        self.sync_input(backend);
    }

    fn sync_input(&mut self, backend: &mut Backend) {
        if let Some(field) = self.focused_field() {
            if let Some(rect) = self.widgets.iter().find(|widget| widget.id == field.id()).map(|widget| widget.rect) {
                let editor = &self.editors[field.index()];
                let display = editor.display_text().into_owned();
                let x = rect.x+6.0+backend.text_width(&display[..editor.display_caret_byte()], FONT, true)-editor.scroll_x;
                let (start,len) = editor.native_selection();
                backend.set_text_state(&editor.text,start,len,[x.clamp(rect.x+6.0,rect.x+rect.w-6.0),rect.y+4.0,1.0,18.0]);
                return;
            }
        }
        backend.set_text_state("",0,0,[0.0,0.0,0.0,0.0]);
    }

    fn activate(&mut self, action: Action, backend: &mut Backend) {
        self.local_status = None;
        match action {
            Action::Field(field) => self.set_focus(Some(field.id().into()), backend),
            Action::Button(id) => match id {
                "fit_button" => self.map.fit(), "trace_selected" => self.map.trace_selected(),
                "evidence_load" => self.map.set_evidence_input(EvidenceInput::File(PathBuf::from(&self.editors[2].text))),
                "evidence_capture" => self.map.set_evidence_input(EvidenceInput::Rails),
                "evidence_clear" => self.map.set_evidence_input(EvidenceInput::Static), _ => {}
            },
            Action::Toggle(id) => match id {
                "file_tree_toggle" => { self.tree_shown = !self.tree_shown; if self.tree_shown { self.file_tree.reveal_pending = true; } else if self.focus.as_deref() == Some("file_tree") { self.set_focus(None,backend); } },
                "show_ignored" => self.map.set_show_ignored(!self.map.show_ignored), "syntax_colors" => self.map.set_syntax_colors(!self.map.syntax_colors), "hide_tests" => self.map.set_hide_tests(!self.map.hide_tests), _ => {}
            },
            Action::Drop(id) => {
                self.popup = if self.popup.is_some_and(|(old,_)| old == id) { None } else { Some((id,self.selected_option(id))) };
            }
            Action::Choose(id,index) => {
                self.popup = None;
                match id {
                    "color_mode" => self.map.set_color_mode(match index { 1 => ColorMode::Recent, 2 => ColorMode::Churn, _ => ColorMode::FileType }),
                    "relationship_mode" => self.map.set_relationship_mode(index),
                    "relationship_depth" => self.map.set_relationship_depth([1,2,3,5,8][index.min(4)]),
                    "evidence_mode" => self.map.set_evidence_mode(index), _ => {}
                }
            }
            Action::TreePane => self.set_focus(Some("file_tree".into()), backend),
            Action::TreeRow(index) => self.browse_select(index, false),
            Action::TreeToggle(index) => {
                self.map.read_ignored_node(index);
                self.file_tree.toggle(self.map.tree(), index);
            }
            Action::TreeScrollbar(_) | Action::None => {}
        }
    }

    fn selected_option(&self, id: &str) -> usize {
        match id { "color_mode" => match self.map.color_mode { ColorMode::FileType=>0,ColorMode::Recent=>1,ColorMode::Churn=>2 }, "relationship_mode"=>self.map.relationship_mode, "relationship_depth"=>[1,2,3,5,8].iter().position(|n| *n==self.map.relationship_depth).unwrap_or(0), "evidence_mode"=>self.map.evidence_mode,_=>0 }
    }

    fn caret_from_pointer(&mut self, field: Field, x: f64, extend: bool, backend: &Backend) {
        let Some(rect) = self.widgets.iter().find(|widget|widget.id==field.id()).map(|widget|widget.rect) else { return };
        let editor = &mut self.editors[field.index()];
        let mut chosen = editor.text.len();
        let position = x-rect.x-6.0+editor.scroll_x;
        let mut previous = 0.0;
        for (byte,ch) in editor.text.char_indices() {
            let end = byte+ch.len_utf8();
            let next = backend.text_width(&editor.text[..end],FONT,true);
            if position < (previous+next)*0.5 { chosen=byte; break; }
            previous=next;
        }
        editor.click_byte(chosen,extend);
    }

    pub fn handle(&mut self, event: Event, backend: &mut Backend) {
        if !matches!(&event, Event::PointerMove { .. }) { self.dismiss_tooltip(); }
        let was_composing = self.focused_field().is_some_and(|field| self.editors[field.index()].is_composing());
        match event {
            Event::Close => self.running=false,
            Event::Resize {width,height,scale} => { self.width=width;self.height=height;self.scale=scale;self.layout(); }
            Event::PointerMove {x,y,..} => {
                self.pointer=(x,y);
                self.update_tooltip_hover();
                if let Some((horizontal, start, offset)) = self.tree_scroll_drag {
                    let position = if horizontal { x } else { y };
                    let (extent, viewport) = if horizontal { (self.tree_content_width(), self.tree_body.w) } else { (self.file_tree.rows.len() as f64*ROW_HEIGHT, self.tree_body.h) };
                    let thumb=(viewport*viewport/extent.max(1.0)).max(20.0).min(viewport);
                    let value = offset + (position-start) * ((extent-viewport).max(0.0)/(viewport-thumb).max(1.0));
                    if horizontal { self.file_tree.scroll_x = value.max(0.0); } else { self.file_tree.scroll_y = value.max(0.0); }
                } else if self.editing_drag {
                    if let Some(field)=self.focused_field() { self.caret_from_pointer(field,x,true,backend); }
                } else { self.map.pointer_move(x,y); }
                let cursor=if self.widgets.iter().any(|w|matches!(w.action,Action::Field(_))&&w.rect.contains(x,y)) { Cursor::Text }
                    else if self.map.has_link_at(x,y)||self.widgets.iter().any(|w|!matches!(w.action,Action::None)&&w.rect.contains(x,y)) { Cursor::Hand }
                    else { Cursor::Arrow };
                backend.set_cursor(cursor);
            }
            Event::PointerDown {x,y,button,clicks,modifiers} => {
                self.pointer=(x,y);
                if button!=0 { self.map.pointer_down_with_modifiers(x,y,button,clicks,modifiers); return; }
                let hit=self.widgets.iter().rev().find(|w|w.rect.contains(x,y)&&!matches!(w.action,Action::None)).cloned();
                if let Some(widget)=hit {
                    if matches!(widget.action, Action::TreePane | Action::TreeRow(_) | Action::TreeToggle(_) | Action::TreeScrollbar(_)) { self.popup=None; self.set_focus(Some("file_tree".into()),backend); }
                    else if !matches!(widget.action,Action::Choose(_,_)) { self.set_focus(Some(widget.id.clone()),backend); }
                    if let Action::TreeScrollbar(horizontal) = widget.action {
                        let position = if horizontal {x} else {y};
                        let offset = if horizontal {self.file_tree.scroll_x} else {self.file_tree.scroll_y};
                        self.tree_scroll_drag = Some((horizontal,position,offset));
                    } else if let Action::Field(field)=widget.action {
                        self.popup=None;
                        if self.editors[field.index()].is_composing() {
                            if self.editors[field.index()].commit_composition().unwrap_or(false) { self.apply_field(field); }
                            backend.cancel_composition();
                        }
                        if clicks>=2 { self.editors[field.index()].select_all(); }
                        else { self.caret_from_pointer(field,x,modifiers&platform::SHIFT!=0,backend); }
                        self.editing_drag=true;
                    } else { self.pressed=Some(widget.id); self.pressed_clicks=clicks; }
                } else {
                    self.popup=None; self.set_focus(None,backend); self.map.pointer_down_with_modifiers(x,y,button,clicks,modifiers);
                }
            }
            Event::PointerUp {x,y,button,..} => {
                self.editing_drag=false;
                self.tree_scroll_drag=None;
                if button==0 {
                    if let Some(id)=self.pressed.take() {
                        if let Some(action)=self.widgets.iter().rev().find(|w|w.id==id&&w.rect.contains(x,y)).map(|w|w.action.clone()) { if let Action::TreeRow(index) = action { self.browse_select(index, self.pressed_clicks>=2); } else { self.activate(action,backend); } }
                    }
                }
                self.map.pointer_up(x,y,button);
            }
            Event::Scroll {x,y,dx,dy,modifiers} => {
                if self.tree_shown && self.tree_rect.contains(x,y) && self.popup.is_none() {
                    self.file_tree.scroll_x -= dx;
                    if modifiers & platform::SHIFT != 0 { self.file_tree.scroll_x -= dy; }
                    else { self.file_tree.scroll_y -= dy; }
                } else if self.inspector.contains(x,y) { self.inspector_scroll=(self.inspector_scroll-dy).clamp(0.0,self.inspector_limit); }
                else if self.popup.is_none() { self.map.scroll(x,y,-dy); }
            }
            Event::Text {text,replace_start,replace_len} => {
                if let Some(field)=self.focused_field() {
                    match self.editors[field.index()].replace_native(&text,replace_start,replace_len) {
                        Ok(true)=>self.apply_field(field),Err(error)=>{self.editors[field.index()].cancel_composition();self.local_status=Some(error);backend.cancel_composition();},_=>{}
                    }
                } else if text==" " { self.activate_focused(backend); }
            }
            Event::Composition {text,selection_start,selection_len,replace_start,replace_len} => {
                if let Some(field)=self.focused_field() {
                    if let Err(error)=self.editors[field.index()].set_composition(&text,selection_start,selection_len,replace_start,replace_len) { self.editors[field.index()].cancel_composition();self.local_status=Some(error);backend.cancel_composition(); }
                }
            }
            Event::Key {code,modifiers} => self.key(code,modifiers,backend),
            Event::Accessibility {id} => if let Some(action)=self.widgets.iter().find(|w|id_hash(&w.id)==id).map(|w|w.action.clone()) { self.activate(action,backend); },
            Event::Error(error) => self.local_status=Some(error),
        }
        if was_composing && !self.focused_field().is_some_and(|field| self.editors[field.index()].is_composing()) { backend.cancel_composition(); }
        self.sync_input(backend);
    }

    fn activate_focused(&mut self,backend:&mut Backend) {
        if let Some(action)=self.widgets.iter().find(|w|Some(w.id.as_str())==self.focus.as_deref()).map(|w|w.action.clone()) { self.activate(action,backend); }
    }

    fn key(&mut self,code:u32,mods:u32,backend:&mut Backend) {
        if mods&platform::COMMAND!=0&&code==u32::from(b'f') {
            let field=if mods&platform::SHIFT!=0 {Field::Filter}else{Field::Search};
            self.set_focus(Some(field.id().into()),backend);self.editors[field.index()].select_all();return;
        }
        if let Some((id,index))=self.popup {
            match code {
                platform::KEY_UP=>self.popup=Some((id,index.saturating_sub(1))),
                platform::KEY_DOWN=>self.popup=Some((id,(index+1).min(options(id).len()-1))),
                platform::KEY_RETURN=>self.activate(Action::Choose(id,index),backend),
                platform::KEY_ESCAPE=>self.popup=None,_=>{}
            }
            return;
        }
        if code==platform::KEY_TAB {
            let focusable:Vec<_>=self.widgets.iter().filter(|w|!matches!(w.action,Action::None|Action::Choose(_,_)|Action::TreeRow(_)|Action::TreeToggle(_)|Action::TreeScrollbar(_))).map(|w|w.id.clone()).collect();
            if !focusable.is_empty() {
                let current=focusable.iter().position(|id|Some(id.as_str())==self.focus.as_deref());
                let next=if mods&platform::SHIFT!=0 {current.map_or(focusable.len()-1,|i|(i+focusable.len()-1)%focusable.len())} else {current.map_or(0,|i|(i+1)%focusable.len())};
                self.set_focus(Some(focusable[next].clone()),backend);
            }
            return;
        }
        if self.focus.as_deref() == Some("file_tree") && self.tree_shown {
            self.tree_key(code);
            return;
        }
        let Some(field)=self.focused_field() else {
            if code==platform::KEY_RETURN {self.activate_focused(backend);}
            if code==platform::KEY_ESCAPE {self.set_focus(None,backend);}
            return;
        };
        let extend=mods&platform::SHIFT!=0;
        let mut changed=false;
        match code {
            platform::KEY_SELECT_ALL=>self.editors[field.index()].select_all(),
            platform::KEY_LEFT=>self.editors[field.index()].move_left(extend),
            platform::KEY_RIGHT=>self.editors[field.index()].move_right(extend),
            platform::KEY_HOME|platform::KEY_UP=>self.editors[field.index()].move_home(extend),
            platform::KEY_END|platform::KEY_DOWN=>self.editors[field.index()].move_end(extend),
            platform::KEY_BACKSPACE=>changed=self.editors[field.index()].backspace(),
            platform::KEY_DELETE=>changed=self.editors[field.index()].delete(),
            platform::KEY_UNDO=>changed=self.editors[field.index()].undo(),
            platform::KEY_COPY=>backend.set_clipboard(self.editors[field.index()].selected_text()),
            platform::KEY_CUT=>{backend.set_clipboard(self.editors[field.index()].selected_text());changed=self.editors[field.index()].insert("").unwrap_or(false);}
            platform::KEY_PASTE=>match self.editors[field.index()].insert(&backend.clipboard()) {Ok(value)=>changed=value,Err(error)=>self.local_status=Some(error)},
            platform::KEY_ESCAPE=>{
                if self.editors[field.index()].is_composing() {self.editors[field.index()].cancel_composition();backend.cancel_composition();}
                else if field==Field::Search {changed=self.editors[0].set_text("").unwrap_or(false);}
                else {self.set_focus(None,backend);}
            }
            platform::KEY_RETURN=>{
                if self.editors[field.index()].is_composing() {
                    if self.editors[field.index()].commit_composition().unwrap_or(false) {self.apply_field(field);}
                    backend.cancel_composition();return;
                }
                match field {Field::Search=>self.map.next_match(),Field::Filter=>self.apply_field(field),Field::Evidence=>self.activate(Action::Button("evidence_load"),backend)}
            }
            _=>{}
        }
        if changed {self.apply_field(field);}
    }

    pub fn snapshot(&self,query:Option<&str>,all:bool)->Value {
        let widgets:Vec<_>=self.widgets.iter().filter(|widget|all||widget.rect.w>0.0&&widget.rect.h>0.0).filter(|widget|query.is_none_or(|q|widget.id.contains(q)||widget.ty.contains(q)||widget.text.contains(q)||widget.value.as_ref().is_some_and(|value|value.contains(q)))).map(|widget|{
            let mut value=json!({"i":widget.id,"ty":widget.ty,"r":[widget.rect.x,widget.rect.y,widget.rect.w,widget.rect.h],"w":0,"enabled":true,"t":widget.text});
            if let Some(text)=&widget.value {value["val"]=json!(text);if widget.ty=="DropDown" {value["selected"]=json!(text);}}
            if let Some(help)=control_help(&widget.action) {value["help"]=json!(help);}
            if let Some(checked)=widget.checked {value["c"]=json!(u8::from(checked));}
            value
        }).collect();
        json!({"s":widgets})
    }

    pub fn state(&self)->Value {json!({"app":"megamap","pid":std::process::id(),"w":[{"i":0,"t":"megamap","sz":[self.width,self.height],"px":[self.width*self.scale,self.height*self.scale],"dpi":self.scale}],"map":self.map.debug_snapshot(),"file_tree":{"shown":self.tree_shown,"rows":self.file_tree.rows.len(),"rendered_rows":self.widgets.iter().filter(|w|w.ty=="TreeRow").count(),"scroll_x":self.file_tree.scroll_x,"scroll_y":self.file_tree.scroll_y}})}
}

fn inset(rect:R,x:f64,y:f64)->R {R{x:rect.x+x,y:rect.y+y,w:(rect.w-2.0*x).max(1.0),h:(rect.h-2.0*y).max(1.0)}}
fn id_hash(id:&str)->u64 {id.bytes().fold(14695981039346656037,|hash,b|(hash^u64::from(b)).wrapping_mul(1099511628211))}
fn options(id:&str)->&'static[&'static str] {
    match id {
        "color_mode"=>&["File type","Recently changed","Most changed"],
        "relationship_mode"=>&["All files","Calls from root","Callers of root"],
        "relationship_depth"=>&["Depth 1","Depth 2","Depth 3","Depth 5","Depth 8"],
        "evidence_mode"=>&["All evidence","Source only","Static + inferred","Loaded app only","Observed only"],
        _=>&["?"]
    }
}

fn wrap(canvas:&Canvas,text:&str,width:f64,size:f64)->Vec<String> {
    let mut lines=Vec::new();
    for paragraph in text.lines() {
        if paragraph.is_empty() {lines.push(String::new());continue;}
        let mut line=String::new();
        for word in paragraph.split_whitespace() {
            let candidate=if line.is_empty(){word.to_owned()}else{format!("{line} {word}")};
            if canvas.text_width(&candidate,size,true)<=width {line=candidate;continue;}
            if !line.is_empty(){lines.push(std::mem::take(&mut line));}
            for ch in word.chars() {
                let mut candidate=line.clone();candidate.push(ch);
                if !line.is_empty()&&canvas.text_width(&candidate,size,true)>width {lines.push(std::mem::take(&mut line));}
                line.push(ch);
            }
        }
        lines.push(line);
    }
    lines
}

fn control_help(action: &Action) -> Option<&'static str> {
    let id = match action {
        Action::Field(field) => field.id(),
        Action::Button(id) | Action::Toggle(id) | Action::Drop(id) | Action::Choose(id, _) => id,
        Action::TreePane => return Some("Browse folders and files. Use arrow keys to navigate, Right/Left to expand or collapse, and Enter to center the selected item in the map."),
        Action::TreeRow(_) => return Some("Select this item in the map and inspector. Double-click a file to zoom to it; double-click a folder to expand or collapse it."),
        Action::TreeToggle(_) => return Some("Expand or collapse this folder. Ignored folders are read only when opened."),
        Action::TreeScrollbar(_) => return Some("Drag to scroll the file tree. Use the wheel to scroll vertically, or Shift-wheel to scroll horizontally."),
        Action::None => return None,
    };
    Some(match id {
        "file_tree_toggle" => "Show or hide the expandable filesystem tree beside the treemap. It shares filters and relationship traces; selecting files in either view keeps the inspector in sync.",
        "search" => "Find files and folders by name or path. Press Enter to fly to the next match; Escape clears the search. Command-F focuses this field.",
        "fit_button" => "Zoom out to fit the currently visible files in the window. Keeps your filters and trace selection.",
        "show_ignored" => "Show files and folders excluded by ignore rules. Ignored entries stay collapsed until you click them to read their contents.",
        "syntax_colors" => "Color Ruby, JavaScript and TypeScript syntax when zoomed in enough to read code. Colored relationship calls remain visible with this off.",
        "color_mode" => "Color files by file type, most recent change, or change frequency. Change-based colors use the repository's Git history.",
        "hide_tests" => "Hide spec and test files and directories from the map and relationship traces.",
        "filter_query" => "Limit the map to matching paths. Space-separated terms mean AND; | means OR; a leading - excludes a term. Globs such as app/**/*.rb work too. Command-Shift-F focuses this field.",
        "relationship_mode" => "Show all files, follow calls made by the trace root, or find callers of the trace root. Select a file and use Trace selected to choose the root.",
        "trace_selected" => "Use the selected file as the root of a relationship trace and fit the results in the window. Starts with outgoing calls when viewing All files.",
        "relationship_depth" => "Choose how many method-call hops to follow. Depth 1 includes direct relationships; deeper calls may stay within the same files. Changing depth keeps your position and zoom.",
        "evidence_mode" => "Choose which relationship evidence to follow: source-resolved calls, labeled inferences, loaded Rails metadata, or observed execution. Observed calls cover only the paths exercised during their capture.",
        "evidence_path" => "Path to an optional saved relationship snapshot (a JSON file), then click Load snapshot. Relative paths start at the mapped project's directory.",
        "evidence_load" => "Load the JSON file named on the left. A snapshot contains Rails method and association metadata or recorded calls, adding relationships source analysis can miss. The app can be offline. Missing or stale data falls back to source analysis.",
        "evidence_capture" => "Start this project's Rails development environment to collect method and association metadata, with a 45-second limit. Requires a working Ruby/Rails setup. Runs only when clicked; unavailable app data falls back to source analysis.",
        "evidence_clear" => "Remove imported or captured app data and use source analysis, including labeled Rails inferences. Does not change files in the project.",
        _ => return None,
    })
}
