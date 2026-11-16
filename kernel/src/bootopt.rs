//! Boot options from the ESP (`\EFI\BOOT\euroos-boot.conf`).
//!
//! EuroOS has no kernel command line (the loader starts the kernel image
//! without load options), so choices that must be made before the drivers
//! bind live in a small `key=value` file next to the kernel images, read once
//! while UEFI boot services still exist. A missing or unreadable file means
//! every default. The options:
//!
//! - `net.driver=user` (default since 2026-11-02): the userspace e1000 driver
//!   (`e1000d`, see udrv.rs); the kernel leaves the e1000 alone, starts the
//!   driver after the scheduler and the IOMMU are up, and runs the network part
//!   of the boot after it (`[net-late]` in main.rs).
//! - `net.driver=kernel`: the in-kernel e1000 driver (the fallback), bound
//!   before the scheduler as before.
//! - `storage.nvme=user`: the NVMe controller goes to the userspace driver
//!   `nvmed` (udrv_blk.rs) as a DATA disk mounted at `/data`; the in-kernel
//!   `nvme.rs` leaves it alone, so the root is never on it. `storage.nvme=kernel`
//!   (the default) keeps the in-kernel driver.
//!
//! - `virtio.transport=legacy`: drive transitional virtio devices (which offer
//!   both) through the legacy I/O BAR, as before 2026-11-05. The default,
//!   `modern`, uses the virtio 1.x capabilities whenever a device has them;
//!   modern-only devices (`disable-legacy=on`, Kyvernon VMM) always use them.
//!
//! - `debug.schedwatch=on`: a hardware write watch on the scheduler state
//!   (BUG-032, memwatch.rs; off by default, one #DB per context switch).
//!
//! - `ui.tour=off`: no first-run tour (onboarding.rs); for unattended images.
//!
//! - `display.mode=<width>x<height>` (for example `display.mode=1280x720`): use
//!   this screen resolution when the firmware offers it, instead of the largest
//!   one up to 1920x1080. An unknown or unavailable mode is ignored and logged.
//!   Meant for testing the desktop at a smaller size.
//!
//! - `serial.chatter=on`: keep the per-event diagnostic lines of the Linux
//!   layer on the serial console after the desktop has started (serial.rs
//!   `QUIET`). They are always in the kernel log (`dmesg`); on the console they
//!   cost the desktop its responsiveness, so the default is off.
//!
//! - `update.check=off`: no automatic update check (update.rs `maybe_check`):
//!   nothing is fetched, staged or installed unless `euroupdate check` is typed.
//!   For test guests that have a route to the internet.
//!
//! - `chrome.input=cdp`: the desktop also copies clicks and typed text for a
//!   hosted Chromium to the page over DevTools, as it did until 2026-11. The
//!   default, `x11`, sends X events only.
//!
//! - `aslr=on|off|legacy`: address-space layout randomisation of Linux
//!   programs (docs/design/ASLR.md, euromm::layout). `legacy` (the default
//!   until step S1 decides otherwise) is the layout of before; `on` places
//!   the demand region, the executable from disk, the mmap floor and the vDSO
//!   at random addresses in their zones; `off` uses the zones of `on` with
//!   every random part 0, so addresses repeat from run to run (debugging,
//!   comparing two logs). An unknown value is ignored and logged.
//!
//! The file is not signed. It only chooses between two code paths that are
//! both part of the signed kernel image; it cannot add code.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static NET_USER: AtomicBool = AtomicBool::new(true);
static NVME_USER: AtomicBool = AtomicBool::new(false);
static SCHED_WATCH: AtomicBool = AtomicBool::new(false);
static TOUR_OFF: AtomicBool = AtomicBool::new(false);
static UPDATE_CHECK_OFF: AtomicBool = AtomicBool::new(false);
static SERIAL_CHATTER: AtomicBool = AtomicBool::new(false);
static CHROME_INPUT_CDP: AtomicBool = AtomicBool::new(false);
static FOUND: AtomicBool = AtomicBool::new(false);
/// `display.mode`: width << 16 | height, 0 = not set.
static DISPLAY_MODE: AtomicUsize = AtomicUsize::new(0);

/// "1280x720" -> (1280, 720); both between 640x480 and 8192x8192.
fn parse_mode(v: &str) -> Option<(usize, usize)> {
    let (w, h) = v.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<usize>().ok()?, h.trim().parse::<usize>().ok()?);
    ((640..=8192).contains(&w) && (480..=8192).contains(&h)).then_some((w, h))
}

/// The resolution asked for with `display.mode`, if any.
pub fn display_mode() -> Option<(usize, usize)> {
    let v = DISPLAY_MODE.load(Ordering::Relaxed);
    (v != 0).then_some((v >> 16, v & 0xFFFF))
}

/// Parse the option text (host-testable shape: pure over the bytes).
fn apply(text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match (k.trim(), v.trim()) {
                ("net.driver", "user") => NET_USER.store(true, Ordering::Relaxed),
                ("net.driver", "kernel") => NET_USER.store(false, Ordering::Relaxed),
                ("storage.nvme", "user") => NVME_USER.store(true, Ordering::Relaxed),
                ("storage.nvme", "kernel") => NVME_USER.store(false, Ordering::Relaxed),
                ("debug.schedwatch", "on") => SCHED_WATCH.store(true, Ordering::Relaxed),
                ("debug.schedwatch", "off") => SCHED_WATCH.store(false, Ordering::Relaxed),
                ("virtio.transport", "legacy") => crate::virtio_pci::PREFER_LEGACY.store(true, Ordering::Relaxed),
                ("virtio.transport", "modern") => crate::virtio_pci::PREFER_LEGACY.store(false, Ordering::Relaxed),
                // First-run tour (kernel/src/onboarding.rs): off for unattended images.
                ("ui.tour", "off") => TOUR_OFF.store(true, Ordering::Relaxed),
                ("ui.tour", "on") => TOUR_OFF.store(false, Ordering::Relaxed),
                ("chrome.input", "cdp") => CHROME_INPUT_CDP.store(true, Ordering::Relaxed),
                ("chrome.input", "x11") => CHROME_INPUT_CDP.store(false, Ordering::Relaxed),
                ("serial.chatter", "on") => SERIAL_CHATTER.store(true, Ordering::Relaxed),
                ("serial.chatter", "off") => SERIAL_CHATTER.store(false, Ordering::Relaxed),
                ("update.check", "off") => UPDATE_CHECK_OFF.store(true, Ordering::Relaxed),
                ("update.check", "on") => UPDATE_CHECK_OFF.store(false, Ordering::Relaxed),
                ("smpuser", v) => match crate::apuser::parse_mode(v) {
                    Some(m) => crate::apuser::set_mode(m),
                    None => crate::serial_println!("[bootopt] smpuser={v} is not off, test or on: ignored"),
                },
                ("sched.drvwindow", v) => match v.parse::<u64>() {
                    Ok(t) if (5..=1000).contains(&t) => crate::sched::set_driver_window(t),
                    _ => crate::serial_println!("[bootopt] sched.drvwindow={v} is not 5..1000 ticks: ignored"),
                },
                ("sched.drvbudget", v) => match v.parse::<u64>() {
                    Ok(p) if (1..=100).contains(&p) => crate::sched::set_driver_budget(p),
                    _ => crate::serial_println!("[bootopt] sched.drvbudget={v} is not 1..100: ignored"),
                },
                ("aslr", v) => match euromm::layout::Mode::parse(v) {
                    Some(m) => crate::ring3::aslr_set_mode(m),
                    None => crate::serial_println!("[bootopt] aslr={v} is not on, off or legacy: ignored"),
                },
                ("display.mode", v) => match parse_mode(v) {
                    Some((w, h)) => DISPLAY_MODE.store(w << 16 | h, Ordering::Relaxed),
                    None => crate::serial_println!("[bootopt] display.mode={v} is not <width>x<height>: ignored"),
                },
                (k, v) => crate::serial_println!("[bootopt] unknown option {k}={v} ignored"),
            }
        }
    }
}

/// Read the option file from the ESP. Call once, before ExitBootServices.
pub fn capture() {
    use uefi::boot;
    use uefi::cstr16;
    use uefi::fs::FileSystem;
    use uefi::proto::media::fs::SimpleFileSystem;

    let Ok(handles) = boot::find_handles::<SimpleFileSystem>() else { return };
    for h in handles {
        let Ok(sfs) = boot::open_protocol_exclusive::<SimpleFileSystem>(h) else { continue };
        let mut fs = FileSystem::new(sfs);
        // Only our own ESP counts: the one that holds the kernel images.
        if !fs.try_exists(cstr16!("\\EFI\\BOOT\\eurokernel-A.efi")).unwrap_or(false) {
            continue;
        }
        if let Ok(bytes) = fs.read(cstr16!("\\EFI\\BOOT\\euroos-boot.conf")) {
            FOUND.store(true, Ordering::Relaxed);
            if bytes.len() <= 4096 {
                apply(&alloc::string::String::from_utf8_lossy(&bytes));
            }
        }
        break;
    }
    crate::serial_println!(
        "[bootopt] {}: net.driver={}, storage.nvme={}",
        if FOUND.load(Ordering::Relaxed) { "\\EFI\\BOOT\\euroos-boot.conf read" } else { "no euroos-boot.conf (defaults)" },
        if net_user_driver() { "user" } else { "kernel" },
        if nvme_user_driver() { "user (data disk only)" } else { "kernel" }
    );
    crate::serial_println!("[bootopt] aslr={}", crate::ring3::aslr_mode().name());
}

/// BUG-032: is the hardware write watch on the scheduler state on (memwatch.rs)?
/// Off by default; `debug.schedwatch=on` arms it.
/// `ui.tour=off`: do not show the first-run tour.
pub fn tour_off() -> bool {
    TOUR_OFF.load(Ordering::Relaxed)
}

/// `chrome.input=cdp`: input for a hosted Chromium is also copied over DevTools.
pub fn chrome_input_cdp() -> bool {
    CHROME_INPUT_CDP.load(Ordering::Relaxed)
}

/// `serial.chatter=on`: the serial console keeps every diagnostic line.
pub fn serial_chatter() -> bool {
    SERIAL_CHATTER.load(Ordering::Relaxed)
}

/// `update.check=off`: the periodic update check does nothing.
pub fn update_check_off() -> bool {
    UPDATE_CHECK_OFF.load(Ordering::Relaxed)
}

pub fn sched_watch() -> bool {
    SCHED_WATCH.load(Ordering::Relaxed)
}

/// Is the userspace NIC driver selected? (the default; `net.driver=kernel` turns it off)
pub fn net_user_driver() -> bool {
    NET_USER.load(Ordering::Relaxed)
}

/// Is the NVMe controller left to the userspace driver `nvmed` (udrv_blk.rs)?
/// Off by default: then the in-kernel `nvme.rs` drives it, root included.
pub fn nvme_user_driver() -> bool {
    NVME_USER.load(Ordering::Relaxed)
}
