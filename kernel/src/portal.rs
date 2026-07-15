//! Kernel side of **EuroPortal** (3F-7): the capability-scoped app-permission
//! broker. A GUI app requests a sensitive [`Resource`]; the broker resolves it
//! to allow/deny/**ask**, and on `ask` the compositor shows a grant dialog. The
//! host-tested logic lives in [`europortal`]; here we hold the live broker,
//! render the dialog, run the boot self-test `[3f7]`, and expose `portal` in the
//! shell.

use alloc::string::String;
use alloc::vec::Vec;

use europortal::{Broker, Outcome, Resource, Scope};
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};

static BROKER: Mutex<Option<Broker>> = Mutex::new(None);

fn with_broker<R>(f: impl FnOnce(&mut Broker) -> R) -> R {
    let mut guard = BROKER.lock();
    let b = guard.get_or_insert_with(Broker::new);
    f(b)
}

/// An app requests access; returns the outcome (Granted / Denied / Prompt(id)).
pub fn request(app: &str, resource: Resource) -> Outcome {
    with_broker(|b| b.request(app, resource))
}

/// Enforcement check at use time (consumes a one-shot grant).
pub fn check(app: &str, resource: &Resource) -> bool {
    with_broker(|b| b.check(app, resource))
}

/// Answer a pending prompt (from the dialog).
pub fn respond(prompt: u64, allow: bool, scope: Scope) -> bool {
    with_broker(|b| b.respond(prompt, allow, scope))
}

/// Close the session's non-persistent grants (called at logout, alongside the
/// session lifecycle in [`crate::session`]).
pub fn end_session() {
    with_broker(|b| b.end_session());
}

/// `[3f7]` boot self-test — the whole portal flow on the live kernel: a benign
/// resource auto-allows; the camera is brokered (prompt → grant-once →
/// consumed/auto-revoked); a persisted refusal stops future prompts; grants are
/// scoped to the exact host; and persistent grants round-trip through a blob.
pub fn selftest() {
    let mut b = Broker::new();

    // (1) Benign → auto-allow, no prompt.
    let benign = b.request("euronotes", Resource::Notifications) == Outcome::Granted;

    // (2) Camera → Ask → grant Once → used once, then auto-revoked.
    let cam_prompt = matches!(b.request("euromeet", Resource::Camera), Outcome::Prompt(_));
    let pending_before = b.pending().len() == 1;
    let id = b.pending()[0].id;
    let granted = b.respond(id, true, Scope::Once);
    let used_once = b.check("euromeet", &Resource::Camera);
    let auto_revoked = !b.check("euromeet", &Resource::Camera);

    // (3) A persisted refusal is remembered — no second prompt.
    let Outcome::Prompt(rid) = b.request("tracker", Resource::Location) else {
        crate::serial_println!("[3f7] portal FAILED (location did not prompt)");
        return;
    };
    b.respond(rid, false, Scope::Persistent);
    let refusal_remembered = b.request("tracker", Resource::Location) == Outcome::Denied;

    // (4) Host-scoped: a grant for one host is not a grant for another.
    let Outcome::Prompt(nid) = b.request("app", Resource::Network(String::from("euro-os.eu"))) else {
        crate::serial_println!("[3f7] portal FAILED (network did not prompt)");
        return;
    };
    b.respond(nid, true, Scope::Session);
    let host_scoped = b.check("app", &Resource::Network(String::from("euro-os.eu")))
        && !b.check("app", &Resource::Network(String::from("evil.example")));

    // (5) Persistent grants survive a serialize/load (on-disk persistence).
    let blob = b.serialize_persistent();
    let mut b2 = Broker::new();
    b2.load_persistent(&blob);
    let persisted = b2.request("tracker", Resource::Location) == Outcome::Denied;

    // Install the broker as the live one.
    *BROKER.lock() = Some(b);

    let ok = benign && cam_prompt && pending_before && granted && used_once && auto_revoked && refusal_remembered && host_scoped && persisted;
    crate::serial_println!(
        "[3f7] EuroPortal (caps, not namespaces): benign-auto-allow={benign}, camera-ask→grant-once→auto-revoke={auto_revoked}, persisted-refusal-remembered={refusal_remembered}, host-scoped-grant={host_scoped}, persistent-grants-on-disk={persisted} → {}",
        if ok { "OK (per-action, user-visible permission portals) ✓" } else { "FAILED ✗" }
    );
}

/// Draw a modal grant dialog for the first pending prompt (if any). Returns the
/// three button rects `[(Allow once), (Allow session), (Deny)]` in screen
/// coordinates so the compositor can hit-test clicks, plus the prompt id.
/// `None` when nothing is pending.
pub fn render_dialog(fb: &FrameBuffer, screen_w: usize, screen_h: usize) -> Option<(u64, [(usize, usize, usize, usize); 3])> {
    let guard = BROKER.lock();
    let b = guard.as_ref()?;
    let p = b.pending().first()?;
    let (app, what, id) = (p.app.clone(), p.resource.describe(), p.id);
    drop(guard);

    let w = 460usize;
    let h = 200usize;
    let x = screen_w.saturating_sub(w) / 2;
    let y = screen_h.saturating_sub(h) / 2;
    // Dim + card.
    fb.fill_rounded_rect(x, y, w, h, crate::eds::RADIUS_M, Color::CARD);
    fb.draw_border(x, y, w, h, 1, Color::BORDER);
    crate::text::draw_px(fb, x + 24, y + 22, "Permission request", Color::INK, 18.0);
    crate::text::draw_px(fb, x + 24, y + 56, &alloc::format!("\u{201C}{app}\u{201D} wants to {what}."), Color::TEXT_SEC, 13.0);
    crate::text::draw_px(fb, x + 24, y + 80, "You are in control — grant only what you intend.", Color::TEXT_DIM, 11.5);

    // Buttons.
    let by = y + h - 52;
    let bw = 130usize;
    let gap = 14usize;
    let b1 = (x + 24, by, bw, 36);
    let b2 = (x + 24 + bw + gap, by, bw, 36);
    let b3 = (x + 24 + 2 * (bw + gap), by, bw - 30, 36);
    fb.fill_rounded_rect(b1.0, b1.1, b1.2, b1.3, crate::eds::RADIUS_S, Color::ACCENT);
    crate::text::draw_px(fb, b1.0 + 20, b1.1 + 9, "Allow once", Color::WHITE, 13.0);
    fb.fill_rounded_rect(b2.0, b2.1, b2.2, b2.3, crate::eds::RADIUS_S, Color::SURFACE);
    crate::text::draw_px(fb, b2.0 + 14, b2.1 + 9, "This session", Color::INK, 13.0);
    fb.fill_rounded_rect(b3.0, b3.1, b3.2, b3.3, crate::eds::RADIUS_S, Color::SURFACE);
    crate::text::draw_px(fb, b3.0 + 24, b3.1 + 9, "Deny", Color::RED, 13.0);
    Some((id, [b1, b2, b3]))
}

/// `portal` shell command: list current grants + the recent audit trail.
/// The permission grants held by one app (formatted), for the unified per-app
/// control screen. Each line is "<what> (<scope>)".
pub fn grant_lines_for(app: &str) -> Vec<String> {
    with_broker(|b| {
        b.list_grants()
            .into_iter()
            .filter(|(a, _, _)| a == app)
            .map(|(_, res, scope)| alloc::format!("{} ({:?})", res.describe(), scope))
            .collect()
    })
}

/// Revoke every permission grant held by `app` (the "reset this app's
/// permissions" action). Returns how many grants were revoked.
pub fn revoke_app(app: &str) -> usize {
    with_broker(|b| {
        let n = b.list_grants().iter().filter(|(a, _, _)| a == app).count();
        b.revoke_all(app);
        n
    })
}

pub fn shell() -> Vec<String> {
    with_broker(|b| {
        let mut out = alloc::vec![String::from("EuroPortal — app permissions (caps, not namespaces)")];
        let grants = b.list_grants();
        if grants.is_empty() {
            out.push(String::from("  no active grants"));
        } else {
            out.push(String::from("  active grants:"));
            for (app, res, scope) in grants {
                out.push(alloc::format!("    {:<14} {:<28} {:?}", app, res.describe(), scope));
            }
        }
        let audit = b.audit();
        out.push(alloc::format!("  audit ({} decisions, most recent last):", audit.len()));
        for e in audit.iter().rev().take(6).rev() {
            out.push(alloc::format!("    {:<14} {:<28} {:?} allowed={}", e.app, e.resource.describe(), e.decision, e.allowed));
        }
        out.push(String::from("  a pending request is shown as a desktop dialog (Allow once / This session / Deny)"));
        out
    })
}
