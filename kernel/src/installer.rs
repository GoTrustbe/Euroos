//! Kernel-zijde van **EuroInstall** (plan Q1): bewijst bij boot dat de installer-
//! *planner* een geldig, geordend stappenplan + partitielayout produceert. De
//! host-geteste kern leeft in [`euroinstall`]; de echte uitvoering (sector-I/O,
//! FDE-enrol, gebruiker) koppelt later het userspace-installerproces. Biedt het
//! `euroinstall`-shellcommando (dry-run van het plan).

use alloc::string::String;
use alloc::vec::Vec;

use euroinstall::{plan, Config, Disk, Step};

use crate::graphics::{Color, FrameBuffer};
use crate::text;

const TITLEBAR_H: usize = 44;

/// Een voorbeeldconfig voor de zelftest + de shell-dry-run.
fn sample_config(live: bool) -> Config {
    Config {
        disk: Disk { total_bytes: 16 * 1024 * 1024 * 1024 }, // 16 GiB
        locale: String::from("nl-BE"),
        keymap: String::from("be-azerty"),
        hostname: String::from("euro-pc"),
        username: String::from("anke"),
        fde: true,
        live,
    }
}

/// Boot-zelftest: bouw een installatieplan + een live-plan en controleer de
/// kernbeloftes (FDE vóór format, partities niet-overlappend, live laat schijf met rust).
pub fn selftest() {
    let install = plan(&sample_config(false));
    let live = plan(&sample_config(true));

    let (steps_ok, parts_ok) = match &install {
        Ok(steps) => {
            let fde = steps.iter().position(|s| *s == Step::EnrollFde);
            let fmt = steps.iter().position(|s| *s == Step::FormatSystem);
            let order_ok = matches!((fde, fmt), (Some(f), Some(m)) if f < m)
                && matches!(steps.first(), Some(Step::Partition(_)))
                && steps.last() == Some(&Step::FinalizeBoot);
            // Partities overlappen niet.
            let parts_ok = if let Some(Step::Partition(p)) = steps.first() {
                p.len() == 4 && p.windows(2).all(|w| w[0].start_lba + w[0].sectors <= w[1].start_lba)
            } else {
                false
            };
            (order_ok, parts_ok)
        }
        Err(_) => (false, false),
    };

    // Live-modus mag geen enkele schijf-schrijfstap bevatten.
    let live_ok = live
        .as_ref()
        .map(|s| !s.iter().any(|st| matches!(st, Step::Partition(_) | Step::FormatSystem | Step::WriteKernelSlots)))
        .unwrap_or(false);

    let ok = steps_ok && parts_ok && live_ok;
    let nsteps = install.as_ref().map(|s| s.len()).unwrap_or(0);
    crate::serial_println!(
        "[install] EuroInstall planner: {nsteps}-staps A/B-installatie (FDE vóór format={steps_ok}, 4 niet-overlappende partities={parts_ok}), live-modus laat schijf met rust={live_ok} → {}",
        if ok { "OK (begeleide installatie + live-image, host-geteste planner) ✓" } else { "MISLUKT" }
    );
}

/// `euroinstall [live]`-shell: toon het installatieplan (dry-run, schrijft niets).
pub fn shell(args: &str) -> Vec<String> {
    let live = args.trim() == "live";
    let cfg = sample_config(live);
    let mut out = alloc::vec![alloc::format!(
        "EuroInstall — {} (dry-run, er wordt niets geschreven):",
        if live { "live-image (RAM-only)" } else { "installatie naar schijf (A/B)" }
    )];
    match plan(&cfg) {
        Ok(steps) => {
            for (i, s) in steps.iter().enumerate() {
                out.push(alloc::format!("  {:>2}. {}", i + 1, euroinstall::describe(s)));
            }
            out.push(String::from("  (echte uitvoering = userspace-installer, Ed25519-getekend; sector-I/O via gpt/eurofs)"));
        }
        Err(e) => out.push(alloc::format!("  config ongeldig: {e:?}")),
    }
    out
}

/// **BB-7** — render de begeleide grafische installer (EuroInstall) in een venster:
/// links de gekozen configuratie + de live full-disk-encryptie (TPM-verzegeld),
/// rechts het ECHTE, geordende installatieplan uit `euroinstall::plan`. De
/// uitvoering = de bewezen `instexec`-sector-I/O (Ed25519-getekende userspace-installer).
pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize) {
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let w = win_w;
    let h = win_h.saturating_sub(TITLEBAR_H);
    fb.fill_rect(x, y, w, h, Color::SURFACE);

    text::draw_px(fb, x + 28, y + 18, "EuroOS installeren", Color::INK, 22.0);
    text::draw_px(
        fb,
        x + 28,
        y + 50,
        "Begeleide, soevereine installatie \u{2014} versleuteld, A/B-kernels, Ed25519-geverifieerd.",
        Color::TEXT_SEC,
        12.5,
    );

    let cfg = sample_config(false);
    let half = w / 2;

    // ── Links: configuratiekaart ──
    let cx = x + 28;
    let cw = half.saturating_sub(44);
    let cyt = y + 84;
    fb.fill_rounded_rect(cx, cyt, cw, 232, crate::eds::RADIUS_M, Color::CARD);
    fb.draw_border(cx, cyt, cw, 232, 1, Color::BORDER);
    text::draw_px(fb, cx + 18, cyt + 16, "Configuratie", Color::INK, 15.0);
    let rows = [
        ("Doelschijf", alloc::format!("{} GiB (GPT, A/B)", cfg.disk.total_bytes / (1024 * 1024 * 1024))),
        ("Taal", cfg.locale.clone()),
        ("Toetsenbord", cfg.keymap.clone()),
        ("Computernaam", cfg.hostname.clone()),
        ("Gebruiker", cfg.username.clone()),
    ];
    let mut ry = cyt + 46;
    for (k, v) in rows.iter() {
        text::draw_px(fb, cx + 18, ry, k, Color::TEXT_DIM, 12.5);
        text::draw_px(fb, cx + 150, ry, v, Color::INK, 12.5);
        ry += 26;
    }
    // FDE-banner.
    fb.fill_rounded_rect(cx + 14, ry + 4, cw - 28, 40, crate::eds::RADIUS_S, Color::SUCCESS_SOFT);
    text::draw_px(fb, cx + 26, ry + 11, "\u{1F512} Full-disk-encryptie aan", Color::SUCCESS, 13.0);
    text::draw_px(fb, cx + 26, ry + 28, "sleutel verzegeld aan de TPM (EuroFDE) \u{2014} vóór format", Color::TEXT_SEC, 11.0);

    // ── Rechts: het ECHTE installatieplan ──
    let px = x + half + 8;
    text::draw_px(fb, px, y + 88, "Installatieplan", Color::INK, 15.0);
    let mut py = y + 116;
    match plan(&cfg) {
        Ok(steps) => {
            for (i, s) in steps.iter().enumerate() {
                if py > y + h - 70 {
                    break;
                }
                text::draw_px(fb, px, py, "\u{2713}", Color::SUCCESS, 13.0);
                text::draw_px(fb, px + 22, py, &alloc::format!("{}. {}", i + 1, euroinstall::describe(s)), Color::TEXT_SEC, 12.5);
                py += 23;
            }
            text::draw_px(fb, px, py + 6, "uitvoering: Ed25519-getekende userspace-installer,", Color::TEXT_DIM, 11.5);
            text::draw_px(fb, px, py + 22, "echte sector-I/O via gpt/eurofs (bewezen door instexec)", Color::TEXT_DIM, 11.5);
        }
        Err(e) => {
            text::draw_px(fb, px, py, &alloc::format!("config ongeldig: {e:?}"), Color::RED, 13.0);
        }
    }

    // ── Installeer-knop ──
    let bw = 200usize;
    let bx = x + w - bw - 28;
    let by = y + h - 52;
    fb.fill_rounded_rect(bx, by, bw, 36, crate::eds::RADIUS_M, Color::ACCENT);
    text::draw_px(fb, bx + 40, by + 9, "EuroOS installeren", Color::WHITE, 14.0);
}
