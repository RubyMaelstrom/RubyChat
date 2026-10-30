//! Damage: the device rectangles a frame changed. Presentation copies only
//! these (and older frames' damage the reused buffer lacks) to the window.

use super::geom::IRect;

/// Past this many rectangles, damage collapses into their bounding box:
/// copying a little extra is cheaper than tracking many pieces.
const MAX_RECTS: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    rects: Vec<IRect>,
}

impl Damage {
    pub fn add(&mut self, rect: IRect) {
        if rect.is_empty() || self.rects.iter().any(|r| r.contains_rect(rect)) {
            return;
        }
        self.rects.retain(|r| !rect.contains_rect(*r));
        self.rects.push(rect);
        if self.rects.len() > MAX_RECTS {
            let bounds = self.bounds();
            self.rects.clear();
            self.rects.push(bounds);
        }
    }

    pub fn extend(&mut self, other: &Damage) {
        for &rect in &other.rects {
            self.add(rect);
        }
    }

    pub fn rects(&self) -> &[IRect] {
        &self.rects
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn bounds(&self) -> IRect {
        self.rects.iter().fold(IRect::EMPTY, |acc, &r| acc.union(r))
    }

    /// Total damaged pixels, counting overlaps twice.
    pub fn area(&self) -> i64 {
        self.rects.iter().map(|r| r.area()).sum()
    }

    pub fn clip(&mut self, bounds: IRect) {
        for rect in &mut self.rects {
            *rect = rect.intersect(bounds);
        }
        self.rects.retain(|r| !r.is_empty());
    }

    pub fn clear(&mut self) {
        self.rects.clear();
    }

    pub fn take(&mut self) -> Damage {
        std::mem::take(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contained_rectangles_are_absorbed_and_many_collapse() {
        let mut damage = Damage::default();
        damage.add(IRect::new(0, 0, 10, 10));
        damage.add(IRect::new(2, 2, 4, 4));
        assert_eq!(damage.rects(), [IRect::new(0, 0, 10, 10)]);
        damage.add(IRect::new(-5, -5, 20, 20));
        assert_eq!(damage.rects(), [IRect::new(-5, -5, 20, 20)]);
        damage.add(IRect::EMPTY);
        assert_eq!(damage.rects().len(), 1);
        let mut many = Damage::default();
        for i in 0..=MAX_RECTS as i32 {
            many.add(IRect::new(i * 10, 0, i * 10 + 5, 5));
        }
        assert_eq!(
            many.rects(),
            [IRect::new(0, 0, MAX_RECTS as i32 * 10 + 5, 5)]
        );
        many.clip(IRect::new(0, 0, 20, 2));
        assert_eq!(many.rects(), [IRect::new(0, 0, 20, 2)]);
    }
}
