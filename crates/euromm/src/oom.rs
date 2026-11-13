//! Out of memory: who has to go. A page fault that cannot be given a frame
//! after everything reclaimable was reclaimed ends ONE process, chosen here,
//! and never the kernel (docs/design/MEMORY-RECLAIM.md).
//!
//! The launched process (the browser itself) is never chosen while it has a
//! child: its children (renderers, utilities) are what it can restart. Among
//! the children the one that holds the most private memory goes.

/// What to do about a fault that found the pool empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Victim {
    /// End this other process (its pid) now and serve the fault from what it held.
    Other(u64),
    /// The faulting process is itself the largest child: it is the one to end.
    Current,
    /// Nothing to end: the faulting process is the launched process and has
    /// no child that holds memory. The fault fails.
    None,
}

/// `children`: (pid, private pages) of every child process. `current`: the
/// pid of the faulting process when it is a child, None when it is the
/// launched process. A child that holds less than `min_pages` is not worth
/// ending (it would not serve the fault).
pub fn pick_victim(children: &[(u64, usize)], current: Option<u64>, min_pages: usize) -> Victim {
    let largest = children.iter().copied().filter(|&(_, p)| p >= min_pages)
        // the most pages; among equals the youngest (highest pid)
        .max_by_key(|&(pid, p)| (p, pid));
    match (largest, current) {
        (Some((pid, _)), Some(cur)) if pid == cur => Victim::Current,
        (Some((pid, _)), _) => Victim::Other(pid),
        // A child that faults with nothing worth ending around it ends itself.
        (None, Some(_)) => Victim::Current,
        (None, None) => Victim::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launched_process_is_never_the_victim_while_a_child_holds_memory() {
        let kids = [(1000, 50), (1001, 4000), (1002, 300)];
        assert_eq!(pick_victim(&kids, None, 16), Victim::Other(1001));
        assert_eq!(pick_victim(&kids[..1], None, 16), Victim::Other(1000));
        assert_eq!(pick_victim(&[], None, 16), Victim::None);
        assert_eq!(pick_victim(&[(1000, 3)], None, 16), Victim::None, "ending it would free nothing");
    }

    #[test]
    fn a_child_that_is_the_largest_goes_itself() {
        let kids = [(1000, 50), (1001, 4000), (1002, 300)];
        assert_eq!(pick_victim(&kids, Some(1001), 16), Victim::Current);
        assert_eq!(pick_victim(&kids, Some(1002), 16), Victim::Other(1001));
        assert_eq!(pick_victim(&[(1002, 3)], Some(1002), 16), Victim::Current);
        // Equal sizes: the youngest.
        assert_eq!(pick_victim(&[(1000, 70), (1005, 70), (1003, 70)], Some(1000), 16), Victim::Other(1005));
    }

    #[test]
    fn every_choice_is_a_child_that_exists() {
        let mut seed = 0x1234_5678u64;
        let mut rnd = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for _ in 0..5000 {
            let n = rnd(6) as usize;
            let kids: Vec<(u64, usize)> = (0..n).map(|i| (1000 + i as u64, rnd(500) as usize)).collect();
            let cur = if n > 0 && rnd(2) == 0 { Some(1000 + rnd(n as u64)) } else { None };
            match pick_victim(&kids, cur, 16) {
                Victim::Other(pid) => {
                    let p = kids.iter().find(|k| k.0 == pid).expect("a pid that is not a child").1;
                    assert!(p >= 16 && Some(pid) != cur);
                    assert!(kids.iter().all(|k| k.1 <= p), "not the largest");
                }
                Victim::Current => assert!(cur.is_some()),
                Victim::None => assert!(cur.is_none() && kids.iter().all(|k| k.1 < 16)),
            }
        }
    }
}
