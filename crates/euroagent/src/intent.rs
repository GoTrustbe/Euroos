//! Intent-routing (Sprint AA, EuroDispatch-kern) — deterministisch, auditeerbaar.
//!
//! Geen AI: een intent (spraak→tekst of systeemevent) wordt op de gedeclareerde
//! triggers van geïnstalleerde agents gematcht. De agent met de hoogste score
//! wint. Matching is taal-onafhankelijke, case-insensitieve woord-overlap zodat
//! "vergadering opnemen" ook "start de vergadering opname" vangt — geen regex nodig.

use alloc::string::String;
use alloc::vec::Vec;

/// Een route-kandidaat: een agentnaam met zijn intent-triggers.
pub struct Route {
    pub agent: String,
    pub intents: Vec<String>,
}

/// Normaliseer naar kleine letters en splits in woorden (alfanumeriek).
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

/// Score hoe goed `intent` op een trigger-patroon `pattern` past.
/// = aantal trigger-woorden dat in het intent voorkomt; een volledige match
/// (alle trigger-woorden aanwezig) krijgt een bonus zodat exacte triggers winnen.
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
        score += 5; // volledige-trigger-bonus
    }
    score
}

/// De beste score van een agent voor dit intent (max over zijn triggers).
pub fn agent_score(intent: &str, route: &Route) -> u32 {
    let iw = words(intent);
    route.intents.iter().map(|p| score_pattern(&iw, p)).max().unwrap_or(0)
}

/// Kies de best passende agent voor `intent`. `None` als niets matcht.
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
        // Extra woorden eromheen mogen de match niet breken.
        assert_eq!(route("kun je de vergadering opnemen alsjeblieft", &r).unwrap().agent, "facilitator");
    }

    #[test]
    fn picks_highest_score() {
        let r = routes();
        // "calendar" matcht calendar-assistant volledig; facilitator scoort 0.
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
        // Slechts één woord overlap ("incident") → toch incident-reporter.
        assert_eq!(route("er is een incident", &r).unwrap().agent, "incident-reporter");
    }
}
