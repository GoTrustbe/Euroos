//! AF / Zero-Trust **P2.3 — behavior detection (anomaly detection)** for agents.
//!
//! Feeds on the audit stream of the MCP gateway ([`crate::mcp::AuditRecord`]): every
//! tool call by an agent passes through here. The monitor learns a *baseline* per
//! agent (which tools, how often) and then flags deviations.
//!
//! **Deliberately deterministic and rule-based — NO ML.** Just like the intent router
//! (word overlap instead of a model), EuroOS chooses explainable, auditable thresholds
//! here: every alert traces back to a concrete rule, not to an opaque score. That fits
//! a sovereign, verifiable system and "assume breach": the monitor does not catch an
//! attack up front, but makes deviant behavior visible so the audit trail + response
//! can intervene.
//!
//! Pure `no_std` logic → host-tested. The kernel feeds records + a monotonic tick.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::mcp::AuditRecord;

/// Kind of deviation. Each traces back to a single rule (explainable).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnomalyKind {
    /// A burst: more calls within the time window than the ceiling allows.
    RateSpike,
    /// A tool this agent NEVER used during the baseline (behavior drift).
    UnseenTool,
    /// A run of consecutive denied calls (capability probing).
    DenialSpike,
    /// The total number of calls exceeded the hard ceiling (runaway).
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

/// One flagged deviation — meant to be written to the audit log.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Anomaly {
    pub agent: String,
    pub kind: AnomalyKind,
    pub detail: String,
}

/// Thresholds. Deliberately explicit + explainable; tunable per deployment.
#[derive(Clone, Copy)]
pub struct MonitorCfg {
    /// How many calls we observe before the baseline is "closed" (learning phase).
    pub baseline_calls: u64,
    /// Maximum number of calls within `window_ticks` (rate ceiling).
    pub max_window_rate: u64,
    /// Length of the sliding window for the rate (in monotonic ticks).
    pub window_ticks: u64,
    /// Number of consecutive denials that triggers a DenialSpike.
    pub denial_run: u64,
    /// Hard ceiling on the total number of calls per agent (runaway safety net).
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

/// The per-agent profile that the monitor builds up.
struct Profile {
    agent: String,
    total_calls: u64,
    seen_tools: Vec<String>,
    /// Timestamps (ticks) of the calls in the current sliding window.
    window: Vec<u64>,
    /// Running run of consecutive denials.
    denial_streak: u64,
    /// Already emitted a runaway alert? (don't keep repeating)
    runaway_fired: bool,
}

impl Profile {
    fn baselined(&self, cfg: &MonitorCfg) -> bool {
        self.total_calls >= cfg.baseline_calls
    }
}

/// The behavior monitor: one per system, feeds on all gateway audit records.
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

    /// Feed one audit record at time `now` (monotonic ticks). Returns every deviation
    /// this record triggers (there may be several). During the learning phase
    /// (baseline not yet closed) tools/rhythm are learned and NO drift alerts are
    /// emitted — except the runaway safety net, which always applies.
    pub fn observe(&mut self, rec: &AuditRecord, now: u64) -> Vec<Anomaly> {
        let cfg = self.cfg;
        let agent = rec.agent.clone();
        let mut out = Vec::new();
        let p = self.profile_mut(&agent);

        p.total_calls += 1;

        // Update the sliding window (drop ticks older than window_ticks).
        let floor = now.saturating_sub(cfg.window_ticks);
        p.window.retain(|&t| t >= floor);
        p.window.push(now);

        // Track the denial run.
        if rec.allowed {
            p.denial_streak = 0;
        } else {
            p.denial_streak += 1;
        }

        // ── Runaway safety net (ALWAYS applies, even during the learning phase) ──────────
        if !p.runaway_fired && p.total_calls > cfg.runaway_total {
            p.runaway_fired = true;
            out.push(Anomaly {
                agent: agent.clone(),
                kind: AnomalyKind::Runaway,
                detail: alloc::format!("{} calls > ceiling {}", p.total_calls, cfg.runaway_total),
            });
        }

        let baselined = p.baselined(&cfg);
        if !baselined {
            // Learning phase: record the tool, emit no drift alerts.
            if !p.seen_tools.iter().any(|t| t == &rec.tool) {
                p.seen_tools.push(rec.tool.clone());
            }
            return out;
        }

        // ── DenialSpike: a run of consecutive denials = probing ───────
        if p.denial_streak == cfg.denial_run {
            out.push(Anomaly {
                agent: agent.clone(),
                kind: AnomalyKind::DenialSpike,
                detail: alloc::format!("{} consecutive denials (reason={})", p.denial_streak, rec.reason),
            });
        }

        // ── UnseenTool: a tool that never appeared in the baseline ───────────
        if !p.seen_tools.iter().any(|t| t == &rec.tool) {
            out.push(Anomaly {
                agent: agent.clone(),
                kind: AnomalyKind::UnseenTool,
                detail: alloc::format!("tool '{}' not in baseline behavior", rec.tool),
            });
            // Add it so we don't alarm again on every subsequent call.
            p.seen_tools.push(rec.tool.clone());
        }

        // ── RateSpike: too many calls within the sliding window ────────────
        if p.window.len() as u64 > cfg.max_window_rate {
            out.push(Anomaly {
                agent,
                kind: AnomalyKind::RateSpike,
                detail: alloc::format!("{} calls in {} ticks > ceiling {}", p.window.len(), cfg.window_ticks, cfg.max_window_rate),
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
        // Learning phase: fs_read 3× → baseline = {fs_read}, no alerts.
        for i in 0..3 {
            assert!(m.observe(&rec("a", "fs_read", true), i).is_empty());
        }
        // Known behavior → no alert.
        assert!(m.observe(&rec("a", "fs_read", true), 10).is_empty());
        // A new tool after the baseline → UnseenTool.
        let al = m.observe(&rec("a", "net_post", true), 11);
        assert_eq!(al.len(), 1);
        assert_eq!(al[0].kind, AnomalyKind::UnseenTool);
        // Second time the same new tool → don't alarm again.
        assert!(m.observe(&rec("a", "net_post", true), 12).is_empty());
    }

    #[test]
    fn consecutive_denials_trigger_probing_alert() {
        let mut m = BehaviorMonitor::new(MonitorCfg { baseline_calls: 1, denial_run: 4, ..MonitorCfg::default() });
        m.observe(&rec("a", "fs_read", true), 0); // baseline closed
        let mut fired = None;
        for i in 1..=4 {
            let al = m.observe(&rec("a", "exec", false), i);
            if al.iter().any(|x| x.kind == AnomalyKind::DenialSpike) {
                fired = Some(i);
            }
        }
        assert_eq!(fired, Some(4)); // exactly at the 4th consecutive denial
    }

    #[test]
    fn allowed_call_resets_denial_streak() {
        let mut m = BehaviorMonitor::new(MonitorCfg { baseline_calls: 1, denial_run: 3, ..MonitorCfg::default() });
        m.observe(&rec("a", "fs_read", true), 0);
        m.observe(&rec("a", "exec", false), 1);
        m.observe(&rec("a", "exec", false), 2);
        m.observe(&rec("a", "fs_read", true), 3); // reset
        // Now only 2 consecutive denials → no DenialSpike (threshold 3).
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
        m.observe(&rec("a", "fs_read", true), 0); // baseline closed
        let mut spike = false;
        for i in 1..=6 {
            // All within one window (ticks 1..6, window=100).
            let al = m.observe(&rec("a", "fs_read", true), i);
            if al.iter().any(|x| x.kind == AnomalyKind::RateSpike) {
                spike = true;
            }
        }
        assert!(spike, "6 calls in a window of 100 ticks > ceiling 5 → RateSpike");
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
        // Calls far apart (each +100 ticks) → the window holds at most 1.
        for i in 1..=10 {
            let al = m.observe(&rec("a", "fs_read", true), i * 100);
            assert!(!al.iter().any(|x| x.kind == AnomalyKind::RateSpike));
        }
    }
}
