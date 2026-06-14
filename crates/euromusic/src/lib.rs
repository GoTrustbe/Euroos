//! EuroMusic — the music player core of EuroOS (Sprint AC-3).
//!
//! A **library** (search, sort, group by album/artist), **playlists**,
//! and a **playback queue** with repeat modes (off/one/all) and
//! **shuffle** (deterministic via a supplied seed, so it stays host-testable).
//! No streaming telemetry — sovereign and local. Pure `no_std` logic.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A single music track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_no: u32,
    pub duration_s: u32,
    pub path: String,
}

impl Track {
    pub fn new(title: &str, artist: &str, album: &str, track_no: u32, duration_s: u32) -> Self {
        Track {
            title: title.to_string(),
            artist: artist.to_string(),
            album: album.to_string(),
            track_no,
            duration_s,
            path: alloc::format!("/music/{artist}/{album}/{track_no:02}-{title}.flac"),
        }
    }
}

/// Format a duration as `M:SS` or `H:MM:SS`.
pub fn format_duration(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        alloc::format!("{}:{:02}:{:02}", h, m, s)
    } else {
        alloc::format!("{}:{:02}", m, s)
    }
}

/// The music library.
#[derive(Debug, Clone, Default)]
pub struct Library {
    pub tracks: Vec<Track>,
}

impl Library {
    pub fn new() -> Self {
        Library::default()
    }
    pub fn add(&mut self, t: Track) {
        self.tracks.push(t);
    }

    /// Search by title/artist/album (substring, case-insensitive).
    pub fn search(&self, query: &str) -> Vec<usize> {
        let q = query.to_lowercase();
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                q.is_empty()
                    || t.title.to_lowercase().contains(&q)
                    || t.artist.to_lowercase().contains(&q)
                    || t.album.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Track indices of an album, sorted by track number.
    pub fn album(&self, name: &str) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.album == name)
            .map(|(i, _)| i)
            .collect();
        v.sort_by_key(|&i| self.tracks[i].track_no);
        v
    }

    /// Unique artists, alphabetically.
    pub fn artists(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for t in &self.tracks {
            if !v.contains(&t.artist) {
                v.push(t.artist.clone());
            }
        }
        v.sort();
        v
    }

    /// Total playing time (seconds).
    pub fn total_duration(&self) -> u64 {
        self.tracks.iter().map(|t| t.duration_s as u64).sum()
    }
}

/// Repeat mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Off,
    One,
    All,
}

/// A playback queue over indices into a [`Library`].
#[derive(Debug, Clone)]
pub struct Player {
    /// The base order (track indices).
    base: Vec<usize>,
    /// The active order (= base, or a shuffle permutation).
    order: Vec<usize>,
    pos: usize,
    pub repeat: Repeat,
    pub shuffle: bool,
}

impl Player {
    /// Create a queue from a sequence of track indices.
    pub fn new(queue: Vec<usize>) -> Self {
        let order = queue.clone();
        Player { base: queue, order, pos: 0, repeat: Repeat::Off, shuffle: false }
    }

    /// The current track index (None if the queue is empty/finished).
    pub fn current(&self) -> Option<usize> {
        self.order.get(self.pos).copied()
    }

    /// Set the repeat mode.
    pub fn set_repeat(&mut self, r: Repeat) {
        self.repeat = r;
    }

    /// Toggle shuffle on/off. On → generate a deterministic permutation from
    /// `seed` (Fisher-Yates with an LCG), with the current track at the front.
    pub fn set_shuffle(&mut self, on: bool, seed: u64) {
        let cur = self.current();
        self.shuffle = on;
        if on {
            self.order = shuffled(&self.base, seed);
            // Keep the current track at the top so it does not jump around.
            if let Some(c) = cur {
                if let Some(p) = self.order.iter().position(|&x| x == c) {
                    self.order.swap(0, p);
                }
            }
            self.pos = 0;
        } else {
            self.order = self.base.clone();
            self.pos = cur.and_then(|c| self.order.iter().position(|&x| x == c)).unwrap_or(0);
        }
    }

    /// Next track according to the repeat mode. Returns the new current index.
    pub fn next(&mut self) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        match self.repeat {
            Repeat::One => {} // stay put
            Repeat::All => {
                // Wrap to 0, also from the "stopped" state (pos == len) after a
                // previous Repeat::Off — `(pos+1) % len` would give 1 instead of 0 there.
                self.pos = if self.pos + 1 >= self.order.len() { 0 } else { self.pos + 1 };
            }
            Repeat::Off => {
                if self.pos + 1 < self.order.len() {
                    self.pos += 1;
                } else {
                    self.pos = self.order.len(); // past the end → stopped
                }
            }
        }
        self.current()
    }

    /// Previous track.
    pub fn prev(&mut self) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        match self.repeat {
            Repeat::One => {}
            Repeat::All => self.pos = (self.pos + self.order.len() - 1) % self.order.len(),
            Repeat::Off => {
                self.pos = self.pos.saturating_sub(1);
            }
        }
        self.current()
    }
}

/// Deterministic Fisher-Yates shuffle with an LCG (no global RNG).
fn shuffled(items: &[usize], seed: u64) -> Vec<usize> {
    let mut v = items.to_vec();
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut i = v.len();
    while i > 1 {
        i -= 1;
        // LCG (Numerical Recipes constants).
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let j = (state >> 33) as usize % (i + 1);
        v.swap(i, j);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        let mut l = Library::new();
        l.add(Track::new("Aurora", "Nordlys", "Polar", 2, 200));
        l.add(Track::new("Borealis", "Nordlys", "Polar", 1, 180));
        l.add(Track::new("Cendres", "Lumière", "Feu", 1, 240));
        l
    }

    #[test]
    fn duration_format() {
        assert_eq!(format_duration(200), "3:20");
        assert_eq!(format_duration(3 * 3600 + 5), "3:00:05");
    }

    #[test]
    fn library_search_album_artists() {
        let l = lib();
        assert_eq!(l.search("nord").len(), 2);
        assert_eq!(l.search("cendres"), alloc::vec![2]);
        // Album sorted by track number → Borealis(1) before Aurora(2).
        let polar = l.album("Polar");
        assert_eq!(polar, alloc::vec![1, 0]);
        assert_eq!(l.artists(), alloc::vec!["Lumière".to_string(), "Nordlys".to_string()]);
        assert_eq!(l.total_duration(), 620);
    }

    #[test]
    fn player_sequential_and_repeat_off() {
        let mut p = Player::new(alloc::vec![0, 1, 2]);
        assert_eq!(p.current(), Some(0));
        assert_eq!(p.next(), Some(1));
        assert_eq!(p.next(), Some(2));
        assert_eq!(p.next(), None); // past the end
    }

    #[test]
    fn player_repeat_all_and_one() {
        let mut p = Player::new(alloc::vec![0, 1, 2]);
        p.set_repeat(Repeat::All);
        p.next();
        p.next();
        assert_eq!(p.next(), Some(0)); // wraps around
        p.set_repeat(Repeat::One);
        assert_eq!(p.next(), Some(0)); // stays put
        assert_eq!(p.prev(), Some(0));
    }

    #[test]
    fn repeat_all_wraps_from_stopped_state() {
        // Play to the end with Repeat::Off (pos lands on len = "stopped"), then switch
        // to All and continue → must wrap to track 0, not 1.
        let mut p = Player::new(alloc::vec![0, 1, 2]);
        assert_eq!(p.next(), Some(1));
        assert_eq!(p.next(), Some(2));
        assert_eq!(p.next(), None); // stopped past the end
        p.set_repeat(Repeat::All);
        assert_eq!(p.next(), Some(0));
    }

    #[test]
    fn player_prev_wraps_in_all() {
        let mut p = Player::new(alloc::vec![0, 1, 2]);
        p.set_repeat(Repeat::All);
        assert_eq!(p.prev(), Some(2)); // 0 → previous → 2
    }

    #[test]
    fn shuffle_is_permutation_and_deterministic() {
        let mut p = Player::new(alloc::vec![0, 1, 2, 3, 4]);
        p.set_shuffle(true, 42);
        let mut seen = Vec::new();
        seen.push(p.current().unwrap());
        while let Some(n) = {
            p.set_repeat(Repeat::Off);
            p.next()
        } {
            seen.push(n);
        }
        seen.sort();
        assert_eq!(seen, alloc::vec![0, 1, 2, 3, 4]); // all tracks exactly once
        // Same seed → same order (reproducible).
        let mut q = Player::new(alloc::vec![0, 1, 2, 3, 4]);
        q.set_shuffle(true, 42);
        let mut p2 = Player::new(alloc::vec![0, 1, 2, 3, 4]);
        p2.set_shuffle(true, 42);
        assert_eq!(q.current(), p2.current());
    }

    #[test]
    fn shuffle_keeps_current_first() {
        let mut p = Player::new(alloc::vec![0, 1, 2, 3, 4]);
        p.next(); // current = 1
        p.set_shuffle(true, 7);
        assert_eq!(p.current(), Some(1)); // current stays at the front
    }
}
