//! Geometry. Layout works in logical pixels (`Point`, `Rect`); painting ends
//! in device pixels (`IRect`) at the window's scale factor.

use std::ops::{Add, Sub};

/// A point in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl Add for Point {
    type Output = Point;

    fn add(self, other: Point) -> Point {
        Point::new(self.x + other.x, self.y + other.y)
    }
}

impl Sub for Point {
    type Output = Point;

    fn sub(self, other: Point) -> Point {
        Point::new(self.x - other.x, self.y - other.y)
    }
}

/// A rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    pub fn center(self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn is_empty(self) -> bool {
        !(self.width > 0.0 && self.height > 0.0)
    }

    /// Whether `point` is inside. The right and bottom edges are outside, so
    /// adjacent rectangles never both contain a point.
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.y >= self.y && point.x < self.right() && point.y < self.bottom()
    }

    pub fn translate(self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Shrinks the rectangle by `dx` on the left and right and `dy` on the top
    /// and bottom, never below zero size.
    pub fn inset(self, dx: f32, dy: f32) -> Self {
        Self::new(
            self.x + dx,
            self.y + dy,
            (self.width - 2.0 * dx).max(0.0),
            (self.height - 2.0 * dy).max(0.0),
        )
    }

    /// The device pixels this rectangle fills once each edge snaps to the
    /// nearest pixel boundary at `scale`: crisp, and adjacent rectangles meet
    /// without gaps or overlap.
    pub fn snap(self, scale: f32) -> IRect {
        IRect::new(
            (self.x * scale).round() as i32,
            (self.y * scale).round() as i32,
            (self.right() * scale).round() as i32,
            (self.bottom() * scale).round() as i32,
        )
    }

    /// The smallest device rectangle containing this one at `scale`.
    pub fn cover(self, scale: f32) -> IRect {
        IRect::new(
            (self.x * scale).floor() as i32,
            (self.y * scale).floor() as i32,
            (self.right() * scale).ceil() as i32,
            (self.bottom() * scale).ceil() as i32,
        )
    }
}

/// A half-open rectangle of device pixels: columns `x0..x1`, rows `y0..y1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub const EMPTY: Self = Self::new(0, 0, 0, 0);

    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub const fn from_size(width: u32, height: u32) -> Self {
        Self::new(0, 0, width as i32, height as i32)
    }

    pub fn width(self) -> i32 {
        (self.x1 - self.x0).max(0)
    }

    pub fn height(self) -> i32 {
        (self.y1 - self.y0).max(0)
    }

    pub fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn area(self) -> i64 {
        i64::from(self.width()) * i64::from(self.height())
    }

    /// The overlap of two rectangles; `EMPTY` when they don't meet.
    pub fn intersect(self, other: Self) -> Self {
        let rect = Self::new(
            self.x0.max(other.x0),
            self.y0.max(other.y0),
            self.x1.min(other.x1),
            self.y1.min(other.y1),
        );
        if rect.is_empty() { Self::EMPTY } else { rect }
    }

    /// The bounding box of both; an empty rectangle contributes nothing.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self::new(
            self.x0.min(other.x0),
            self.y0.min(other.y0),
            self.x1.max(other.x1),
            self.y1.max(other.y1),
        )
    }

    pub fn contains_rect(self, other: Self) -> bool {
        other.is_empty()
            || (other.x0 >= self.x0
                && other.y0 >= self.y0
                && other.x1 <= self.x1
                && other.y1 <= self.y1)
    }

    pub fn translate(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_keeps_neighbors_seamless_at_fractional_scales() {
        let scale = 1.25;
        let left = Rect::new(0.0, 0.0, 13.3, 10.0).snap(scale);
        let right = Rect::new(13.3, 0.0, 20.0, 10.0).snap(scale);
        assert_eq!(left.x1, right.x0, "no gap or overlap between neighbors");
        assert_eq!(
            Rect::new(1.0, 2.0, 3.0, 4.0).snap(2.0),
            IRect::new(2, 4, 8, 12)
        );
        assert_eq!(
            Rect::new(0.2, 0.2, 1.0, 1.0).cover(1.0),
            IRect::new(0, 0, 2, 2)
        );
    }

    #[test]
    fn rectangle_set_operations() {
        let a = IRect::new(0, 0, 10, 10);
        let b = IRect::new(5, 5, 20, 20);
        assert_eq!(a.intersect(b), IRect::new(5, 5, 10, 10));
        assert_eq!(a.union(b), IRect::new(0, 0, 20, 20));
        assert!(
            a.intersect(IRect::new(10, 0, 12, 4)).is_empty(),
            "edges are exclusive"
        );
        assert_eq!(IRect::EMPTY.union(b), b);
        assert!(a.contains_rect(IRect::new(1, 1, 9, 9)));
        assert!(!a.contains_rect(b));
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(!r.contains(Point::new(10.0, 5.0)));
        assert_eq!(r.inset(6.0, 1.0).width, 0.0);
    }
}
