//! **EuroPortal** — capability-scoped **app permission portals** (3F-7).
//!
//! Where sovereign data-control becomes visible to the user. A GUI app never
//! holds ambient authority over the camera, the microphone, your files or the
//! network; it must **request** a [`Resource`], and the [`Broker`] resolves that
//! request against policy into `Allow` / `Deny` / **`Ask`** — the seam EuroGuard
//! deliberately left open until a dialog existed. When the user says yes, the
//! grant is recorded with a [`Scope`] (this once / this session / remembered),
//! and every request+decision goes to a tamper-evident-ready audit trail.
//!
//! This generalizes the agent JIT-elevation model (elevate-for-the-task,
//! auto-revoke) from WASM agents to desktop apps — "caps, not namespaces."
//!
//! Pure `no_std` logic, host-tested. The kernel wires it to a compositor grant
//! dialog and persists the remembered grants.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A sensitive resource an app can request. The `detail` (path, host, …) scopes
/// the grant to exactly what was asked for — a camera grant is not a file grant,
/// and a grant for `example.com` is not a grant for the whole network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// Read files under a path prefix.
    FileRead(String),
    /// Write files under a path prefix.
    FileWrite(String),
    Camera,
    Microphone,
    Location,
    /// Take a screenshot / capture the screen.
    ScreenCapture,
    /// Read the clipboard (writing is unprivileged).
    ClipboardRead,
    /// Reach a network host.
    Network(String),
    /// Post desktop notifications.
    Notifications,
}

impl Resource {
    /// The coarse kind (ignores the detail) — used for the default policy.
    pub fn kind(&self) -> ResourceKind {
        match self {
            Resource::FileRead(_) => ResourceKind::FileRead,
            Resource::FileWrite(_) => ResourceKind::FileWrite,
            Resource::Camera => ResourceKind::Camera,
            Resource::Microphone => ResourceKind::Microphone,
            Resource::Location => ResourceKind::Location,
            Resource::ScreenCapture => ResourceKind::ScreenCapture,
            Resource::ClipboardRead => ResourceKind::ClipboardRead,
            Resource::Network(_) => ResourceKind::Network,
            Resource::Notifications => ResourceKind::Notifications,
        }
    }

    /// A human, translatable-ready label for the dialog.
    pub fn describe(&self) -> String {
        match self {
            Resource::FileRead(p) => alloc::format!("read files in {p}"),
            Resource::FileWrite(p) => alloc::format!("write files in {p}"),
            Resource::Camera => "use the camera".to_string(),
            Resource::Microphone => "use the microphone".to_string(),
            Resource::Location => "access your location".to_string(),
            Resource::ScreenCapture => "capture the screen".to_string(),
            Resource::ClipboardRead => "read the clipboard".to_string(),
            Resource::Network(h) => alloc::format!("connect to {h}"),
            Resource::Notifications => "show notifications".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    FileRead,
    FileWrite,
    Camera,
    Microphone,
    Location,
    ScreenCapture,
    ClipboardRead,
    Network,
    Notifications,
}

/// How long a granted permission lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A single action, then auto-revoked (the JIT model).
    Once,
    /// Until the session ends (logout).
    Session,
    /// Remembered across reboots (persisted).
    Persistent,
}

/// The policy decision for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Auto-allowed without prompting.
    Allow,
    /// Auto-denied without prompting.
    Deny,
    /// Ask the user (the portal dialog).
    Ask,
}

/// What the broker returns for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Access is granted (an existing grant or an auto-allow).
    Granted,
    /// Access is refused (auto-deny or a recorded denial).
    Denied,
    /// The user must decide; `prompt` identifies the pending request.
    Prompt(u64),
}

/// A recorded grant.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Grant {
    app: String,
    resource: Resource,
    scope: Scope,
}

/// A pending prompt awaiting the user's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: u64,
    pub app: String,
    pub resource: Resource,
}

/// One audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub app: String,
    pub resource: Resource,
    pub decision: Decision,
    /// True if access was ultimately allowed (grant existed or user consented).
    pub allowed: bool,
}

/// The default policy: which resource kinds auto-allow, auto-deny, or ask.
/// Deliberately conservative — anything that can exfiltrate or surveil defaults
/// to `Ask`; only clearly-benign, user-visible actions auto-allow.
fn default_decision(kind: ResourceKind) -> Decision {
    use ResourceKind::*;
    match kind {
        // Benign, non-exfiltrating, immediately visible to the user.
        Notifications | ClipboardRead => Decision::Allow,
        // Everything sensitive is brokered.
        Camera | Microphone | Location | ScreenCapture | FileRead | FileWrite | Network => Decision::Ask,
    }
}

/// The permission broker. Holds recorded grants, explicit per-app rules, the
/// pending prompts and the audit trail.
pub struct Broker {
    grants: Vec<Grant>,
    /// Explicit overrides `(app, kind) -> Decision` (e.g. a signed app manifest,
    /// or a persisted "always deny").
    rules: Vec<(String, ResourceKind, Decision)>,
    pending: Vec<Pending>,
    audit: Vec<AuditEntry>,
    next_prompt: u64,
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

impl Broker {
    pub fn new() -> Self {
        Self { grants: Vec::new(), rules: Vec::new(), pending: Vec::new(), audit: Vec::new(), next_prompt: 1 }
    }

    /// Install an explicit rule (an app manifest declaration or a remembered
    /// "always allow/deny"). Overrides the default policy for that kind.
    pub fn set_rule(&mut self, app: &str, kind: ResourceKind, decision: Decision) {
        self.rules.retain(|(a, k, _)| !(a == app && *k == kind));
        self.rules.push((app.to_string(), kind, decision));
    }

    fn rule_for(&self, app: &str, kind: ResourceKind) -> Option<Decision> {
        self.rules.iter().find(|(a, k, _)| a == app && *k == kind).map(|(_, _, d)| *d)
    }

    /// Is there a live grant that covers this exact request? (Session/Persistent
    /// grants persist; a `Once` grant also counts as covering — it is consumed
    /// by [`check`], not by a repeat request.)
    fn has_grant(&self, app: &str, resource: &Resource) -> bool {
        self.grants.iter().any(|g| g.app == app && &g.resource == resource)
    }

    /// An app requests access. Returns whether it is granted, denied, or needs a
    /// user prompt. Every request is audited.
    pub fn request(&mut self, app: &str, resource: Resource) -> Outcome {
        // 1. An existing grant wins immediately.
        if self.has_grant(app, &resource) {
            self.audit.push(AuditEntry { app: app.to_string(), resource, decision: Decision::Allow, allowed: true });
            return Outcome::Granted;
        }
        // 2. Explicit rule, else the default policy.
        let decision = self.rule_for(app, resource.kind()).unwrap_or_else(|| default_decision(resource.kind()));
        match decision {
            Decision::Allow => {
                self.audit.push(AuditEntry { app: app.to_string(), resource, decision, allowed: true });
                Outcome::Granted
            }
            Decision::Deny => {
                self.audit.push(AuditEntry { app: app.to_string(), resource, decision, allowed: false });
                Outcome::Denied
            }
            Decision::Ask => {
                let id = self.next_prompt;
                self.next_prompt += 1;
                self.pending.push(Pending { id, app: app.to_string(), resource: resource.clone() });
                self.audit.push(AuditEntry { app: app.to_string(), resource, decision, allowed: false });
                Outcome::Prompt(id)
            }
        }
    }

    /// The prompts currently awaiting a user decision (for the dialog).
    pub fn pending(&self) -> &[Pending] {
        &self.pending
    }

    /// The user answers a prompt. `allow=false` records a denial (a `Persistent`
    /// scope makes it a remembered "always deny"). Returns whether access is now
    /// granted. Unknown prompt id → `false`.
    pub fn respond(&mut self, prompt: u64, allow: bool, scope: Scope) -> bool {
        let Some(pos) = self.pending.iter().position(|p| p.id == prompt) else {
            return false;
        };
        let p = self.pending.remove(pos);
        // Update the audit outcome for this decision.
        if let Some(last) = self.audit.iter_mut().rev().find(|e| e.app == p.app && e.resource == p.resource) {
            last.allowed = allow;
        }
        if allow {
            self.grants.push(Grant { app: p.app, resource: p.resource, scope });
            true
        } else {
            if scope == Scope::Persistent {
                // Remember the refusal so it does not ask again.
                self.set_rule(&p.app, p.resource.kind(), Decision::Deny);
            }
            false
        }
    }

    /// Enforcement check at the moment the app actually uses the resource.
    /// Returns true if allowed; a `Once` grant is **consumed** here (auto-revoke).
    /// A resource with no grant and an auto-allow policy is allowed; otherwise
    /// denied (the app must go through [`request`] first).
    pub fn check(&mut self, app: &str, resource: &Resource) -> bool {
        // Consume a matching Once grant.
        if let Some(pos) = self.grants.iter().position(|g| g.app == app && &g.resource == resource) {
            let consume = self.grants[pos].scope == Scope::Once;
            if consume {
                self.grants.remove(pos);
            }
            return true;
        }
        // No grant: only auto-allow kinds pass without one.
        let decision = self.rule_for(app, resource.kind()).unwrap_or_else(|| default_decision(resource.kind()));
        decision == Decision::Allow
    }

    /// End the session: drop all `Session` grants (Persistent ones survive).
    pub fn end_session(&mut self) {
        self.grants.retain(|g| g.scope != Scope::Session);
    }

    /// Revoke every grant for an app (e.g. the user toggles it off in settings).
    pub fn revoke_all(&mut self, app: &str) {
        self.grants.retain(|g| g.app != app);
    }

    /// The current grants as `(app, resource, scope)` for a settings view.
    pub fn list_grants(&self) -> Vec<(String, Resource, Scope)> {
        self.grants.iter().map(|g| (g.app.clone(), g.resource.clone(), g.scope)).collect()
    }

    /// The audit trail (most recent last).
    pub fn audit(&self) -> &[AuditEntry] {
        &self.audit
    }

    /// Serialize the **persistent** grants + deny rules to a line-based blob for
    /// on-disk persistence across reboots.
    pub fn serialize_persistent(&self) -> String {
        let mut out = String::new();
        for g in self.grants.iter().filter(|g| g.scope == Scope::Persistent) {
            out.push_str(&alloc::format!("grant\t{}\t{}\n", g.app, encode_resource(&g.resource)));
        }
        for (app, kind, dec) in &self.rules {
            if *dec == Decision::Deny {
                out.push_str(&alloc::format!("deny\t{}\t{}\n", app, encode_kind(*kind)));
            }
        }
        out
    }

    /// Load persistent grants + deny rules from [`serialize_persistent`] output.
    pub fn load_persistent(&mut self, blob: &str) {
        for line in blob.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["grant", app, res] => {
                    if let Some(r) = decode_resource(res) {
                        self.grants.push(Grant { app: app.to_string(), resource: r, scope: Scope::Persistent });
                    }
                }
                ["deny", app, kind] => {
                    if let Some(k) = decode_kind(kind) {
                        self.set_rule(app, k, Decision::Deny);
                    }
                }
                _ => {}
            }
        }
    }
}

fn encode_kind(k: ResourceKind) -> &'static str {
    use ResourceKind::*;
    match k {
        FileRead => "file_read",
        FileWrite => "file_write",
        Camera => "camera",
        Microphone => "microphone",
        Location => "location",
        ScreenCapture => "screen",
        ClipboardRead => "clipboard",
        Network => "network",
        Notifications => "notifications",
    }
}

fn decode_kind(s: &str) -> Option<ResourceKind> {
    use ResourceKind::*;
    Some(match s {
        "file_read" => FileRead,
        "file_write" => FileWrite,
        "camera" => Camera,
        "microphone" => Microphone,
        "location" => Location,
        "screen" => ScreenCapture,
        "clipboard" => ClipboardRead,
        "network" => Network,
        "notifications" => Notifications,
        _ => return None,
    })
}

fn encode_resource(r: &Resource) -> String {
    match r {
        Resource::FileRead(p) => alloc::format!("file_read:{p}"),
        Resource::FileWrite(p) => alloc::format!("file_write:{p}"),
        Resource::Network(h) => alloc::format!("network:{h}"),
        Resource::Camera => "camera".to_string(),
        Resource::Microphone => "microphone".to_string(),
        Resource::Location => "location".to_string(),
        Resource::ScreenCapture => "screen".to_string(),
        Resource::ClipboardRead => "clipboard".to_string(),
        Resource::Notifications => "notifications".to_string(),
    }
}

fn decode_resource(s: &str) -> Option<Resource> {
    if let Some(p) = s.strip_prefix("file_read:") {
        return Some(Resource::FileRead(p.to_string()));
    }
    if let Some(p) = s.strip_prefix("file_write:") {
        return Some(Resource::FileWrite(p.to_string()));
    }
    if let Some(h) = s.strip_prefix("network:") {
        return Some(Resource::Network(h.to_string()));
    }
    Some(match s {
        "camera" => Resource::Camera,
        "microphone" => Resource::Microphone,
        "location" => Resource::Location,
        "screen" => Resource::ScreenCapture,
        "clipboard" => Resource::ClipboardRead,
        "notifications" => Resource::Notifications,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benign_resources_auto_allow() {
        let mut b = Broker::new();
        assert_eq!(b.request("notes", Resource::Notifications), Outcome::Granted);
        assert_eq!(b.request("notes", Resource::ClipboardRead), Outcome::Granted);
    }

    #[test]
    fn sensitive_resources_prompt_then_grant() {
        let mut b = Broker::new();
        let out = b.request("meet", Resource::Camera);
        let id = match out {
            Outcome::Prompt(id) => id,
            _ => panic!("expected a prompt, got {out:?}"),
        };
        assert_eq!(b.pending().len(), 1);
        // Before consent, use is denied.
        assert!(!b.check("meet", &Resource::Camera));
        // User consents for the session.
        assert!(b.respond(id, true, Scope::Session));
        assert!(b.pending().is_empty());
        // Now use is allowed, and a session grant is not consumed.
        assert!(b.check("meet", &Resource::Camera));
        assert!(b.check("meet", &Resource::Camera));
    }

    #[test]
    fn once_grant_auto_revokes_after_use() {
        let mut b = Broker::new();
        let Outcome::Prompt(id) = b.request("shot", Resource::ScreenCapture) else { panic!() };
        b.respond(id, true, Scope::Once);
        assert!(b.check("shot", &Resource::ScreenCapture)); // consumed
        assert!(!b.check("shot", &Resource::ScreenCapture)); // gone
    }

    #[test]
    fn denial_persisted_stops_future_prompts() {
        let mut b = Broker::new();
        let Outcome::Prompt(id) = b.request("tracker", Resource::Location) else { panic!() };
        assert!(!b.respond(id, false, Scope::Persistent));
        // A second request is now auto-denied — no prompt.
        assert_eq!(b.request("tracker", Resource::Location), Outcome::Denied);
    }

    #[test]
    fn grants_are_scoped_to_the_exact_detail() {
        let mut b = Broker::new();
        let Outcome::Prompt(id) = b.request("app", Resource::Network("example.com".into())) else { panic!() };
        b.respond(id, true, Scope::Session);
        assert!(b.check("app", &Resource::Network("example.com".into())));
        // A grant for example.com is NOT a grant for another host.
        assert!(!b.check("app", &Resource::Network("evil.example".into())));
        assert!(matches!(b.request("app", Resource::Network("evil.example".into())), Outcome::Prompt(_)));
    }

    #[test]
    fn session_end_drops_session_grants_but_keeps_persistent() {
        let mut b = Broker::new();
        let Outcome::Prompt(i1) = b.request("a", Resource::Microphone) else { panic!() };
        b.respond(i1, true, Scope::Session);
        let Outcome::Prompt(i2) = b.request("a", Resource::Camera) else { panic!() };
        b.respond(i2, true, Scope::Persistent);
        b.end_session();
        assert!(!b.check("a", &Resource::Microphone)); // session grant gone
        assert!(b.check("a", &Resource::Camera)); // persistent survives
    }

    #[test]
    fn persistent_grants_roundtrip_on_disk() {
        let mut b = Broker::new();
        let Outcome::Prompt(id) = b.request("cam-app", Resource::Camera) else { panic!() };
        b.respond(id, true, Scope::Persistent);
        let Outcome::Prompt(id2) = b.request("bad-app", Resource::Location) else { panic!() };
        b.respond(id2, false, Scope::Persistent);
        let blob = b.serialize_persistent();

        let mut b2 = Broker::new();
        b2.load_persistent(&blob);
        assert!(b2.check("cam-app", &Resource::Camera)); // remembered allow
        assert_eq!(b2.request("bad-app", Resource::Location), Outcome::Denied); // remembered deny
    }

    #[test]
    fn app_manifest_rule_can_pre_authorize() {
        let mut b = Broker::new();
        // A signed manifest could declare an allow up-front → no prompt.
        b.set_rule("trusted", ResourceKind::Microphone, Decision::Allow);
        assert_eq!(b.request("trusted", Resource::Microphone), Outcome::Granted);
        assert!(b.check("trusted", &Resource::Microphone));
    }

    #[test]
    fn revoke_all_clears_app_grants() {
        let mut b = Broker::new();
        let Outcome::Prompt(id) = b.request("a", Resource::Camera) else { panic!() };
        b.respond(id, true, Scope::Session);
        assert!(b.check("a", &Resource::Camera));
        b.revoke_all("a");
        assert!(!b.check("a", &Resource::Camera));
    }

    #[test]
    fn audit_records_every_decision() {
        let mut b = Broker::new();
        b.request("x", Resource::Notifications); // allow
        let Outcome::Prompt(id) = b.request("x", Resource::Camera) else { panic!() };
        b.respond(id, true, Scope::Once);
        assert_eq!(b.audit().len(), 2);
        assert_eq!(b.audit()[0].decision, Decision::Allow);
        assert_eq!(b.audit()[1].decision, Decision::Ask);
        assert!(b.audit()[1].allowed); // updated after consent
    }
}
