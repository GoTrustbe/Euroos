//! Boot self-test for **EuroClip** (AC-2): clipboard history with dedup,
//! pinning, GDPR expiry and password exclusion. Core: [`euroclip`].

use crate::serial_println;
use euroclip::{Clipboard, CopyResult};

pub fn selftest() {
    let mut cb = Clipboard::new(5, 100);
    cb.copy_text("first", 1);
    cb.copy_text("second", 2);
    let promoted = cb.copy_text("first", 3) == CopyResult::Promoted;
    let secret = cb.copy_text("Xq7!vR2p#Lm", 4) == CopyResult::RejectedSecret;
    cb.set_pinned(0, true); // pin 'first'
    cb.expire(1000); // unpinned entries expire
    let only_pinned = cb.history().len() == 1 && cb.current().map(|i| i.text.as_str()) == Some("first");
    let persist = cb.persistable().len() == 1;

    let ok = promoted && secret && only_pinned && persist;
    serial_println!(
        "[cl] EuroClip: dedup-promote={}, password-excluded={}, after-expire only-pinned={}, GDPR-persist={}/1 {}",
        promoted, secret, only_pinned, cb.persistable().len(),
        if ok { "✓" } else { "✗ FAIL" }
    );
}
