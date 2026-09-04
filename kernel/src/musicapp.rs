//! Boot self-test for **EuroMusic** (AC-3): library + playback queue.
//! Core: [`euromusic`].

use crate::serial_println;
use euromusic::{Library, Player, Repeat, Track};

pub fn selftest() {
    let mut lib = Library::new();
    lib.add(Track::new("Aurora", "Nordlys", "Polar", 2, 200));
    lib.add(Track::new("Borealis", "Nordlys", "Polar", 1, 180));
    lib.add(Track::new("Cendres", "Lumière", "Feu", 1, 240));

    let search_ok = lib.search("nord").len() == 2;
    let album_ok = lib.album("Polar") == alloc::vec![1, 0]; // sorted by track number
    let dur_ok = lib.total_duration() == 620;

    // Queue: sequential + repeat-all + shuffle permutation.
    let mut p = Player::new(alloc::vec![0, 1, 2]);
    let seq_ok = p.current() == Some(0) && p.next_index() == Some(1) && p.next_index() == Some(2) && p.next_index().is_none();
    p.set_repeat(Repeat::All);
    let wrap_ok = p.next_index() == Some(0); // past the end, all wraps back to 0

    let mut s = Player::new(alloc::vec![0, 1, 2, 3, 4]);
    s.set_shuffle(true, 42);
    let mut seen = alloc::vec![s.current().unwrap()];
    s.set_repeat(Repeat::Off);
    while let Some(n) = s.next_index() {
        seen.push(n);
    }
    seen.sort();
    let shuffle_ok = seen == alloc::vec![0, 1, 2, 3, 4];

    let ok = search_ok && album_ok && dur_ok && seq_ok && wrap_ok && shuffle_ok;
    serial_println!(
        "[mu] EuroMusic: search={}, album-sort={}, duration={}, queue={}, repeat-all-wrap={}, shuffle-permutation={} {}",
        search_ok, album_ok, dur_ok, seq_ok, wrap_ok, shuffle_ok,
        if ok { "✓" } else { "✗ FAIL" }
    );
}
