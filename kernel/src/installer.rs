//! Kernel side of **EuroInstall** (plan Q1): proves at boot that the installer
//! *planner* produces a valid, ordered step plan + partition layout. The
//! host-tested core lives in [`euroinstall`]; the real execution (sector I/O,
//! FDE enroll, user) is wired up later by the userspace installer process. Provides the
//! `euroinstall` shell command (dry-run of the plan).

use alloc::string::String;
use alloc::vec::Vec;

use euroinstall::{plan, Config, Disk, Step};

use crate::graphics::{Color, FrameBuffer};
use crate::text;

const TITLEBAR_H: usize = 44;

/// A sample config for the self-test + the shell dry-run.
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

/// Boot self-test: build an install plan + a live plan and check the
/// core promises (FDE before format, partitions non-overlapping, live leaves the disk alone).
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
            // Partitions do not overlap.
            let parts_ok = if let Some(Step::Partition(p)) = steps.first() {
                p.len() == 4 && p.windows(2).all(|w| w[0].start_lba + w[0].sectors <= w[1].start_lba)
            } else {
                false
            };
            (order_ok, parts_ok)
        }
        Err(_) => (false, false),
    };

    // Live mode must not contain any disk write step.
    let live_ok = live
        .as_ref()
        .map(|s| !s.iter().any(|st| matches!(st, Step::Partition(_) | Step::FormatSystem | Step::WriteKernelSlots)))
        .unwrap_or(false);

    let ok = steps_ok && parts_ok && live_ok;
    let nsteps = install.as_ref().map(|s| s.len()).unwrap_or(0);
    crate::serial_println!(
        "[install] EuroInstall planner: {nsteps}-step A/B install (FDE before format={steps_ok}, 4 non-overlapping partitions={parts_ok}), live mode leaves disk alone={live_ok} → {}",
        if ok { "OK (guided install + live image, host-tested planner) ✓" } else { "FAILED" }
    );
}

/// `euroinstall [live]` shell: show the install plan (dry-run, writes nothing).
pub fn shell(line: &str) -> Vec<String> {
    let toks: Vec<&str> = line.split_whitespace().collect();
    let args: Vec<&str> = if toks.first() == Some(&"euroinstall") { toks[1..].to_vec() } else { toks };

    // Flags: --to N · --hostname H · --user U · live.
    let (mut target, mut hostname, mut user, mut live) = (None::<usize>, None::<String>, None::<String>, false);
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--to" => { target = args.get(i + 1).and_then(|v| v.parse().ok()); i += 2; continue; }
            "--hostname" => { hostname = args.get(i + 1).map(|s| String::from(*s)); i += 2; continue; }
            "--user" => { user = args.get(i + 1).map(|s| String::from(*s)); i += 2; continue; }
            "live" => live = true,
            _ => {}
        }
        i += 1;
    }

    // `--to N`: REAL install to virtio disk N (provisions + makes bootable).
    if let Some(dev) = target {
        return do_install(dev, hostname, user);
    }

    // Otherwise: dry-run plan + available target disks + usage.
    let cfg = sample_config(live);
    let mut out = alloc::vec![alloc::format!(
        "EuroInstall — {} (dry-run, nothing is written):",
        if live { "live image (RAM-only)" } else { "install to disk (A/B)" }
    )];
    match plan(&cfg) {
        Ok(steps) => {
            for (i, s) in steps.iter().enumerate() {
                out.push(alloc::format!("  {:>2}. {}", i + 1, euroinstall::describe(s)));
            }
        }
        Err(e) => out.push(alloc::format!("  config invalid: {e:?}")),
    }
    let n = crate::virtio_blk::device_count();
    if n == 0 {
        out.push(String::from("  target disks: (no virtio-blk disk attached)"));
    } else {
        out.push(String::from("  target disks:"));
        for d in 0..n {
            let mib = crate::virtio_blk::capacity_sectors_dev(d) * 512 / 1024 / 1024;
            let blank = if crate::instexec::disk_is_blank(d) { "blank" } else { "in use" };
            out.push(alloc::format!("    virtio-blk {d}  ·  {mib} MiB  ·  {blank}"));
        }
    }
    let media = if crate::instexec::media_available() { "yes" } else { "no (only after a UEFI boot)" };
    out.push(alloc::format!("  install-media: {media}"));
    out.push(String::from("  REAL install:  euroinstall --to <N> [--hostname H] [--user U]   \u{26A0} ERASES disk N"));
    out
}

/// Perform a REAL install to virtio disk `dev` with optional hostname/user.
fn do_install(dev: usize, hostname: Option<String>, user: Option<String>) -> Vec<String> {
    if !crate::instexec::media_available() {
        return alloc::vec![String::from("euroinstall: no install media — only possible from a real UEFI boot.")];
    }
    if !crate::virtio_blk::present_dev(dev) {
        return alloc::vec![alloc::format!("euroinstall: no virtio-blk disk {dev} (see `euroinstall` for the list).")];
    }
    let mut cfg = crate::instexec::default_config();
    if let Some(h) = hostname { cfg.hostname = h; }
    if let Some(u) = user { cfg.username = u; }
    let mib = crate::virtio_blk::capacity_sectors_dev(dev) * 512 / 1024 / 1024;
    let ok = crate::instexec::install_to_disk(dev, &cfg);
    alloc::vec![
        alloc::format!("EuroInstall \u{2192} virtio-blk {dev} ({mib} MiB) · hostname={} · user={}", cfg.hostname, cfg.username),
        if ok {
            String::from("  \u{2713} bootable + provisioned EuroOS written — the disk now boots standalone (see [q1x3]).")
        } else {
            String::from("  \u{2717} install failed (disk too small, no media, or write error).")
        },
    ]
}

/// **BB-7** — render the guided graphical installer (EuroInstall) in a window:
/// on the left the chosen configuration + the live full-disk encryption (TPM-sealed),
/// on the right the REAL, ordered install plan from `euroinstall::plan`. The
/// execution = the proven `instexec` sector I/O (Ed25519-signed userspace installer).
pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize) {
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let w = win_w;
    let h = win_h.saturating_sub(TITLEBAR_H);
    fb.fill_rect(x, y, w, h, Color::SURFACE);

    text::draw_px(fb, x + 28, y + 18, "Install EuroOS", Color::INK, 22.0);
    text::draw_px(
        fb,
        x + 28,
        y + 50,
        "Guided, sovereign install \u{2014} encrypted, A/B kernels, Ed25519-verified.",
        Color::TEXT_SEC,
        12.5,
    );

    let cfg = sample_config(false);
    let half = w / 2;

    // ── Left: configuration card ──
    let cx = x + 28;
    let cw = half.saturating_sub(44);
    let cyt = y + 84;
    fb.fill_rounded_rect(cx, cyt, cw, 232, crate::eds::RADIUS_M, Color::CARD);
    fb.draw_border(cx, cyt, cw, 232, 1, Color::BORDER);
    text::draw_px(fb, cx + 18, cyt + 16, "Configuration", Color::INK, 15.0);
    let rows = [
        ("Target disk", alloc::format!("{} GiB (GPT, A/B)", cfg.disk.total_bytes / (1024 * 1024 * 1024))),
        ("Language", cfg.locale.clone()),
        ("Keyboard", cfg.keymap.clone()),
        ("Computer name", cfg.hostname.clone()),
        ("User", cfg.username.clone()),
    ];
    let mut ry = cyt + 46;
    for (k, v) in rows.iter() {
        text::draw_px(fb, cx + 18, ry, k, Color::TEXT_DIM, 12.5);
        text::draw_px(fb, cx + 150, ry, v, Color::INK, 12.5);
        ry += 26;
    }
    // FDE banner.
    fb.fill_rounded_rect(cx + 14, ry + 4, cw - 28, 40, crate::eds::RADIUS_S, Color::SUCCESS_SOFT);
    text::draw_px(fb, cx + 26, ry + 11, "\u{1F512} Full-disk encryption on", Color::SUCCESS, 13.0);
    text::draw_px(fb, cx + 26, ry + 28, "key sealed to the TPM (EuroFDE) \u{2014} before format", Color::TEXT_SEC, 11.0);

    // ── Right: the REAL install plan ──
    let px = x + half + 8;
    text::draw_px(fb, px, y + 88, "Install plan", Color::INK, 15.0);
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
            text::draw_px(fb, px, py + 6, "execution: Ed25519-signed userspace installer,", Color::TEXT_DIM, 11.5);
            text::draw_px(fb, px, py + 22, "real sector I/O via gpt/eurofs (proven by instexec)", Color::TEXT_DIM, 11.5);
        }
        Err(e) => {
            text::draw_px(fb, px, py, &alloc::format!("config invalid: {e:?}"), Color::RED, 13.0);
        }
    }

    // ── Install button ──
    let bw = 200usize;
    let bx = x + w - bw - 28;
    let by = y + h - 52;
    fb.fill_rounded_rect(bx, by, bw, 36, crate::eds::RADIUS_M, Color::ACCENT);
    text::draw_px(fb, bx + 40, by + 9, "Install EuroOS", Color::WHITE, 14.0);
}
