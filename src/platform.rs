use std::{ffi::{CStr, CString, c_char, c_void}, marker::PhantomData, path::Path, rc::Rc};

pub type Rect = [f32; 4];
pub type Color = [f32; 4];
pub const SHIFT: u32 = 1;
pub const CONTROL: u32 = 2;
pub const ALT: u32 = 4;
pub const COMMAND: u32 = 8;
pub const KEY_BACKSPACE: u32 = 8;
pub const KEY_TAB: u32 = 9;
pub const KEY_RETURN: u32 = 13;
pub const KEY_ESCAPE: u32 = 27;
pub const KEY_LEFT: u32 = 0x100;
pub const KEY_RIGHT: u32 = 0x101;
pub const KEY_UP: u32 = 0x102;
pub const KEY_DOWN: u32 = 0x103;
pub const KEY_HOME: u32 = 0x104;
pub const KEY_END: u32 = 0x105;
pub const KEY_DELETE: u32 = 0x106;
pub const KEY_SELECT_ALL: u32 = 0x200;
pub const KEY_COPY: u32 = 0x201;
pub const KEY_CUT: u32 = 0x202;
pub const KEY_PASTE: u32 = 0x203;
pub const KEY_UNDO: u32 = 0x204;
pub const ATLAS_CAPACITY_BYTES: u64 = 4096 * 4096;
pub const INSTANCE_CAPACITY: usize = 500_000;

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct Quad {
    pub rect: Rect,
    pub uv: Rect,
    pub color: Color,
    pub clip: Rect,
    pub edge: Color,
    pub params: [f32; 4],
}

impl Quad {
    pub fn rect(rect: Rect, color: Color, edge: Color, border: f32, hatch: f32, clip: Rect) -> Self {
        Self { rect, uv: [0.0; 4], color, clip, edge, params: [border, hatch, 0.0, 0.0] }
    }

    pub fn triangle(rect: Rect, color: Color, clip: Rect) -> Self {
        Self { rect, uv: [0.0; 4], color, clip, edge: [0.0; 4], params: [0.0, 0.0, 2.0, 0.0] }
    }
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FrameStats {
    pub instances: u64,
    pub draw_calls: u64,
    pub blits: u64,
    pub atlas_glyphs: u64,
    pub atlas_used_bytes: u64,
    pub atlas_upload_bytes: u64,
    pub dropped_instances: u64,
    pub dropped_glyphs: u64,
    pub skipped: u64,
    pub cpu_ms: f64,
    /// GPU duration of the completed command previously using this frame slot.
    pub gpu_ms: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    PointerMove { x: f64, y: f64, modifiers: u32 },
    PointerDown { x: f64, y: f64, modifiers: u32, button: u32, clicks: u32 },
    PointerUp { x: f64, y: f64, modifiers: u32, button: u32, clicks: u32 },
    Scroll { x: f64, y: f64, dx: f64, dy: f64, modifiers: u32 },
    Key { code: u32, modifiers: u32 },
    Text { text: String, replace_start: usize, replace_len: usize },
    Composition { text: String, selection_start: usize, selection_len: usize, replace_start: usize, replace_len: usize },
    Resize { width: f64, height: f64, scale: f64 },
    Close,
    Accessibility { id: u64 },
    Error(String),
}

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum Cursor { Arrow = 0, Hand = 1, Text = 4 }

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum AccessibilityRole { Button = 0, Text = 1, CheckBox = 2, Popup = 3 }

pub struct Accessibility<'a> {
    pub id: u64,
    pub role: AccessibilityRole,
    pub enabled: bool,
    pub focused: bool,
    pub rect: [f64; 4],
    pub label: &'a str,
    pub value: &'a str,
}

#[repr(C)]
struct RawEvent {
    kind: u32, code: u32, modifiers: u32, button: u32, clicks: u32,
    x: f64, y: f64, dx: f64, dy: f64, width: f64, height: f64, scale: f64,
    text: *const u8, text_len: usize,
    selection_start: u64, selection_len: u64, replace_start: u64, replace_len: u64, id: u64,
}

#[repr(C)]
struct RawAccessibility {
    id: u64, role: u32, enabled: u32, focused: u32,
    rect: [f64; 4], label: *const c_char, value: *const c_char,
}

unsafe extern "C" {
    fn mm_open(title: *const c_char, width: f64, height: f64, hidden: i32) -> *mut c_void;
    fn mm_last_error(context: *mut c_void) -> *const c_char;
    fn mm_poll(context: *mut c_void, event: *mut RawEvent, wait_ms: f64) -> i32;
    fn mm_size(context: *mut c_void, width: *mut f64, height: *mut f64, scale: *mut f64);
    fn mm_begin(context: *mut c_void, clear: *const f32);
    fn mm_quads(context: *mut c_void, quads: *const Quad, count: usize);
    fn mm_text(context: *mut c_void, text: *const u8, len: usize, x: f64, y: f64, size: f64, color: *const f32, clip: *const f32, mono: i32) -> f64;
    fn mm_text_width(context: *mut c_void, text: *const u8, len: usize, size: f64, mono: i32) -> f64;
    fn mm_present(context: *mut c_void, stats: *mut FrameStats) -> i32;
    fn mm_capture(context: *mut c_void, path: *const c_char) -> i32;
    fn mm_text_state(context: *mut c_void, text: *const u8, len: usize, selection_start: u64, selection_len: u64, caret: *const f64);
    fn mm_cancel_composition(context: *mut c_void);
    fn mm_cursor(context: *mut c_void, cursor: u32);
    fn mm_title(context: *mut c_void, title: *const c_char);
    fn mm_clipboard(context: *mut c_void) -> *const c_char;
    fn mm_set_clipboard(context: *mut c_void, text: *const c_char);
    fn mm_accessibility(context: *mut c_void, elements: *const RawAccessibility, count: usize);
    fn mm_close(context: *mut c_void);
}

pub struct Backend { context: *mut c_void, _main_thread: PhantomData<Rc<()>> }

fn string(value: &str) -> CString { CString::new(value.replace('\0', "")).expect("NUL removed") }
fn native_string(value: *const c_char) -> String {
    if value.is_null() { String::new() } else { unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() } }
}

impl Backend {
    pub fn new(title: &str, width: f64, height: f64, hidden: bool) -> Result<Self, String> {
        if !width.is_finite() || !height.is_finite() { return Err("Window dimensions must be finite".into()); }
        let title = string(title);
        let context = unsafe { mm_open(title.as_ptr(), width, height, hidden as i32) };
        if context.is_null() { return Err(native_string(unsafe { mm_last_error(context) })); }
        Ok(Self { context, _main_thread: PhantomData })
    }

    pub fn size(&self) -> (f64, f64, f64) {
        let (mut w, mut h, mut scale) = (0.0, 0.0, 1.0);
        unsafe { mm_size(self.context, &mut w, &mut h, &mut scale); }
        (w, h, scale)
    }

    pub fn poll(&mut self, wait_ms: f64) -> Option<Event> {
        let mut raw: RawEvent = unsafe { std::mem::zeroed() };
        if unsafe { mm_poll(self.context, &mut raw, wait_ms) } == 0 { return None; }
        let text = || if raw.text.is_null() { String::new() } else {
            unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(raw.text, raw.text_len)).into_owned() }
        };
        Some(match raw.kind {
            1 => Event::PointerMove { x: raw.x, y: raw.y, modifiers: raw.modifiers },
            2 => Event::PointerDown { x: raw.x, y: raw.y, modifiers: raw.modifiers, button: raw.button, clicks: raw.clicks },
            3 => Event::PointerUp { x: raw.x, y: raw.y, modifiers: raw.modifiers, button: raw.button, clicks: raw.clicks },
            4 => Event::Scroll { x: raw.x, y: raw.y, dx: raw.dx, dy: raw.dy, modifiers: raw.modifiers },
            5 => Event::Key { code: raw.code, modifiers: raw.modifiers },
            6 => Event::Text { text: text(), replace_start: raw.replace_start as usize, replace_len: raw.replace_len as usize },
            7 => Event::Composition { text: text(), selection_start: raw.selection_start as usize, selection_len: raw.selection_len as usize, replace_start: raw.replace_start as usize, replace_len: raw.replace_len as usize },
            8 => Event::Resize { width: raw.width, height: raw.height, scale: raw.scale },
            9 => Event::Close,
            10 => Event::Accessibility { id: raw.id },
            11 => Event::Error(text()),
            other => Event::Error(format!("Unknown native event {other}")),
        })
    }

    pub fn begin_frame(&mut self, clear: Color) { unsafe { mm_begin(self.context, clear.as_ptr()); } }
    pub fn quads(&mut self, quads: &[Quad]) { unsafe { mm_quads(self.context, quads.as_ptr(), quads.len()); } }
    pub fn text(&mut self, text: &str, x: f64, y: f64, size: f64, color: Color, clip: Rect, monospace: bool) -> f64 {
        unsafe { mm_text(self.context, text.as_ptr(), text.len(), x, y, size, color.as_ptr(), clip.as_ptr(), monospace as i32) }
    }
    pub fn text_width(&self, text: &str, size: f64, monospace: bool) -> f64 {
        unsafe { mm_text_width(self.context, text.as_ptr(), text.len(), size, monospace as i32) }
    }
    pub fn present(&mut self) -> Result<FrameStats, String> {
        let mut stats = FrameStats::default();
        if unsafe { mm_present(self.context, &mut stats) } == 0 { return Err(native_string(unsafe { mm_last_error(self.context) })); }
        Ok(stats)
    }
    pub fn capture(&mut self, path: &Path) -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| "Capture path contains NUL")?;
        if unsafe { mm_capture(self.context, path.as_ptr()) } == 0 { Err(native_string(unsafe { mm_last_error(self.context) })) } else { Ok(()) }
    }
    pub fn set_text_state(&mut self, text: &str, selection_start: usize, selection_len: usize, caret: [f64; 4]) {
        unsafe { mm_text_state(self.context, text.as_ptr(), text.len(), selection_start as u64, selection_len as u64, caret.as_ptr()); }
    }
    pub fn cancel_composition(&mut self) { unsafe { mm_cancel_composition(self.context); } }
    pub fn set_cursor(&mut self, cursor: Cursor) { unsafe { mm_cursor(self.context, cursor as u32); } }
    pub fn set_title(&mut self, title: &str) { let title = string(title); unsafe { mm_title(self.context, title.as_ptr()); } }
    pub fn clipboard(&self) -> String { native_string(unsafe { mm_clipboard(self.context) }) }
    pub fn set_clipboard(&mut self, text: &str) { let text = string(text); unsafe { mm_set_clipboard(self.context, text.as_ptr()); } }
    pub fn accessibility(&mut self, elements: &[Accessibility<'_>]) {
        let labels: Vec<_> = elements.iter().map(|e| string(e.label)).collect();
        let values: Vec<_> = elements.iter().map(|e| string(e.value)).collect();
        let raw: Vec<_> = elements.iter().enumerate().map(|(i,e)| RawAccessibility {
            id: e.id, role: e.role as u32, enabled: e.enabled as u32, focused: e.focused as u32,
            rect: e.rect, label: labels[i].as_ptr(), value: values[i].as_ptr(),
        }).collect();
        unsafe { mm_accessibility(self.context, raw.as_ptr(), raw.len()); }
    }
}

impl Drop for Backend { fn drop(&mut self) { unsafe { mm_close(self.context); } } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_quad_layout_has_the_metal_instance_stride() {
        assert_eq!(std::mem::size_of::<Quad>(), 96);
        assert_eq!(std::mem::offset_of!(Quad, params), 80);
        assert_eq!(std::mem::size_of::<RawEvent>(), 136);
        assert_eq!(std::mem::offset_of!(RawEvent, text), 80);
        assert_eq!(std::mem::offset_of!(RawEvent, replace_start), 112);
        assert_eq!(std::mem::size_of::<FrameStats>(), 88);
        assert_eq!(std::mem::size_of::<RawAccessibility>(), 72);
    }
}
