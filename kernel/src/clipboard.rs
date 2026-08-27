//! The live system clipboard: one shared [`euroclip::Clipboard`] the whole
//! desktop copies into and pastes out of. The engine (history, dedup, pinning,
//! secret-exclusion, GDPR expiry) is host-tested in [`euroclip`]; here we hold
//! the single live instance, hand out `copy`/`paste`, and expose it to the shell.
//! Wiring this is what turns "engine exists" into "copy and paste actually work".

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use euroclip::{Clipboard, CopyResult};
use spin::Mutex;

/// Room for the last 32 clips, kept for a generous window (in tick units).
static CLIP: Mutex<Option<Clipboard>> = Mutex::new(None);
/// Monotonic timestamp source for clip ordering (the engine wants a `now`).
static SEQ: AtomicU64 = AtomicU64::new(1);

fn with<R>(f: impl FnOnce(&mut Clipboard) -> R) -> R {
    let mut g = CLIP.lock();
    let c = g.get_or_insert_with(|| Clipboard::new(32, u64::MAX));
    f(c)
}

fn now() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Copy text onto the clipboard. Returns `false` when the engine refused it
/// (it looks like a secret, e.g. a password or key, and is excluded by policy).
pub fn copy(text: &str) -> bool {
    let r = with(|c| c.copy_text(text, now()));
    !matches!(r, CopyResult::RejectedSecret)
}

/// The current clipboard text (what a paste would insert), if any.
pub fn paste() -> Option<String> {
    with(|c| c.current().map(|i| i.text.clone()))
}

/// Whether there is anything to paste (so a menu can enable/disable Paste).
pub fn has_content() -> bool {
    with(|c| c.current().is_some())
}

/// Recent history lines for the `clip` shell command (most recent first).
pub fn history_lines() -> Vec<String> {
    with(|c| {
        c.history()
            .iter()
            .rev()
            .map(|i| {
                let pin = if i.pinned { "* " } else { "  " };
                alloc::format!("{pin}{}", i.text)
            })
            .collect()
    })
}

/// `[clip]` boot self-test: a real copy → paste round-trip on the live
/// clipboard, and proof that a secret-looking string is excluded.
pub fn selftest() {
    let ok_copy = copy("hello from euroos");
    let round_trip = paste().as_deref() == Some("hello from euroos");
    let secret_excluded = !copy("Xq7!vR2p#Lm9$");
    // The secret was refused, so paste still returns the earlier text.
    let paste_intact = paste().as_deref() == Some("hello from euroos");
    let ok = ok_copy && round_trip && secret_excluded && paste_intact;
    crate::serial_println!(
        "[clip] System clipboard: copy→paste round-trip={round_trip}, secret-excluded={secret_excluded}, history-intact={paste_intact} → {}",
        if ok { "OK (live copy/paste, not just an engine) ✓" } else { "FAILED ✗" }
    );
}
