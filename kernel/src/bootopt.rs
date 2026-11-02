//! Boot options from the ESP (`\EFI\BOOT\euroos-boot.conf`).
//!
//! EuroOS has no kernel command line (the loader starts the kernel image
//! without load options), so choices that must be made before the drivers
//! bind live in a small `key=value` file next to the kernel images, read once
//! while UEFI boot services still exist. A missing or unreadable file means
//! every default. Today there is one option:
//!
//! - `net.driver=kernel` (default): the in-kernel e1000 driver.
//! - `net.driver=user`: the userspace e1000 driver (`e1000d`, see udrv.rs);
//!   the kernel leaves the e1000 alone and starts the driver after the
//!   scheduler and the IOMMU are up.
//!
//! The file is not signed. It only chooses between two code paths that are
//! both part of the signed kernel image; it cannot add code.

use core::sync::atomic::{AtomicBool, Ordering};

static NET_USER: AtomicBool = AtomicBool::new(false);
static FOUND: AtomicBool = AtomicBool::new(false);

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
        "[bootopt] {}: net.driver={}",
        if FOUND.load(Ordering::Relaxed) { "\\EFI\\BOOT\\euroos-boot.conf read" } else { "no euroos-boot.conf (defaults)" },
        if net_user_driver() { "user" } else { "kernel" }
    );
}

/// Was `net.driver=user` chosen?
pub fn net_user_driver() -> bool {
    NET_USER.load(Ordering::Relaxed)
}
