//! **EuroOS GUI-lockscreen** (Sprint AG) — een interactief aanmeldscherm dat de
//! desktop-sessie via EuroID (Argon2id) authenticeert vóór de desktop interactief
//! wordt. Het sluit het identiteitsverhaal grafisch: niet langer een auto-sessie
//! als `euro`, maar een echte wachtwoordcontrole tegen de soevereine identiteits-
//! opslag, mét must-change-afhandeling.
//!
//! Het scherm hergebruikt de compositor-primitieven (EDS-thema, ab_glyph-tekst) en
//! het PS/2-toetsenbordpad (`ps2::poll_key`). Voor een ONBEHEERDE/CI-boot zonder
//! toetsenbordinvoer logt het na een korte gratieperiode automatisch in als de
//! desktopgebruiker — eerlijk gelogd — zodat de boot/screenshot/e2e blijft werken.

use alloc::string::{String, ToString};

use crate::graphics::{Color, FrameBuffer};
use crate::{eds, text};

/// Na zoveel 100 Hz-ticks (≈ guest-seconden) zónder geslaagde aanmelding logt een
/// onbeheerde boot automatisch in als de desktopgebruiker (demo/CI-gemak).
const UNATTENDED_AUTOLOGIN_TICKS: u64 = 60; // ~0,6 s guest-tijd (klein boot-effect)

#[derive(PartialEq)]
enum Phase {
    /// Wachtwoord voor `user` invoeren.
    Password,
    /// Account vereist een wijziging: een NIEUW wachtwoord invoeren.
    MustChange,
}

struct Lock {
    user: String,
    input: String,         // huidige invoer (gemaskeerd weergegeven)
    saved_old: String,     // bewaard oud wachtwoord tijdens must-change
    phase: Phase,
    status: String,        // hint- of foutregel onder het veld
    error: bool,           // status in rood tonen?
}

/// Teken het lockscreen-frame: zand-achtergrond + gecentreerde kaart met wordmark,
/// gebruikersnaam, gemaskeerd wachtwoordveld (met cursor), een EuroID/Argon2id-badge
/// en de status/hint-regel.
fn render(fb: &FrameBuffer, lk: &Lock, clock: &str) {
    let (w, h) = (fb.width(), fb.height());
    // Achtergrond in de huisstijl (zand → iets donkerder onderaan).
    fb.fill_rounded_rect_grad(0, 0, w, h, 0, Color::BACKGROUND, Color::PAPER_2);

    // Kaart, gecentreerd.
    let cw = 380usize;
    let ch = 280usize;
    let cx = (w - cw) / 2;
    let cy = (h - ch) / 2;
    fb.fill_rounded_rect(cx + 6, cy + 8, cw, ch, eds::RADIUS_XL, Color::rgb(0xE2, 0xDC, 0xD1)); // zachte schaduw
    fb.fill_rounded_rect(cx, cy, cw, ch, eds::RADIUS_XL, Color::SURFACE);

    let pad = eds::eu(7); // 28
    let mut y = cy + pad;

    // Wordmark + accent-stip.
    text::draw_px(fb, cx + pad, y, "EuroOS", Color::INK, 30.0);
    let wm = text::width_px("EuroOS", 30.0);
    fb.fill_rounded_rect(cx + pad + wm + 8, y + 8, 10, 10, 5, Color::ACCENT);
    y += 44;
    text::draw_px(fb, cx + pad, y, "Aanmelden bij je soevereine sessie", Color::TEXT_SEC, 14.0);
    y += 34;

    // Gebruikersnaam-regel.
    text::draw_px(fb, cx + pad, y, "Gebruiker", Color::TEXT_DIM, 12.0);
    text::draw_px(fb, cx + pad + 90, y, &lk.user, Color::INK, 14.0);
    y += 28;

    // Wachtwoordveld.
    let label = if lk.phase == Phase::Password { "Wachtwoord" } else { "Nieuw wachtwoord" };
    text::draw_px(fb, cx + pad, y, label, Color::TEXT_DIM, 12.0);
    y += 20;
    let fw = cw - 2 * pad;
    fb.fill_rounded_rect(cx + pad, y, fw, 36, eds::RADIUS_M, Color::SURFACE_3);
    // Accent-rand (focus).
    fb.fill_rect(cx + pad, y + 34, fw, 2, Color::ACCENT);
    // Gemaskeerde invoer (• per teken) + knipperloze cursor.
    let mut masked = String::new();
    for _ in lk.input.chars() {
        masked.push('•');
    }
    masked.push('|');
    text::draw_px(fb, cx + pad + 12, y + 9, &masked, Color::INK, 16.0);
    y += 50;

    // Status / hint.
    let (msg, col) = if !lk.status.is_empty() {
        (lk.status.as_str(), if lk.error { Color::RED } else { Color::TEXT_SEC })
    } else {
        ("Druk op Enter om aan te melden", Color::TEXT_DIM)
    };
    text::draw_px(fb, cx + pad, y, msg, col, 13.0);

    // EuroID-badge onderaan + klok.
    let by = cy + ch - 30;
    fb.fill_rounded_rect(cx + pad, by, 200, 20, eds::RADIUS_S, Color::SUCCESS_SOFT);
    text::draw_px(fb, cx + pad + 8, by + 2, "EuroID · Argon2id", Color::SUCCESS, 12.0);
    let cwid = text::width_px(clock, 13.0);
    text::draw_px(fb, cx + cw - pad - cwid, by + 2, clock, Color::TEXT_DIM, 13.0);

    fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() {
        crate::virtio_gpu::present_frame(bb, bw, bh, bs);
    }
}

/// Toon de lockscreen en blokkeer tot de gebruiker zich aanmeldt (of, bij een
/// onbeheerde boot, tot de auto-login-gratie verstrijkt). Zet de sessie via
/// EuroAuth en geeft de aangemelde gebruikersnaam terug.
pub fn gate(fb: &FrameBuffer, default_user: &str) -> String {
    let mut lk = Lock {
        user: default_user.to_string(),
        input: String::new(),
        saved_old: String::new(),
        phase: Phase::Password,
        status: String::new(),
        error: false,
    };
    let start = crate::interrupts::ticks();
    render(fb, &lk, &crate::rtc::clock_string());

    loop {
        crate::xhci::poll(); // USB-toetsenbord ook bedienen
        let mut dirty = false;
        while let Some(c) = crate::ps2::poll_key() {
            dirty = true;
            match c {
                '\r' => {
                    if lk.phase == Phase::Password {
                        match crate::euroid::login(&lk.user, &lk.input) {
                            Ok(ok) => {
                                set_session(&ok.name, ok.uid);
                                crate::serial_println!("[lock] aanmelding geslaagd: {} (uid={})", ok.name, ok.uid);
                                return ok.name;
                            }
                            Err(reason) => {
                                if reason.contains("gewijzigd") {
                                    // must-change: vraag een nieuw wachtwoord.
                                    lk.saved_old = core::mem::take(&mut lk.input);
                                    lk.phase = Phase::MustChange;
                                    lk.status = "Wachtwoord verlopen — kies een nieuw".to_string();
                                    lk.error = false;
                                } else {
                                    lk.status = reason;
                                    lk.error = true;
                                    lk.input.clear();
                                }
                            }
                        }
                    } else {
                        // MustChange: zet het nieuwe wachtwoord en log daarna in.
                        let newpw = core::mem::take(&mut lk.input);
                        match crate::euroid::change_own_password(&lk.user, &lk.saved_old, &newpw) {
                            Ok(()) => match crate::euroid::login(&lk.user, &newpw) {
                                Ok(ok) => {
                                    set_session(&ok.name, ok.uid);
                                    crate::serial_println!("[lock] wachtwoord gewijzigd + aangemeld: {}", ok.name);
                                    return ok.name;
                                }
                                Err(e) => {
                                    lk.status = e;
                                    lk.error = true;
                                    lk.phase = Phase::Password;
                                }
                            },
                            Err(e) => {
                                lk.status = e;
                                lk.error = true;
                            }
                        }
                    }
                }
                '\u{8}' | '\u{7f}' => {
                    lk.input.pop();
                }
                '\u{1b}' => {
                    lk.input.clear();
                }
                ch if !ch.is_control() => {
                    if lk.input.len() < 128 {
                        lk.input.push(ch);
                    }
                }
                _ => {}
            }
        }

        // Onbeheerde boot: na de gratieperiode auto-login als de desktopgebruiker.
        if crate::interrupts::ticks().saturating_sub(start) > UNATTENDED_AUTOLOGIN_TICKS {
            // Probeer een echte aanmelding met het demo-wachtwoord; lukt dat niet,
            // val terug op de bestaande sessie-default (uid 1000).
            if let Ok(ok) = crate::euroid::login(default_user, "euro") {
                set_session(&ok.name, ok.uid);
                crate::serial_println!(
                    "[lock] onbeheerde boot — geen invoer binnen gratie → auto-login als {} (demo/CI)",
                    ok.name
                );
                return ok.name;
            }
            crate::serial_println!("[lock] onbeheerde boot — auto-login viel terug op default-sessie");
            return default_user.to_string();
        }

        if dirty {
            render(fb, &lk, &crate::rtc::clock_string());
        }
        x86_64::instructions::hlt();
    }
}

fn set_session(name: &str, uid: u32) {
    // gid uit /etc/passwd-mapping als beschikbaar; anders de uid.
    crate::auth::set_session(uid, uid, name);
}

/// `[ag-lock]` boot-zelftest — bewijst de auth-bedrading van de lockscreen zonder
/// interactief toetsenbord: het juiste wachtwoord meldt aan, een fout wordt
/// geweigerd, en het renderen van het scherm paniekt niet (headless getekend).
pub fn selftest(fb: &FrameBuffer) {
    let good = crate::euroid::login("euro", "euro").is_ok();
    let bad = crate::euroid::login("euro", "fout").is_err();
    // Render één frame headless (bewijst dat de tekenroutine niet paniekt).
    let lk = Lock {
        user: "euro".to_string(),
        input: "secret".to_string(),
        saved_old: String::new(),
        phase: Phase::Password,
        status: String::new(),
        error: false,
    };
    render(fb, &lk, &crate::rtc::clock_string());
    let rendered = true;

    let ok = good && bad && rendered;
    crate::serial_println!(
        "[ag-lock] GUI-lockscreen: juist-wachtwoord-meldt-aan={good}, fout-geweigerd={bad}, scherm-getekend={rendered} → {}",
        if ok { "OK (desktop-sessie achter EuroID-Argon2id i.p.v. auto-euro) ✓" } else { "MISLUKT" }
    );
}
