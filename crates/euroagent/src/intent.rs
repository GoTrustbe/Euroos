//! Intent routing (Sprint AA, EuroDispatch core) — deterministic, auditable.
//!
//! No AI: an intent (speech-to-text or system event) is matched against the
//! declared triggers of installed agents. The agent with the highest score
//! wins. Matching is language-independent, case-insensitive word overlap so
//! "record meeting" also catches "start the meeting recording" — no regex needed.

use alloc::string::String;
use alloc::vec::Vec;

/// A route candidate: an agent name with its intent triggers.
pub struct Route {
    pub agent: String,
    pub intents: Vec<String>,
}

/// Normalize to lowercase and split into words (alphanumeric).
fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                cur.push(lc);
            }
        } else if !cur.is_empty() {
            out.push(core::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Score how well `intent` matches a trigger pattern `pattern`.
/// = number of trigger words that appear in the intent; a full match
/// (all trigger words present) gets a bonus so exact triggers win.
fn score_pattern(intent_words: &[String], pattern: &str) -> u32 {
    let pw = words(pattern);
    if pw.is_empty() {
        return 0;
    }
    let mut hits = 0u32;
    for w in &pw {
        if intent_words.iter().any(|iw| iw == w) {
            hits += 1;
        }
    }
    if hits == 0 {
        return 0;
    }
    let mut score = hits;
    if hits as usize == pw.len() {
        score += 5; // full-trigger bonus
    }
    score
}

/// The best score of an agent for this intent (max over its triggers).
pub fn agent_score(intent: &str, route: &Route) -> u32 {
    let iw = words(intent);
    route.intents.iter().map(|p| score_pattern(&iw, p)).max().unwrap_or(0)
}

/// Pick the best-matching agent for `intent`. `None` if nothing matches.
pub fn route<'a>(intent: &str, routes: &'a [Route]) -> Option<&'a Route> {
    let iw = words(intent);
    routes
        .iter()
        .map(|r| (r, r.intents.iter().map(|p| score_pattern(&iw, p)).max().unwrap_or(0)))
        .filter(|(_, s)| *s > 0)
        .max_by_key(|(_, s)| *s)
        .map(|(r, _)| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes() -> Vec<Route> {
        alloc::vec![
            Route {
                agent: "facilitator".to_string(),
                intents: alloc::vec!["vergadering opnemen".to_string(), "start recording".to_string()],
            },
            Route {
                agent: "calendar-assistant".to_string(),
                intents: alloc::vec!["agenda vandaag".to_string(), "calendar".to_string()],
            },
            Route {
                agent: "incident-reporter".to_string(),
                intents: alloc::vec!["nis2 incident melden".to_string()],
            },
        ]
    }

    #[test]
    fn exact_trigger_wins() {
        let r = routes();
        assert_eq!(route("vergadering opnemen", &r).unwrap().agent, "facilitator");
    }

    #[test]
    fn fuzzy_word_overlap() {
        let r = routes();
        // Extra words around it must not break the match.
        assert_eq!(route("kun je de vergadering opnemen alsjeblieft", &r).unwrap().agent, "facilitator");
    }

    #[test]
    fn picks_highest_score() {
        let r = routes();
        // "calendar" matches calendar-assistant fully; facilitator scores 0.
        assert_eq!(route("open my calendar", &r).unwrap().agent, "calendar-assistant");
    }

    #[test]
    fn no_match_is_none() {
        let r = routes();
        assert!(route("speel muziek af", &r).is_none());
    }

    #[test]
    fn partial_beats_nothing() {
        let r = routes();
        // Only one word overlap ("incident") → still incident-reporter.
        assert_eq!(route("er is een incident", &r).unwrap().agent, "incident-reporter");
    }
}
