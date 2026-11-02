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
//! The file is not signed. It only chooses between two code paths that are
//! both part of the signed kernel image; it cannot add code.

use core::sync::atomic::{AtomicBool, Ordering};

static NET_USER: AtomicBool = AtomicBool::new(true);
static NVME_USER: AtomicBool = AtomicBool::new(false);
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
                ("storage.nvme", "user") => NVME_USER.store(true, Ordering::Relaxed),
                ("storage.nvme", "kernel") => NVME_USER.store(false, Ordering::Relaxed),
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
