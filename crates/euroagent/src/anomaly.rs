//! AF / Zero-Trust **P2.3 — gedragsdetectie (anomaly detection)** voor agents.
//!
//! Voedt op de audit-stroom van de MCP-gateway ([`crate::mcp::AuditRecord`]): élke
//! tool-aanroep van een agent passeert hier. De monitor leert per agent een
//! *baseline* (welke tools, hoe vaak) en signaleert daarna afwijkingen.
//!
//! **Bewust deterministisch en regelgebaseerd — GEEN ML.** Net als de intent-router
//! (woord-overlap i.p.v. een model) kiest EuroOS hier voor uitlegbare, auditbare
//! drempels: elke alert is herleidbaar tot een concrete regel, niet tot een
//! ondoorzichtige score. Dat past bij een soeverein, controleerbaar systeem en bij
//! "assume breach": de monitor vángt geen aanval vooraf, maar maakt afwijkend gedrag
//! zichtbaar zodat het audit-spoor + respons kan ingrijpen.
//!
//! Pure `no_std`-logica → host-getest. De kernel voedt records + een monotone tick.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::mcp::AuditRecord;

/// Soort afwijking. Elk is herleidbaar tot één regel (uitlegbaar).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnomalyKind {
    /// Een burst: meer aanroepen binnen het tijdvenster dan het plafond toelaat.
    RateSpike,
    /// Een tool die deze agent tijdens de baseline NOOIT gebruikte (gedragsdrift).
    UnseenTool,
    /// Een reeks opeenvolgende geweigerde aanroepen (capability-aftasten/probing).
    DenialSpike,
    /// Het totaal aantal aanroepen overschreed het harde plafond (op hol geslagen).
    Runaway,
}

impl AnomalyKind {
    pub fn tag(self) -> &'static str {
        match self {
            AnomalyKind::RateSpike => "RATE_SPIKE",
            AnomalyKind::UnseenTool => "UNSEEN_TOOL",
            AnomalyKind::DenialSpike => "DENIAL_SPIKE",
            AnomalyKind::Runaway => "RUNAWAY",
        }
    }
}

/// Eén gesignaleerde afwijking — bedoeld om naar het audit-log te schrijven.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Anomaly {
    pub agent: String,
    pub kind: AnomalyKind,
    pub detail: String,
}

/// Drempels. Bewust expliciet + uitlegbaar; te tunen per implementatie.
#[derive(Clone, Copy)]
pub struct MonitorCfg {
    /// Hoeveel aanroepen we observeren vóór de baseline "dicht" is (leerfase).
    pub baseline_calls: u64,
    /// Maximum aantal aanroepen binnen `window_ticks` (rate-plafond).
    pub max_window_rate: u64,
    /// Lengte van het schuifvenster voor de rate (in monotone ticks).
    pub window_ticks: u64,
    /// Aantal opeenvolgende weigeringen dat een DenialSpike triggert.
    pub denial_run: u64,
    /// Hard plafond op het totaal aantal aanroepen per agent (runaway-vangnet).
    pub runaway_total: u64,
}

impl Default for MonitorCfg {
    fn default() -> Self {
        MonitorCfg {
            baseline_calls: 8,
            max_window_rate: 20,
            window_ticks: 1_000,
            denial_run: 4,
            runaway_total: 10_000,
        }
    }
}

/// Het per-agent profiel dat de monitor opbouwt.
struct Profile {
    agent: String,
    total_calls: u64,
    seen_tools: Vec<String>,
    /// Tijdstempels (ticks) van de aanroepen in het huidige schuifvenster.
    window: Vec<u64>,
    /// Lopende reeks opeenvolgende weigeringen.
    denial_streak: u64,
    /// Al een runaway-alert afgegeven? (niet blijven herhalen)
    runaway_fired: bool,
}

impl Profile {
    fn baselined(&self, cfg: &MonitorCfg) -> bool {
        self.total_calls >= cfg.baseline_calls
    }
}

/// De gedragsmonitor: één per systeem, voedt op alle gateway-audit-records.
pub struct BehaviorMonitor {
    cfg: MonitorCfg,
    profiles: Vec<Profile>,
}

impl BehaviorMonitor {
    pub fn new(cfg: MonitorCfg) -> Self {
        BehaviorMonitor { cfg, profiles: Vec::new() }
    }

    fn profile_mut(&mut self, agent: &str) -> &mut Profile {
        if let Some(i) = self.profiles.iter().position(|p| p.agent == agent) {
            return &mut self.profiles[i];
        }
        self.profiles.push(Profile {
            agent: agent.to_string(),
            total_calls: 0,
            seen_tools: Vec::new(),
            window: Vec::new(),
            denial_streak: 0,
            runaway_fired: false,
        });
        let last = self.profiles.len() - 1;
        &mut self.profiles[last]
    }

    /// Voed één audit-record op tijdstip `now` (monotone ticks). Geeft elke afwijking
    /// terug die dit record triggert (kan er meerdere zijn). Tijdens de leerfase
    /// (baseline nog niet dicht) worden tools/ritme geleerd en GEEN drift-alerts
    /// gegeven — wel het runaway-vangnet, dat altijd geldt.
    pub fn observe(&mut self, rec: &AuditRecord, now: u64) -> Vec<Anomaly> {
        let cfg = self.cfg;
        let agent = rec.agent.clone();
        let mut out = Vec::new();
        let p = self.profile_mut(&agent);

        p.total_calls += 1;

        // Schuifvenster bijwerken (verwijder ticks ouder dan window_ticks).
        let floor = now.saturating_sub(cfg.window_ticks);
        p.window.retain(|&t| t >= floor);
        p.window.push(now);

        // Weiger-reeks bijhouden.
        if rec.allowed {
            p.denial_streak = 0;
        } else {
            p.denial_streak += 1;
        }

        // ── Runaway-vangnet (geldt ALTIJD, ook tijdens de leerfase) ──────────
        if !p.runaway_fired && p.total_calls > cfg.runaway_total {
            p.runaway_fired = true;
            out.push(Anomaly {
                agent: agent.clone(),
                kind: AnomalyKind::Runaway,
                detail: alloc::format!("{} aanroepen > plafond {}", p.total_calls, cfg.runaway_total),
            });
        }

        let baselined = p.baselined(&cfg);
        if !baselined {
            // Leerfase: registreer de tool, geef geen drift-alerts.
            if !p.seen_tools.iter().any(|t| t == &rec.tool) {
                p.seen_tools.push(rec.tool.clone());
            }
            return out;
        }

        // ── DenialSpike: een reeks opeenvolgende weigeringen = probing ───────
        if p.denial_streak == cfg.denial_run {
            out.push(Anomaly {
                agent: agent.clone(),
                kind: AnomalyKind::DenialSpike,
                detail: alloc::format!("{} opeenvolgende weigeringen (reason={})", p.denial_streak, rec.reason),
            });
        }

        // ── UnseenTool: een tool die in de baseline nooit voorkwam ───────────
        if !p.seen_tools.iter().any(|t| t == &rec.tool) {
            out.push(Anomaly {
                agent: agent.clone(),
                kind: AnomalyKind::UnseenTool,
                detail: alloc::format!("tool '{}' niet in baseline-gedrag", rec.tool),
            });
            // Voeg toe zodat we niet bij élke volgende call opnieuw alarmeren.
            p.seen_tools.push(rec.tool.clone());
        }

        // ── RateSpike: te veel aanroepen binnen het schuifvenster ────────────
        if p.window.len() as u64 > cfg.max_window_rate {
            out.push(Anomaly {
                agent,
                kind: AnomalyKind::RateSpike,
                detail: alloc::format!("{} aanroepen in {} ticks > plafond {}", p.window.len(), cfg.window_ticks, cfg.max_window_rate),
            });
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(agent: &str, tool: &str, allowed: bool) -> AuditRecord {
        AuditRecord {
            agent: agent.to_string(),
            tool: tool.to_string(),
            allowed,
            succeeded: allowed,
            reason: if allowed { "ok" } else { "insufficient_capability" },
        }
    }

    #[test]
    fn baseline_then_unseen_tool_drift() {
        let mut m = BehaviorMonitor::new(MonitorCfg { baseline_calls: 3, ..MonitorCfg::default() });
        // Leerfase: fs_read 3× → baseline = {fs_read}, geen alerts.
        for i in 0..3 {
            assert!(m.observe(&rec("a", "fs_read", true), i).is_empty());
        }
        // Bekend gedrag → geen alert.
        assert!(m.observe(&rec("a", "fs_read", true), 10).is_empty());
        // Een nieuwe tool ná de baseline → UnseenTool.
        let al = m.observe(&rec("a", "net_post", true), 11);
        assert_eq!(al.len(), 1);
        assert_eq!(al[0].kind, AnomalyKind::UnseenTool);
        // Tweede keer dezelfde nieuwe tool → niet opnieuw alarmeren.
        assert!(m.observe(&rec("a", "net_post", true), 12).is_empty());
    }

    #[test]
    fn consecutive_denials_trigger_probing_alert() {
        let mut m = BehaviorMonitor::new(MonitorCfg { baseline_calls: 1, denial_run: 4, ..MonitorCfg::default() });
        m.observe(&rec("a", "fs_read", true), 0); // baseline dicht
        let mut fired = None;
        for i in 1..=4 {
            let al = m.observe(&rec("a", "exec", false), i);
            if al.iter().any(|x| x.kind == AnomalyKind::DenialSpike) {
                fired = Some(i);
            }
        }
        assert_eq!(fired, Some(4)); // precies bij de 4e opeenvolgende weigering
    }

    #[test]
    fn allowed_call_resets_denial_streak() {
        let mut m = BehaviorMonitor::new(MonitorCfg { baseline_calls: 1, denial_run: 3, ..MonitorCfg::default() });
        m.observe(&rec("a", "fs_read", true), 0);
        m.observe(&rec("a", "exec", false), 1);
        m.observe(&rec("a", "exec", false), 2);
        m.observe(&rec("a", "fs_read", true), 3); // reset
        // Nu nog maar 2 opeenvolgende weigeringen → geen DenialSpike (drempel 3).
        let al = m.observe(&rec("a", "exec", false), 4);
        assert!(!al.iter().any(|x| x.kind == AnomalyKind::DenialSpike));
    }

    #[test]
    fn rate_spike_within_window() {
        let mut m = BehaviorMonitor::new(MonitorCfg {
            baseline_calls: 1,
            max_window_rate: 5,
            window_ticks: 100,
            ..MonitorCfg::default()
        });
        m.observe(&rec("a", "fs_read", true), 0); // baseline dicht
        let mut spike = false;
        for i in 1..=6 {
            // Allemaal binnen één venster (ticks 1..6, window=100).
            let al = m.observe(&rec("a", "fs_read", true), i);
            if al.iter().any(|x| x.kind == AnomalyKind::RateSpike) {
                spike = true;
            }
        }
        assert!(spike, "6 aanroepen in een venster van 100 ticks > plafond 5 → RateSpike");
    }

    #[test]
    fn old_calls_leave_the_window() {
        let mut m = BehaviorMonitor::new(MonitorCfg {
            baseline_calls: 1,
            max_window_rate: 3,
            window_ticks: 10,
            ..MonitorCfg::default()
        });
        m.observe(&rec("a", "fs_read", true), 0);
        // Aanroepen ver uit elkaar (telkens +100 ticks) → venster bevat er max 1.
        for i in 1..=10 {
            let al = m.observe(&rec("a", "fs_read", true), i * 100);
            assert!(!al.iter().any(|x| x.kind == AnomalyKind::RateSpike));
        }
    }
}
