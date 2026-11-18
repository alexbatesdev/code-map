use crate::model::R;
use std::ops::{Add, AddAssign, Div, Mul, Sub, SubAssign};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

pub const fn vec2(x: f64, y: f64) -> Vec2 { Vec2 { x, y } }

impl Vec2 {
    pub fn length(self) -> f64 { self.x.hypot(self.y) }
}

impl Add for Vec2 { type Output = Self; fn add(self, rhs: Self) -> Self { vec2(self.x + rhs.x, self.y + rhs.y) } }
impl Sub for Vec2 { type Output = Self; fn sub(self, rhs: Self) -> Self { vec2(self.x - rhs.x, self.y - rhs.y) } }
impl Mul<f64> for Vec2 { type Output = Self; fn mul(self, rhs: f64) -> Self { vec2(self.x * rhs, self.y * rhs) } }
impl Div<f64> for Vec2 { type Output = Self; fn div(self, rhs: f64) -> Self { vec2(self.x / rhs, self.y / rhs) } }
impl AddAssign for Vec2 { fn add_assign(&mut self, rhs: Self) { *self = *self + rhs; } }
impl SubAssign for Vec2 { fn sub_assign(&mut self, rhs: Self) { *self = *self - rhs; } }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub pos: Vec2,
    pub size: Vec2,
}

impl Rect {
    pub fn contains(self, point: Vec2) -> bool {
        point.x >= self.pos.x && point.y >= self.pos.y && point.x < self.pos.x + self.size.x && point.y < self.pos.y + self.size.y
    }
}

impl From<R> for Rect { fn from(value: R) -> Self { Self { pos: vec2(value.x, value.y), size: vec2(value.w, value.h) } } }
impl From<Rect> for R { fn from(value: Rect) -> Self { Self { x: value.pos.x, y: value.pos.y, w: value.size.x, h: value.size.y } } }

pub fn intersection(a: R, b: R) -> Option<R> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.w).min(b.x + b.w);
    let bottom = (a.y + a.h).min(b.y + b.h);
    (right > x && bottom > y).then_some(R { x, y, w: right - x, h: bottom - y })
}
