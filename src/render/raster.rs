//! Anti-aliased filling. A path is flattened into line segments whose signed
//! area is accumulated per pixel and then summed along each row; the absolute
//! sum, clamped to one, is the pixel's coverage. That is exact for the
//! flattened outline (the technique of font-rs and stb_truetype).
//!
//! Overlapping contours of the same orientation add up before clamping, so
//! glyph outlines fill by the nonzero rule, and the pieces `stroke` emits,
//! which all share one orientation, fill as their union.

use super::canvas::MaskRef;
use super::geom::{IRect, Point};

/// Flattening tolerance in device pixels: the farthest a flattened segment
/// may stray from its curve.
pub const TOLERANCE: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathEl {
    MoveTo(Point),
    LineTo(Point),
    QuadTo(Point, Point),
    CurveTo(Point, Point, Point),
    Close,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    elements: Vec<PathEl>,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    /// A closed polygon through `points`.
    pub fn polygon(points: &[Point]) -> Self {
        let mut path = Self::new();
        if let Some((first, rest)) = points.split_first() {
            path.move_to(first.x, first.y);
            for point in rest {
                path.line_to(point.x, point.y);
            }
            path.close();
        }
        path
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.elements.push(PathEl::MoveTo(Point::new(x, y)));
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.elements.push(PathEl::LineTo(Point::new(x, y)));
    }

    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.elements
            .push(PathEl::QuadTo(Point::new(cx, cy), Point::new(x, y)));
    }

    pub fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.elements.push(PathEl::CurveTo(
            Point::new(c1x, c1y),
            Point::new(c2x, c2y),
            Point::new(x, y),
        ));
    }

    pub fn close(&mut self) {
        self.elements.push(PathEl::Close);
    }

    /// Appends every element of `other`.
    pub fn extend(&mut self, other: &Path) {
        self.elements.extend_from_slice(&other.elements);
    }

    pub fn elements(&self) -> &[PathEl] {
        &self.elements
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The path with every point (control points included) mapped by `f`.
    pub fn map(&self, f: impl Fn(Point) -> Point) -> Self {
        let elements = self
            .elements
            .iter()
            .map(|element| match *element {
                PathEl::MoveTo(p) => PathEl::MoveTo(f(p)),
                PathEl::LineTo(p) => PathEl::LineTo(f(p)),
                PathEl::QuadTo(c, p) => PathEl::QuadTo(f(c), f(p)),
                PathEl::CurveTo(c1, c2, p) => PathEl::CurveTo(f(c1), f(c2), f(p)),
                PathEl::Close => PathEl::Close,
            })
            .collect();
        Self { elements }
    }

    /// The smallest and largest coordinates of every point, control points
    /// included, so the box contains the whole path.
    pub fn bounds(&self) -> Option<(Point, Point)> {
        let mut bounds: Option<(Point, Point)> = None;
        let mut add = |p: Point| {
            bounds = Some(match bounds {
                None => (p, p),
                Some((min, max)) => (
                    Point::new(min.x.min(p.x), min.y.min(p.y)),
                    Point::new(max.x.max(p.x), max.y.max(p.y)),
                ),
            });
        };
        for element in &self.elements {
            match *element {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => add(p),
                PathEl::QuadTo(c, p) => {
                    add(c);
                    add(p);
                }
                PathEl::CurveTo(c1, c2, p) => {
                    add(c1);
                    add(c2);
                    add(p);
                }
                PathEl::Close => {}
            }
        }
        bounds
    }

    /// The path flattened into polylines, each with whether it was closed.
    pub fn polylines(&self, tolerance: f32) -> Vec<(Vec<Point>, bool)> {
        let mut out = Vec::new();
        let mut current: Vec<Point> = Vec::new();
        let mut last = Point::default();
        fn finish(current: &mut Vec<Point>, closed: bool, out: &mut Vec<(Vec<Point>, bool)>) {
            if current.len() > 1 {
                out.push((std::mem::take(current), closed));
            } else {
                current.clear();
            }
        }
        for element in &self.elements {
            match *element {
                PathEl::MoveTo(p) => {
                    finish(&mut current, false, &mut out);
                    current.push(p);
                    last = p;
                }
                PathEl::LineTo(p) => {
                    if current.is_empty() {
                        current.push(last);
                    }
                    current.push(p);
                    last = p;
                }
                PathEl::QuadTo(c, p) => {
                    if current.is_empty() {
                        current.push(last);
                    }
                    flatten_quad(last, c, p, tolerance, &mut current);
                    last = p;
                }
                PathEl::CurveTo(c1, c2, p) => {
                    if current.is_empty() {
                        current.push(last);
                    }
                    flatten_cubic(last, c1, c2, p, tolerance, &mut current);
                    last = p;
                }
                PathEl::Close => {
                    if let Some(&first) = current.first() {
                        finish(&mut current, true, &mut out);
                        last = first;
                    }
                }
            }
        }
        finish(&mut current, false, &mut out);
        out
    }
}

fn lerp_len(a: Point, b: Point, c: Point) -> f32 {
    let x = a.x - 2.0 * b.x + c.x;
    let y = a.y - 2.0 * b.y + c.y;
    (x * x + y * y).sqrt()
}

/// Uniform subdivision: a quadratic's chord error with `n` segments is at
/// most |p0 − 2c + p1| / (4n²).
fn flatten_quad(p0: Point, c: Point, p1: Point, tolerance: f32, out: &mut Vec<Point>) {
    let n = ((lerp_len(p0, c, p1) / (4.0 * tolerance)).sqrt().ceil() as usize).clamp(1, 256);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let mt = 1.0 - t;
        out.push(Point::new(
            p0.x * mt * mt + 2.0 * c.x * mt * t + p1.x * t * t,
            p0.y * mt * mt + 2.0 * c.y * mt * t + p1.y * t * t,
        ));
    }
}

/// Uniform subdivision: a cubic's chord error with `n` segments is at most
/// 3M / (4n²), M being the larger second difference of its control points.
fn flatten_cubic(p0: Point, c1: Point, c2: Point, p1: Point, tolerance: f32, out: &mut Vec<Point>) {
    let m = lerp_len(p0, c1, c2).max(lerp_len(c1, c2, p1));
    let n = ((3.0 * m / (4.0 * tolerance)).sqrt().ceil() as usize).clamp(1, 256);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let mt = 1.0 - t;
        let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
        out.push(Point::new(
            p0.x * a + c1.x * b + c2.x * c + p1.x * d,
            p0.y * a + c1.y * b + c2.y * c + p1.y * d,
        ));
    }
}

/// An alpha mask placed in device pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mask {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn as_ref(&self) -> MaskRef<'_> {
        MaskRef {
            width: self.width,
            height: self.height,
            data: &self.data,
        }
    }

    pub fn bytes(&self) -> usize {
        self.data.len()
    }
}

/// Rasterizes the filled `path`, in device coordinates, within `clip`.
pub fn fill(path: &Path, clip: IRect) -> Option<Mask> {
    let (min, max) = path.bounds()?;
    if !(min.x.is_finite() && min.y.is_finite() && max.x.is_finite() && max.y.is_finite()) {
        return None;
    }
    let bounds = IRect::new(
        min.x.floor() as i32,
        min.y.floor() as i32,
        max.x.ceil() as i32 + 1,
        max.y.ceil() as i32 + 1,
    )
    .intersect(clip);
    if bounds.is_empty() {
        return None;
    }
    let mut accumulator = Accumulator::new(bounds.width() as usize, bounds.height() as usize);
    let origin = Point::new(bounds.x0 as f32, bounds.y0 as f32);
    for (points, _) in path.polylines(TOLERANCE) {
        for pair in points.windows(2) {
            accumulator.line(pair[0] - origin, pair[1] - origin);
        }
        // A fill closes every contour.
        accumulator.line(points[points.len() - 1] - origin, points[0] - origin);
    }
    Some(Mask {
        left: bounds.x0,
        top: bounds.y0,
        width: bounds.width() as u32,
        height: bounds.height() as u32,
        data: accumulator.coverage(),
    })
}

/// Signed area per cell. Each row has two spare cells beyond its last
/// column so a segment on the right edge can spill without bounds checks.
struct Accumulator {
    width: usize,
    height: usize,
    stride: usize,
    cells: Vec<f32>,
}

impl Accumulator {
    fn new(width: usize, height: usize) -> Self {
        let stride = width + 2;
        Self {
            width,
            height,
            stride,
            cells: vec![0.0; stride * height],
        }
    }

    /// Accumulates the segment, split where it leaves the columns: area left
    /// of column 0 counts toward every column (it lies on column 0's left
    /// edge), and area right of the last column is never visible.
    fn line(&mut self, p0: Point, p1: Point) {
        let right = self.width as f32;
        let mut cuts = [0.0f32, 1.0, 1.0, 1.0];
        let mut count = 1;
        if p0.x != p1.x {
            for edge in [0.0, right] {
                let t = (edge - p0.x) / (p1.x - p0.x);
                if t > 0.0 && t < 1.0 {
                    cuts[count] = t;
                    count += 1;
                }
            }
        }
        cuts[count] = 1.0;
        cuts[..=count].sort_by(f32::total_cmp);
        for pair in cuts[..=count].windows(2) {
            let at = |t: f32| {
                Point::new(
                    (p0.x + (p1.x - p0.x) * t).clamp(0.0, right),
                    p0.y + (p1.y - p0.y) * t,
                )
            };
            self.clamped_line(at(pair[0]), at(pair[1]));
        }
    }

    /// A segment whose x lies within the columns.
    fn clamped_line(&mut self, p0: Point, p1: Point) {
        if p0.y == p1.y {
            return;
        }
        let (dir, top, bottom) = if p0.y < p1.y {
            (1.0, p0, p1)
        } else {
            (-1.0, p1, p0)
        };
        let dxdy = (bottom.x - top.x) / (bottom.y - top.y);
        let y_start = top.y.max(0.0);
        let y_end = bottom.y.min(self.height as f32);
        if y_start >= y_end {
            return;
        }
        let mut x = top.x + (y_start - top.y) * dxdy;
        let right = self.width as f32;
        for row in y_start as usize..(y_end.ceil() as usize).min(self.height) {
            let dy = ((row + 1) as f32).min(y_end) - (row as f32).max(y_start);
            if dy <= 0.0 {
                continue;
            }
            let next = (x + dxdy * dy).clamp(0.0, right);
            self.span(row, x, next, dy * dir);
            x = next;
        }
    }

    /// Distributes a segment within one row, from `x` to `next` with signed
    /// height `d`, over the cells it crosses.
    fn span(&mut self, row: usize, x: f32, next: f32, d: f32) {
        let base = row * self.stride;
        let cells = &mut self.cells[base..base + self.stride];
        let (x0, x1) = if x < next { (x, next) } else { (next, x) };
        let x0_floor = x0.floor();
        let x0i = x0_floor as usize;
        let x1_ceil = x1.ceil();
        let x1i = x1_ceil as usize;
        if x1i <= x0i + 1 {
            let mid = 0.5 * (x + next) - x0_floor;
            cells[x0i] += d - d * mid;
            cells[x0i + 1] += d * mid;
            return;
        }
        let s = (x1 - x0).recip();
        let x0f = x0 - x0_floor;
        let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
        let x1f = x1 - x1_ceil + 1.0;
        let am = 0.5 * s * x1f * x1f;
        cells[x0i] += d * a0;
        if x1i == x0i + 2 {
            cells[x0i + 1] += d * (1.0 - a0 - am);
        } else {
            let a1 = s * (1.5 - x0f);
            cells[x0i + 1] += d * (a1 - a0);
            for cell in &mut cells[x0i + 2..x1i - 1] {
                *cell += d * s;
            }
            let a2 = a1 + (x1i - x0i - 3) as f32 * s;
            cells[x1i - 1] += d * (1.0 - a2 - am);
        }
        cells[x1i] += d * am;
    }

    fn coverage(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.width * self.height];
        for row in 0..self.height {
            let cells = &self.cells[row * self.stride..row * self.stride + self.width];
            let mut sum = 0.0f32;
            for (cell, coverage) in cells.iter().zip(&mut out[row * self.width..]) {
                sum += cell;
                *coverage = (sum.abs().min(1.0) * 255.0 + 0.5) as u8;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERYTHING: IRect = IRect::new(-1000, -1000, 1000, 1000);

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Path {
        Path::polygon(&[
            Point::new(x, y),
            Point::new(x + w, y),
            Point::new(x + w, y + h),
            Point::new(x, y + h),
        ])
    }

    fn at(mask: &Mask, x: i32, y: i32) -> u8 {
        if x < mask.left
            || y < mask.top
            || x >= mask.left + mask.width as i32
            || y >= mask.top + mask.height as i32
        {
            return 0;
        }
        mask.data[((y - mask.top) as u32 * mask.width + (x - mask.left) as u32) as usize]
    }

    fn total(mask: &Mask) -> f32 {
        mask.data.iter().map(|&c| f32::from(c) / 255.0).sum()
    }

    #[test]
    fn pixel_aligned_rectangles_are_crisp() {
        let mask = fill(&rect(1.0, 1.0, 2.0, 2.0), EVERYTHING).unwrap();
        for y in 0..5 {
            for x in 0..5 {
                let inside = (1..3).contains(&x) && (1..3).contains(&y);
                assert_eq!(at(&mask, x, y), if inside { 255 } else { 0 }, "({x}, {y})");
            }
        }
    }

    #[test]
    fn partial_pixels_get_partial_coverage() {
        let mask = fill(&rect(0.5, 0.0, 1.0, 1.0), EVERYTHING).unwrap();
        assert_eq!(at(&mask, 0, 0), 128);
        assert_eq!(at(&mask, 1, 0), 128);
        // A diagonal halves its pixel.
        let triangle = Path::polygon(&[
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(1.0, 1.0),
        ]);
        assert_eq!(at(&fill(&triangle, EVERYTHING).unwrap(), 0, 0), 128);
    }

    #[test]
    fn coverage_sums_to_the_area_for_either_orientation() {
        let points = [
            Point::new(2.3, 1.7),
            Point::new(17.9, 4.2),
            Point::new(6.1, 15.4),
        ];
        let area = 0.5
            * ((points[1].x - points[0].x) * (points[2].y - points[0].y)
                - (points[2].x - points[0].x) * (points[1].y - points[0].y))
                .abs();
        let forward = fill(&Path::polygon(&points), EVERYTHING).unwrap();
        let mut reversed = points;
        reversed.reverse();
        let backward = fill(&Path::polygon(&reversed), EVERYTHING).unwrap();
        assert!(
            (total(&forward) - area).abs() < 0.5,
            "{} vs {area}",
            total(&forward)
        );
        assert_eq!(forward, backward);
    }

    #[test]
    fn curves_flatten_closely() {
        // A circle of radius 10 from four cubic quarter arcs.
        let (cx, cy, r, k) = (20.0, 20.0, 10.0, 0.552_284_8 * 10.0);
        let mut path = Path::new();
        path.move_to(cx + r, cy);
        path.curve_to(cx + r, cy + k, cx + k, cy + r, cx, cy + r);
        path.curve_to(cx - k, cy + r, cx - r, cy + k, cx - r, cy);
        path.curve_to(cx - r, cy - k, cx - k, cy - r, cx, cy - r);
        path.curve_to(cx + k, cy - r, cx + r, cy - k, cx + r, cy);
        path.close();
        let mask = fill(&path, EVERYTHING).unwrap();
        // Flattened arcs are inscribed: the loss is at most the perimeter
        // times the tolerance.
        let area = std::f32::consts::PI * r * r;
        let loss = area - total(&mask);
        assert!(
            loss >= 0.0 && loss < std::f32::consts::TAU * r * TOLERANCE,
            "{loss}"
        );
        assert_eq!(at(&mask, 20, 20), 255);
        assert_eq!(at(&mask, 11, 11), 0, "wholly outside the radius");
    }

    #[test]
    fn opposite_contours_cut_holes_and_like_contours_union() {
        let mut ring = rect(0.0, 0.0, 6.0, 6.0);
        // The inner square runs the other way: a hole.
        ring.extend(&Path::polygon(&[
            Point::new(2.0, 2.0),
            Point::new(2.0, 4.0),
            Point::new(4.0, 4.0),
            Point::new(4.0, 2.0),
        ]));
        let mask = fill(&ring, EVERYTHING).unwrap();
        assert_eq!(at(&mask, 1, 1), 255);
        assert_eq!(at(&mask, 3, 3), 0);
        let mut overlap = rect(0.0, 0.0, 4.0, 4.0);
        overlap.extend(&rect(2.0, 2.0, 4.0, 4.0));
        let mask = fill(&overlap, EVERYTHING).unwrap();
        assert_eq!(at(&mask, 3, 3), 255, "clamped, not doubled or cancelled");
        assert_eq!(at(&mask, 5, 5), 255);
    }

    #[test]
    fn clipping_keeps_coverage_exact_at_the_clip_edges() {
        let path = Path::polygon(&[
            Point::new(-5.0, 0.0),
            Point::new(15.0, 0.0),
            Point::new(15.0, 20.0),
        ]);
        let clip = IRect::new(0, 0, 10, 10);
        let clipped = fill(&path, clip).unwrap();
        let whole = fill(&path, EVERYTHING).unwrap();
        assert_eq!(
            (clipped.left, clipped.top, clipped.width, clipped.height),
            (0, 0, 10, 10)
        );
        for y in 0..10 {
            for x in 0..10 {
                assert_eq!(at(&clipped, x, y), at(&whole, x, y), "({x}, {y})");
            }
        }
        assert!(fill(&rect(20.0, 20.0, 2.0, 2.0), clip).is_none());
    }
}
