//! EuroWeb flexbox (Sprint AB-B4): the CSS Flexible Box main-axis algorithm.
//!
//! Computes for a flex container the **main-axis size and position** of each
//! item: flex-basis as the starting point, free space distributed via `flex-grow`,
//! shortfall absorbed via `flex-shrink` (weighted to basis), and alignment via
//! `justify-content`. Direction (`row`/`column`) only determines which axis is the
//! main axis; the computation is identical. Pure, host-tested `no_std` logic that
//! the [`crate::layout`] engine can call for `display:flex`.

use alloc::vec::Vec;

/// A single flex item on the main axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlexItem {
    /// Base size (flex-basis or width/height) in px.
    pub basis: f32,
    pub grow: f32,
    pub shrink: f32,
    /// Minimum size (item never shrinks below this).
    pub min: f32,
}

impl FlexItem {
    pub fn new(basis: f32, grow: f32, shrink: f32) -> Self {
        FlexItem { basis, grow, shrink, min: 0.0 }
    }
}

/// The result per item: position + size on the main axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlexResult {
    pub main_pos: f32,
    pub main_size: f32,
}

/// Alignment of the free space on the main axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
}

/// Compute the main-axis layout. `gap` = fixed spacing between items.
pub fn solve(container: f32, items: &[FlexItem], justify: Justify, gap: f32) -> Vec<FlexResult> {
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let total_gap = gap * (n as f32 - 1.0);
    let total_basis: f32 = items.iter().map(|i| i.basis).sum();
    let free = container - total_basis - total_gap;

    // 1) Determine the size of each item.
    let mut sizes: Vec<f32> = items.iter().map(|i| i.basis).collect();
    if free > 0.0 {
        let total_grow: f32 = items.iter().map(|i| i.grow).sum();
        if total_grow > 0.0 {
            for (s, it) in sizes.iter_mut().zip(items) {
                *s += free * (it.grow / total_grow);
            }
        }
    } else if free < 0.0 {
        // Shortfall: distribute weighted to shrink×basis (CSS convention).
        let weighted: f32 = items.iter().map(|i| i.shrink * i.basis).sum();
        if weighted > 0.0 {
            for (s, it) in sizes.iter_mut().zip(items) {
                let share = (it.shrink * it.basis) / weighted;
                *s = (*s + free * share).max(it.min);
            }
        }
    }

    // 2) Actually used space (after grow/shrink) → remaining free space for justify.
    let used: f32 = sizes.iter().sum::<f32>() + total_gap;
    let leftover = (container - used).max(0.0);

    // 3) Start position + spacing according to justify-content.
    let (mut pos, between) = match justify {
        Justify::Start => (0.0, gap),
        Justify::End => (leftover, gap),
        Justify::Center => (leftover / 2.0, gap),
        Justify::SpaceBetween => {
            let extra = if n > 1 { leftover / (n as f32 - 1.0) } else { 0.0 };
            (0.0, gap + extra)
        }
        Justify::SpaceAround => {
            let unit = leftover / n as f32;
            (unit / 2.0, gap + unit)
        }
    };

    let mut out = Vec::with_capacity(n);
    for (i, &size) in sizes.iter().enumerate() {
        out.push(FlexResult { main_pos: pos, main_size: size });
        pos += size + if i + 1 < n { between } else { 0.0 };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn grow_distributes_free_space() {
        // Container 300, two items basis 50, grow 1 each → each 50 + 100 = 150.
        let items = [FlexItem::new(50.0, 1.0, 1.0), FlexItem::new(50.0, 1.0, 1.0)];
        let r = solve(300.0, &items, Justify::Start, 0.0);
        assert!(approx(r[0].main_size, 150.0) && approx(r[1].main_size, 150.0));
        assert!(approx(r[0].main_pos, 0.0) && approx(r[1].main_pos, 150.0));
    }

    #[test]
    fn grow_weighted() {
        // grow 1 vs 3 → free 200 distributed 50/150.
        let items = [FlexItem::new(50.0, 1.0, 0.0), FlexItem::new(50.0, 3.0, 0.0)];
        let r = solve(300.0, &items, Justify::Start, 0.0);
        assert!(approx(r[0].main_size, 100.0)); // 50 + 50
        assert!(approx(r[1].main_size, 200.0)); // 50 + 150
    }

    #[test]
    fn no_grow_leaves_basis() {
        let items = [FlexItem::new(50.0, 0.0, 0.0), FlexItem::new(50.0, 0.0, 0.0)];
        let r = solve(300.0, &items, Justify::Start, 0.0);
        assert!(approx(r[0].main_size, 50.0) && approx(r[1].main_size, 50.0));
    }

    #[test]
    fn shrink_on_overflow() {
        // Container 100, two items basis 80, shrink 1 → shrink to 50 each.
        let items = [FlexItem::new(80.0, 0.0, 1.0), FlexItem::new(80.0, 0.0, 1.0)];
        let r = solve(100.0, &items, Justify::Start, 0.0);
        assert!(approx(r[0].main_size, 50.0) && approx(r[1].main_size, 50.0));
    }

    #[test]
    fn shrink_respects_min() {
        let mut a = FlexItem::new(80.0, 0.0, 1.0);
        a.min = 70.0;
        let b = FlexItem::new(80.0, 0.0, 1.0);
        let r = solve(100.0, &[a, b], Justify::Start, 0.0);
        assert!(r[0].main_size >= 70.0); // clamps at min
    }

    #[test]
    fn justify_center_and_between() {
        let items = [FlexItem::new(50.0, 0.0, 0.0), FlexItem::new(50.0, 0.0, 0.0)];
        // Center: leftover 200 → start at 100.
        let c = solve(300.0, &items, Justify::Center, 0.0);
        assert!(approx(c[0].main_pos, 100.0));
        // Space-between: first at 0, second at 250.
        let sb = solve(300.0, &items, Justify::SpaceBetween, 0.0);
        assert!(approx(sb[0].main_pos, 0.0) && approx(sb[1].main_pos, 250.0));
    }

    #[test]
    fn gap_between_items() {
        let items = [FlexItem::new(50.0, 0.0, 0.0), FlexItem::new(50.0, 0.0, 0.0)];
        let r = solve(300.0, &items, Justify::Start, 20.0);
        assert!(approx(r[1].main_pos, 70.0)); // 50 + gap 20
    }

    #[test]
    fn empty_is_empty() {
        assert!(solve(300.0, &[], Justify::Start, 0.0).is_empty());
    }
}
