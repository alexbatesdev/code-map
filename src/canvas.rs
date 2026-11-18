use crate::{math::intersection, model::R, platform::{Backend, Quad}};

const QUAD_LIMIT: usize = crate::platform::INSTANCE_CAPACITY;

pub struct Canvas<'a> {
    backend: &'a mut Backend,
    pending: Vec<Quad>,
    submitted: usize,
    pub dropped_quads: usize,
}

impl<'a> Canvas<'a> {
    pub fn new(backend: &'a mut Backend) -> Self { Self::with_buffer(backend, Vec::new()) }

    pub fn with_buffer(backend: &'a mut Backend, mut buffer: Vec<Quad>) -> Self {
        buffer.clear();
        Self { backend, pending: buffer, submitted: 0, dropped_quads: 0 }
    }

    pub fn rect(&mut self, rect: R, color: [f32; 4], edge: [f32; 4], border: f32, hatch: f32, clip: R) {
        if !valid(rect) || !valid(clip) || intersection(rect, clip).is_none() { return; }
        self.push(Quad::rect(raw(rect), color, edge, border, hatch, raw(clip)));
    }

    pub fn triangle(&mut self, rect: R, color: [f32; 4], clip: R) {
        if !valid(rect) || !valid(clip) || intersection(rect, clip).is_none() { return; }
        self.flush();
        self.push(Quad::triangle(raw(rect), color, raw(clip)));
    }

    fn push(&mut self, quad: Quad) {
        if self.submitted + self.pending.len() >= QUAD_LIMIT { self.dropped_quads += 1; return; }
        self.pending.push(quad);
    }

    pub fn text(&mut self, text: &str, x: f64, y: f64, size: f64, color: [f32; 4], clip: R, monospace: bool) -> f64 {
        if text.is_empty() || !size.is_finite() || size <= 0.0 || !x.is_finite() || !y.is_finite() || !valid(clip) { return 0.0; }
        self.flush();
        self.backend.text(text, x, y, size, color, raw(clip), monospace)
    }

    pub fn text_width(&self, text: &str, size: f64, monospace: bool) -> f64 {
        if text.is_empty() || !size.is_finite() || size <= 0.0 { return 0.0; }
        self.backend.text_width(text, size, monospace)
    }

    pub fn flush(&mut self) {
        if self.pending.is_empty() { return; }
        self.backend.quads(&self.pending);
        self.submitted += self.pending.len();
        self.pending.clear();
    }

    pub fn into_buffer(mut self) -> Vec<Quad> {
        self.flush();
        std::mem::take(&mut self.pending)
    }
}

fn valid(rect: R) -> bool {
    [rect.x, rect.y, rect.w, rect.h].iter().all(|v| v.is_finite() && v.abs() <= f32::MAX as f64) && rect.w > 0.0 && rect.h > 0.0
}

fn raw(rect: R) -> [f32; 4] { [rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32] }
