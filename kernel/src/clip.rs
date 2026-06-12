//! Boot-zelftest voor **EuroClip** (AC-2): klembordgeschiedenis met dedup,
//! vastmaken, GDPR-expiratie en wachtwoord-uitsluiting. Kern: [`euroclip`].

use crate::serial_println;
use euroclip::{Clipboard, CopyResult};

pub fn selftest() {
    let mut cb = Clipboard::new(5, 100);
    cb.copy_text("eerste", 1);
    cb.copy_text("tweede", 2);
    let promoted = cb.copy_text("eerste", 3) == CopyResult::Promoted;
    let secret = cb.copy_text("Xq7!vR2p#Lm", 4) == CopyResult::RejectedSecret;
    cb.set_pinned(0, true); // pin 'eerste'
    cb.expire(1000); // niet-vastgemaakte verlopen
    let only_pinned = cb.history().len() == 1 && cb.current().map(|i| i.text.as_str()) == Some("eerste");
    let persist = cb.persistable().len() == 1;

    let ok = promoted && secret && only_pinned && persist;
    serial_println!(
        "[cl] EuroClip: dedup-promote={}, wachtwoord-uitgesloten={}, na-expire enkel-vastgemaakt={}, GDPR-persist={}/1 {}",
        promoted, secret, only_pinned, cb.persistable().len(),
        if ok { "✓" } else { "✗ FOUT" }
    );
}
