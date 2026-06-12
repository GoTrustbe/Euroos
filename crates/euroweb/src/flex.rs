//! EuroWeb flexbox (Sprint AB-B4): de CSS Flexible Box-hoofdas-algoritmiek.
//!
//! Berekent voor een flex-container de **hoofdas-grootte en -positie** van elk
//! item: flex-basis als startpunt, vrije ruimte verdeeld via `flex-grow`, tekort
//! opgevangen via `flex-shrink` (gewogen naar basis), en uitlijning via
//! `justify-content`. Richting (`row`/`column`) bepaalt enkel welke as de hoofdas
//! is; de berekening is identiek. Pure, host-geteste `no_std`-logica die de
//! [`crate::layout`]-engine voor `display:flex` kan aanroepen.

use alloc::vec::Vec;

/// Eén flex-item op de hoofdas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlexItem {
    /// Uitgangsgrootte (flex-basis of breedte/hoogte) in px.
    pub basis: f32,
    pub grow: f32,
    pub shrink: f32,
    /// Minimale grootte (item krimpt nooit hieronder).
    pub min: f32,
}

impl FlexItem {
    pub fn new(basis: f32, grow: f32, shrink: f32) -> Self {
        FlexItem { basis, grow, shrink, min: 0.0 }
    }
}

/// De uitkomst per item: positie + grootte op de hoofdas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlexResult {
    pub main_pos: f32,
    pub main_size: f32,
}

/// Uitlijning van de vrije ruimte op de hoofdas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
}

/// Bereken de hoofdas-layout. `gap` = vaste tussenruimte tussen items.
pub fn solve(container: f32, items: &[FlexItem], justify: Justify, gap: f32) -> Vec<FlexResult> {
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let total_gap = gap * (n as f32 - 1.0);
    let total_basis: f32 = items.iter().map(|i| i.basis).sum();
    let free = container - total_basis - total_gap;

    // 1) Bepaal de grootte van elk item.
    let mut sizes: Vec<f32> = items.iter().map(|i| i.basis).collect();
    if free > 0.0 {
        let total_grow: f32 = items.iter().map(|i| i.grow).sum();
        if total_grow > 0.0 {
            for (s, it) in sizes.iter_mut().zip(items) {
                *s += free * (it.grow / total_grow);
            }
        }
    } else if free < 0.0 {
        // Tekort: verdeel gewogen naar shrink×basis (CSS-conventie).
        let weighted: f32 = items.iter().map(|i| i.shrink * i.basis).sum();
        if weighted > 0.0 {
            for (s, it) in sizes.iter_mut().zip(items) {
                let share = (it.shrink * it.basis) / weighted;
                *s = (*s + free * share).max(it.min);
            }
        }
    }

    // 2) Werkelijk gebruikte ruimte (na grow/shrink) → resterende vrije ruimte voor justify.
    let used: f32 = sizes.iter().sum::<f32>() + total_gap;
    let leftover = (container - used).max(0.0);

    // 3) Beginpositie + tussenruimte volgens justify-content.
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
        // Container 300, twee items basis 50, grow 1 elk → elk 50 + 100 = 150.
        let items = [FlexItem::new(50.0, 1.0, 1.0), FlexItem::new(50.0, 1.0, 1.0)];
        let r = solve(300.0, &items, Justify::Start, 0.0);
        assert!(approx(r[0].main_size, 150.0) && approx(r[1].main_size, 150.0));
        assert!(approx(r[0].main_pos, 0.0) && approx(r[1].main_pos, 150.0));
    }

    #[test]
    fn grow_weighted() {
        // grow 1 vs 3 → vrije 200 verdeeld 50/150.
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
        // Container 100, twee items basis 80, shrink 1 → krimpen naar 50 elk.
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
        assert!(r[0].main_size >= 70.0); // klemt op min
    }

    #[test]
    fn justify_center_and_between() {
        let items = [FlexItem::new(50.0, 0.0, 0.0), FlexItem::new(50.0, 0.0, 0.0)];
        // Center: leftover 200 → start op 100.
        let c = solve(300.0, &items, Justify::Center, 0.0);
        assert!(approx(c[0].main_pos, 100.0));
        // Space-between: eerste op 0, tweede op 250.
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
