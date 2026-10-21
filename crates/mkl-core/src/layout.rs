//! Monitor arrangement and screen-edge geometry.
//!
//! Every device is placed in a shared 2-D "layout space" (physical pixels). A device's
//! monitors keep their own relative arrangement; the device's bounding box is positioned at
//! `positions[id]`. Crossing an edge is resolved by looking for the nearest monitor of another
//! device that sits beyond that edge and overlaps it on the perpendicular axis.

use std::collections::HashMap;

use mkl_protocol::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Pixels inside the target edge where the cursor appears (avoids instantly bouncing back).
pub const EDGE_INSET: i32 = 2;
/// Largest gap between two devices in layout space that is still treated as adjacent.
pub const MAX_GAP: i32 = 600;
/// Overlap tolerated between devices that were dropped slightly on top of each other.
pub const EDGE_TOLERANCE: i32 = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    pub positions: HashMap<String, (i32, i32)>,
    pub revision: u64,
}

/// Monitor rectangles of one device translated into layout space.
pub fn layout_rects(screens: &[Rect], pos: (i32, i32)) -> Vec<Rect> {
    let Some(b) = Rect::bounding(screens) else { return Vec::new() };
    screens.iter().map(|r| r.translate(pos.0 - b.x, pos.1 - b.y)).collect()
}

pub fn nearest(rects: &[Rect], x: i32, y: i32) -> Option<Rect> {
    rects.iter().copied().min_by_key(|r| r.distance_sq(x, y))
}

pub fn contains_any(rects: &[Rect], x: i32, y: i32) -> bool {
    rects.iter().any(|r| r.contains(x, y))
}

pub fn clamp_to_screens(rects: &[Rect], x: i32, y: i32) -> (i32, i32) {
    nearest(rects, x, y).map(|r| r.clamp(x, y)).unwrap_or((x, y))
}

/// Where the cursor is parked on the controlling machine while it drives another one.
pub fn anchor_point(screens: &[Rect]) -> (i32, i32) {
    screens.first().map(|r| (r.x + r.w / 2, r.y + r.h / 2)).unwrap_or((400, 300))
}

/// Detects the cursor resting on an outer edge of the local desktop (an edge with no other
/// local monitor beyond it). Returns the direction and the clamped edge point.
pub fn edge_hit(screens: &[Rect], x: i32, y: i32, corner: i32) -> Option<(Dir, (i32, i32))> {
    edge_near(screens, x, y, corner, 0)
}

/// Like [`edge_hit`], but also matches points within `margin` pixels of an outer edge. The
/// returned point is projected onto the edge itself.
pub fn edge_near(screens: &[Rect], x: i32, y: i32, corner: i32, margin: i32) -> Option<(Dir, (i32, i32))> {
    let m = nearest(screens, x, y)?;
    let (cx, cy) = m.clamp(x, y);
    let checks = [
        (Dir::Left, cx <= m.x + margin, (m.x - 1, cy), (m.x, cy)),
        (Dir::Right, cx >= m.right() - 1 - margin, (m.right(), cy), (m.right() - 1, cy)),
        (Dir::Up, cy <= m.y + margin, (cx, m.y - 1), (cx, m.y)),
        (Dir::Down, cy >= m.bottom() - 1 - margin, (cx, m.bottom()), (cx, m.bottom() - 1)),
    ];
    for (dir, at_edge, beyond, on_edge) in checks {
        if !at_edge || contains_any(screens, beyond.0, beyond.1) {
            continue;
        }
        let in_corner = match dir {
            Dir::Left | Dir::Right => cy < m.y + corner || cy > m.bottom() - 1 - corner,
            Dir::Up | Dir::Down => cx < m.x + corner || cx > m.right() - 1 - corner,
        };
        if !in_corner {
            return Some((dir, on_edge));
        }
    }
    None
}

/// Direction in which `(nx, ny)` leaves `cur` (the axis with the larger overshoot wins).
pub fn exit_dir(cur: &Rect, nx: i32, ny: i32) -> Dir {
    let candidates = [
        (Dir::Right, nx - (cur.right() - 1)),
        (Dir::Left, cur.x - nx),
        (Dir::Down, ny - (cur.bottom() - 1)),
        (Dir::Up, cur.y - ny),
    ];
    candidates.into_iter().max_by_key(|(_, over)| *over).map(|(d, _)| d).unwrap_or(Dir::Right)
}

fn overlaps(a0: i32, a1: i32, b0: i32, b1: i32) -> bool {
    a0.max(b0) < a1.min(b1)
}

fn inset(len: i32) -> i32 {
    EDGE_INSET.min((len - 1).max(0))
}

impl Layout {
    pub fn to_sync(&self) -> mkl_protocol::LayoutSync {
        let mut positions: Vec<_> = self.positions.iter().map(|(id, (x, y))| (id.clone(), *x, *y)).collect();
        positions.sort();
        mkl_protocol::LayoutSync { revision: self.revision, positions }
    }

    /// Finds the device and point (in that device's own coordinates) reached by leaving
    /// device `from` at `pt` (in `from`'s coordinates) in direction `dir`.
    pub fn resolve(
        &self,
        from: &str,
        screens: &HashMap<String, Vec<Rect>>,
        pt: (i32, i32),
        dir: Dir,
    ) -> Option<(String, (i32, i32))> {
        let from_pos = *self.positions.get(from)?;
        let from_screens = screens.get(from)?;
        let fb = Rect::bounding(from_screens)?;
        let lx = from_pos.0 + pt.0 - fb.x;
        let ly = from_pos.1 + pt.1 - fb.y;
        let src = nearest(&layout_rects(from_screens, from_pos), lx, ly)?;

        let mut best: Option<(i64, String, (i32, i32))> = None;
        for (id, scr) in screens {
            if id == from {
                continue;
            }
            let Some(&pos) = self.positions.get(id) else { continue };
            let Some(tb) = Rect::bounding(scr) else { continue };
            for r in layout_rects(scr, pos) {
                let (gap, overlap, entry) = match dir {
                    Dir::Right => (
                        r.x - src.right(),
                        overlaps(src.y, src.bottom(), r.y, r.bottom()),
                        (r.x + inset(r.w), ly.clamp(r.y, r.bottom() - 1)),
                    ),
                    Dir::Left => (
                        src.x - r.right(),
                        overlaps(src.y, src.bottom(), r.y, r.bottom()),
                        (r.right() - 1 - inset(r.w), ly.clamp(r.y, r.bottom() - 1)),
                    ),
                    Dir::Down => (
                        r.y - src.bottom(),
                        overlaps(src.x, src.right(), r.x, r.right()),
                        (lx.clamp(r.x, r.right() - 1), r.y + inset(r.h)),
                    ),
                    Dir::Up => (
                        src.y - r.bottom(),
                        overlaps(src.x, src.right(), r.x, r.right()),
                        (lx.clamp(r.x, r.right() - 1), r.bottom() - 1 - inset(r.h)),
                    ),
                };
                if !overlap || gap < -EDGE_TOLERANCE || gap > MAX_GAP {
                    continue;
                }
                let perp = match dir {
                    Dir::Left | Dir::Right => (ly - entry.1).abs(),
                    Dir::Up | Dir::Down => (lx - entry.0).abs(),
                } as i64;
                let score = gap.abs() as i64 * 4 + perp;
                let target_pt = (entry.0 - pos.0 + tb.x, entry.1 - pos.1 + tb.y);
                if best.as_ref().map_or(true, |b| score < b.0) {
                    best = Some((score, id.clone(), target_pt));
                }
            }
        }
        best.map(|(_, id, p)| (id, p))
    }

    /// Position for a device that has none yet: to the right of everything already placed.
    pub fn default_position(&self, screens: &HashMap<String, Vec<Rect>>) -> (i32, i32) {
        let mut right = 0;
        let mut top = None;
        for (id, (x, y)) in &self.positions {
            let w = screens.get(id).and_then(|s| Rect::bounding(s)).map(|b| b.w).unwrap_or(1920);
            right = right.max(x + w);
            top = Some(top.map_or(*y, |t: i32| t.min(*y)));
        }
        (right, top.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Layout, HashMap<String, Vec<Rect>>) {
        let mut l = Layout::default();
        l.positions.insert("a".into(), (0, 0));
        l.positions.insert("b".into(), (1920, 0));
        let mut s = HashMap::new();
        s.insert("a".to_string(), vec![Rect::new(0, 0, 1920, 1080)]);
        s.insert("b".to_string(), vec![Rect::new(0, 0, 2560, 1440)]);
        (l, s)
    }

    #[test]
    fn right_edge_reaches_neighbour() {
        let (l, s) = setup();
        assert_eq!(l.resolve("a", &s, (1919, 500), Dir::Right), Some(("b".into(), (2, 500))));
        assert_eq!(l.resolve("a", &s, (0, 500), Dir::Left), None);
        assert_eq!(l.resolve("a", &s, (500, 1079), Dir::Down), None);
    }

    #[test]
    fn coming_back_clamps_to_overlap() {
        let (l, s) = setup();
        assert_eq!(l.resolve("b", &s, (0, 1300), Dir::Left), Some(("a".into(), (1917, 1079))));
    }

    #[test]
    fn multi_monitor_with_negative_coordinates() {
        let mut l = Layout::default();
        l.positions.insert("a".into(), (0, 0));
        l.positions.insert("b".into(), (3840, 0));
        let mut s = HashMap::new();
        s.insert("a".to_string(), vec![Rect::new(0, 0, 1920, 1080), Rect::new(-1920, 0, 1920, 1080)]);
        s.insert("b".to_string(), vec![Rect::new(0, 0, 1920, 1080)]);
        assert_eq!(l.resolve("a", &s, (1919, 10), Dir::Right), Some(("b".into(), (2, 10))));
        assert_eq!(l.resolve("b", &s, (0, 10), Dir::Left), Some(("a".into(), (1917, 10))));
    }

    #[test]
    fn vertical_arrangement() {
        let mut l = Layout::default();
        l.positions.insert("a".into(), (0, 0));
        l.positions.insert("b".into(), (200, 1080));
        let mut s = HashMap::new();
        s.insert("a".to_string(), vec![Rect::new(0, 0, 1920, 1080)]);
        s.insert("b".to_string(), vec![Rect::new(0, 0, 1366, 768)]);
        assert_eq!(l.resolve("a", &s, (1000, 1079), Dir::Down), Some(("b".into(), (800, 2))));
        assert_eq!(l.resolve("a", &s, (100, 1079), Dir::Down), Some(("b".into(), (0, 2))));
        assert_eq!(l.resolve("b", &s, (10, 0), Dir::Up), Some(("a".into(), (210, 1077))));
    }

    #[test]
    fn edge_detection_skips_inner_edges_and_corners() {
        let screens = vec![Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 1920, 1080)];
        assert_eq!(edge_hit(&screens, 1919, 500, 10), None);
        assert_eq!(edge_hit(&screens, 3839, 500, 10), Some((Dir::Right, (3839, 500))));
        assert_eq!(edge_hit(&screens, 3839, 3, 10), None);
        assert_eq!(edge_hit(&screens, 0, 500, 10), Some((Dir::Left, (0, 500))));
        assert_eq!(edge_hit(&screens, 500, 500, 10), None);
    }

    #[test]
    fn exit_direction() {
        let r = Rect::new(0, 0, 100, 100);
        assert_eq!(exit_dir(&r, 105, 50), Dir::Right);
        assert_eq!(exit_dir(&r, -3, 50), Dir::Left);
        assert_eq!(exit_dir(&r, 50, 120), Dir::Down);
        assert_eq!(exit_dir(&r, 50, -1), Dir::Up);
    }

    #[test]
    fn default_position_appends_to_the_right() {
        let (l, s) = setup();
        assert_eq!(l.default_position(&s), (1920 + 2560, 0));
    }
}
