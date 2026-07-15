//! Basic spell-check: flag words not in a small built-in dictionary. Honest
//! scope, this is a common-word list, not a full lexicon, enough to underline
//! obvious typos in EuroText. A real dictionary drops in the same way.

use alloc::vec::Vec;

/// A compact common-word dictionary (lower-case). Deliberately small and honest.
const WORDS: &[&str] = &[
    "the", "be", "to", "of", "and", "a", "in", "that", "have", "it", "for", "not",
    "on", "with", "he", "as", "you", "do", "at", "this", "but", "his", "by", "from",
    "they", "we", "say", "her", "she", "or", "an", "will", "my", "one", "all", "would",
    "there", "their", "what", "so", "up", "out", "if", "about", "who", "get", "which",
    "go", "me", "when", "make", "can", "like", "time", "no", "just", "him", "know",
    "take", "people", "into", "year", "your", "good", "some", "could", "them", "see",
    "other", "than", "then", "now", "look", "only", "come", "its", "over", "think",
    "also", "back", "after", "use", "two", "how", "our", "work", "first", "well", "way",
    "even", "new", "want", "because", "any", "these", "give", "day", "most", "us",
    "hello", "world", "welcome", "euroos", "file", "open", "save", "text", "note",
    "system", "network", "settings", "desktop", "window", "quick", "brown", "fox",
    "jumps", "lazy", "dog", "over", "test", "example", "sovereign", "secure", "kernel",
    "is", "are", "was", "were", "has", "had", "here", "type", "click", "menu", "app",
    "apps", "user", "name", "path", "line", "code", "data", "left", "right", "top",
];

/// Normalise a token for lookup (lower-case, strip surrounding punctuation).
fn normalise(w: &str) -> &str {
    w.trim_matches(|c: char| !c.is_ascii_alphabetic())
}

/// Is `word` spelled correctly (in the dictionary, case-insensitive)? Tokens
/// that are empty, numeric, or contain non-letters are treated as correct.
pub fn is_word(word: &str) -> bool {
    let w = normalise(word);
    if w.is_empty() || w.chars().any(|c| !c.is_ascii_alphabetic()) {
        return true;
    }
    let lower: alloc::string::String = w.chars().map(|c| c.to_ascii_lowercase()).collect();
    WORDS.contains(&lower.as_str())
}

/// The misspelled spans in a line, as (start column, length) over the ORIGINAL
/// string, so a renderer can underline them.
pub fn misspellings(line: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut col = 0usize;
    for token in split_keep_positions(line) {
        let (start, word) = token;
        if !word.is_empty() && !is_word(word) {
            out.push((start, word.len()));
        }
        let _ = col;
        col = start + word.len();
    }
    out
}

/// Split a line into (start_index, word) at ASCII whitespace.
fn split_keep_positions(line: &str) -> Vec<(usize, &str)> {
    let mut v = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                v.push((s, &line[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        v.push((s, &line[s..]));
    }
    v
}

/// `[spell]` boot self-test: known words pass, an obvious typo is flagged.
pub fn selftest() {
    let known = is_word("hello") && is_word("world") && is_word("The");
    let typo = !is_word("helllo") && !is_word("wrold");
    let spans = misspellings("the quick brWon fox");
    let one_flagged = spans.len() == 1 && spans[0].0 == 10; // "brWon" starts at col 10
    let ok = known && typo && one_flagged;
    crate::serial_println!(
        "[spell] Spell-check: known-words-pass={known}, typos-flagged={typo}, span-located={one_flagged} → {}",
        if ok { "OK (underlines unknown words) ✓" } else { "FAILED ✗" }
    );
}
