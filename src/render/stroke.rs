//! Strokes for thin chrome lines and icons. Each flattened segment becomes a
//! quad, and round joins and caps become discs, all in one orientation so the
//! rasterizer fills their union (see `raster`).

use std::f32::consts::TAU;

use super::geom::Point;
use super::raster::{Path, TOLERANCE};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cap {
    #[default]
    Butt,
    Round,
}

/// The outline of `path` stroked `width` wide, in the path's coordinates.
pub fn stroke(path: &Path, width: f32, cap: Cap) -> Path {
    let half = width / 2.0;
    let mut out = Path::new();
    if half <= 0.0 {
        return out;
    }
    for (points, closed) in path.polylines(TOLERANCE) {
        let mut points = points;
        points.dedup_by(|a, b| (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4);
        if closed && points.len() > 2 && points.first() == points.last() {
            points.pop();
        }
        if points.len() < 2 {
            if cap == Cap::Round
                && let Some(&point) = points.first()
            {
                disc(&mut out, point, half);
            }
            continue;
        }
        for pair in points.windows(2) {
            quad(&mut out, pair[0], pair[1], half);
        }
        let n = points.len();
        if closed {
            quad(&mut out, points[n - 1], points[0], half);
            for i in 0..n {
                join(
                    &mut out,
                    points[(i + n - 1) % n],
                    points[i],
                    points[(i + 1) % n],
                    half,
                );
            }
        } else {
            for i in 1..n - 1 {
                join(&mut out, points[i - 1], points[i], points[i + 1], half);
            }
            if cap == Cap::Round {
                disc(&mut out, points[0], half);
                disc(&mut out, points[n - 1], half);
            }
        }
    }
    out
}

fn quad(out: &mut Path, a: Point, b: Point, half: f32) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f32::EPSILON {
        return;
    }
    let n = Point::new(-dy / length * half, dx / length * half);
    polygon(out, &[a + n, b + n, b - n, a - n]);
}

/// A round join where the direction turns; straight runs need none.
fn join(out: &mut Path, before: Point, at: Point, after: Point, half: f32) {
    let (ax, ay) = (at.x - before.x, at.y - before.y);
    let (bx, by) = (after.x - at.x, after.y - at.y);
    let cross = ax * by - ay * bx;
    let dot = ax * bx + ay * by;
    let lengths = (ax * ax + ay * ay).sqrt() * (bx * bx + by * by).sqrt();
    if lengths > 0.0 && dot > 0.0 && (cross / lengths).abs() < 0.02 {
        return;
    }
    disc(out, at, half);
}

fn disc(out: &mut Path, center: Point, radius: f32) {
    // Enough sides that the polygon stays within the flattening tolerance.
    let sides = (TAU / (2.0 * (1.0 - TOLERANCE / radius).clamp(-1.0, 1.0).acos())).ceil();
    let sides = if sides.is_finite() {
        (sides as usize).clamp(8, 64)
    } else {
        8
    };
    let points: Vec<Point> = (0..sides)
        .map(|i| {
            let angle = i as f32 * TAU / sides as f32;
            Point::new(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            )
        })
        .collect();
    polygon(out, &points);
}

/// Appends a closed polygon in the orientation every stroke piece shares.
fn polygon(out: &mut Path, points: &[Point]) {
    let mut area = 0.0;
    for (i, a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        area += a.x * b.y - b.x * a.y;
    }
    if area < 0.0 {
        let reversed: Vec<Point> = points.iter().rev().copied().collect();
        out.extend(&Path::polygon(&reversed));
    } else {
        out.extend(&Path::polygon(points));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::geom::IRect;
    use crate::render::raster::fill;

    const EVERYTHING: IRect = IRect::new(-100, -100, 100, 100);

    fn coverage(path: &Path, x: i32, y: i32) -> u8 {
        let mask = fill(path, EVERYTHING).unwrap();
        let (column, row) = (x - mask.left, y - mask.top);
        if column < 0 || row < 0 || column >= mask.width as i32 || row >= mask.height as i32 {
            return 0;
        }
        mask.data[(row as u32 * mask.width + column as u32) as usize]
    }

    #[test]
    fn a_pixel_aligned_line_fills_whole_rows() {
        let mut line = Path::new();
        line.move_to(0.0, 2.0);
        line.line_to(10.0, 2.0);
        let outline = stroke(&line, 2.0, Cap::Butt);
        assert_eq!(coverage(&outline, 5, 1), 255);
        assert_eq!(coverage(&outline, 5, 2), 255);
        let mask = fill(&outline, EVERYTHING).unwrap();
        assert_eq!(
            (mask.top, mask.height),
            (1, 3),
            "rows 1 and 2, plus the empty spare row"
        );
        assert_eq!(*mask.data.last().unwrap(), 0);
    }

    #[test]
    fn round_caps_extend_past_the_ends_and_corners_stay_filled() {
        let mut corner = Path::new();
        corner.move_to(5.0, 5.0);
        corner.line_to(15.0, 5.0);
        corner.line_to(15.0, 15.0);
        let butt = stroke(&corner, 3.0, Cap::Butt);
        let round = stroke(&corner, 3.0, Cap::Round);
        assert_eq!(coverage(&butt, 4, 5), 0);
        assert!(coverage(&round, 4, 5) > 200, "the cap reaches back");
        // The join fills the corner's outside.
        assert_eq!(coverage(&butt, 15, 4), 255);
        assert_eq!(coverage(&butt, 10, 5), 255);
    }
}
