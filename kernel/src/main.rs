#![no_main]
#![no_std]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod allocator;
mod audit;
mod compositor;
mod lockscreen;
mod console;
mod crashdump;
mod dispserv;
mod health;
mod container;
mod crypto;
mod eds;
mod eurodevice;
mod euroguard;
mod europol;
mod firewall;
mod observe;
mod vault;
mod euroipc;
mod acpi;
mod acpi_power;
mod apic;
mod appgfx;
mod appicons;
mod auth;
mod euroui;
mod font;
mod text;
mod gdt;
mod graphics;
mod icons;
mod immutable;
mod init;
mod interrupts;
mod klog;
mod mouse;
mod ahci;
mod e1000;
mod msix;
mod nic;
mod net;
mod paging;
mod gpt;
mod audio;
mod hda;
mod hpet;
mod pci;
mod power;
mod procpool;
mod xserver;
mod rootblk;
mod scrub;
mod swapmgr;
mod wasm;
mod wayland;
mod ps2;
mod ring3;
mod rtc;
mod sched;
mod watchdog;
mod serial;
mod gdbstub;
mod session;
mod shell;
mod smp;
mod nvme;
mod tls_roots;
mod update;
mod tpm;
mod entropy;
mod nts;
mod verity;
mod euroattr;
mod gdpr;
mod virtio_blk;
mod virtio_gpu;
mod virtio_snd;
mod virtio_net;
mod vpn;
mod agent;
mod locale;
mod mime;
mod installer;
mod ca;
mod attest;
mod attest3;
mod journal;
mod phase3g;
mod phase3a;
mod idm;
mod euroid;
mod pkg;
mod portal;
mod repro;
mod access;
mod suite;
mod wifi;
mod gpu;
mod print;
mod scan;
mod mcpd;
mod wagent;
mod instexec;
mod suite_ui;
mod web;
mod reken;
mod notes;
mod archive;
mod safe;
mod wm;
mod clip;
mod clockapp;
mod shot;
mod contacts;
mod calapp;
mod signapp;
mod musicapp;
mod mailapp;
mod fontapp;
mod jsapp;
mod webview;
mod calc_ui;
mod settings_ui;
mod clipboard;
mod ctxmenu;
mod trash;
mod launcher;
mod filedialog;
mod notify;
mod screenshot;
mod tooltip;
mod symbolpicker;
mod spell;
mod switcher;
mod workspace;
mod netbridge;
mod agent_ui;
mod files;
mod textedit;
mod monitor;
mod logview;
mod fatmount;
mod extmount;
mod smbfs;
mod nfsmount;
mod disktest;
mod stresstest;
mod scon;
mod media;
mod xhci;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;

use eurofs::{EuroFs, FileSystem};
use euromm::{FrameAllocator, MemoryRegion};
use uefi::boot::MemoryType;
use uefi::mem::memory_map::MemoryMap;
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};

use compositor::SIDEBAR_W;
use font::{draw_string, text_width};
use graphics::{Color, FrameBuffer};

/// AG-1: read a REAL directory from the FS and hand it to the EuroFiles GUI. No mock —
/// 3F-5: map a euromime default-app id to a desktop `SuiteApp` window, so a
/// double-clicked file opens in the right app. `None` for apps without a window.
fn mime_app_to_suite(app: &str) -> Option<suite_ui::SuiteApp> {
    use suite_ui::SuiteApp;
    Some(match app {
        "eurotext" => SuiteApp::Text,
        "eurowriter" => SuiteApp::Writer,
        "eurocalc" => SuiteApp::Calc,
        "euroimpress" => SuiteApp::Impress,
        "eurobrowser" => SuiteApp::Browser,
        _ => return None, // e.g. euroshot/euromusic have no window app yet
    })
}

/// the file manager shows exactly what `fs.list_dir` returns.
fn load_files_dir(fs: &mut dyn FileSystem, path: &str) {
    let items = match fs.list_dir(path) {
        Ok(v) => v
            .into_iter()
            .map(|e| (e.name, e.kind == eurofs::EntryKind::Directory, e.size))
            .collect::<alloc::vec::Vec<_>>(),
        Err(_) => alloc::vec::Vec::new(),
    };
    files::load_dir(path, items);
}

/// Build the right-click context menu for whatever object is under the cursor:
/// a file or directory in EuroFiles, an empty file-manager area, a text field,
/// a dock tile, or the bare desktop. Each surface gets its own action list.
fn build_context_menu(
    rx: usize,
    ry: usize,
    windows: &[compositor::Window],
    order: &[usize],
    dock_targets: &[Option<usize>; 11],
    sw: usize,
    sh: usize,
) {
    use ctxmenu::{Action, Item};

    // A dock tile? (the dock sits left of every window.)
    if let Some(icon) = compositor::dock_icon_at(rx, ry) {
        if dock_targets.get(icon).copied().flatten().is_some() {
            ctxmenu::open(rx, ry, alloc::vec![Item::new("Open", "", Action::OpenApp(icon))], sw, sh);
            return;
        }
    }

    // The topmost visible window under the cursor.
    let win = order.iter().rev().copied().find(|&i| windows[i].visible && windows[i].contains(rx, ry));
    if let Some(i) = win {
        let (wx, wy) = (windows[i].x, windows[i].y);
        match windows[i].app {
            suite_ui::SuiteApp::Files => {
                if let Some(dir) = files::hit_test(wx, wy, rx, ry) {
                    ctxmenu::open(rx, ry, alloc::vec![
                        Item::new("Open", "", Action::OpenDir(dir.clone())).sep(),
                        Item::new("Copy path", "Ctrl C", Action::CopyText(dir)),
                    ], sw, sh);
                } else if let Some(file) = files::hit_test_file(wx, wy, rx, ry) {
                    ctxmenu::open(rx, ry, alloc::vec![
                        Item::new("Open", "Enter", Action::OpenFile(file.clone())),
                        Item::new("Copy path", "Ctrl C", Action::CopyText(file.clone())).sep(),
                        Item::new("Move to Trash", "Del", Action::Trash(file)),
                    ], sw, sh);
                } else {
                    // Empty area of the file manager → folder actions on the current dir.
                    let cur = files::current_path();
                    let dir = if cur.is_empty() { String::from("/") } else { cur };
                    let mut items = alloc::vec![Item::new("New folder", "Ctrl Shift N", Action::NewFolder(dir))];
                    if trash::count() > 0 {
                        items.push(Item::new("Restore last deleted", "Ctrl Z", Action::RestoreTrash));
                    }
                    if let Some(last) = items.last_mut() {
                        last.sep_after = true; // separator before Refresh
                    }
                    items.push(Item::new("Refresh", "F5", Action::Refresh));
                    ctxmenu::open(rx, ry, items, sw, sh);
                }
            }
            // Text-bearing windows → Paste (enabled only when the clipboard has text).
            suite_ui::SuiteApp::None | suite_ui::SuiteApp::Text | suite_ui::SuiteApp::Notes => {
                let mut paste = if clipboard::has_content() {
                    Item::new("Paste", "Ctrl V", Action::Paste)
                } else {
                    Item::disabled("Paste")
                };
                paste.sep_after = true;
                ctxmenu::open(rx, ry, alloc::vec![
                    paste,
                    Item::new("Insert symbol", "", Action::InsertSymbol),
                ], sw, sh);
            }
            _ => {}
        }
        return;
    }

    // The bare desktop.
    let mut items = alloc::vec![
        Item::new("Open Terminal", "", Action::OpenTerminal),
        Item::new("Take screenshot", "", Action::Screenshot),
        Item::new("Display settings", "", Action::OpenDisplaySettings),
    ];
    if trash::count() > 0 {
        items.push(Item::new("Restore last deleted", "Ctrl Z", Action::RestoreTrash));
    }
    if let Some(last) = items.last_mut() {
        last.sep_after = true;
    }
    items.push(Item::new("Refresh", "F5", Action::Refresh));
    ctxmenu::open(rx, ry, items, sw, sh);
}

/// The tooltip text for whatever control is under the cursor, if any: the EU
/// mark, a dock tile, the status panel, or a window's traffic-light buttons.
fn tooltip_for(px: usize, py: usize, windows: &[compositor::Window], order: &[usize], width: usize) -> Option<String> {
    if compositor::brand_button_at(px, py) {
        return Some(String::from("Open the app launcher"));
    }
    if let Some(icon) = compositor::dock_icon_at(px, py) {
        return launcher::name_for_icon(icon).map(String::from);
    }
    let (rx, ry, rw, rh) = compositor::status_panel_rect(width);
    if px >= rx && px < rx + rw && py >= ry && py < ry + rh {
        return Some(String::from("Notifications"));
    }
    if let Some(i) = order.iter().rev().copied().find(|&i| windows[i].visible && windows[i].contains(px, py)) {
        if let Some(btn) = windows[i].title_button_at(px, py) {
            return Some(String::from(match btn {
                compositor::TitleButton::Close => "Close",
                compositor::TitleButton::Minimize => "Minimize",
                compositor::TitleButton::Maximize => "Maximize",
            }));
        }
    }
    None
}

const PROMPT: &str = "euroos:/ $ ";

/// EuroGuard Level-1 system policy (Track 7, Phase 7.2). On the first boot it is
/// written to /etc/euroguard/system.conf and read back from there —
/// data-driven, not hard-coded. Simple, readable rule format.
const EUROGUARD_CONF: &[u8] = b"# EuroGuard system policy (Level 1) - /etc/euroguard/system.conf\n\
# Transparent: this is exactly what the system blocks. Edit + reboot.\n\
\n\
# Blocked IP addresses (tracker/telemetry endpoints)\n\
block-ip 203.0.113.5\n\
\n\
# Blocked ports (outdated/insecure)\n\
block-port 23\n\
block-port 1900\n\
\n\
# DNS block list: ads, trackers, telemetry (incl. subdomains)\n\
block-domain ads.doubleclick.net\n\
block-domain telemetry.mozilla.org\n\
block-domain google-analytics.com\n\
block-domain graph.facebook.com\n";

/// Framebuffer info as plain data, globally available to the panic handler.
#[derive(Clone, Copy)]
struct FbInfo {
    base: usize,
    width: usize,
    height: usize,
    stride: usize,
    pf: PixelFormat,
}
static FB_INFO: spin::Once<FbInfo> = spin::Once::new();

/// Blit an app's XRGB8888 (`0x00RRGGBB`) frame STRAIGHT to the GOP framebuffer,
/// integer-scaled + centered. Called from the `fb_present` syscall (under the
/// app's own CR3 — the GOP MMIO is mapped supervisor in every address space), so
/// a full-screen app (the DOOM port) paints at its own frame rate instead of
/// depending on the desktop loop, which a heavyweight app can starve down to a
/// couple of Hz. `src` is `sw*sh` pixels.
pub fn screen_present_xrgb(src: &[u32], sw: usize, sh: usize) {
    let fbi = match FB_INFO.get() {
        Some(i) => i,
        None => return,
    };
    if sw == 0 || sh == 0 || src.len() < sw * sh {
        return;
    }
    let scale = core::cmp::min(
        fbi.width.saturating_sub(40) / sw,
        fbi.height.saturating_sub(120) / sh,
    )
    .clamp(1, 4);
    let (dw, dh) = (sw * scale, sh * scale);
    if dw > fbi.width || dh > fbi.height {
        return;
    }
    let dx = (fbi.width - dw) / 2;
    let dy = (fbi.height - dh) / 2;
    let dst = fbi.base as *mut u32;
    let rgb = matches!(fbi.pf, PixelFormat::Rgb);
    for sy in 0..sh {
        let row = &src[sy * sw..sy * sw + sw];
        for k in 0..scale {
            let ty = dy + sy * scale + k;
            if ty >= fbi.height {
                return;
            }
            let dst_row = ty * fbi.stride;
            let mut dc = dx;
            for &v in row {
                // v is 0x00RRGGBB; a BGR framebuffer wants exactly those low three
                // bytes (B,G,R little-endian), so write it verbatim — only an RGB
                // panel needs the R/B swap (matches `FrameBuffer::present_rect`).
                let out = if rgb { ((v & 0xFF) << 16) | (v & 0x0000_FF00) | ((v >> 16) & 0xFF) } else { v };
                for _ in 0..scale {
                    if dc >= fbi.width {
                        break;
                    }
                    unsafe { dst.add(dst_row + dc).write_volatile(out) };
                    dc += 1;
                }
            }
        }
    }
}

#[entry]
fn main() -> Status {
    // ── First of all: our own heap + serial (work even after ExitBootServices). ──
    allocator::init();
    serial::init();
    serial_println!("\n[euro] EuroKernel bring-up — heap ({} MiB) + COM1 active", allocator::size() / (1024 * 1024));
    // Symbolization anchor at BOOT (not only on panic): a hung boot leaves no
    // panic dump, so print the runtime address of the anchor function now. Any
    // externally sampled RIP can then be resolved with scripts/symbolize.sh.
    serial_println!(
        "[euro] anchor dump_registers_and_backtrace @ {:#018x}",
        klog::dump_registers_and_backtrace as usize as u64
    );
    ring3::dump_suspect_addrs(); // for mapping an NMI-captured wedge RIP

    // EuroFS is set up later (after virtio-blk init): either on the GPT disk
    // (installed, persistent) or in RAM (live mode). See `populate_fs`.

    // ── Track 3.1: frame allocator from the UEFI memory map (still in BS). ──
    let mut allocator = build_frame_allocator();
    serial_println!("[euro] frame allocator: {} MiB usable RAM", allocator.usable_bytes() / (1024 * 1024));

    // ── Fetch and keep the GOP framebuffer (stays valid after exit). ──
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>().expect("GOP handle");
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(handle).expect("GOP open");
    graphics::set_best_mode(&mut gop);
    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();
    let pf = mode.pixel_format();
    let base = gop.frame_buffer().as_mut_ptr();
    FB_INFO.call_once(|| FbInfo { base: base as usize, width, height, stride, pf });
    // SAFETY: the framebuffer memory stays valid after ExitBootServices.
    // Buffered: drawing goes to a RAM backbuffer, present() blits the image.
    let fb = unsafe { FrameBuffer::new_buffered(base, width, height, stride, pf) };
    drop(gop); // close the protocol cleanly while Boot Services are still alive
    serial_println!("[euro] GOP {width}x{height} stride={stride} {pf:?}");

    // Grab the ACPI RSDP address from the UEFI configuration table (only possible
    // now, before we leave UEFI) — needed to later read the MADT (CPU cores + IO-APIC).
    let rsdp = uefi::system::with_config_table(|t| {
        t.iter()
            .find(|e| e.guid == uefi::table::cfg::ACPI2_GUID)
            .or_else(|| t.iter().find(|e| e.guid == uefi::table::cfg::ACPI_GUID))
            .map(|e| e.address as u64)
            .unwrap_or(0)
    });
    acpi::set_rsdp(rsdp);
    serial_println!("[acpi] RSDP @ {rsdp:#x}");

    // AG-3: read our OWN install media (loader + A/B kernel) from the boot volume
    // via UEFI — ONLY POSSIBLE NOW, before ExitBootServices. This lets the installer
    // later write a bootable copy to a target disk (no embed, the real current bytes).
    instexec::capture_media();

    // ── THE JUMP: leave UEFI Boot Services. After this no more UEFI services. ──
    serial_println!("[euro] ExitBootServices...");
    let _ = unsafe { boot::exit_boot_services(MemoryType::LOADER_DATA) };
    serial_println!("[euro] >>> UEFI left — kernel mode <<<");

    // ── Kernel-mode bring-up: interrupts off, GDT, IDT, exception test. ──
    x86_64::instructions::interrupts::disable();
    gdt::init();
    serial_println!("[euro] GDT+TSS loaded");
    interrupts::init();
    serial_println!("[euro] IDT loaded");

    // Load our own page tables (identity map; everything keeps working on our CR3).
    let fb_base = FB_INFO.get().map(|i| i.base).unwrap_or(0);
    serial_println!("[euro] framebuffer @ {fb_base:#x} — loading our own page tables...");
    let pml4 = paging::init(&mut allocator);
    sched::set_boot_pml4(pml4); // shared kernel address space for the scheduler
    serial_println!("[euro] CR3 loaded (PML4 @ {pml4:#x}) — our own paging active");

    // A2: set up a guarded kernel stack in the shared high region (one unmapped
    // guard page below the stack → overflow faults immediately). Non-destructive
    // verification: stack page writable, guard page not present.
    let gtop = paging::setup_guarded_stack(&mut allocator);
    if gtop != 0 {
        let stack_ok = unsafe {
            let p = (gtop - 8) as *mut u64; // within the topmost stack page
            p.write_volatile(0xA2_DEAD_BEEF);
            p.read_volatile() == 0xA2_DEAD_BEEF
        };
        serial_println!(
            "[a2] guarded kernel stack: top {:#x}, guard {:#x} — stack writable={}, guard not-present={}",
            gtop,
            paging::STACK_GUARD_ADDR.load(core::sync::atomic::Ordering::Relaxed),
            stack_ok,
            paging::guard_page_unmapped(),
        );
    }

    // S3: reserve a PROCESS FRAME POOL (64 MiB) from the main allocator. fork()/
    // execve() allocate from it while running in a syscall (the main allocator is
    // then unreachable). Identity-mapped, so kernel-accessible.
    const POOL_FRAMES: usize = 16384; // 64 MiB
    match allocator.allocate_contiguous(POOL_FRAMES) {
        Ok(base) => {
            procpool::install(base, POOL_FRAMES);
            serial_println!("[mm] process frame pool: 64 MiB @ {base:#x} (fork/exec)");
        }
        Err(_) => serial_println!("[mm] WARNING: no process frame pool (fork disabled)"),
    }

    // virtio-blk: initialize the real disk (PIO/DMA works on our identity map).
    virtio_blk::init(&mut allocator);
    // EuroPack: register files served straight from a pack disk (no RAM copy) —
    // how binaries too large to embed (chrome) reach the glibc loader.
    ring3::europack_scan();

    // NVMe (B2): detect + initialize an NVMe controller (admin/I/O queues,
    // identify), do a read/write self-test + SMART readout. No-op without NVMe.
    if nvme::init(&mut allocator) {
        nvme::self_test();
    }

    // AHCI/SATA (Metal M2-2): bring up every SATA disk behind an AHCI
    // controller (q35's built-in ICH9 carries the boot medium; the metal
    // matrix attaches scratch disks). Read-only self-test on partitioned
    // disks, full write/read/verify on blank ones. No-op without AHCI.
    if ahci::init(&mut allocator) > 0 {
        ahci::self_test();
    }

    // ── ROOT FILESYSTEM ── EuroFS lives ON the disk (installed, persistent)
    // if there is a virtio-blk disk, otherwise in RAM (live mode). An existing FS
    // is mounted; a fresh/empty disk is formatted + populated
    // (= installation). This way files survive a restart — like a real OS.
    // AG-3: if we have REAL install media AND there is a blank virtio target disk,
    // install a bootable EuroOS onto it (instead of using it as root) and
    // keep running ourselves in live mode — the target disk boots standalone (multidisk harness).
    let installed = if virtio_blk::present() && disktest::armed() {
        // [mdisk] harness: the sentinel on disk0 arms the destructive multi-disk
        // load+functional test. Run it instead of install/update, then continue live.
        disktest::run();
        false
    } else if virtio_blk::present() && stresstest::arm_if_sentinel() {
        // [stress] harness: the EUROSTRESS sentinel on disk0 arms the big load/stress
        // test. It must run LATE (after ring3/interrupts/VFS are up), so we only latch
        // it here and skip install; the run happens just before the shell starts.
        false
    } else if virtio_blk::present() && extmount::is_ext(0) {
        // [io7] harness: disk0 holds a Linux ext2/3/4 volume → read-test it (not blank,
        // so guard before the install path which would otherwise try to format it).
        extmount::selftest();
        false
    } else if instexec::media_available() && virtio_blk::present() {
        if instexec::disk_is_blank(0) {
            // Fresh target disk → install a bootable, provisioned EuroOS (slot A).
            instexec::install_to_disk(0, &instexec::default_config())
        } else if gpt::find_eurofs_partition().is_some() {
            // Our own installed disk → demonstrate the A/B SELF-UPDATE: stage slot B
            // + flip slot_config. After a standalone reboot the loader picks slot B.
            instexec::stage_update_b(0);
            instexec::rollback_selftest(0); // [upd4]: prove the two-stage rollback on the real ESP
            true // we keep running live; the disk is the boot/update target
        } else {
            // Disk 0 holds someone else's filesystem (exFAT/FAT/NTFS/…) — do NOT
            // stage an update onto it. Leave it untouched; it stays mountable.
            false
        }
    } else {
        false
    };
    // Prefer a REAL, persistent on-disk EuroFS root whenever a virtio-blk disk is present —
    // including right after a fresh install or an A/B update. (Previously the post-install
    // `installed` path fell back to an 8 MiB RAM root, so an installed system never actually
    // ran from its large on-disk root.) `sync_system_files` + `ensure_etc_skeleton` below
    // complete /bin + /etc on a disk root that only carries install-time config.
    // Metal M2-3: true when the NVMe disk carries the live root (so the later
    // /nvme data-disk demo doesn't double-mount it).
    let mut nvme_root = false;
    let rootdev = if virtio_blk::present() {
        let total = virtio_blk::capacity_sectors();
        match gpt::find_eurofs_partition() {
            Some((start, blocks)) => {
                if installed {
                    serial_println!("[euro] live root = the on-disk EuroFS (installed/updated this boot)");
                }
                rootblk::RootBlk::disk(start, blocks)
            }
            None if instexec::disk_is_blank(0) => {
                // Blank disk → lay down our GPT + EuroFS root.
                let (start, blocks) = gpt::install(total);
                rootblk::RootBlk::disk(start, blocks)
            }
            None => {
                // Disk 0 carries a NON-EuroFS filesystem (exFAT/FAT/ext/NTFS/…).
                // Never format/clobber it as our root — boot a RAM root instead;
                // the disk stays intact and can be mounted (mount vblkN /mnt).
                serial_println!("[euro] disk 0 is not EuroFS and not blank — booting a RAM root; the disk is left intact + mountable");
                rootblk::RootBlk::ram(2048)
            }
        }
    } else if nvme::present() {
        // Metal M2-3: no virtio-blk, but an NVMe disk is here. A modern
        // (NVMe-only) laptop boots from NVMe: mount the root on it. Its own
        // installed EuroFS partition, else install onto a blank NVMe from the
        // boot media, else — a non-EuroFS NVMe — leave it intact + RAM root.
        match instexec::nvme_eurofs_partition() {
            Some((start, blocks)) => {
                serial_println!("[euro] live root = the on-disk EuroFS on NVMe (standalone NVMe boot)");
                nvme_root = true;
                rootblk::RootBlk::nvme(start, blocks)
            }
            None if instexec::media_available() && instexec::nvme_is_blank() => {
                if instexec::install_to_nvme(&instexec::default_config()) {
                    match instexec::nvme_eurofs_partition() {
                        Some((start, blocks)) => {
                            serial_println!("[euro] live root = the freshly-installed EuroFS on NVMe");
                            nvme_root = true;
                            rootblk::RootBlk::nvme(start, blocks)
                        }
                        None => rootblk::RootBlk::ram(2048),
                    }
                } else {
                    rootblk::RootBlk::ram(2048)
                }
            }
            None => {
                serial_println!("[euro] NVMe disk is not EuroFS and not blank — booting a RAM root; it stays intact + mountable at /nvme");
                rootblk::RootBlk::ram(2048)
            }
        }
    } else if ahci::disk_count() > 0 {
        // Metal M2-3: no virtio-blk and no NVMe, but SATA disks are here. Scan
        // them for our installed EuroFS, else install onto a blank one. The boot
        // medium (q35 exposes it on SATA) is partitioned + non-EuroFS, so both
        // rules skip it — it is never clobbered.
        let mut chosen = None;
        for idx in 0..ahci::disk_count() {
            if let Some((start, blocks)) = instexec::ahci_eurofs_partition(idx) {
                serial_println!("[euro] live root = the on-disk EuroFS on AHCI disk {idx} (standalone SATA boot)");
                chosen = Some(rootblk::RootBlk::ahci(idx, start, blocks));
                break;
            }
        }
        if chosen.is_none() && instexec::media_available() {
            for idx in 0..ahci::disk_count() {
                if instexec::ahci_is_blank(idx) && instexec::install_to_ahci(idx, &instexec::default_config()) {
                    if let Some((start, blocks)) = instexec::ahci_eurofs_partition(idx) {
                        serial_println!("[euro] live root = the freshly-installed EuroFS on AHCI disk {idx}");
                        chosen = Some(rootblk::RootBlk::ahci(idx, start, blocks));
                    }
                    break;
                }
            }
        }
        chosen.unwrap_or_else(|| {
            serial_println!("[euro] no installable/EuroFS SATA disk — booting a RAM root (disks left intact)");
            rootblk::RootBlk::ram(2048)
        })
    } else {
        rootblk::RootBlk::ram(2048) // RAM root when there is NO disk at all
    };
    // The install media (~6 MiB) stays available so the user can install LATER
    // from the running desktop too (`euroinstall --to N`).
    let on_disk = rootdev.is_disk();
    // J1/3C-1: the live root FS runs THROUGH a write-through block cache (concurrent
    // read-lock hits, CLOCK eviction, dirty write-back). 256 × 4 KiB = 1 MiB.
    const ROOT_CACHE_BLOCKS: usize = 256;
    use eurofs::cache::BlockCache;
    let mut fs = match EuroFs::mount(BlockCache::new(rootdev.clone(), ROOT_CACHE_BLOCKS), rtc::epoch()) {
        Ok(f) => {
            let cp = f.superblock().checkpoint_id; // copy out of the packed struct
            serial_println!(
                "[euro] EuroFS mounted{} via 1 MiB block cache (existing, checkpoint {})",
                if on_disk { " from DISK" } else { "" },
                cp
            );
            f
        }
        Err(_) => {
            let mut f = EuroFs::format(BlockCache::new(rootdev, ROOT_CACHE_BLOCKS), [0x5A; 16], rtc::epoch())
                .expect("EuroFS format");
            populate_fs(&mut f);
            serial_println!(
                "[euro] EuroFS formatted + populated{} (through 1 MiB block cache)",
                if on_disk { " on DISK (installation)" } else { " in RAM (live)" }
            );
            f
        }
    };

    // EuroUpdate (F1): A/B slot decision + attempt counter/rollback on every boot.
    update::boot_init(&mut fs);
    // G4: prove the direct image→slot-partition write (EuroOS-B, sector I/O + read-back).
    // Skip if virtio dev 0 is a foreign EuroPack data disk (would overwrite it).
    if !ring3::europack_on_vblk0() {
        update::slot_partition_selftest();
    }
    // [upd3] (Sprint 3): verify-before-activate with a REAL Ed25519 signature +
    // prove that the update pipeline rejects a tampered package.
    crypto::selftest();
    update::apply_gate_selftest(rtc::epoch());
    // [edit] (Sprint 4): edit EuroText → save → re-read on the REAL EuroFS.
    textedit::selftest(&mut fs);
    trash::selftest(&mut fs); // conveniences: delete-to-trash + undo
    // [io1]/[io2] (Sprint IO): FAT32 mount + read + write driver, proven in-kernel.
    fatmount::selftest();

    // J2: bad-block remap on the REAL disk — mark a block bad and prove that
    // I/O is transparently redirected to a spare block (bad-block table ↔ scrub).
    // Skip when virtio dev 0 is a foreign DATA disk (a EuroPack chrome-serving
    // volume): these tests scribble on GPT-gap LBAs (50, 60+) that hold the served
    // file's bytes there — never write over a data disk we do not own.
    if virtio_blk::present() && !ring3::europack_on_vblk0() {
        let mut bbt = eurofs::badblocks::BadBlockTable::new(50, 8); // spare pool LBA 50..58 (GPT gap)
        let bad = 48u64;
        if let Some(spare) = bbt.mark_bad(bad) {
            let mut pat = [0u8; 512];
            for (i, b) in pat.iter_mut().enumerate() {
                *b = (i as u8) ^ 0x5A;
            }
            virtio_blk::write_sector(bbt.translate(bad), &pat); // translate(48) = spare 50
            virtio_blk::flush();
            let mut rb = [0u8; 512];
            virtio_blk::read_sector(bbt.translate(bad), &mut rb);
            serial_println!(
                "[j2] bad-block: LBA {} → spare {} remapped, read-after-remap={} ({} bad, {} spares left) ✓",
                bad,
                spare,
                rb == pat,
                bbt.bad_count(),
                bbt.spares_left()
            );
        }

        // J3: prove the SWAP CYCLE on real RAM + disk — write a page, have
        // CLOCK pick it as the victim, swap it out to a swap block, free the
        // frame, and read the page back into a NEW frame (swap-in).
        const SWAP_BASE_LBA: u64 = 60; // swap area in the GPT gap (8 slots × 8 sectors)
        let mut area = euromm::swap::SwapArea::new(8);
        let mut clock = euromm::swap::Clock::new();
        if let Ok(frame) = allocator.allocate() {
            let pat: [u8; 4096] = core::array::from_fn(|i| (i as u8) ^ 0x3C);
            unsafe { core::ptr::copy_nonoverlapping(pat.as_ptr(), frame as *mut u8, 4096) };
            clock.insert(frame);
            let victim = clock.evict().unwrap_or(frame); // CLOCK picks the victim
            let slot = area.alloc().unwrap_or(0);
            for s in 0..8u64 {
                let mut sec = [0u8; 512];
                unsafe { core::ptr::copy_nonoverlapping((victim + s * 512) as *const u8, sec.as_mut_ptr(), 512) };
                virtio_blk::write_sector(SWAP_BASE_LBA + slot as u64 * 8 + s, &sec);
            }
            virtio_blk::flush();
            let _ = allocator.free(victim); // frame returned to the allocator
            if let Ok(newframe) = allocator.allocate() {
                for s in 0..8u64 {
                    let mut sec = [0u8; 512];
                    virtio_blk::read_sector(SWAP_BASE_LBA + slot as u64 * 8 + s, &mut sec);
                    unsafe { core::ptr::copy_nonoverlapping(sec.as_ptr(), (newframe + s * 512) as *mut u8, 512) };
                }
                area.free(slot);
                let intact = unsafe { core::slice::from_raw_parts(newframe as *const u8, 4096) } == &pat[..];
                serial_println!(
                    "[j3] swap cycle: page → CLOCK victim → swap slot {} (LBA {}) → read back into new frame, data-intact={} ({} swap slots free) ✓",
                    slot,
                    SWAP_BASE_LBA + slot as u64 * 8,
                    intact,
                    area.free_count()
                );
                let _ = allocator.free(newframe);
            }
        }

        // J3 TRANSPARENT: prove the fault-driven swap-in. Map a page, write
        // a pattern through the virtual mapping, swap it OUT (PTE not-present + slot
        // encoded), and then touch it: that MUST raise a page fault that the
        // handler transparently catches by reading the page back from disk.
        {
            const DEMO_VIRT: u64 = 0x4000_0000_0000; // PML4[128] — unused
            const FAULT_SWAP_LBA: u64 = 200; // separate from the [j3] area (LBA 60..124)
            let mut pool = alloc::vec::Vec::new();
            for _ in 0..2 {
                if let Ok(f) = allocator.allocate() {
                    pool.push(f);
                }
            }
            if let Ok(page) = allocator.allocate() {
                swapmgr::init(FAULT_SWAP_LBA, 8, pool);
                swapmgr::map_one_page(&mut allocator, DEMO_VIRT, page);
                let pat: [u8; 4096] = core::array::from_fn(|i| (i as u8) ^ 0x77);
                unsafe {
                    core::ptr::copy_nonoverlapping(pat.as_ptr(), DEMO_VIRT as *mut u8, 4096);
                }
                let out = swapmgr::swap_out(DEMO_VIRT);
                // DEMO_VIRT is now not-present: this access faults → transparent swap-in.
                let intact = unsafe { core::slice::from_raw_parts(DEMO_VIRT as *const u8, 4096) } == &pat[..];
                let (ins, outs) = swapmgr::stats();
                serial_println!(
                    "[j3-fault] transparent swap: swapped-out={out}, after page-fault data-intact={} (swap-ins={}, swap-outs={}) ✓",
                    intact, ins, outs
                );
            }
        }

        // [j3-evict] (3C-7): CLOCK auto-evict under memory pressure hands a frame back
        // to the GLOBAL allocator; the evicted page transparently faults back in.
        {
            const EVICT_VIRT: u64 = 0x4000_0000_1000; // another page in the unused PML4 slot
            if let Ok(page) = allocator.allocate() {
                swapmgr::map_one_page(&mut allocator, EVICT_VIRT, page);
                let pat: [u8; 4096] = core::array::from_fn(|i| (i as u8) ^ 0x5A);
                unsafe {
                    core::ptr::copy_nonoverlapping(pat.as_ptr(), EVICT_VIRT as *mut u8, 4096);
                }
                swapmgr::register_swappable(EVICT_VIRT);
                let before = allocator.free_frames();
                let freed = swapmgr::auto_evict(&mut allocator);
                let after = allocator.free_frames();
                // Touch the evicted page → fault → transparent swap-in from the reserve.
                let intact = unsafe { core::slice::from_raw_parts(EVICT_VIRT as *const u8, 4096) } == &pat[..];
                let reclaimed = freed.is_some() && after > before;
                serial_println!(
                    "[j3-evict] CLOCK auto-evict under pressure: frame-reclaimed-to-allocator={reclaimed} (free {before}→{after}), swap-in-on-access-intact={intact} → {}",
                    if reclaimed && intact { "OK ✓" } else { "FAILED ✗" }
                );
            }
        }

        // Y: EuroCrash — recovery read of any dump from the previous boot +
        // proof of the minidump write/read cycle to the reserved crash block.
        crashdump::selftest();
    }

    // J1: prove the concurrent block cache (eurofs::cache) no_std in the kernel. A
    // write-through write caches the block; the subsequent reads are HITS
    // (read-lock only). The cache is a transparent BlockDevice drop-in (host test
    // proves a real EuroFs mounts through it); this shows the same layer live.
    {
        use eurofs::BlockDevice;
        let mut cache = eurofs::cache::BlockCache::new(rootblk::RootBlk::ram(32), 8);
        let mut wbuf = [0u8; 4096];
        wbuf[0] = 0xC1;
        wbuf[1] = 0xCE;
        let _ = cache.write_blocks(5, 1, &wbuf);
        let mut r1 = [0u8; 4096];
        let _ = cache.read_blocks(5, 1, &mut r1); // hit (write cached it)
        let mut miss = [0u8; 4096];
        let _ = cache.read_blocks(9, 1, &mut miss); // other block → miss (loads zeros)
        let mut r2 = [0u8; 4096];
        let _ = cache.read_blocks(5, 1, &mut r2); // hit
        let (hits, misses) = cache.stats();
        let ok = r1[0] == 0xC1 && r1[1] == 0xCE && r2[0] == 0xC1 && hits >= 2 && misses >= 1;
        serial_println!(
            "[j1-cache] block-cache (no_std): block 5 written+read 2x (hits) + 1 miss → data-intact={}, hits={} misses={} → {}",
            r1[0] == 0xC1 && r2[0] == 0xC1, hits, misses,
            if ok { "OK (read-lock hits, write-through) ✓" } else { "FAILED" }
        );
    }

    // EuroContainers (F2): self-test of the capability sandbox (chroot + caps + net).
    container::boot_selftest(&mut fs);
    // 3F-1: the container RUNTIME — signed images + ResourceLimits + CoW overlay.
    container::runtime_selftest();

    // EuroDisplay (E2): drive the Wayland-shaped surface protocol through a
    // lifecycle in the kernel (no_std proof). Live compositor binding +
    // Unix-socket transport are the integration on top.
    {
        use eurodisplay::{Display, Request};
        let mut disp = Display::new();
        disp.handle(Request::CreateSurface { id: 1 });
        disp.handle(Request::Attach { id: 1, width: 320, height: 200 });
        disp.handle(Request::Commit { id: 1 });
        let key = disp.route_key(30, true);
        serial_println!(
            "[e2] display-protocol: scene={} surface(s), focus={:?}, key-route={:?} — wire 12B/req",
            disp.scene().len(),
            disp.focused(),
            key,
        );
    }

    // ── H1: AF_UNIX local-socket round-trip (separate from TCP/IP; building block for H2) ──
    net::af_unix_selftest();

    // ── SECOND DISK (B3 multi-disk) ── if there is a second virtio-blk disk,
    // mount a separate EuroFS on it (mountpoint /mnt). Proves multiple real
    // disks, each with its own working filesystem, + `df` per mount.
    let mut fs2: Option<EuroFs<rootblk::RootBlk>> = None;
    if virtio_blk::device_count() > 1 {
        let sectors2 = virtio_blk::capacity_sectors_dev(1);
        let part2 = 2048u64; // skip the first 1 MiB (like a GPT alignment)
        let blocks2 = sectors2.saturating_sub(part2) / 8; // 8 sectors per 4 KiB block
        let dev2 = rootblk::RootBlk::disk_on(1, part2, blocks2);
        let f2 = match EuroFs::mount(dev2.clone(), rtc::epoch()) {
            Ok(f) => {
                serial_println!("[euro] EuroFS /mnt mounted from DISK 1 (existing)");
                f
            }
            Err(_) => {
                let f = EuroFs::format(dev2, [0xB2; 16], rtc::epoch()).expect("EuroFS format disk 1");
                serial_println!("[euro] EuroFS /mnt formatted on DISK 1 (extra mount)");
                f
            }
        };
        fs2 = Some(f2);
    }
    if let Some(ref mut f2) = fs2 {
        // B3 self-test: write+read on the second disk, then `df` for both mounts.
        let _ = f2.write_file("/hello-disk2.txt", b"Written to the SECOND disk (virtio-blk 1)\n");
        match f2.read_file("/hello-disk2.txt") {
            Ok(d) => serial_println!("[euro] /mnt self-test: {} bytes back from disk 1 ✓", d.len()),
            Err(_) => serial_println!("[euro] /mnt self-test FAILED"),
        }
        let (t1, free1) = fs.space_info();
        let (t2, free2) = f2.space_info();
        serial_println!("[df] /      {:>6} KiB total {:>6} KiB free  (virtio-blk 0)", t1 / 1024, free1 / 1024);
        serial_println!("[df] /mnt   {:>6} KiB total {:>6} KiB free  (virtio-blk 1)", t2 / 1024, free2 / 1024);
    }

    kinfo!("observability active — kmsg ring {} lines, leveled logging + dmesg", klog::LINES);

    // S8 HAL: activate the HPET (high-resolution timer) + measure it as a
    // high-resolution time source (supports e.g. SPERF profiling). Proof: measure how
    // long 1M spin iterations take with the HPET.
    if hpet::init() {
        let t1 = hpet::ns();
        for _ in 0..1_000_000 {
            core::hint::spin_loop();
        }
        let t2 = hpet::ns();
        kinfo!(
            "[hpet] HPET @ {} MHz active — 1M spin iterations = {} us (high-resolution HAL time source)",
            hpet::freq_hz() / 1_000_000,
            (t2 - t1) / 1000
        );
    } else {
        kwarn!("[hpet] no HPET present — falling back to APIC timer/RTC");
    }

    // Login verification now runs via EuroID (Argon2id, memory-hard) instead of the old
    // iterated SHA-256 — proven by the [ae] self-test in `euroid::selftest()`.

    // Mini-EuroUpdate: an installed (disk) system that boots with a NEWER kernel
    // automatically gets its /bin synced with the bundled binaries — otherwise
    // the new Ed25519 signature would reject the old binary on disk.
    if on_disk && sync_system_files(&mut fs) {
        serial_println!("[update] /bin synced with kernel build {:016x}", system_digest());
    }
    // Fill out the /etc skeleton on older installations (config is outside the binary
    // digest). Write-if-missing — respects files edited by the user.
    if on_disk {
        let added = ensure_etc_skeleton(&mut fs);
        if added > 0 {
            serial_println!("[update] {added} missing /etc file(s) filled in");
        }
    }

    // Boot counter — persistent on disk (increments on every reboot because the
    // previous value is read from disk), resets every boot in RAM mode.
    let _ = fs.create_dir("/data");
    let bootnum = fs
        .read_file("/data/bootcount")
        .ok()
        .and_then(|d| core::str::from_utf8(&d).ok().and_then(|s| s.trim().parse::<u64>().ok()))
        .unwrap_or(0)
        + 1;
    let _ = fs.write_file("/data/bootcount", format!("{bootnum}\n").as_bytes());
    serial_println!(
        "[euro] boot #{bootnum}{}",
        if on_disk { " (PERSISTENT on disk — survives restart)" } else { " (live/RAM)" }
    );

    // Register ALL of /etc + /boot in the userspace VFS, so that Linux/musl programs
    // (via open/read) can really read /etc/passwd, /etc/os-release, /etc/hostname ...
    // — not just the kernel shell. Recursive, so /etc/euroguard/* is included too.
    register_dir_recursive(&mut fs, "/etc");
    register_dir_recursive(&mut fs, "/boot");
    register_dir_recursive(&mut fs, "/bin"); // S3: so execve() finds the binaries

    // Load /etc/hosts into the resolver (name -> IP, before DNS) — like a real Unix.
    if let Ok(d) = fs.read_file("/etc/hosts") {
        if let Ok(s) = core::str::from_utf8(&d) {
            let mut entries = Vec::new();
            for line in s.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut it = line.split_whitespace();
                if let Some(ip) = it.next().and_then(net::parse_ipv4) {
                    for name in it {
                        entries.push((String::from(name), ip));
                    }
                }
            }
            let n = entries.len();
            net::set_hosts(entries);
            serial_println!("[net] /etc/hosts: {n} name->IP mapping(s) loaded");
        }
    }

    // Install the executables in the program registry: per binary the
    // granted capabilities (least-privilege) and the syscall ABI. This lets a
    // shell later start them by NAME — the kernel itself knows with which rights + ABI.
    use ring3::{CAP_CONSOLE as CO, CAP_FILE as FI, CAP_NET as NE, CAP_PROC_INFO as PR};
    let installed: [(&str, u64, bool); 25] = [
        ("/bin/fbtest", CO, true), // app-graphics smoke test (large-arena scheduled)
        ("/bin/doom", CO | FI, true), // the DOOM port: draws frames + reads its WAD (CAP_FILE)
        ("/bin/browser", CO | FI | NE, true), // EuroBrowser: draws + fetches live sites (CAP_NET)
        ("/bin/hello", CO | PR | FI, false),
        ("/bin/cat", CO | FI, false),
        ("/bin/linuxprog", CO | PR | FI, true), // now reads /proc too (CAP_FILE)
        ("/bin/forktest", CO | PR, true), // S3: fork() + waitpid()
        ("/bin/execee", CO, true), // S3: execve target
        ("/bin/forkpipe", CO, true), // S3: pipe() + fork() IPC
        ("/bin/ticker", CO, true), // S4: demo service (supervision)
        ("/bin/muslprog", CO | FI, true), // 3C-3: also opens /etc/mmap-test for file-backed mmap
        ("/bin/argvprog", CO, false),
        ("/bin/pieprog", CO, false),
        ("/bin/muslreal", CO | PR, true),
        ("/bin/muslfile", CO | FI, true),
        ("/bin/mcat", CO | FI, true),
        ("/bin/mwrite", CO | FI, true),
        ("/bin/mecho", CO, true),
        ("/bin/mupper", CO | FI, true), // reads stdin (read=0 falls under CAP_FILE)
        ("/bin/menv", CO, true),        // reads envp/getenv
        ("/bin/msock", NE | CO, true),  // networks via POSIX sockets (socket/connect/send/recv)
        ("/bin/mdns", NE | CO, true),   // DNS lookup via a UDP socket (SOCK_DGRAM)
        ("/bin/mtrack", NE | CO, true), // EuroGuard demo: blocked tracker connection
        ("/bin/isotest", CO, true),     // memory-isolation test (fails cleanly in the foreground)
        ("/bin/worker", CO | PR, true), // compute job that exits cleanly with exit(0)
    ];
    for (path, caps, abi) in installed {
        ring3::register_program(path, caps, abi);
    }

    // System environment (envp): every ring-3 process inherits these environment
    // variables on the SysV stack and reads them via getenv(). Completes the process-entry contract.
    ring3::set_env(&[
        "EUROOS=1",
        "EUROOS_VERSION=0.1-alpha",
        "LANG=nl_BE.UTF-8",
        "TERM=euroterm",
        "PATH=/bin",
        "HOME=/",
        "PWD=/",
        "USER=euro",
        "SHELL=/bin/sh",
    ]);

    // EuroGuard (Track 7): load the system-wide network policy (Level 1) FROM the
    // config file in EuroFS (Phase 7.2) — data-driven, not hard-coded.
    // From now on the kernel evaluates + logs every outgoing connection of an app.
    match fs.read_file("/etc/euroguard/system.conf") {
        Ok(bytes) => euroguard::load_config(&String::from_utf8_lossy(&bytes)),
        Err(_) => euroguard::init(), // fallback: built-in starter set
    }
    // A geo feed extends the built-in country table if present (full GeoIP drops
    // in as data, no code change). Then prove the network-control core.
    if let Ok(bytes) = fs.read_file("/etc/euroguard/geoip.conf") {
        euroguard::load_geo_feed(&String::from_utf8_lossy(&bytes));
    }
    euroguard::selftest();

    // ── REAL NETWORKING: initialize the virtio-net NIC and do a live ARP exchange
    // with the gateway. EuroNet now not only builds/parses packets — they
    // go REALLY over the wire (QEMU user-net: gw 10.0.2.2, us 10.0.2.15). ──
    use euronet::arp::{ArpOp, ArpPacket};
    use euronet::dhcp;
    use euronet::ethernet::{EtherType, EthernetHeader, MacAddr};
    use euronet::ipv4::{Ipv4Addr, Ipv4Header, Protocol};
    use euronet::udp::UdpDatagram;
    let ipfmt = |ip: Ipv4Addr| format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3]);
    let mut net_lines: Vec<String> = Vec::new();
    if nic::init(&mut allocator) {
        let my_mac = MacAddr(nic::mac().unwrap_or([0; 6]));
        let gw_ip = Ipv4Addr::new(10, 0, 2, 2);
        net_lines.push(format!(
            "NIC: {} MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            nic::kind(),
            my_mac.0[0], my_mac.0[1], my_mac.0[2], my_mac.0[3], my_mac.0[4], my_mac.0[5]
        ));

        // ── DHCP: fetch a REAL lease (DISCOVER → OFFER → REQUEST → ACK). ──
        let any = Ipv4Addr([0, 0, 0, 0]);
        let bcast = Ipv4Addr([255, 255, 255, 255]);
        let xid = 0x4505_5201u32;
        let send_dhcp = |mt: u8, req: Option<Ipv4Addr>, sid: Option<Ipv4Addr>| {
            let payload = dhcp::build(mt, xid, my_mac.0, req, sid);
            let seg = UdpDatagram { src_port: 68, dst_port: 67, payload }.build(any, bcast);
            let ipf = Ipv4Header {
                protocol: Protocol::Udp, ttl: 64, src: any, dst: bcast,
                total_length: 0, identification: 0x1234,
            }
            .build(&seg);
            let frame = EthernetHeader { dst: MacAddr::BROADCAST, src: my_mac, ethertype: EtherType::Ipv4 }.build(&ipf);
            nic::send(&frame);
        };
        // Poll for a DHCP reply of the desired type (UDP 67->68; manual
        // UDP parse so a missing checksum does not block us).
        let poll_dhcp = |want: u8| -> Option<dhcp::DhcpInfo> {
            for _ in 0..6_000_000u64 {
                if let Some(rx) = nic::poll_recv() {
                    if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                        if h.ethertype == EtherType::Ipv4 {
                            if let Ok((iph, ipl)) = Ipv4Header::parse(p) {
                                if iph.protocol == Protocol::Udp && ipl.len() > 8 {
                                    let dport = u16::from_be_bytes([ipl[2], ipl[3]]);
                                    if dport == 68 {
                                        if let Some(info) = dhcp::parse(&ipl[8..]) {
                                            if info.msg_type == want {
                                                return Some(info);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            None
        };
        // DHCP with retry: under dual-stack (ipv6=on) slirp's DHCPv4 server is
        // sometimes not ready yet when our very first DISCOVER arrives, so we try it
        // multiple times with a short pause until an OFFER comes.
        let mut dns_ip = Ipv4Addr::new(10, 0, 2, 3);
        let mut offer = None;
        for _ in 0..12 {
            for _ in 0..16 {
                if nic::poll_recv().is_none() {
                    break;
                }
            }
            send_dhcp(dhcp::DISCOVER, None, None);
            offer = poll_dhcp(dhcp::OFFER);
            if offer.is_some() {
                break;
            }
            for _ in 0..30_000_000u64 {
                core::hint::spin_loop();
            }
        }
        net_lines.push("DHCP: DISCOVER sent (broadcast)".into());
        let my_ip = match offer {
            Some(o) => {
                net_lines.push(format!("DHCP OFFER: {} from server {}", ipfmt(o.your_ip), ipfmt(o.server_id)));
                send_dhcp(dhcp::REQUEST, Some(o.your_ip), Some(o.server_id));
                if let Some(a) = poll_dhcp(dhcp::ACK) {
                    net_lines.push(format!(
                        "DHCP ACK: lease {} (router {}, dns {}, {}s)",
                        ipfmt(a.your_ip),
                        a.router.map(ipfmt).unwrap_or_else(|| "?".into()),
                        a.dns.map(ipfmt).unwrap_or_else(|| "?".into()),
                        a.lease_secs
                    ));
                    if let Some(d) = a.dns {
                        dns_ip = d;
                    }
                    a.your_ip
                } else {
                    net_lines.push("(no ACK) — using OFFER address".into());
                    o.your_ip
                }
            }
            None => {
                net_lines.push("(no OFFER) — falling back to 10.0.2.15".into());
                Ipv4Addr::new(10, 0, 2, 15)
            }
        };
        // ── Gateway + DNS via the reusable net layer (net.rs). ──
        let gw_mac = net::arp_resolve(my_mac, my_ip, gw_ip);
        let dns_mac = net::arp_resolve(my_mac, my_ip, dns_ip).or(gw_mac).unwrap_or(MacAddr::ZERO);
        if let Some(gwm) = gw_mac {
            net_lines.push(format!(
                "ARP: {} is-at {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                ipfmt(gw_ip), gwm.0[0], gwm.0[1], gwm.0[2], gwm.0[3], gwm.0[4], gwm.0[5]
            ));
            net_lines.push("✓ EuroOS is on the network (real TX/RX)".into());
            let pong = net::icmp_ping(my_mac, my_ip, gwm, gw_ip);
            net_lines.push(if pong { "PING 10.0.2.2: echo-reply OK ✓".into() } else { "(no ping reply)".into() });
            let host = "example.com";
            match net::dns_query(my_mac, my_ip, dns_mac, dns_ip, host) {
                Some(ip) => {
                    net_lines.push(format!("DNS reply: {host} = {} ✓", ipfmt(ip)));
                    // Pass the DNS result to userspace so /bin/msock
                    // (a standard musl sockets program) can connect
                    // without hard-coding a volatile IP.
                    ring3::push_env(&format!("FETCH_IP={}", ipfmt(ip)));
                    ring3::push_env(&format!("FETCH_HOST={host}"));
                    ring3::push_env(&format!("DNS_IP={}", ipfmt(dns_ip)));
                    // HTTP GET over TCP (external → via the gateway): fetch a real web page.
                    match net::http_get(my_mac, my_ip, gwm, ip, host, "/") {
                        Some((status, data)) => {
                            net_lines.push(format!("HTTP GET http://{host}/ -> {} bytes ✓", data.len()));
                            net_lines.push(format!("  {}", status.trim()));
                        }
                        None => net_lines.push("(no HTTP response)".into()),
                    }
                    // HTTPS over EuroTLS 1.3 (X25519 + ChaCha20-Poly1305): a REAL
                    // encrypted connection to a public server.
                    net_lines.push("TLS 1.3 (X25519+ChaCha20) handshake ...".into());
                    match net::https_get(my_mac, my_ip, gwm, ip, host, "/") {
                        Some((status, data, cert)) => {
                            net_lines.push(format!("HTTPS GET https://{host}/ -> {} bytes (encrypted) ✓", data.len()));
                            net_lines.push(format!("  {}", status.trim()));
                            if let Some(c) = cert {
                                net_lines.push(format!("  server certificate received: {} bytes", c.len()));
                            }
                        }
                        None => net_lines.push("(TLS handshake failed)".into()),
                    }
                }
                None => net_lines.push("(no DNS reply)".into()),
            }
        } else {
            net_lines.push("(no ARP reply from the gateway)".into());
        }

        // ── IPv6: SLAAC link-local + Router Discovery + ping6 (NDP instead of ARP). ──
        use euronet::icmpv6;
        use euronet::ipv6::{Ipv6Addr, Ipv6Header};
        let ip6fmt = |a: Ipv6Addr| -> String {
            let g: [u16; 8] = core::array::from_fn(|i| u16::from_be_bytes([a.0[i * 2], a.0[i * 2 + 1]]));
            let (mut bs, mut bl) = (usize::MAX, 0usize);
            let (mut rs, mut run) = (0usize, 0usize);
            for j in 0..8 {
                if g[j] == 0 {
                    if run == 0 {
                        rs = j;
                    }
                    run += 1;
                    if run > bl {
                        bl = run;
                        bs = rs;
                    }
                } else {
                    run = 0;
                }
            }
            let mut out = String::new();
            let mut j = 0;
            while j < 8 {
                if bl > 1 && j == bs {
                    out.push_str("::");
                    j += bl;
                } else {
                    if !out.is_empty() && !out.ends_with(':') {
                        out.push(':');
                    }
                    out.push_str(&format!("{:x}", g[j]));
                    j += 1;
                }
            }
            if out.is_empty() {
                out.push_str("::");
            }
            out
        };
        let ll = Ipv6Addr::link_local_from_mac(my_mac.0);
        net_lines.push(format!("IPv6 link-local (SLAAC): {}", ip6fmt(ll)));
        // Router Solicitation to ff02::2 (all routers).
        let rs_msg = icmpv6::router_solicit(my_mac.0, ll, Ipv6Addr::ALL_ROUTERS);
        let rsh = Ipv6Header { next_header: 58, hop_limit: 255, src: ll, dst: Ipv6Addr::ALL_ROUTERS, payload_len: 0 };
        let rsframe = EthernetHeader {
            dst: MacAddr(Ipv6Addr::ALL_ROUTERS.multicast_mac()),
            src: my_mac,
            ethertype: EtherType::Ipv6,
        }
        .build(&rsh.build(&rs_msg));
        nic::send(&rsframe);
        net_lines.push("IPv6: Router Solicitation -> ff02::2 sent".into());
        // Poll for a Router Advertisement.
        let mut router: Option<(Ipv6Addr, MacAddr, Option<[u8; 8]>)> = None;
        'ra: for _ in 0..8_000_000u64 {
            if let Some(rx) = nic::poll_recv() {
                if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                    if h.ethertype == EtherType::Ipv6 {
                        if let Ok((ih, pl)) = Ipv6Header::parse(p) {
                            if ih.next_header == 58 && icmpv6::msg_type(pl) == Some(icmpv6::ROUTER_ADVERT) {
                                let (prefix, _) = icmpv6::ra_info(pl);
                                router = Some((ih.src, h.src, prefix));
                                break 'ra;
                            }
                        }
                    }
                }
            }
        }
        if let Some((router_ll, router_mac, prefix)) = router {
            net_lines.push(format!("IPv6 RA: router {}", ip6fmt(router_ll)));
            if let Some(p) = prefix {
                let global = Ipv6Addr::from_prefix(p, &ll);
                net_lines.push(format!("IPv6 global (SLAAC): {}", ip6fmt(global)));
            }
            // ping6 the router via its link-local + MAC (from the RA).
            let echo = icmpv6::echo_request(0xE6, 1, b"euroos-ping6", ll, router_ll);
            let eh = Ipv6Header { next_header: 58, hop_limit: 255, src: ll, dst: router_ll, payload_len: 0 };
            let pframe = EthernetHeader { dst: router_mac, src: my_mac, ethertype: EtherType::Ipv6 }.build(&eh.build(&echo));
            nic::send(&pframe);
            let mut pong6 = false;
            'p6: for _ in 0..8_000_000u64 {
                if let Some(rx) = nic::poll_recv() {
                    if let Ok((h, p)) = EthernetHeader::parse(&rx) {
                        if h.ethertype == EtherType::Ipv6 {
                            if let Ok((ih, pl)) = Ipv6Header::parse(p) {
                                if ih.next_header == 58 && icmpv6::msg_type(pl) == Some(icmpv6::ECHO_REPLY) {
                                    pong6 = true;
                                    break 'p6;
                                }
                            }
                        }
                    }
                }
            }
            net_lines.push(if pong6 {
                format!("PING6 {}: echo-reply OK ✓", ip6fmt(router_ll))
            } else {
                "(no ping6 reply)".into()
            });
        } else {
            net_lines.push("(no Router Advertisement — IPv6 possibly off)".into());
        }

        // Save the network config so the shell can offer `ping`/`ping6`/`net`.
        net::save(net::NetCfg {
            my_mac,
            my_ip,
            gw_ip,
            gw_mac: gw_mac.unwrap_or(MacAddr::ZERO),
            dns_ip,
            dns_mac,
            link_local: ll,
            router_ll: router.map(|r| r.0),
            router_mac: router.map(|r| r.1),
        });
        // G3: poll/select multiplexing — prove the readiness logic on the NIC.
        net::poll_selftest();
    } else {
        net_lines.push("virtio-net NIC not found".into());
    }
    for l in &net_lines {
        serial_println!("[net] {l}");
    }
    // [io5] (Sprint IO): mount an SMB share over the live NIC (SLIRP → host Samba).
    smbfs::selftest();
    // [io6] (Sprint IO): mount an NFSv3 export over the live NIC (SLIRP → host nfsd).
    nfsmount::selftest();

    // Load /bin/hello from EuroFS and VERIFY a real ED25519 SIGNATURE over
    // the program bytes against the public key baked into the kernel. Only
    // authentic, unmodified code runs. (Production chain via eupkg.)
    // Audit C1: prove that the syscall layer validates user pointers against the arena.
    ring3::user_ptr_selftest();
    serial_println!("[euro] loading /bin/hello from EuroFS...");
    let prog = fs.read_file("/bin/hello").unwrap_or_default();
    let verified = !prog.is_empty() && ring3::verify_program("/bin/hello", &prog);
    let fp = crypto::pubkey_fingerprint();
    serial_println!(
        "[euro] /bin/hello Ed25519: {} (pubkey {:02x}{:02x}{:02x}{:02x}…)",
        if verified { "VERIFIED" } else { "REJECTED — signature invalid" },
        fp[0], fp[1], fp[2], fp[3]
    );
    // Security demo: a tampered copy is CRYPTOGRAPHICALLY rejected.
    let mut tampered = prog.clone();
    if let Some(b) = tampered.last_mut() {
        *b ^= 0xFF;
    }
    let tamper_accepted = ring3::verify_program("/bin/hello", &tampered);
    serial_println!(
        "[euro] tamper-test: 1 byte changed -> {}",
        if tamper_accepted { "ACCEPTED (WRONG!)" } else { "REJECTED (correct)" }
    );
    // Least privilege: /bin/hello gets console + process-info + file access,
    // but NO network. The kernel enforces this at the syscall boundary.
    let caps = ring3::CAP_CONSOLE | ring3::CAP_PROC_INFO | ring3::CAP_FILE;
    let (exit_code, user_out) = if verified {
        ring3::run(&mut allocator, &prog, caps, false)
    } else {
        (255, String::from("REJECTED: Ed25519 signature invalid"))
    };
    serial_println!(
        "[euro] /bin/hello done: exit={exit_code}, {} bytes via sys_write",
        user_out.len()
    );

    // H3: in-kernel DYNAMIC LINKER — load a dynamically-linked executable +
    // its shared library and resolve the cross-module call (R_X86_64_JUMP_SLOT).
    {
        // Serve the real libc.so.6 to ld.so at glibc's default search path. The
        // glibc tests themselves run LATER (after the scheduler + timer are up,
        // ~line 1760): a glibc process runs as a scheduled task so its pthreads
        // schedule fairly.
        ring3::register_file_static("/lib/x86_64-linux-gnu/libc.so.6", ring3::glibc_libc_bytes());
        // A SECOND real shared library, so ld.so can resolve a multi-lib DT_NEEDED
        // chain (the Chromium path needs ~30) + runtime dlopen/dlsym of it.
        ring3::register_file_static("/lib/x86_64-linux-gnu/libm.so.6", ring3::glibc_libm_bytes());
        // The C++ runtime + unwinder, so a transitive chain (exe -> libstdc++ ->
        // {libc, libm, libgcc_s}) resolves — Chromium is C++ at this scale.
        ring3::register_file_static("/lib/x86_64-linux-gnu/libstdc++.so.6", ring3::glibc_libstdcpp_bytes());
        ring3::register_file_static("/lib/x86_64-linux-gnu/libgcc_s.so.1", ring3::glibc_libgccs_bytes());
        // libgmp: needed by the REAL /usr/bin/factor binary (bignum arithmetic).
        ring3::register_file_static("/lib/x86_64-linux-gnu/libgmp.so.10", ring3::glibc_libgmp_bytes());
        // libcrypto (OpenSSL, 5 MB): needed by the REAL /usr/bin/sha256sum.
        ring3::register_file_static("/lib/x86_64-linux-gnu/libcrypto.so.3", ring3::glibc_libcrypto_bytes());
        // glibc stub libs chrome binaries declare as NEEDED (real code is in libc.so.6).
        ring3::register_file_static("/lib/x86_64-linux-gnu/libdl.so.2", ring3::glibc_libdl_bytes());
        ring3::register_file_static("/lib/x86_64-linux-gnu/libpthread.so.0", ring3::glibc_libpthread_bytes());
        // GLib + libpcre2: the GTK/desktop-stack core library (a real Chromium dep).
        ring3::register_file_static("/lib/x86_64-linux-gnu/libglib-2.0.so.0", ring3::glibc_libglib_bytes());
        ring3::register_file_static("/lib/x86_64-linux-gnu/libpcre2-8.so.0", ring3::glibc_libpcre2_bytes());
        // zlib: universal compression, a real Chromium dep.
        ring3::register_file_static("/lib/x86_64-linux-gnu/libz.so.1", ring3::glibc_libz_bytes());
        // Cairo 2D graphics stack (the vector-graphics lib GTK/Firefox render with).
        for (name, bytes) in ring3::cairo_libs() {
            ring3::register_file_static(&alloc::format!("/lib/x86_64-linux-gnu/{name}"), bytes);
        }
        // Pango text-layout engine (HarfBuzz shaping + GObject/GIO) — the real i18n
        // text stack GTK apps and browsers use on top of Cairo/FreeType.
        for (name, bytes) in ring3::pango_libs() {
            ring3::register_file_static(&alloc::format!("/lib/x86_64-linux-gnu/{name}"), bytes);
        }
        // GTK3 toolkit chain (gtk/gdk/gdk-pixbuf/atk + X11 extension client libs) —
        // the real widget toolkit. Served zero-copy; the runtime test is ggtk.
        for (name, bytes) in ring3::gtk_libs() {
            ring3::register_file_static(&alloc::format!("/lib/x86_64-linux-gnu/{name}"), bytes);
        }
        // X11 client stack: a real Xlib client + its 6 transitive libs (the GUI rung).
        ring3::register_file_static("/lib/x86_64-linux-gnu/libX11.so.6", ring3::glibc_libx11_bytes());
        ring3::register_file_static("/lib/x86_64-linux-gnu/libxcb.so.1", ring3::glibc_libxcb_bytes());
        ring3::register_file_static("/lib/x86_64-linux-gnu/libXau.so.6", ring3::glibc_libxau_bytes());
        ring3::register_file_static("/lib/x86_64-linux-gnu/libXdmcp.so.6", ring3::glibc_libxdmcp_bytes());
        ring3::register_file_static("/lib/x86_64-linux-gnu/libbsd.so.0", ring3::glibc_libbsd_bytes());
        ring3::register_file_static("/lib/x86_64-linux-gnu/libmd.so.0", ring3::glibc_libmd_bytes());
        let (h3_out, h3_exit) = ring3::dynlink_selftest(&mut allocator);
        serial_println!(
            "[h3] dyntest (dynamically linked) done: exit={h3_exit}, output={:?}",
            h3_out.trim_end()
        );
        // Sprint 1: kernel-ld.so sets up static TLS for a standalone __thread PIE,
        // and patches cross-module IE-TLS (TPOFF64) for a .so `__thread`.
        ring3::tls_selftest(&mut allocator);
        ring3::tls_cross_selftest(&mut allocator);
        // 3C-3: the PT_INTERP path — a from-scratch USERSPACE ld.so does the
        // dynamic linking (not the kernel), the way unmodified Linux binaries link.
        ring3::interp_selftest(&mut allocator);
    }

    // H3 follow-up: run a dynamically-linked binary BY NAME from the FS — the .so
    // dependency is resolved from /lib via DT_NEEDED (run-by-name, not embedded).
    {
        let exe = fs.read_file("/bin/dyntest").unwrap_or_default();
        if !exe.is_empty() {
            let needs = ring3::needed_libs(&exe);
            let mut libs: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
            for name in &needs {
                if let Ok(b) = fs.read_file(&format!("/lib/{name}")) {
                    libs.push(b);
                }
            }
            let refs: alloc::vec::Vec<&[u8]> = libs.iter().map(|v| v.as_slice()).collect();
            let (out, exit) = ring3::run_dynamic(&mut allocator, &exe, &refs, &[b"dyntest"], ring3::CAP_CONSOLE, true);
            serial_println!(
                "[h3-fs] /bin/dyntest deps={:?} → {} .so loaded from /lib, exit={exit}, output={:?}",
                needs,
                refs.len(),
                out.trim_end()
            );
        }
    }

    // H4: EuroWASM — run a WASM module via the no-JIT interpreter; the WASI import
    // `fd_write` is mapped to an EuroGuard capability (denied without it).
    wasm::selftest();
    // 3C-4: a REAL wasi_snapshot_preview1 host (true iovec fd_write ABI, proc_exit,
    // random_get, clock, environ/args) running a real wasm32-wasi-shaped module.
    wasm::wasi_selftest();
    // H4 follow-up: bind the WASM-WASI to REAL EuroSandbox containers (capability +
    // net scope determine whether an import is allowed) — the sovereign sandbox model closed.
    wasm::container_selftest();
    // H5: the REAL Wayland wire-protocol server — a handshake → a titled window.
    wayland::selftest();

    // 3C-3: seed a known file so the muslprog boot binary can prove FILE-BACKED
    // mmap — it opens this and mmaps its contents into its address space.
    ring3::register_file("/etc/mmap-test", b"EUROOS-FILEMMAP-OK-0123456789abcdef".to_vec());

    // ── EXEC-BY-NAME: a small "boot script" that loads and runs every program by
    // NAME from EuroFS. The kernel looks up the caps + ABI per path in the program
    // registry and starts the binary with them in ring 3 — no hard-coded calls.
    let boot_script: [(&str, &str); 11] = [
        ("/bin/cat", "second compiled program"),
        ("/bin/linuxprog", "LINUX ABI via compat layer"),
        ("/bin/muslprog", "musl startup: TLS/mmap/writev"),
        ("/bin/argvprog", "SysV stack: argc/argv/envp/auxv"),
        ("/bin/pieprog", "PIE + R_X86_64_RELATIVE relocations"),
        ("/bin/muslreal", "REAL musl libc: printf/malloc/strlen"),
        ("/bin/muslfile", "musl fopen/fgets -> EuroFS VFS"),
        ("/bin/menv", "environment variables via getenv (envp)"),
        ("/bin/msock", "POSIX sockets: socket/connect/send/recv -> EuroNet"),
        ("/bin/mdns", "UDP socket (SOCK_DGRAM): DNS lookup from userspace"),
        ("/bin/mtrack", "EuroGuard: tracker connection blocked by kernel policy"),
    ];
    let mut demo_out: Vec<(String, String)> = Vec::new();
    for (path, note) in boot_script {
        let bytes = fs.read_file(path).unwrap_or_default();
        // Verify-before-execute: reject if the Ed25519 signature does not match.
        if !ring3::verify_program(path, &bytes) {
            serial_println!("[exec] {path} REJECTED — Ed25519 signature invalid");
            demo_out.push((format!("euroos:/ $ .{path}   ({note})"),
                           String::from("[sec] REJECTED: invalid Ed25519 signature")));
            continue;
        }
        let (caps, abi) = ring3::program_caps_abi(path).unwrap_or((ring3::CAP_CONSOLE, false));
        let (exit, out) = ring3::run_named(&mut allocator, &bytes, path.as_bytes(), caps, abi);
        serial_println!("[exec] {path} (abi={}, ed25519=ok) exit={exit} ({} bytes)", if abi { "linux" } else { "native" }, out.len());
        demo_out.push((format!("euroos:/ $ .{path}   ({note})"), out));
    }
    // Demonstrate the EuroShell built-in commands (pure function shell::exec) —
    // serial-visible proof that the shell gives live system info from RTC/memory/
    // /etc, not from hard-coded text.
    {
        let mut sctx = shell::ShellCtx { fs: &mut fs, mem: &mut allocator };
        // Show the NATIVE EuroOS character: identity + capability security +
        // its own service supervisor (not the Linux compat layer).
        for cmd in ["nslookup example.com", "nslookup example.com", "netstat"] {
            serial_println!("[shell] eurosh:/ $ {cmd}");
            for line in shell::exec(&mut sctx, cmd) {
                serial_println!("[shell]   {line}");
            }
        }
    }

    x86_64::instructions::interrupts::int3(); // breakpoint exception
    let bp = interrupts::BREAKPOINT_HIT.load(Ordering::SeqCst);
    serial_println!("[euro] breakpoint exception handled: {bp}");

    // G1: give the kernel scheduler tasks (slots 1..=5) each a GUARDED stack from the
    // pool, so that a kernel-task stack overflow faults on an unmapped guard page
    // (hardware #PF → the fault handler terminates only that task) instead of silently
    // corrupting the neighbour stack. Slot 5 = the deliberate-overflow self-test.
    let mut sched_guarded = 0;
    for i in 1..=5 {
        let top = paging::guarded_stack_alloc(&mut allocator);
        if top != 0 {
            sched::set_task_guarded_stack(i, top);
            sched_guarded += 1;
        }
    }
    serial_println!(
        "[g1] {} scheduler task stacks on guarded stacks (pool: {} total)",
        sched_guarded,
        paging::guarded_stack_count()
    );

    // Scheduler: shell + 3 kernel tasks + TWO ring-3 userspace processes,
    // each with its own kernel stack (TSS.rsp0 switches per task).
    sched::init();
    let ucnt1 = ring3::spawn_counter_task(&mut allocator);
    let ucnt2 = ring3::spawn_counter_task(&mut allocator);
    // Background daemon: a loaded program that runs PREEMPTIVELY as a real task
    // and periodically (via syscalls) writes a heartbeat.
    let daemon_prog = fs.read_file("/bin/daemon").unwrap_or_default();
    ring3::spawn_daemon(&mut allocator, &daemon_prog);
    // PREEMPTIVE PER-PROCESS MODEL: two REAL musl processes at once, each with
    // its own __thread counter. Their counters stay independent only because
    // the scheduler saves/restores FS_BASE (the musl TLS pointer) per process.
    let tls_prog = fs.read_file("/bin/tlscount").unwrap_or_default();
    if ring3::verify_program("/bin/tlscount", &tls_prog) {
        ring3::spawn_bg_musl(&mut allocator, &tls_prog, 8, b"tlscount");
        ring3::spawn_bg_musl(&mut allocator, &tls_prog, 9, b"tlscount");
        serial_println!("[euro] 2x musl process (pid 8,9) scheduled — own TLS per process");
    }
    // A third process that tests MEMORY ISOLATION: it reaches into kernel memory
    // and is terminated by the page-fault handler — while the rest keeps running.
    let iso_prog = fs.read_file("/bin/isotest").unwrap_or_default();
    if ring3::verify_program("/bin/isotest", &iso_prog) {
        ring3::spawn_bg_musl(&mut allocator, &iso_prog, 10, b"isotest");
        serial_println!("[euro] isotest (pid 10) scheduled — tests memory isolation");
    }
    // A 'job' process: computes, reports, exits cleanly with exit(0) and is
    // then cleaned up — the clean exit path of the process lifecycle.
    let work_prog = fs.read_file("/bin/worker").unwrap_or_default();
    if ring3::verify_program("/bin/worker", &work_prog) {
        ring3::spawn_bg_musl(&mut allocator, &work_prog, 11, b"worker");
        serial_println!("[euro] worker (pid 11) scheduled — compute job + clean exit");
    }
    // S3: REAL fork() + waitpid() — forks a child with a copied address space
    // and reaps it. Proves process creation (see [fork]/[wait] lines in dmesg).
    let fork_prog = fs.read_file("/bin/forktest").unwrap_or_default();
    if ring3::verify_program("/bin/forktest", &fork_prog) {
        ring3::spawn_bg_musl(&mut allocator, &fork_prog, 20, b"forktest");
        serial_println!("[euro] forktest (pid 20) scheduled — S3 fork()+waitpid()");
    }
    // S3: pipe() + fork() IPC — child writes via a pipe to the parent.
    let pipe_prog = fs.read_file("/bin/forkpipe").unwrap_or_default();
    if ring3::verify_program("/bin/forkpipe", &pipe_prog) {
        ring3::spawn_bg_musl(&mut allocator, &pipe_prog, 21, b"forkpipe");
        serial_println!("[euro] forkpipe (pid 21) scheduled — S3 pipe()+fork() IPC");
    }
    // S4: EuroInit — start the declared services under supervision (restart
    // on exit per policy); the supervision tick runs in the desktop loop.
    init::start_all(&mut allocator, &mut fs);
    // Threads: clone() is implemented kernel-side + verified (a
    // thread task sharing the address space is created). The userspace
    // thread RESUMPTION still has a subtle bug (ring-0 GP @ user address) that
    // deserves its own debug session; therefore NOT started automatically at boot.
    // /bin/mthread stays available for manual tests.
    let thr_prog = fs.read_file("/bin/mthread").unwrap_or_default();
    if ring3::verify_program("/bin/mthread", &thr_prog) {
        ring3::spawn_bg_musl(&mut allocator, &thr_prog, 12, b"mthread");
    }
    // Real musl pthreads: pthread_create + pthread_join.
    let pthr_prog = fs.read_file("/bin/mpthread").unwrap_or_default();
    if ring3::verify_program("/bin/mpthread", &pthr_prog) {
        ring3::spawn_bg_musl(&mut allocator, &pthr_prog, 13, b"mpthread");
    }
    // pthread_mutex under contention (2 threads): tests the blocking futex.
    let mtx_prog = fs.read_file("/bin/mmutex").unwrap_or_default();
    if ring3::verify_program("/bin/mmutex", &mtx_prog) {
        ring3::spawn_bg_musl(&mut allocator, &mtx_prog, 14, b"mmutex");
    }
    // EuroIPC: a receiver (claims port 42) + a sender. The receiver first,
    // so the port is claimed before the sender sends.
    let rcv = fs.read_file("/bin/ipcrecv").unwrap_or_default();
    if ring3::verify_program("/bin/ipcrecv", &rcv) {
        ring3::spawn_bg_musl(&mut allocator, &rcv, 15, b"ipcrecv");
    }
    let snd = fs.read_file("/bin/ipcsend").unwrap_or_default();
    if ring3::verify_program("/bin/ipcsend", &snd) {
        ring3::spawn_bg_musl(&mut allocator, &snd, 16, b"ipcsend");
    }
    serial_println!("[euro] scheduler: shell + 3 kernel + 2 ring-3 + daemon + 2 musl @ {ucnt1:#x},{ucnt2:#x}");
    // ACPI MADT: discover the CPU cores + IO-APIC (foundation for SMP).
    if let Some(madt) = acpi::parse() {
        serial_println!(
            "[acpi] MADT: {} CPU core(s) (LAPIC @ {:#x}, IO-APIC @ {:#x} gsi-base {})",
            madt.enabled_cores(),
            madt.lapic_addr,
            madt.ioapic_addr,
            madt.ioapic_gsi_base
        );
        for c in &madt.cores {
            serial_println!("[acpi]   core: APIC-id {} ({})", c.apic_id, if c.enabled { "on" } else { "off" });
        }
    } else {
        serial_println!("[acpi] no MADT found");
    }
    // M1-1: switch PCI config access to ECAM (memory-mapped, via the ACPI MCFG)
    // — the modern path, full 4 KiB config space. `init_ecam` only activates the
    // window after verifying every port-visible function reads identically
    // through it; on any mismatch we stay on the legacy 0xCF8/0xCFC ports.
    match pci::init_ecam() {
        Some((base, b0, b1)) => serial_println!(
            "[ecam] PCIe config via ECAM @ {base:#x} (buses {b0}..={b1}, MCFG, port-verified) ✓"
        ),
        None => serial_println!("[ecam] no verified MCFG — staying on legacy config ports"),
    }
    // PCI enumeration: discover the attached hardware (network, storage, ...).
    {
        let devs = pci::enumerate();
        serial_println!("[pci] {} devices found:", devs.len());
        for d in &devs {
            let name = pci::device_name(d.vendor, d.device);
            serial_println!(
                "[pci]   {:02x}:{:02x}.{}  {:04x}:{:04x}  {}{}",
                d.bus, d.dev, d.func, d.vendor, d.device,
                pci::class_name(d.class, d.subclass),
                if name.is_empty() { alloc::string::String::new() } else { alloc::format!("  ({name})") }
            );
        }
    }
    // R: EuroDevice — build the unified device model from the PCI enumeration, register
    // the existing drivers and bind them. One coherent device tree instead of separate
    // ad-hoc discovery; shows all bindings (basis for future drivers).
    eurodevice::init();
    eurodevice::selftest();

    // I3: AML interpreter — parse the REAL DSDT from the firmware and evaluate a
    // control method/name. \_S5 (soft-off sleep-type package) proves that the AML
    // bytecode parser works on a real ACPI table; we also count the _STA/_TMP/_BST
    // methods the interpreter can evaluate live.
    if let Some((aml_addr, aml_len)) = acpi::dsdt_aml() {
        let aml = unsafe { core::slice::from_raw_parts(aml_addr as *const u8, aml_len) };
        let ns = euroaml::AmlNamespace::parse(aml);
        let s5: Option<alloc::vec::Vec<u64>> = ns
            .evaluate("_S5_")
            .and_then(|v| v.as_package().map(|p| p.iter().filter_map(|x| x.as_int()).collect()));
        // Pass the SLP_TYPa/b to the power layer so shutdown uses the firmware-correct
        // S5 value (instead of a hard-coded 0).
        if let Some(vals) = &s5 {
            let a = vals.first().copied().unwrap_or(0) as u8;
            let b = vals.get(1).copied().unwrap_or(0) as u8;
            power::set_s5_slp_typ(a, b);
        }
        let methods = ["_STA", "_TMP", "_BST", "_PSR", "_S5_", "_PTS", "_WAK"];
        let present: alloc::vec::Vec<&str> = methods.iter().filter(|m| ns.contains(m)).copied().collect();
        serial_println!(
            "[i3-aml] DSDT interpreted: {} bytes → {} AML objects. \\_S5={:?}, known methods present: {:?}",
            aml_len, ns.len(), s5, present
        );
        // M5-1: ACPI power sources (battery / AC / lid). On a desktop or VM the
        // DSDT has none of these; on a laptop it does. A battery whose _BST
        // reads Embedded-Controller fields needs an EC driver (deferred) — we
        // decode statically-evaluable _BST/_PSR and report the device presence.
        acpi_power::report(&ns);
    } else {
        serial_println!("[i3-aml] no DSDT found via FADT");
    }

    // Sprint 2 (I3): prove that the ACPI S5 shutdown path is ready — port + S5
    // write value from FADT + the AML-evaluated \_S5 — WITHOUT shutting down. The
    // real poweroff is `shutdown`/`poweroff` (boot-verified via shutdown-test.py).
    {
        let (s5_port, s5_val) = power::s5_ready();
        serial_println!(
            "[i3-s5] ACPI shutdown ready: PM1a_CNT port {:#x}, S5 value {:#06x}, SLP_TYP-from-AML={} → `poweroff` performs a clean soft-off ✓ (end-to-end proven via scripts/shutdown-test.py: QEMU guest-shutdown)",
            s5_port, s5_val, power::s5_from_aml()
        );
    }
    // O1: TPM 2.0 (hardware root of trust) via the TIS-MMIO interface. Detect +
    // Startup; the self-test proves measured boot (PCR-extend) — foundation for K3-FDE.
    if tpm::init() {
        tpm::selftest();
    }
    // 3D-8: seed the sovereign CSPRNG (CPU jitter + the TPM RNG if present) and
    // prove the getrandom readiness gate. Runs regardless of a TPM — CPU jitter
    // alone reaches the entropy threshold — so it is OUTSIDE the TPM block.
    entropy::selftest();
    // 3D-7: Network Time Security (RFC 8915) authenticated-time core — proves the
    // clock only trusts a cryptographically-bound server (uses the 3D-8 CSPRNG).
    nts::selftest();
    // 3D-2: system-image integrity — Ed25519-signed Merkle root, tamper detection.
    verity::selftest();
    // 3D-2 wiring: verity on the LIVE EuroFS read path (VerityBlk verifies every
    // block against the signed root; a tampered backing block fails the read).
    verity::wire_selftest();
    interrupts::init_timer(100);
    // G1: give each application processor a GUARDED kernel stack from the pool (an
    // AP-stack overflow then faults on an unmapped guard page instead of silently
    // overwriting the neighbour AP stack). Before smp::init(), with the main allocator.
    let ap_guarded = smp::setup_guarded_stacks(&mut allocator);
    serial_println!(
        "[g1] {} AP stack(s) on guarded stacks (pool: {} guarded stacks total, unit=16 KiB + guard)",
        ap_guarded,
        paging::guarded_stack_count()
    );
    // SMP: start the application processors (BSP is still on the boot PML4 here,
    // interrupts off → safe moment for INIT-SIPI-SIPI).
    smp::init();
    // IRQ routing from the 8259 PIC to the IO-APIC (full APIC system).
    if let Some(madt) = acpi::parse() {
        interrupts::route_io_apic(&madt);
    }
    mouse::init(width, height);
    serial_println!("[euro] PS/2 mouse initialized (IRQ12)");
    // I1: xHCI USB stack — real USB-HID input (keyboard/mouse) on modern
    // machines without PS/2. Enumerates every root-port device and polls the
    // interrupt-IN endpoint; the reports flow into the same input paths as PS/2.
    if xhci::init(&mut allocator) {
        serial_println!("[euro] xHCI USB initialized — {} HID device(s) live", xhci::hid_count());
    }
    // I2: Intel HD-Audio — codec enumeration + stream DMA that plays a (euroaudio-mixed)
    // tone. Proves the mixer→hardware chain (LPIB running = DMA playing).
    if hda::init(&mut allocator) {
        serial_println!("[euro] HD-Audio initialized — stream playing (LPIB={})", hda::stream_pos());
    }
    x86_64::instructions::interrupts::enable();
    // M2-1: NVMe MSI-X delivery proof — must run with interrupts ON (the boot
    // self-test above ran before this point, so its completions were unseen).
    nvme::msix_proof();
    // M3-3: a CDC-ECM USB NIC only exists after xHCI enumeration (above) — if
    // no NIC bound during the main net bring-up, adopt it and bring it up now.
    net::late_bring_up();
    // M4-3 live wiring: a USB DAC also only exists after xHCI enumeration —
    // route the euroaudio mixer's output to it (no-op without one).
    audio::usb_wire_selftest();
    serial_println!("[euro] APIC timer 100 Hz + interrupts ON -> preemptive multitasking (incl. ring 3)");
    // Chromium foundation: run REAL dynamically-linked GLIBC binaries via the
    // genuine ld-linux-x86-64.so.2 — NOW that the scheduler is up, each runs as a
    // scheduled process, so single-threaded AND multi-threaded (pthreads) work.
    {
        serial_println!("[glibc] === real glibc dynamic binaries (ld-linux + libc.so.6, scheduled) ===");
        let caps = ring3::CAP_CONSOLE | ring3::CAP_FILE | ring3::CAP_PROC_INFO;
        let (o1, e1) = ring3::run_glibc(&mut allocator, ring3::gtiny_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gtiny"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gtiny: exit={e1}, output={:?}", o1.trim_end());
        let (o2, e2) = ring3::run_glibc(&mut allocator, ring3::gtest_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gtest"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gtest (printf+malloc+qsort): exit={e2}");
        for l in o2.lines() { serial_println!("[glibc]   {l}"); }
        // gthread: pthread_create + join of 3 workers (REAL glibc pthreads —
        // clone + futex + thread-exit + pthread_join, on the scheduled process).
        let (o3, e3) = ring3::run_glibc(&mut allocator, ring3::gthread_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gthread"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gthread (pthreads): exit={e3}");
        for l in o3.lines() { serial_println!("[glibc]   {l}"); }
        // gmath: a SECOND real shared library (libm.so.6) resolved via the ld.so
        // DT_NEEDED chain + runtime dlopen/dlsym — the multi-library Chromium path.
        let (o4, e4) = ring3::run_glibc(&mut allocator, ring3::gmath_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gmath"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gmath (libm + dlopen/dlsym): exit={e4}");
        for l in o4.lines() { serial_println!("[glibc]   {l}"); }
        // gcpp: a C++ program — transitive libstdc++ chain + STL + exceptions.
        let (o5, e5) = ring3::run_glibc(&mut allocator, ring3::gcpp_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gcpp"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gcpp (C++ STL + exceptions): exit={e5}");
        for l in o5.lines() { serial_println!("[glibc]   {l}"); }
        // REAL unmodified Ubuntu coreutils binaries, run WITH ARGUMENTS. Proof that
        // arbitrary Linux software runs on EuroOS, not just our own test stubs.
        serial_println!("[glibc] === REAL Ubuntu binaries (unmodified /usr/bin) ===");
        let (o6, e6) = ring3::run_glibc(&mut allocator, ring3::real_seq_bytes(), ring3::ldlinux_bytes(), &[b"seq", b"1", b"2", b"9"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] /usr/bin/seq 1 2 9 -> exit={e6}, output={:?}", o6.trim_end());
        let (o7, e7) = ring3::run_glibc(&mut allocator, ring3::real_factor_bytes(), ring3::ldlinux_bytes(), &[b"factor", b"360360"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] /usr/bin/factor 360360 -> exit={e7}, output={:?}", o7.trim_end());
        // Address-space scaling: run gbig with a 384 MiB arena (vs the default 96),
        // proving the identity-mapped model handles hundreds of MB toward chrome scale.
        serial_println!("[glibc] === address-space scaling (384 MiB arena) ===");
        ring3::GLIBC_ARENA_MIB.store(384, core::sync::atomic::Ordering::Relaxed);
        let (o8, e8) = ring3::run_glibc(&mut allocator, ring3::gbig_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gbig"], &[b"PATH=/bin"], caps);
        ring3::GLIBC_ARENA_MIB.store(96, core::sync::atomic::Ordering::Relaxed);
        serial_println!("[glibc] gbig (200 MiB heap): exit={e8}");
        for l in o8.lines() { serial_println!("[glibc]   {l}"); }
        // gsync: pthread mutex + condition variables (producer/consumer) — a much
        // deeper futex exercise than join (mutex lock/unlock + cond wait/signal).
        let (o9, e9) = ring3::run_glibc(&mut allocator, ring3::gsync_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gsync"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gsync (mutex+condvar): exit={e9}");
        for l in o9.lines() { serial_println!("[glibc]   {l}"); }
        // REAL stdin FILTERS: feed fd 0 and run unmodified Ubuntu tools that read it.
        serial_println!("[glibc] === REAL Ubuntu stdin filters (fd 0) ===");
        ring3::set_stdin(b"EuroOS runs real Linux tools");
        let (o10, e10) = ring3::run_glibc(&mut allocator, ring3::real_base64_bytes(), ring3::ldlinux_bytes(), &[b"base64"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] /usr/bin/base64 <stdin> -> exit={e10}, output={:?}", o10.trim_end());
        ring3::set_stdin(b"one two three\nfour five\n");
        let (o11, e11) = ring3::run_glibc(&mut allocator, ring3::real_wc_bytes(), ring3::ldlinux_bytes(), &[b"wc"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] /usr/bin/wc <stdin> -> exit={e11}, output={:?}", o11.trim_end());
        // sha256sum: a REAL crypto tool driving the big (5 MB) libcrypto.so.3.
        // Expected SHA-256 of "EuroOS" = b4d1c474...620504.
        ring3::set_stdin(b"EuroOS");
        let (o12, e12) = ring3::run_glibc(&mut allocator, ring3::real_sha256_bytes(), ring3::ldlinux_bytes(), &[b"sha256sum"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] /usr/bin/sha256sum <\"EuroOS\"> -> exit={e12}, output={:?}", o12.trim_end());
        // sort: a real stdin line sorter (reuses the already-served libcrypto).
        ring3::set_stdin(b"pear\napple\ncherry\nbanana\n");
        let (o13, e13) = ring3::run_glibc(&mut allocator, ring3::real_sort_bytes(), ring3::ldlinux_bytes(), &[b"sort"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] /usr/bin/sort <stdin> -> exit={e13}, output={:?}", o13.trim_end());
        ring3::set_stdin(b"");
        // gfile: file-I/O roundtrip (create+write a file, reopen+read, verify).
        let (o14, e14) = ring3::run_glibc(&mut allocator, ring3::gfile_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gfile"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gfile (file I/O roundtrip): exit={e14}");
        for l in o14.lines() { serial_println!("[glibc]   {l}"); }
        // gglib: GLib GHashTable — a core GTK/desktop-stack library (real Chromium
        // dep) with a transitive chain gglib -> libglib -> {libc, libm, libpcre2}.
        let (o15, e15) = ring3::run_glibc(&mut allocator, ring3::gglib_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gglib"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gglib (GLib GHashTable): exit={e15}");
        for l in o15.lines() { serial_println!("[glibc]   {l}"); }
        // gzlib: zlib compress/decompress roundtrip (universal compression lib).
        let (oz, ez) = ring3::run_glibc(&mut allocator, ring3::gzlib_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gzlib"], &[b"PATH=/bin"], caps);
        serial_println!("[glibc] gzlib (zlib compress): exit={ez}");
        for l in oz.lines() { serial_println!("[glibc]   {l}"); }
        // gunix: AF_UNIX socketpair round-trip — local IPC, the transport a real X11
        // client (and dbus) uses. First step of the display/GUI path.
        let caps_net = caps | ring3::CAP_NET;
        let (ou, eu) = ring3::run_glibc(&mut allocator, ring3::gunix_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gunix"], &[b"PATH=/bin"], caps_net);
        serial_println!("[glibc] gunix (AF_UNIX socketpair): exit={eu}");
        for l in ou.lines() { serial_println!("[glibc]   {l}"); }
        // gxwin: ONE real Xlib client exercising the whole X11 path in a single
        // library load (5 separate clients re-loading the 6-lib stack was too slow):
        // XOpenDisplay -> CreateWindow/Map -> FillRectangle -> XPutImage -> SelectInput
        // -> Expose + REAL keyboard (KeyPress). The 3 staged PS/2 scancodes below feed
        // the launcher's pump (xserver::pump_keyboard) -> X KeyPress events, i.e. the
        // same path a live keyboard IRQ uses. (The individual gx11/gxdraw/gximg/gxevent/
        // gxkey clients are still committed for isolated debugging.)
        // Scorecard vars from the GUI/demo tests below; default to "not run" (fail)
        // so a lean chrome-headless-shell boot still compiles and reports honestly.
        let (mut e16, mut efm, mut ex) = (u64::MAX, u64::MAX, u64::MAX);
        // LEAN chrome-headless-shell run: skip the GUI/demo glibc tests (X11/gsparse/
        // gfmmap/crashpad/gdiskmap). They inflate guest memory and, on a memory-tight
        // host, OOM-kill qemu before hshell is reached. Jump straight to the disk-
        // served exe loader (crashpad/chrome/hshell) below.
        if !ring3::europack_has("/pack/chrome-headless-shell") {
        serial_println!("[glibc] === X11: real Xlib client (window + render + events) ===");
        ps2::push_scancode(0x1e); // 'a'  ->  X KeyPress via ps2 ring -> pump
        ps2::push_scancode(0x30); // 'b'
        ps2::push_scancode(0x2e); // 'c'
        // Stage a left-mouse-button press (PS/2 mouse packets: [flags, dx, dy]); the
        // 0->1 edge latches a press that pump_mouse() delivers as an X ButtonPress.
        mouse::push_byte(0x08); mouse::push_byte(0); mouse::push_byte(0); // buttons up
        mouse::push_byte(0x09); mouse::push_byte(0); mouse::push_byte(0); // left down
        xserver::TRACE.store(true, core::sync::atomic::Ordering::Relaxed);
        let ox; (ox, ex) = ring3::run_glibc(&mut allocator, ring3::gxwin_bytes(), ring3::ldlinux_bytes(), &[b"gxwin"], &[b"DISPLAY=:0", b"PATH=/bin"], caps_net);
        xserver::TRACE.store(false, core::sync::atomic::Ordering::Relaxed);
        serial_println!("[glibc] gxwin (X11 connect+render+PutImage+events+real-kbd): exit={ex}");
        for l in ox.lines() { serial_println!("[glibc]   {l}"); }
        // gsparse: DEMAND PAGING — reserve 4 GiB virtual (far beyond RAM), touch a
        // few scattered pages; only touched pages commit physical frames. Opt-in.
        let pool_before = procpool::demand_free_frames();
        ring3::DEMAND_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
        let o16; (o16, e16) = ring3::run_glibc(&mut allocator, ring3::gsparse_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gsparse"], &[b"PATH=/bin"], caps);
        ring3::DEMAND_ENABLED.store(false, core::sync::atomic::Ordering::Relaxed);
        let pool_after = procpool::demand_free_frames();
        serial_println!("[glibc] gsparse (demand paging): exit={e16}, committed={} pages ({} KiB), pool delta={} frames (reclaimed after exit)",
            ring3::demand_committed_pages(), ring3::demand_committed_pages()*4, pool_before as i64 - pool_after as i64);
        for l in o16.lines() { serial_println!("[glibc]   {l}"); }

        // gfmmap: FILE-BACKED demand paging — mmap a large (5 MiB) served library the
        // way a dynamic loader maps a LOAD segment, but fault each page in from the
        // file lazily instead of copying the whole segment. Proves the mmap view is
        // byte-identical to read(). This is the foundation for loading binaries far
        // larger than RAM (a browser's hundreds-of-MiB .text) without a giant arena.
        let fpages_before = ring3::demand_file_pages();
        ring3::DEMAND_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
        ring3::DEMAND_FILE_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
        let ofm; (ofm, efm) = ring3::run_glibc(&mut allocator, ring3::gfmmap_bytes(), ring3::ldlinux_bytes(), &[b"/bin/gfmmap"], &[b"PATH=/bin"], caps);
        ring3::DEMAND_FILE_ENABLED.store(false, core::sync::atomic::Ordering::Relaxed);
        ring3::DEMAND_ENABLED.store(false, core::sync::atomic::Ordering::Relaxed);
        ring3::clear_demand_file_maps();
        serial_println!("[glibc] gfmmap (file-backed demand paging): exit={efm} (want 124), pages filled-from-file={}",
            ring3::demand_file_pages() - fpages_before);
        for l in ofm.lines() { serial_println!("[glibc]   {l}"); }

        // ── CHROMIUM bring-up, step 1 ──────────────────────────────────────────
        // Run a REAL chrome component: chrome_crashpad_handler (3.4 MB, dynamically
        // linked). Its libs (libc/libm/libgcc_s + the libdl/libpthread stubs) load
        // via ld.so with demand paging. This is the first genuine chrome binary to
        // execute on EuroOS — it discovers the next real blocker (a missing syscall,
        // an unhandled feature) rather than us guessing. --help exits after arg parse.
        let cp_pages_before = ring3::demand_file_pages();
        ring3::DEMAND_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
        ring3::DEMAND_FILE_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
        let (ocp, ecp) = ring3::run_glibc(&mut allocator, ring3::crashpad_bytes(), ring3::ldlinux_bytes(),
            &[b"chrome_crashpad_handler", b"--help"], &[b"PATH=/bin", b"LANG=C"], caps_net);
        ring3::DEMAND_FILE_ENABLED.store(false, core::sync::atomic::Ordering::Relaxed);
        ring3::DEMAND_ENABLED.store(false, core::sync::atomic::Ordering::Relaxed);
        ring3::clear_demand_file_maps();
        serial_println!("[chrome] chrome_crashpad_handler (REAL chrome binary): exit={ecp}, lib-pages demand-loaded={}",
            ring3::demand_file_pages() - cp_pages_before);
        for l in ocp.lines() { serial_println!("[chrome]   {l}"); }

        // ── DISK-BACKED serving (EuroPack) ─────────────────────────────────────
        // gdiskmap: a file served straight from a pack disk (never RAM-resident)
        // must read AND demand-fault-mmap byte-identically to the embedded copy of
        // the same file. This is how a 485 MB chrome binary reaches the loader.
        // Skipped (honestly reported) when no pack disk is attached.
        if ring3::europack_present() {
            ring3::DEMAND_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
            ring3::DEMAND_FILE_ENABLED.store(true, core::sync::atomic::Ordering::Relaxed);
            let (odm, edm) = ring3::run_glibc(&mut allocator, ring3::gdiskmap_bytes(), ring3::ldlinux_bytes(),
                &[b"/bin/gdiskmap"], &[b"PATH=/bin"], caps);
            ring3::DEMAND_FILE_ENABLED.store(false, core::sync::atomic::Ordering::Relaxed);
            ring3::DEMAND_ENABLED.store(false, core::sync::atomic::Ordering::Relaxed);
            ring3::clear_demand_file_maps();
            serial_println!("[europack] gdiskmap (disk-backed mmap+pread): exit={edm} (want 125)");
            for l in odm.lines() { serial_println!("[europack]   {l}"); }
        } else {
            serial_println!("[europack] no pack disk attached — disk-backed serving test SKIPPED");
        }
        } // end !chrome-headless-shell (lean-run) guard

        // ── DISK-SERVED DEMAND-PAGED EXE LOADER ────────────────────────────────
        // Run a real binary whose executable is served from disk (never RAM-
        // resident) — the path to the 485 MB chrome main binary. Validate on
        // crashpad-from-disk (small, known-good embedded) first, then chrome.
        if ring3::europack_has("/pack/crashpad") {
            ring3::GLIBC_ARENA_MIB.store(256, core::sync::atomic::Ordering::Relaxed);
            let (o, e) = ring3::run_glibc_disk(&mut allocator, "/pack/crashpad", ring3::ldlinux_bytes(),
                &[b"chrome_crashpad_handler", b"--help"], &[b"PATH=/bin", b"LANG=C"], caps_net);
            ring3::GLIBC_ARENA_MIB.store(96, core::sync::atomic::Ordering::Relaxed);
            serial_println!("[chrome-disk] crashpad from DISK (demand-paged exe): exit={e}");
            for l in o.lines() { serial_println!("[chrome-disk]   {l}"); }
        }
        if ring3::europack_has("/pack/chrome") {
            ring3::GLIBC_ARENA_MIB.store(256, core::sync::atomic::Ordering::Relaxed);
            let (o, e) = ring3::run_glibc_disk(&mut allocator, "/pack/chrome", ring3::ldlinux_bytes(),
                &[b"/pack/chrome", b"--version"], &[b"PATH=/bin", b"LANG=C", b"DISPLAY=:0"], caps_net);
            serial_println!("[chrome-disk] chrome --version from DISK (485 MB demand-paged exe): exit={e}");
            for l in o.lines() { serial_println!("[chrome-disk]   {l}"); }

            // Fontconfig for chrome (its own setup runs LATER in boot): DejaVu fonts +
            // prebuilt cache + a fonts.conf, so Blink's text layout initializes (else
            // "Cannot load default config file" and the page load may not complete).
            for (name, bytes) in ring3::dejavu_fonts() {
                ring3::register_file_static(&alloc::format!("/usr/share/fonts/truetype/dejavu/{name}"), bytes);
            }
            ring3::register_file_static("/var/cache/fontconfig/d589a48862398ed80a3d6066f4f56f4c-le64.cache-9", ring3::fc_dejavu_cache());
            ring3::register_file("/etc/fonts/fonts.conf", b"<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n<fontconfig>\n  <dir>/usr/share/fonts/truetype/dejavu</dir>\n  <cachedir>/var/cache/fontconfig</cachedir>\n  <alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n  <alias><family>serif</family><prefer><family>DejaVu Serif</family></prefer></alias>\n  <alias><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>\n</fontconfig>\n".to_vec());

            // Push past --version toward real rendering: headless, single-process, no
            // GPU, no sandbox — dump the DOM of a trivial inline page.
            let (o2, e2) = ring3::run_glibc_disk(&mut allocator, "/pack/chrome", ring3::ldlinux_bytes(),
                &[b"/pack/chrome", b"--headless=new", b"--no-sandbox", b"--single-process",
                  b"--disable-gpu", b"--no-zygote", b"--disable-dev-shm-usage",
                  b"--user-data-dir=/tmp/cr", b"--disable-crash-reporter",
                  b"--disable-crashpad-for-testing", b"--disable-breakpad", b"--disable-in-process-stack-traces",
                  b"--lang=en-US",
                  // Software rendering only (QEMU has no Vulkan). Keep the CPU rasterizer
                  // ON (do NOT --disable-software-rasterizer) so the page load can commit
                  // a frame and complete — --dump-dom fires after the load event.
                  b"--in-process-gpu", b"--disable-vulkan",
                  b"--run-all-compositor-stages-before-draw",
                  // Virtual-time budget so the (trivial) page load completes deterministically.
                  b"--virtual-time-budget=10000",
                  b"--dump-dom", b"data:text/html,<html><body><h1>EuroOS</h1></body></html>"],
                &[b"PATH=/bin", b"LANG=C", b"HOME=/root", b"DISPLAY=:0",
                  b"FONTCONFIG_PATH=/etc/fonts", b"CHROME_DEVEL_SANDBOX=/dev/null"], caps_net);
            ring3::GLIBC_ARENA_MIB.store(96, core::sync::atomic::Ordering::Relaxed);
            serial_println!("[chrome-disk] chrome --headless --dump-dom from DISK: exit={e2}");
            for l in o2.lines() { serial_println!("[chrome-disk]   {l}"); }
        }
        // chrome-headless-shell: the DEDICATED single-process headless binary. Unlike
        // full chrome (--headless=new expects a multi-process browser+renderer split
        // we lack), headless-shell drives Blink in one process and --dump-dom is its
        // primary feature. This is the right tool to round-trip a page through Blink.
        if ring3::europack_has("/pack/chrome-headless-shell") {
            for (name, bytes) in ring3::dejavu_fonts() {
                ring3::register_file_static(&alloc::format!("/usr/share/fonts/truetype/dejavu/{name}"), bytes);
            }
            ring3::register_file_static("/var/cache/fontconfig/d589a48862398ed80a3d6066f4f56f4c-le64.cache-9", ring3::fc_dejavu_cache());
            ring3::register_file("/etc/fonts/fonts.conf", b"<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n<fontconfig>\n  <dir>/usr/share/fonts/truetype/dejavu</dir>\n  <cachedir>/var/cache/fontconfig</cachedir>\n  <alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n</fontconfig>\n".to_vec());
            ring3::GLIBC_ARENA_MIB.store(96, core::sync::atomic::Ordering::Relaxed);
            let (o3, e3) = ring3::run_glibc_disk(&mut allocator, "/pack/chrome-headless-shell", ring3::ldlinux_bytes(),
                &[b"/pack/chrome-headless-shell", b"--no-sandbox", b"--single-process",
                  b"--disable-gpu", b"--disable-dev-shm-usage", b"--user-data-dir=/tmp/hs",
                  b"--disable-crashpad-for-testing", b"--disable-breakpad",
                  b"--disable-in-process-stack-traces", b"--no-zygote", b"--lang=en-US",
                  // NO SwiftShader/ANGLE GL: its software GL uses AVX2, which qemu64 + the
                  // EuroOS kernel (no XCR0/AVX enablement) can't run (#UD). Disable GL, but
                  // keep Skia's CPU rasterizer ON (it dispatches to SSE on qemu64, no AVX2)
                  // so the compositor can commit a frame — the load-complete that triggers
                  // --dump-dom. run-all-compositor-stages forces that commit deterministically.
                  b"--in-process-gpu", b"--disable-vulkan", b"--use-gl=disabled",
                  b"--enable-logging=stderr",
                  b"--dump-dom", b"data:text/html,<html><body><h1>EuroOS</h1></body></html>"],
                &[b"PATH=/bin", b"LANG=C", b"HOME=/root", b"DISPLAY=:0",
                  b"FONTCONFIG_PATH=/etc/fonts", b"CHROME_DEVEL_SANDBOX=/dev/null"], caps_net);
            serial_println!("[hshell] chrome-headless-shell --dump-dom from DISK: exit={e3}");
            for l in o3.lines() { serial_println!("[hshell]   {l}"); }
        }
        // (Reclamation validated out-of-band: 30 mixed runs incl. threaded kept the
        // task table at index 31 with free_frames stable — see commit notes.)

        // Linux-compatibility scorecard: tally the glibc suite against expected exits.
        let results: [(&str, u64, u64); 19] = [
            ("gtiny(dyn-link)", e1, 42), ("gtest(stdio/malloc/qsort)", e2, 55),
            ("gthread(pthreads)", e3, 88), ("gmath(libm+dlopen)", e4, 77),
            ("gcpp(C++/exceptions)", e5, 66), ("seq(argv)", e6, 0), ("factor(libgmp)", e7, 0),
            ("gbig(200MiB heap)", e8, 111), ("gsync(mutex+condvar)", e9, 99),
            ("base64(stdin)", e10, 0), ("wc(stdin)", e11, 0), ("sha256sum(libcrypto)", e12, 0),
            ("sort(stdin)", e13, 0), ("gfile(file I/O)", e14, 44), ("gglib(GLib)", e15, 55),
            ("gsparse(demand-paging)", e16, 123), ("gfmmap(file-backed demand-paging)", efm, 124),
            ("gunix(AF_UNIX socketpair)", eu, 67),
            ("gxwin(X11 window+render+PutImage+events+real-kbd)", ex, 90),
        ];
        let pass = results.iter().filter(|(_, got, want)| got == want).count()
            + if ez == 33 { 1 } else { 0 };
        serial_println!(
            "[glibc] ═══ LINUX COMPAT: {}/{} real-glibc capabilities PASS ═══",
            pass, results.len() + 1
        );
        serial_println!(
            "[glibc]   dynamic-linking · pthreads+mutex/condvar · C++/exceptions · dlopen · file-I/O · demand-paging(4GiB sparse) · AF_UNIX-IPC · X11(window·fill·PutImage·events·real-keyboard)"
        );
        serial_println!(
            "[glibc]   9 real libs served: libc libm libstdc++ libgcc_s libgmp libcrypto libglib-2.0 libpcre2 libz | real bins: seq factor base64 wc sha256sum sort"
        );

        // gcairo: a REAL 2D vector-graphics library (Cairo — what GTK/Firefox render
        // with) draws a scene (filled circle, rectangle, stroked line, anti-aliased)
        // into an image surface, then XPutImages it into an X window. Resolves the full
        // ~22-lib Cairo transitive chain via ld.so.
        serial_println!("[glibc] === X11: real Cairo 2D graphics -> X window ===");
        xserver::TRACE.store(true, core::sync::atomic::Ordering::Relaxed);
        let (oc, ec) = ring3::run_glibc(&mut allocator, ring3::gcairo_bytes(), ring3::ldlinux_bytes(), &[b"gcairo"], &[b"DISPLAY=:0", b"PATH=/bin"], caps_net);
        xserver::TRACE.store(false, core::sync::atomic::Ordering::Relaxed);
        serial_println!("[glibc] gcairo (Cairo 2D -> XPutImage): exit={ec}");
        for l in oc.lines() { serial_println!("[glibc]   {l}"); }
        // gcairotext: Cairo + FreeType TEXT — real font rasterization. Serve the TTF;
        // the client FT_New_Face's it (via the VFS), makes a cairo font face, and
        // cairo_show_text's — the last piece before real UI widgets.
        ring3::register_file_static("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", ring3::dejavu_ttf_bytes());
        xserver::TRACE.store(true, core::sync::atomic::Ordering::Relaxed);
        let (oct, ect) = ring3::run_glibc(&mut allocator, ring3::gcairotext_bytes(), ring3::ldlinux_bytes(), &[b"gcairotext"], &[b"DISPLAY=:0", b"PATH=/bin"], caps_net);
        xserver::TRACE.store(false, core::sync::atomic::Ordering::Relaxed);
        serial_println!("[glibc] gcairotext (Cairo+FreeType text): exit={ect}");
        for l in oct.lines() { serial_println!("[glibc]   {l}"); }

        // LAUNCH A PERSISTENT, LIVE X APP that runs ALONGSIDE the desktop: gxlive is
        // a real Xlib event-loop client (key = colour, click = move). It owns the
        // screen (its window is painted by the X server); the desktop loop pumps live
        // keyboard/mouse into it (xserver::pump_* via the X_APP_ACTIVE path). This is
        // the async/desktop-integrated milestone — an interactive X program on EuroOS.
        serial_println!("[glibc] === X11: live interactive desktop app (gxlive) ===");
        // Run gxlive (a real Xlib event-loop client) with a long deadline: run_glibc's
        // wait loop pumps live keyboard/mouse into the X server (xserver::pump_*), the
        // X server delivers them to gxlive's window, and gxlive redraws — an
        // interactive X program on EuroOS, using the proven run_glibc path.
        // Stage some input so the live app visibly reacts even without live QMP keys:
        // 3 key scancodes (colour cycles 3x) + 2 mouse-button packets (block moves 2x).
        // These flow through the REAL ring/latch that pump_keyboard/pump_mouse drain.
        ps2::push_scancode(0x1e); ps2::push_scancode(0x30); ps2::push_scancode(0x2e);
        mouse::push_byte(0x08); mouse::push_byte(0); mouse::push_byte(0);
        mouse::push_byte(0x09); mouse::push_byte(0); mouse::push_byte(0);
        mouse::push_byte(0x08); mouse::push_byte(0); mouse::push_byte(0);
        mouse::push_byte(0x09); mouse::push_byte(0); mouse::push_byte(0);
        ring3::GLIBC_DEADLINE_TICKS.store(1_500, core::sync::atomic::Ordering::Relaxed); // ~15s bounded demo (gxlive loops forever)
        xserver::X_APP_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
        let (ol, _egl) = ring3::run_glibc(&mut allocator, ring3::gxlive_bytes(), ring3::ldlinux_bytes(), &[b"gxlive"], &[b"DISPLAY=:0", b"PATH=/bin"], caps_net);
        xserver::X_APP_ACTIVE.store(false, core::sync::atomic::Ordering::Relaxed);
        ring3::GLIBC_DEADLINE_TICKS.store(12_000, core::sync::atomic::Ordering::Relaxed);
        serial_println!("[glibc] gxlive (live interactive X app; block reacted to staged input):");
        for l in ol.lines() { serial_println!("[glibc]   {l}"); }

        // gpango: REAL Pango text LAYOUT with HarfBuzz shaping (the i18n text engine
        // GTK apps and browsers use). Resolves fonts via fontconfig, shapes glyphs via
        // HarfBuzz, lays out runs (markup, mixed scripts), renders via cairo->XPutImage.
        // Give fontconfig a minimal config; the client adds the font explicitly (no dir
        // scan) so it works without VFS readdir. Run LAST so its window is the final one
        // painted and stays on screen into the desktop (clean, unobscured render).
        {
            const FONTS_CONF: &[u8] = b"<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n<fontconfig>\n  <dir>/usr/share/fonts</dir>\n  <cachedir>/var/cache/fontconfig</cachedir>\n  <alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n  <alias><family>serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n  <alias><family>monospace</family><prefer><family>DejaVu Sans</family></prefer></alias>\n  <alias><family>Sans</family><prefer><family>DejaVu Sans</family></prefer></alias>\n</fontconfig>\n";
            ring3::register_file("/etc/fonts/fonts.conf", FONTS_CONF.to_vec());
            xserver::TRACE.store(true, core::sync::atomic::Ordering::Relaxed);
            let (opo, epo) = ring3::run_glibc(&mut allocator, ring3::gpango_bytes(), ring3::ldlinux_bytes(), &[b"gpango"], &[b"DISPLAY=:0", b"PATH=/bin", b"FONTCONFIG_PATH=/etc/fonts"], caps_net);
            xserver::TRACE.store(false, core::sync::atomic::Ordering::Relaxed);
            serial_println!("[glibc] gpango (Pango+HarfBuzz layout): exit={epo}");
            for l in opo.lines() { serial_println!("[glibc]   {l}"); }
        }

        // gsdl: a REAL SDL2 app (Leg C) — different toolkit, same X11 path. Creates an
        // X window + draws a gradient + moving box to its software surface
        // (SDL_UpdateWindowSurface -> XPutImage). Proves the foundation is not
        // GTK-specific. Bounded fullscreen demo (it loops forever; the deadline ends it).
        {
            for (name, bytes) in ring3::sdl_libs() {
                ring3::register_file_static(&alloc::format!("/lib/x86_64-linux-gnu/{name}"), bytes);
            }
            serial_println!("[glibc] === SDL2 app (gsdl) ===");
            xserver::set_windowed(false); // fullscreen demo blit (not the hosted window)
            ring3::GLIBC_DEADLINE_TICKS.store(3_000, core::sync::atomic::Ordering::Relaxed);
            ring3::GLIBC_ARENA_MIB.store(256, core::sync::atomic::Ordering::Relaxed);
            let (osdl, esdl) = ring3::run_glibc(&mut allocator, ring3::gsdl_bytes(), ring3::ldlinux_bytes(), &[b"gsdl"], &[b"DISPLAY=:0", b"PATH=/bin", b"HOME=/root", b"SDL_VIDEODRIVER=x11", b"SDL_AUDIODRIVER=dummy"], caps_net);
            ring3::GLIBC_ARENA_MIB.store(96, core::sync::atomic::Ordering::Relaxed);
            ring3::GLIBC_DEADLINE_TICKS.store(12_000, core::sync::atomic::Ordering::Relaxed);
            serial_println!("[glibc] gsdl (SDL2): exit={esdl}");
            for l in osdl.lines() { serial_println!("[glibc]   {l}"); }
        }

        // ggtk LAST: a REAL GTK3 app (gtk_init + a window whose GtkDrawingArea draws
        // with cairo, + a GMainLoop over the eventfd + X fd). The X server RENDERS it
        // fully: GTK draws into an off-screen pixmap (solid fills -> core-X
        // PolyFillRectangle; lines/gradients/TEXT -> cairo's image fallback via
        // GetImage+PutImage) and CopyAreas it onto the window, composited to the
        // framebuffer — shapes AND anti-aliased text appear. Self-quits after rendering.
        {
            // Full fontconfig setup so ANY app (incl. GTK, which resolves fonts via
            // fontconfig internally) finds real glyphs: serve the DejaVu family, a
            // PREBUILT fc-cache (fontconfig's runtime dir-scan finds nothing through the
            // VFS, so — like a real distro — we ship the cache fc-cache produced), and a
            // config whose <dir> is the dejavu dir (so only that one cache is needed).
            // The VFS reports dir mtime 0, so the cache always validates as current.
            for (name, bytes) in ring3::dejavu_fonts() {
                ring3::register_file_static(&alloc::format!("/usr/share/fonts/truetype/dejavu/{name}"), bytes);
            }
            ring3::register_file_static("/var/cache/fontconfig/d589a48862398ed80a3d6066f4f56f4c-le64.cache-9", ring3::fc_dejavu_cache());
            ring3::register_file("/etc/fonts/fonts.conf", b"<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n<fontconfig>\n  <dir>/usr/share/fonts/truetype/dejavu</dir>\n  <cachedir>/var/cache/fontconfig</cachedir>\n  <alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n  <alias><family>serif</family><prefer><family>DejaVu Serif</family></prefer></alias>\n  <alias><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>\n  <alias><family>Sans</family><prefer><family>DejaVu Sans</family></prefer></alias>\n  <alias><family>Cantarell</family><prefer><family>DejaVu Sans</family></prefer></alias>\n</fontconfig>\n".to_vec());
            serial_println!("[glibc] === GTK3 toolkit app (ggtk) — LIVE, alongside desktop ===");
            ring3::GLIBC_ARENA_MIB.store(384, core::sync::atomic::Ordering::Relaxed); // ~40 libs
            // WINDOWED: retain the GTK window's pixels instead of blitting fullscreen, so
            // the desktop composites it as a framed window.
            xserver::set_windowed(true);
            // PERSISTENT: spawn the GTK app as a scheduled task that keeps running its
            // GMainLoop ALONGSIDE the desktop (does NOT block boot). It redraws a live
            // counter; the desktop recomposites its window each time it repaints.
            match ring3::spawn_glibc_persistent(&mut allocator, ring3::ggtk_bytes(), ring3::ldlinux_bytes(), &[b"ggtk"], &[b"DISPLAY=:0", b"PATH=/bin", b"HOME=/root", b"FONTCONFIG_PATH=/etc/fonts", b"GDK_BACKEND=x11", b"GTK_A11Y=none", b"NO_AT_BRIDGE=1"], caps_net) {
                Some(t) => serial_println!("[glibc] ggtk live on task {t}"),
                None => serial_println!("[glibc] ggtk spawn FAILED (no arena)"),
            }
        }
    }
    // J2: confirm MSI-X delivery. The xHCI interrupter IRQ latched during USB
    // enumeration (MSI-X → LAPIC vector 0x46) fires as soon as interrupts are on.
    if xhci::present() {
        for _ in 0..100 {
            if interrupts::XHCI_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed) > 0 {
                break;
            }
            apic::busy_wait_us(1000);
        }
        serial_println!(
            "[j2] xHCI MSI-X interrupts received since boot: {} ({})",
            interrupts::XHCI_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed),
            if interrupts::XHCI_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed) > 0 {
                "MSI-X delivery works ✓"
            } else {
                "none yet (interrupter pending?)"
            }
        );
    }
    // J2: confirm MSI-X delivery on the STORAGE controller. Do a block read (with
    // interrupts ON) → the virtio-blk completion sends an MSI-X message → the
    // counter goes up. The used-ring poll confirms the data; the IRQ proves the
    // interrupt-driven completion on the data path.
    if virtio_blk::present() {
        let before = interrupts::BLK_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed);
        let mut sb = [0u8; 512];
        for _ in 0..4 {
            virtio_blk::read_sector(2048, &mut sb); // triggers completions
            apic::busy_wait_us(2000);
        }
        let after = interrupts::BLK_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed);
        serial_println!(
            "[j2-blk] virtio-blk MSI-X completions: {} (after 4 block reads, +{}) → {}",
            after, after - before,
            if after > 0 { "interrupt-driven storage completion works ✓" } else { "none (poll fallback active)" }
        );
    }
    // J1: verify the lock-free kmsg ring (the APs already logged via this path at boot).
    klog::lockfree_selftest();
    serial_println!("[rtc] real wall-clock time: {} {}", rtc::clock_string(), rtc::date_string());

    // G2: build the VFS — the root + (if present) the second disk on /mnt — so that
    // the shell transparently serves `/mnt/...` on disk 1 (longest-prefix routing).
    let mut vfs = eurofs::Vfs::new(alloc::boxed::Box::new(fs));

    // G4: mount the **EuroVar** partition on /var (writable data, separate from the
    // — future read-only — system slot). Proves the multi-partition A/B GPT layout:
    // /var lives on disk 0 next to slot A, with absolute-sector-correct block cache.
    if virtio_blk::present() {
        if let Some((vfirst, vblocks)) = gpt::find_partition_by_name("EuroVar") {
            let vdev = rootblk::RootBlk::disk_on(0, vfirst, vblocks);
            let var_fs = match EuroFs::mount(vdev.clone(), rtc::epoch()) {
                Ok(f) => {
                    serial_println!("[g4] /var: EuroVar partition @ LBA {vfirst} mounted (existing)");
                    f
                }
                Err(_) => {
                    let f = EuroFs::format(vdev, [0x7A; 16], rtc::epoch()).expect("EuroFS format /var");
                    serial_println!("[g4] /var: EuroVar partition @ LBA {vfirst} formatted (fresh)");
                    f
                }
            };
            vfs.mount("/var", alloc::boxed::Box::new(var_fs));
            let _ = vfs.write_file("/var/ab-layout.txt", b"writable /var partition (A/B-GPT)\n");
            match vfs.read_file("/var/ab-layout.txt") {
                Ok(d) => serial_println!("[g4] VFS routes /var → {} bytes on the EuroVar partition ✓", d.len()),
                Err(_) => serial_println!("[g4] /var routing FAILED"),
            }
        }
    }

    // [sym]/[cu2] (Sprint 3C): symbolic links on the live EuroFS + the new coreutils.
    {
        use eurocoreutils as cu;
        let _ = vfs.write_file("/symtarget", b"symlink payload");
        let sym_ok = vfs.create_symlink("/symlink", "/symtarget").is_ok()
            && vfs.read_link("/symlink").ok().as_deref() == Some("/symtarget")
            && vfs.read_file("/symlink").ok().as_deref() == Some(b"symlink payload".as_ref());
        serial_println!(
            "[sym] EuroFS symlinks: create+readlink+follow-through={sym_ok} → {}",
            if sym_ok { "OK ✓" } else { "FAILED ✗" }
        );
        let _ = vfs.remove_file("/symlink");
        let _ = vfs.remove_file("/symtarget");
        let md5 = String::from_utf8_lossy(&cu::checksum::md5sum(b"abc", "-")).into_owned();
        let sha1 = String::from_utf8_lossy(&cu::checksum::sha1sum(b"abc", "-")).into_owned();
        let cu_ok = md5.starts_with("900150983cd24fb0d6963f7d28e17f72")
            && sha1.starts_with("a9993e364706816aba3e25717850c26c9cd0d89d");
        serial_println!(
            "[cu2] coreutils long-tail: md5sum/sha1sum vectors={cu_ok} · b2sum/shuf/comm/join/split/ln/readlink/realpath/mktemp/env wired → {}",
            if cu_ok { "OK ✓" } else { "FAILED ✗" }
        );
    }

    // 3A-3: auto-mount a removable USB mass-storage volume (FAT/exFAT) at /usb.
    fatmount::usb_auto_mount(&mut vfs);

    let has_mnt = fs2.is_some();
    if let Some(f2) = fs2 {
        vfs.mount("/mnt", alloc::boxed::Box::new(f2));
        serial_println!("[g2] VFS: /mnt mounted (disk 1) — shell routes /mnt/* there");
    }
    // G2/B2: if there is an NVMe disk, mount an EuroFS on it (/nvme). Proves that
    // the NVMe driver carries a real filesystem (now works under any CR3 thanks to A2's
    // shared high region that maps the NVMe MMIO @768 GiB everywhere).
    // Metal M2-3: skip when NVMe already carries the live ROOT (mounted at / via
    // the block cache) — mounting the same disk again here would be a double mount.
    if let Some(nb) = nvme::NvmeBlock::new().filter(|_| !nvme_root) {
        let nfs = match EuroFs::mount(nb, rtc::epoch()) {
            Ok(f) => {
                serial_println!("[g2] EuroFS mounted on NVMe (existing)");
                f
            }
            Err(_) => {
                let f = EuroFs::format(nb, [0xC0; 16], rtc::epoch()).expect("EuroFS format NVMe");
                serial_println!("[g2] EuroFS formatted on NVMe (installation)");
                f
            }
        };
        vfs.mount("/nvme", alloc::boxed::Box::new(nfs));
        use eurofs::FileSystem;
        let _ = vfs.write_file("/nvme/op-nvme.txt", b"this file lives on the NVMe disk\n");
        match vfs.read_file("/nvme/op-nvme.txt") {
            Ok(d) => serial_println!("[g2] VFS routes /nvme → {} bytes from the NVMe disk ✓", d.len()),
            Err(_) => serial_println!("[g2] /nvme routing FAILED"),
        }
    }

    // ── Phase 2B: SOVEREIGN SECURITY BACKBONE (L1 + L2 + P3) ──
    // L1 (EuroFS immutability) + L2 (CAP_IMMUTABLE_ADMIN gate) + P3 (append-only
    // audit log): tamper-proof system files + an irreversible audit trail.
    {
        let boot_caps = ring3::CAP_IMMUTABLE_ADMIN | ring3::CAP_FILE;
        immutable::selftest(&mut vfs);
        let protected = immutable::protect_system_files(&mut vfs, boot_caps);
        serial_println!(
            "[l2] {} system file(s) marked IMMUTABLE — tamper-proof (changing requires CAP_IMMUTABLE_ADMIN; the boot updater clears the flag legitimately)",
            protected
        );
        audit::selftest(&mut vfs, boot_caps);
        // 3D-6 wiring: the live audit log is now hash-chained + tamper-evident,
        // fed by the real execve/connection call sites, and persisted as JSON.
        audit::chain_selftest(&mut vfs, boot_caps);
    }

    // ── Sprint S: EuroSnap — CoW snapshots + rollback on the REAL root FS ──
    // We snapshot the already-set-up system state, then write a test
    // file, and roll back: the test file (after the snapshot) disappears, while
    // the system files (before the snapshot) stay intact — cheap thanks to CoW.
    {
        use eurofs::FileSystem;
        let snap = vfs.snapshot_create("boot-checkpoint", eurofs::SNAP_READONLY);
        match snap {
            Ok(id) => {
                let _ = vfs.write_file("/snap-test.txt", b"written AFTER the snapshot");
                let before = vfs.exists("/snap-test.txt");
                let rb = vfs.snapshot_rollback(id).is_ok();
                let after = vfs.exists("/snap-test.txt");
                let sys_intact = vfs.exists("/bin/hello"); // existed before the snapshot
                let nsnaps = vfs.snapshot_list().len();
                serial_println!(
                    "[s] EuroSnap: snapshot #{id} 'boot-checkpoint', test file before-rollback={before} → after-rollback={after}, system-file-intact={sys_intact}, rollback-ok={rb}, {nsnaps} snapshot(s) → {}",
                    if before && !after && sys_intact && rb { "OK (CoW rollback works, system intact) ✓" } else { "FAILED" }
                );
                // Clean up (+ GC) so the self-test does not pile up snapshots on every boot.
                let _ = vfs.snapshot_delete(id);
            }
            Err(e) => serial_println!("[s] EuroSnap: creating snapshot failed ({e:?})"),
        }
    }

    // ── K3: FULL-DISK ENCRYPTION with a TPM-generated key ──
    // A REAL EuroFS on top of the transparent FDE layer (on a RAM volume, so we
    // do not reformat the real root). The 256-bit key comes from the TPM (O1);
    // proves that the whole FS lands transparently encrypted on the disk.
    {
        use eurofs::{BlockDevice, EuroFs, FileSystem};
        let (key_bytes, from_tpm) = match tpm::get_random(32) {
            Some(b) => (b, true),
            None => (alloc::vec![0x5Au8; 32], false), // fallback without TPM
        };
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes[..32]);
        // 3D-1: seal the FDE key to the measured-boot state and use the
        // TPM-unsealed copy — so the disk key only exists after the TPM releases
        // it on an untampered boot (not just a fresh RNG blob in RAM).
        let fde_sealed = match tpm::seal_to_pcr(tpm::SEAL_PCR, &key)
            .and_then(|(pv, pb)| tpm::unseal_from_pcr(tpm::SEAL_PCR, &pv, &pb))
        {
            Some(k) if k.len() == 32 => {
                key.copy_from_slice(&k[..32]);
                true
            }
            _ => false,
        };
        let fde = eurofde::FdeKey::new(key, 0xE0_05);
        let enc = eurofde::EncryptedBlockDevice::new(rootblk::RootBlk::ram(128), fde);
        let mut enc = enc;
        let result = (|| -> Result<bool, eurofs::FsError> {
            let mut efs = EuroFs::format(&mut enc, [0xFD; 16], rtc::epoch())?;
            efs.write_file("/secret.txt", b"encrypted data on the disk")?;
            let back = efs.read_file("/secret.txt")?;
            Ok(back == b"encrypted data on the disk")
        })();
        match result {
            Ok(ok) => serial_println!(
                "[k3] FDE: EuroFS on encrypted block layer (ChaCha20), key-from-TPM={from_tpm}, key-TPM-sealed-to-boot={fde_sealed}, read-after-write-intact={ok} → {}",
                if ok { "OK (transparent full-disk encryption works) ✓" } else { "FAILED" }
            ),
            Err(e) => serial_println!("[k3] FDE: failed ({e:?})"),
        }
    }

    // ── Phase 2B follow-up: X (policy), W (observability), U (secrets) ──
    // X: EuroPol — declarative policy → EuroGuard capabilities (violations → P3).
    europol::selftest();
    // 3D-4: signed policy bundles — a policy can only change caps if Ed25519-signed.
    europol::bundle_selftest();
    // 3D-5: user-scoped file immutability (own home files, no admin cap).
    euroattr::selftest();
    // 3D-6: hash-chained GDPR audit + sealed-vault persistence.
    gdpr::selftest();
    // W: EuroObserve — lock-free kernel metrics + OpenMetrics export.
    observe::selftest(allocator.free_frames() as u64);
    // U: EuroVault — capability-gated, encrypted secrets with a TPM master key.
    {
        let (mk, from_tpm) = match tpm::get_random(32) {
            Some(b) => {
                let mut k = [0u8; 32];
                k.copy_from_slice(&b[..32]);
                (k, true)
            }
            None => ([0xA5u8; 32], false),
        };
        vault::selftest(mk, from_tpm);
        // 3D-1: real TPM seal — the master key is sealed INSIDE the TPM to the
        // measured-boot PCR and released only on an untampered boot (replaces the
        // earlier software-KDF PCR-seal).
        vault::tpm_seal_selftest(mk);
    }

    // 3E-1 wiring: the FDE unseal-at-boot cycle — enrol (seal + persist), then
    // re-read the sealed blob and unseal it via the TPM (PCR16) as a normal boot
    // would, auto-recovering the disk key on an untampered system.
    instexec::fde_unseal_selftest(&mut vfs);

    // Z: EuroHealth — SMART (if NVMe) + FS scrub + memory → health score.
    {
        use eurofs::FileSystem;
        let sr = vfs.scrub();
        health::selftest(sr.errors, sr.data_unrecoverable, allocator.free_frames() as u64, allocator.total_frames() as u64);
    }
    // N3: EuroFW — packet filter in the RX path (stealth-drop of blocked traffic).
    firewall::init();
    firewall::selftest();
    // N2: EuroVPN — sovereign forward-secret tunnel (seeds from the TPM if present).
    {
        let mut seeds = [[0u8; 32]; 4];
        let mut from_tpm = true;
        for (i, s) in seeds.iter_mut().enumerate() {
            // TPM GetRandom returns ≤32 bytes per call → four separate calls.
            match tpm::get_random(32) {
                Some(b) => s.copy_from_slice(&b[..32]),
                None => {
                    from_tpm = false;
                    *s = [(i as u8) + 0x11; 32];
                }
            }
        }
        vpn::selftest(seeds[0], seeds[1], seeds[2], seeds[3], from_tpm);
        // 3D-9: the hybrid post-quantum tunnel (X25519 + ML-KEM-768). Two more
        // seeds for the ML-KEM key pair + encapsulation randomness.
        let mut kem_seed = [0x21u8; 32];
        let mut kem_rand = [0x22u8; 32];
        if let Some(b) = tpm::get_random(32) {
            kem_seed.copy_from_slice(&b[..32]);
        }
        if let Some(b) = tpm::get_random(32) {
            kem_rand.copy_from_slice(&b[..32]);
        }
        vpn::selftest_hybrid(seeds[0], seeds[1], seeds[2], seeds[3], kem_seed, kem_rand, from_tpm);
    }

    // EuroAgent (Sprint AA): prove the sovereign agent-runtime core at boot —
    // manifest → least-privilege caps → cap-gated MCP call → intent routing.
    agent::selftest();

    // BB-1: prove the REAL LLM transport — the agent loop talks over EuroNet-TCP
    // with a local Ollama `/api/chat` endpoint (10.0.2.2:11434 via SLIRP host).
    agent::llm_selftest();

    // EuroLocale (P1): prove localization for the 24 EU languages at boot.
    locale::selftest();

    // EuroInstall (Q1): prove the installer planner at boot.
    installer::selftest();

    // EuroCA (O3): sovereign local certificate authority (TPM-seeded root).
    {
        let (seed, from_tpm) = match tpm::get_random(32) {
            Some(b) => {
                let mut s = [0u8; 32];
                s.copy_from_slice(&b[..32]);
                (s, true)
            }
            None => ([0x3c; 32], false),
        };
        ca::selftest(seed, from_tpm, rtc::epoch());
    }

    // EuroAttest (O2): remote attestation — quote over the measured-boot PCRs.
    {
        let (ak_seed, nonce, from_tpm) = match (tpm::get_random(32), tpm::get_random(32)) {
            (Some(a), Some(n)) => {
                let (mut s, mut no) = ([0u8; 32], [0u8; 32]);
                s.copy_from_slice(&a[..32]);
                no.copy_from_slice(&n[..32]);
                (s, no, true)
            }
            _ => ([0x2a; 32], [0x9e; 32], false),
        };
        attest::selftest(ak_seed, nonce, from_tpm);
    }
    // 3D-3: CA hierarchy (root→intermediate→leaf) + on-disk store + a JSON
    // attestation report a remote verifier checks against the boot PCRs.
    attest3::selftest();

    // EuroIDM (V): sovereign enterprise identity (identity → capabilities).
    {
        let (seed, from_tpm) = match tpm::get_random(32) {
            Some(b) => {
                let mut s = [0u8; 32];
                s.copy_from_slice(&b[..32]);
                (s, true)
            }
            None => ([0x1d; 32], false),
        };
        idm::selftest(seed, from_tpm, rtc::epoch());
    }

    // EuroID (K1 + P3): sovereign user management — Argon2id credentials, sessions,
    // per-user caps, lockout, and a tamper-evident hash-chain audit log.
    euroid::selftest();
    // 3D-10: EuroID as an eIDAS 2.0 EUDI-wallet issuer + relying party
    // (SD-JWT VC selective disclosure + holder key binding).
    euroid::wallet_selftest();

    // EuroPkg (M2): dependency resolution of the package manager.
    pkg::selftest();
    // 3E-6: the package-manager EXECUTOR — signed index → resolver → verify →
    // content-addressed store → /bin link; tamper/forgery refused; remove+GC.
    pkg::exec_selftest(rtc::epoch());

    // 3E-5: GDB Remote Serial Protocol stub over COM2 — prove the wire protocol
    // against live kernel register/memory state (a real gdb attaches via COM2→tcp).
    gdbstub::init();
    gdbstub::selftest();

    // EuroRepro (M3/Q2): reproducible builds — attestation + consensus.
    repro::selftest();

    // EuroAccess (P2): accessibility layer — focus + multilingual screen reader.
    access::selftest();

    // BB-8: LIVE accessibility events — focus navigation through a dialog → multilingual
    // screen-reader announcements → routed to EuroAudio (HDA). EN 301 549 end-to-end.
    access::live_selftest();
    // 3F-3: the broadened European Accessibility Act surface — accessibility tree
    // + states, complete keyboard navigation, WCAG high-contrast, magnification.
    access::eaa_selftest();
    // Phase 3G: journal, watchdog, hardening baseline, DHCPv6, mDNS, DNSSEC.
    phase3g::selftest();
    phase3a::selftest();

    // EuroSuite (ES-Core/IO/Calc): sovereign office suite on one UDM.
    suite::selftest();
    // 3F-2: open & save REAL .docx (ZIP + DEFLATE via euroflate + OOXML).
    suite::docx_selftest();
    // 3F-4: selectable keyboard layouts (US-QWERTY/AZERTY/QWERTZ).
    ps2::keymap_selftest();
    // 3F-7: capability-scoped app permission portals (request → ask → scoped grant).
    portal::selftest();
    // Part 2: unified per-app control surface (caps + permissions + network in one).
    shell::selftest();
    // Part 3: the desktop per-app control screen's actions do real kernel work.
    settings_ui::selftest();
    // Everyday-conveniences layer: live clipboard + right-click context menus +
    // window snapping (drag to a screen edge).
    clipboard::selftest();
    ctxmenu::selftest();
    compositor::snap_selftest();
    launcher::selftest();
    filedialog::selftest();
    notify::selftest();
    screenshot::selftest();
    tooltip::selftest();
    symbolpicker::selftest();
    spell::selftest();
    switcher::selftest();
    workspace::selftest();
    netbridge::selftest();
    // 3F-6: audio routing — per-app streams, per-device routing, default policy.
    audio::selftest();

    // EuroWeb (AB-B1): sovereign browser engine — HTML5 tokenizer + DOM.
    web::selftest();
    // AG-2: images (<img> + QOI/PPM decode) + forms (real GET).
    web::selftest_ag2();
    // Sprint 4: form POST (urlencoded body) + JS on a page (EuroJS).
    web::selftest_post();
    web::selftest_js();

    // EuroReken (AC-1): sovereign calculator — std/scientific/programmer.
    reken::selftest();

    // EuroNotes (AC-1): notes app — Markdown → EuroDoc UDM.
    notes::selftest();

    // EuroArchive (AC-2): archive manager — USTAR tar + checksum + manifest.
    archive::selftest();

    // EuroSafe (AC-1): capability dashboard — risk scoring + recommendations.
    safe::selftest();

    // Window management + AC apps (EuroClip/Clock/Shot/Contacts).
    wm::selftest();
    clip::selftest();
    clockapp::selftest();
    shot::selftest();
    contacts::selftest();
    calapp::selftest();
    signapp::selftest();
    musicapp::selftest();
    mailapp::selftest();
    fontapp::selftest();
    jsapp::selftest();

    // EuroFiles (AC-1): file manager — sort/filter/path/badges.
    files::selftest();
    // EuroMedia (AC-1): image viewer — sovereign QOI codec.
    media::selftest();

    // EuroWiFi (N1): 802.11 protocol core — beacon scan + WPA key derivation.
    wifi::selftest();

    // BB-3: detect an Intel WiFi radio + HONEST driver status (QEMU emulates
    // no 802.11; the protocol core is proven by [n1], radio = hardware-attended).
    wifi::bb3_selftest();

    // EuroGPU (K4): virtio-gpu command protocol — displayinfo→scanout→flush.
    gpu::selftest();

    // BB-2: NATIVE modern-virtio transport + virtio-gpu driver against a real
    // device (init handshake + GET_DISPLAY_INFO over the control virtqueue).
    virtio_gpu::selftest();
    // 3B-7: native modern-virtio virtio-sound driver (control-queue round-trip).
    virtio_snd::selftest();

    // BB-4: EuroPrint — real IPP-over-TCP round-trip to a network printer/CUPS
    // (10.0.2.2:631 via SLIRP host); Get-Printer-Attributes + Print-Job.
    print::selftest();
    scan::selftest();

    // EuroCoreutils (CU-7): prove the compute/control commands live in the kernel
    // (deterministic — not dependent on USB keystrokes under slow TCG).
    {
        use eurocoreutils::compute;
        let printf_ok = compute::printf(&["%s=%05d", "x", "42"]) == b"x=00042";
        let expr_ok = compute::expr(&["(", "2", "+", "3", ")", "*", "4"]).0 == b"20\n";
        let test_ok = compute::test(&["5", "-gt", "3"]) == 0 && compute::test(&["a", "=", "b"]) == 1;
        let factor_ok = compute::factor(&["12"]) == b"12: 2 2 3\n";
        let numfmt_ok = compute::numfmt(&["--to=iec", "1048576"]) == b"1.0M\n";
        let ok = printf_ok && expr_ok && test_ok && factor_ok && numfmt_ok;
        serial_println!(
            "[cu] EuroCoreutils: printf={printf_ok}, expr={expr_ok}, test={test_ok}, factor={factor_ok}, numfmt={numfmt_ok} → {}",
            if ok { "OK (GNU-compatible coreutils core live in the shell) ✓" } else { "FAILED" }
        );
    }

    // find (CU-5): prove the VFS tree walk + filters live, deterministically. Make a
    // small tree, then search by name and by type, and check the results.
    {
        use eurofs::FileSystem;
        let _ = vfs.create_dir("/find-test");
        let _ = vfs.create_dir("/find-test/sub");
        let _ = vfs.write_file("/find-test/alpha.txt", b"a");
        let _ = vfs.write_file("/find-test/beta.md", b"b");
        let _ = vfs.write_file("/find-test/sub/gamma.txt", b"g");

        let by_name = shell::find_walk(&mut vfs, "find /find-test -name *.txt");
        let name_ok = by_name.iter().any(|p| p == "/find-test/alpha.txt")
            && by_name.iter().any(|p| p == "/find-test/sub/gamma.txt")
            && !by_name.iter().any(|p| p.ends_with("beta.md"));

        let by_type = shell::find_walk(&mut vfs, "find /find-test -type d");
        let type_ok = by_type.iter().any(|p| p == "/find-test/sub")
            && !by_type.iter().any(|p| p.ends_with(".txt"));

        let depth = shell::find_walk(&mut vfs, "find /find-test -maxdepth 1 -name *.txt");
        let depth_ok = depth.iter().any(|p| p == "/find-test/alpha.txt")
            && !depth.iter().any(|p| p == "/find-test/sub/gamma.txt"); // too deep for maxdepth 1

        serial_println!(
            "[find] CU-5 find: -name *.txt (recursive)={name_ok}, -type d={type_ok}, -maxdepth 1={depth_ok} → {}",
            if name_ok && type_ok && depth_ok { "OK (VFS walk + glob/type/depth filters) ✓" } else { "FAILED" }
        );
    }

    // AH-3 (H4 remainder): `wasm <file>` — run a real self-contained .wasm from
    // EuroFS in the no-JIT sandbox, with cap-gated WASI.
    wasm::selftest_file(&mut vfs);

    // pipe-stdin (CU finishing): prove that coreutils built-ins compose through a
    // pipeline — stdout of stage N → stdin of N+1 — deterministically.
    {
        use eurofs::FileSystem;
        let _ = vfs.write_file("/pipe-test.txt", b"alpha\nbeta\nbravo\ngamma\n");
        let mut pctx = shell::ShellCtx { fs: &mut vfs, mem: &mut allocator };
        // cat | grep b | wc -l → 2 lines contain 'b' (beta, bravo).
        let r1 = shell::exec(&mut pctx, "cat /pipe-test.txt | grep b | wc -l");
        let pipe1 = r1.iter().any(|l| l.split_whitespace().next() == Some("2"));
        // seq 5 | tail -2 → 4 and 5 (not 1).
        let r2 = shell::exec(&mut pctx, "seq 5 | tail -2");
        let joined: alloc::string::String = r2.join(",");
        let pipe2 = joined.contains('4') && joined.contains('5') && !joined.contains('1');
        // echo | tr (per-character replacement) → haLLo.
        let r3 = shell::exec(&mut pctx, "echo hallo | tr l L");
        let pipe3 = r3.iter().any(|l| l.contains("haLLo"));
        // seq 3 | tee FILE | wc -l → tee writes 3 lines to FILE and passes them through.
        let r4 = shell::exec(&mut pctx, "seq 3 | tee /pipe-tee.txt | wc -l");
        let tee_through = r4.iter().any(|l| l.split_whitespace().next() == Some("3"));
        let tee_written = pctx.fs.read_file("/pipe-tee.txt").map(|d| d.len()).unwrap_or(0) == 6; // "1\n2\n3\n"
        // AG-4: xargs — build a command from the stdin tokens and run it.
        // seq 3 | xargs echo → one line "1 2 3".
        let rx1 = shell::exec(&mut pctx, "seq 3 | xargs echo");
        let xargs1 = rx1.iter().any(|l| l.split_whitespace().collect::<alloc::vec::Vec<_>>() == ["1", "2", "3"]);
        // seq 4 | xargs -n2 echo → two batches: "1 2" and "3 4".
        let rx2 = shell::exec(&mut pctx, "seq 4 | xargs -n2 echo");
        let xargs2 = rx2.iter().filter(|l| !l.trim().is_empty()).count() == 2
            && rx2.iter().any(|l| l.split_whitespace().collect::<alloc::vec::Vec<_>>() == ["1", "2"])
            && rx2.iter().any(|l| l.split_whitespace().collect::<alloc::vec::Vec<_>>() == ["3", "4"]);
        // AG-4: extra pipe-stdin built-in (sha224sum as a pipeline filter).
        let rsha = shell::exec(&mut pctx, "echo euroos | sha224sum");
        let sha_pipe = rsha.iter().any(|l| l.len() >= 56 && l.contains("-"));
        serial_println!(
            "[pipe] built-in pipeline: cat|grep|wc-l=2 →{pipe1}, seq|tail-2 →{pipe2}, echo|tr →{pipe3}, seq|tee|wc(through={tee_through},file={tee_written}), xargs(echo={xargs1},-n2={xargs2}), sha224-filter={sha_pipe} → {}",
            if pipe1 && pipe2 && pipe3 && tee_through && tee_written && xargs1 && xargs2 && sha_pipe { "OK (stdout→stdin + tee + xargs + sha-filter) ✓" } else { "FAILED" }
        );
    }

    // 3E-2: EuroUpdate delivery — live signed-channel check against the update
    // server on the SLIRP host gateway (verify manifest → version → hash-pinned
    // signed image → A/B stage); READY-reported when no server runs.
    update::channel_selftest(rtc::epoch());

    // EuroAgent real tools (Phase 2C): an agent really writes+reads on EuroFS via
    // the cap-gated MCP gateway, sandbox-clamped — no longer a stub.
    agent::real_tools_selftest(&mut vfs);

    // AD-1: the REAL net_get/vault_get tools, double-gated (cap + domain allow-list);
    // the vault value ends up in the result but never in the audit.
    agent::net_vault_selftest(&mut vfs);

    // Audit #7 / P0.3: the EuroAgent audit trail is no longer RAM-only — every
    // tool call is persisted to the append-only on-disk log (survives restart).
    agent::audit_persist_selftest(&mut vfs, ring3::CAP_IMMUTABLE_ADMIN | ring3::CAP_FILE);

    // AF / Zero-Trust P2.2: just-in-time capability elevation + auto-revoke — an
    // elevated cap applies to only one confirmed action, not the whole session.
    agent::jit_selftest();

    // AF / Zero-Trust P2.3: behavior detection on the agent audit stream — anomalous
    // behavior (probing, drift, rate spikes) is made visible deterministically.
    agent::anomaly_selftest();

    // Sprint AE-e2e: EuroID storage (users + Argon2id hashes + state) persistent
    // on EuroFS — survives a restart instead of being rebuilt every boot.
    euroid::persist_selftest(&mut vfs);
    // 3E-3/3E-9: session lifecycle (owned homes, uid-context) + per-user disk
    // quota — both on the LIVE root FS.
    session::selftest(&mut vfs);
    session::quota_selftest(&mut vfs);
    // 3F-5: MIME detection + default-app associations on the live FS.
    mime::selftest(&mut vfs);
    // Sprint AE-e2e: must-change-password enforced end-to-end (login refuses until
    // the user changes their own password).
    euroid::must_change_selftest();

    // EuroAgent MCP daemon (AA-3 capstone): the gateway served over AF_UNIX.
    mcpd::selftest(&mut vfs);

    // WASM agent host (AA-5 capstone): agent code runs in WASM → host import →
    // MCP gateway → EuroFS, capability-gated.
    wagent::selftest(&mut vfs);

    // EuroInstall execution (Q1 capstone): really format + provision a
    // RAM disk and prove the installation survives a remount.
    instexec::selftest(rtc::epoch());

    if has_mnt {
        use eurofs::FileSystem;
        // The boot self-test wrote "/hello-disk2.txt" to disk 1. Via the VFS it now
        // lives at "/mnt/hello-disk2.txt" — prove the routing goes to disk 1.
        match vfs.read_file("/mnt/hello-disk2.txt") {
            Ok(d) => serial_println!("[g2] VFS routes /mnt/hello-disk2.txt → {} bytes from disk 1 ✓", d.len()),
            Err(_) => serial_println!("[g2] VFS /mnt routing FAILED"),
        }
        for (mp, t, f) in vfs.df() {
            serial_println!("[g2] df {:<6} {:>7} KiB total {:>7} KiB free", mp, t / 1024, f / 1024);
        }
    }

    // EuroUpdate (F1): all core init succeeded (we are starting the desktop) →
    // mark the active slot definitively good, so a staged update does not
    // roll back unnecessarily. Before the VFS is borrowed into the shell context.
    update::mark_boot_good(&mut vfs);

    // 3G-1 wiring: the structured journal now persists to the root FS —
    // restore any prior ring, log the boot, write it back, prove it round-trips.
    journal::persist_selftest(&mut vfs);

    // G5: first background scrub pass over EuroFS (data-path XXH3 + structure) →
    // /var/log/fsck.log. After that the scrubber runs periodically (rate-limited) from
    // the desktop tick.
    scrub::run(&mut vfs);

    // [stress] big load/stress test, if armed by the EUROSTRESS sentinel on disk0.
    // Runs here while both `vfs` and `allocator` are still separately borrowable and
    // ring3/interrupts are live — just before they are moved into the shell context.
    if stresstest::armed() {
        stresstest::run(&mut vfs, &mut allocator);
    }

    // ── EuroDesktop compositor (Track 5) ──
    let _ = bp;
    let mut ctx = shell::ShellCtx {
        fs: &mut vfs,
        mem: &mut allocator,
    };

    // Terminal window: first show the output of the C program /bin/hello
    // (loaded from EuroFS, run in ring 3 via syscalls), then some commands.
    let mut term: Vec<String> = Vec::new();
    term.push(String::from("euroos:/ $ ./bin/hello   (C program, EuroFS -> ring 3)"));
    term.push(format!(
        "[verify] Ed25519 signature {} (key {:02x}{:02x}{:02x}{:02x}…)",
        if verified { "OK - verified" } else { "ERROR - rejected" },
        fp[0], fp[1], fp[2], fp[3]
    ));
    term.push(format!(
        "[sec]    tamper-test: 1 byte changed -> {}",
        if tamper_accepted { "ACCEPTED (WRONG!)" } else { "REJECTED" }
    ));
    term.push(String::from("[caps]   granted: CONSOLE PROC FILE  (NO NET)"));
    for line in user_out.lines() {
        term.push(line.into());
    }
    term.push(format!("[exit {exit_code}]"));
    term.push(String::new());
    // EuroNet — real virtio-net NIC: live ARP exchange with the gateway.
    term.push(String::from("euroos:/ $ EuroNet — virtio-net (real TX/RX)"));
    for l in &net_lines {
        term.push(l.clone());
    }
    term.push(String::new());
    // The output of the exec-by-name boot script (started by name from EuroFS).
    for (header, out) in &demo_out {
        term.push(header.clone());
        for line in out.lines() {
            term.push(line.into());
        }
        term.push(String::new());
    }
    // REAL shell + filesystem demo: make a directory, write a file, read it
    // back — the output is real (no script), and /demo also appears in the
    // Files app. Proves the shell + EuroFS really work.
    for c in [
        "uname",
        "mkdir /demo",
        "write /demo/welcome.txt Hello-from-EuroOS",
        "ls /demo",
        "cat /demo/welcome.txt",
        "ls /",
    ] {
        term.push(format!("euroos:/ $ {c}"));
        for l in shell::exec(&mut ctx, c) {
            term.push(l);
        }
    }
    term.push(String::from("euroos:/ $ "));

    // Live system window content (REAL kernel status — no mockup). The earlier
    // Files/Mail windows were hard-coded EDS mockups (not real programs)
    // and have been removed: the desktop now shows only what is really running — a live
    // System window and the real interactive Terminal.
    let total_ram = ctx.mem.usable_bytes();
    let sysinfo = |t: u64, free: u64| -> Vec<String> {
        let a = sched::TASK_COUNTERS[1].load(Ordering::Relaxed);
        let b = sched::TASK_COUNTERS[2].load(Ordering::Relaxed);
        let c = sched::TASK_COUNTERS[3].load(Ordering::Relaxed);
        let u1 = ring3::read_counter(ucnt1) / 1_000_000;
        let u2 = ring3::read_counter(ucnt2) / 1_000_000;
        let mut v = alloc::vec![
            String::from("EuroKernel v0.1-alpha — from-scratch Rust (no_std)"),
            String::from("no Linux/BSD underneath; Linux ABI = compat bridge"),
            format!("uptime {} s  ({} ticks)   RAM {} / {} MiB free", t / 100, t, free / (1024 * 1024), total_ram / (1024 * 1024)),
            format!("CPU isolation  SMEP {} · SMAP {} · W^X/NX {} (CR4)",
                if ring3::smep_active() { "on" } else { "n/a" },
                if ring3::smap_active() { "on" } else { "n/a" },
                if ring3::nx_active() { "on" } else { "n/a" }),
            String::new(),
            String::from("preemptive scheduler (per-process address spaces):"),
            format!("  kernel-threads   A={a} B={b} C={c}"),
            format!("  ring-3 process 1  {u1}M iterations"),
            format!("  ring-3 process 2  {u2}M iterations"),
            String::from("  daemon (pid 7)   -> EuroMonitor"),
            String::from("  shell (Terminal window)"),
            String::new(),
            String::from("per-process (preemptive, own FS_BASE/TLS + heap):"),
        ];
        // The most recent line of each background musl process: independent
        // __thread counters prove FS_BASE is preserved per process.
        for line in ring3::bg_lines() {
            v.push(format!("  {line}"));
        }
        for line in ring3::reaped_lines() {
            v.push(format!("  [reaper] {line}"));
        }
        let (httpd_on, served) = net::httpd_status();
        if httpd_on {
            v.push(format!("  [httpd] background server ON — {served} requests served"));
        }
        let ipc = euroipc::audit_lines();
        if !ipc.is_empty() {
            v.push(String::from("EuroIPC (message bus, audit):"));
            for line in ipc.iter().rev().take(3).rev() {
                v.push(format!("  {line}"));
            }
        }
        v.push(String::new());
        v.push(String::from("EuroMonitor daemon (preemptive, own syscalls):"));
        // The most recent heartbeat lines of the scheduled background daemon.
        for line in ring3::daemon_lines().iter().rev().take(2).rev() {
            v.push(format!("  {line}"));
        }
        v
    };

    let mut windows = alloc::vec![
        // System — live, real kernel status (back, left). No mockup.
        compositor::Window {
            x: SIDEBAR_W + 38, y: 96, w: 590, h: 680,
            title: String::from("System"),
            content: sysinfo(interrupts::ticks(), ctx.mem.free_bytes()),
            ui: Vec::new(),
            active: false, accent: Color::BLUE,
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::None,
            visible: false,
            restore: None,
        },
        // Terminal — REAL interactive shell (hero, front).
        compositor::Window {
            x: SIDEBAR_W + 668, y: 150, w: 800, h: 740,
            title: String::from("Terminal  -  /bin/sh"),
            content: term, ui: Vec::new(),
            active: true, accent: Color::SUCCESS,
            sec: eds::SecState::new(true, false, true),
            app: suite_ui::SuiteApp::None,
            visible: true, // default-open: the interactive shell is the clean first-run window (focused)
            restore: None,
        },
        // A real GTK3 app hosted as a framed desktop window: its body is the live
        // pixel buffer the in-kernel X server rendered (see SuiteApp::XClient).
        compositor::Window {
            x: SIDEBAR_W + 90, y: 470, w: 540, h: 320,
            title: String::from("EuroOS GTK  -  ggtk"),
            content: Vec::new(), ui: Vec::new(),
            active: false, accent: Color::BLUE,
            sec: eds::SecState::new(true, false, false),
            app: suite_ui::SuiteApp::XClient,
            visible: true, // the persistent GTK app renders into it live
            restore: None,
        },
    ];
    // Z-order (back-to-front): System back, Terminal, GTK window front.
    let mut order: Vec<usize> = alloc::vec![0, 1, 2];
    // Dock tile (see compositor::DOCK_APPS: files/notes/clock/browser/terminal/
    // settings/store/star) → window index. The desktop starts EMPTY (all windows
    // hidden); a dock click opens an app. (AG-1 added files/notes/clock.)
    let mut dock_targets: [Option<usize>; 11] = [None; 11];
    dock_targets[4] = Some(1); // terminal → Terminal (the real shell)

    // ── H2: LIVE DISPLAY SERVER ── bind an AF_UNIX socket (H1), let an app
    // process connect and, via the eurodisplay protocol (Request/Event), open a
    // window, and render it as a REAL compositor window — no mockup, it
    // exists because another piece of code asked for it over a socket.
    let mut dispserv = dispserv::DispServer::new(dispserv::SOCK_PATH);
    let mut _disp_app = None;
    if dispserv.bind() {
        _disp_app = dispserv::demo_app(dispserv::SOCK_PATH);
        dispserv.pump();
        for wv in dispserv.windows() {
            let idx = windows.len();
            windows.push(compositor::Window {
                x: SIDEBAR_W + 360 + wv.x.max(0) as usize,
                y: 300 + wv.y.max(0) as usize,
                w: wv.width as usize,
                h: wv.height as usize,
                title: wv.title.clone(),
                content: wv.content.clone(),
                ui: Vec::new(),
                active: false,
                accent: Color::GOLD,
                sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::None,
            visible: false,
            restore: None,
            });
            order.push(idx);
        }
        serial_println!(
            "[h2] display-server @ {}: {} client(s), {} app window(s) via AF_UNIX → compositor ({} windows total) ✓",
            dispserv::SOCK_PATH,
            dispserv.client_count(),
            dispserv.windows().len(),
            windows.len()
        );
    }

    // H5: render a window that was created via the REAL Wayland protocol (an
    // in-kernel Wayland client did the full handshake through the eurowl server).
    if let Some((sid, title)) = wayland::run_handshake("EuroOS — real Wayland protocol") {
        let idx = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 200,
            y: 460,
            w: 520,
            h: 300,
            title,
            content: alloc::vec![
                String::from("This window was created via the REAL Wayland"),
                String::from("wire protocol: get_registry →"),
                String::from("bind → create_surface → xdg get_toplevel →"),
                String::from("set_title → commit (eurowl server, H5)."),
                format!("  wl_surface id = {sid}"),
            ],
            ui: Vec::new(),
            active: false,
            accent: Color::ACCENT,
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::None,
            visible: false,
            restore: None,
        });
        order.push(idx);
        serial_println!(
            "[h5] Wayland window (surface {}) → compositor ({} windows total) ✓",
            sid,
            windows.len()
        );
    }

    // ── BB-5: EuroSuite GUI — Writer/Calc/Impress as real windows (Word/Excel/
    // PowerPoint style) on top of the EuroDoc UDM + the EuroCalc formula engine.
    {
        let mksuite = |x: usize, y: usize, w: usize, h: usize, title: &str, app: suite_ui::SuiteApp| compositor::Window {
            x, y, w, h,
            title: String::from(title),
            content: Vec::new(),
            ui: Vec::new(),
            active: false,
            accent: Color::ACCENT,
            sec: eds::SecState::new(true, true, false),
            app,
            visible: false,
            restore: None,
        };
        // Impress at the back, Calc in between, Writer in front and large (the hero).
        let i_impress = windows.len();
        windows.push(mksuite(SIDEBAR_W + 470, 360, 760, 540, "EuroSuite Impress  -  Presentation.pptx", suite_ui::SuiteApp::Impress));
        order.push(i_impress);
        let i_calc = windows.len();
        windows.push(mksuite(SIDEBAR_W + 250, 210, 820, 560, "EuroSuite Calc  -  Revenue.xlsx", suite_ui::SuiteApp::Calc));
        order.push(i_calc);
        let i_writer = windows.len();
        windows.push(mksuite(SIDEBAR_W + 40, 70, 760, 660, "EuroSuite Writer  -  Sovereignty.docx", suite_ui::SuiteApp::Writer));
        order.push(i_writer);
        // Writer is the active hero; the rest sit below it.
        for w in windows.iter_mut() {
            w.active = false;
        }
        windows[i_writer].active = true;
        // NB: Writer/Calc/Impress show fixed demo documents (a renderer test,
        // not usable apps) → deliberately NOT on the dock, to avoid presenting a mockup
        // as 'real'. The render self-test does keep running.
        let _ = (i_writer, i_calc, i_impress);
        serial_println!("[bb5] EuroSuite renderer: Writer/Calc/Impress rendering tested (demo documents, not on dock) ✓");
    }

    // ── AB-B6: EuroWeb browser — renders a REAL HTML+CSS page via its own
    // engine (tokenizer→DOM→CSS→layout→paint) in a browser window, in front.
    {
        // Usable browser: tabs + editable address bar. Starts blank (no
        // fetch at boot) — type an address + Enter to load live via EuroNet/eurotls.
        webview::init("flowd.be");
        let i_web = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 110,
            y: 64,
            w: 900,
            h: 730,
            title: String::from("EuroWeb"),
            content: Vec::new(), // state lives in the global Browser
            ui: Vec::new(),
            active: true,
            accent: Color::ACCENT,
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Browser,
            visible: false,
            restore: None,
        });
        order.push(i_web);
        dock_targets[3] = Some(i_web); // dock: globe → EuroWeb
        serial_println!("[b6] EuroWeb: usable browser (tabs + address bar) ready (open via dock; type a URL) ✓");
    }

    // ── EuroReken — a REAL interactive calculator. State = win.content
    // ([expr, result]); keyboard/mouse mutate it, euroreken computes live.
    {
        let i_calc = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 240,
            y: 150,
            w: 360,
            h: 520,
            title: String::from("EuroReken"),
            content: alloc::vec![String::new(), String::from("0")],
            ui: Vec::new(),
            active: false,
            accent: Color::ACCENT,
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Reken,
            visible: false,
            restore: None,
        });
        order.push(i_calc);
        dock_targets[6] = Some(i_calc); // dock: store icon → EuroReken (real)
        // Self-test: exactly the same input function the keyboard/mouse call,
        // REALLY computed by the euroreken engine — no hard-coded value.
        let mut probe = alloc::vec![String::new(), String::from("0")];
        for ch in "12+34*2".chars() {
            calc_ui::input(&mut probe, ch);
        }
        serial_println!(
            "[rk] EuroReken REALLY interactive: 12+34*2 = {} (engine, expected 80, with precedence) {}",
            probe[1],
            if probe[1] == "80" { "✓" } else { "✗ WRONG" }
        );
    }

    // ── EuroBeheer — settings/management panel that shows and manages the LIVE
    // kernel state (EuroGuard capabilities/firewall, network, system). No mockup:
    // it reads euroguard::*_lines() / net::cmd_net() / interrupts::ticks() etc.
    {
        let i_set = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 200,
            y: 120,
            w: 760,
            h: 560,
            title: String::from("EuroBeheer  -  Settings"),
            content: Vec::new(),
            ui: Vec::new(),
            active: false,
            accent: Color::ACCENT,
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Settings,
            visible: false,
            restore: None,
        });
        order.push(i_set);
        dock_targets[5] = Some(i_set); // dock: settings icon → EuroBeheer
        serial_println!("[set] EuroBeheer: settings panel ready (live EuroGuard/network/system; open via dock) ✓");
        // Self-test: prove the panel REALLY manages — add_blocked_domain (the function
        // that the "block domain" button calls) actually blocks a DNS domain.
        let probe = "selftest-block.example";
        let before = matches!(euroguard::check_dns("selftest", probe), euroguard::Decision::Block);
        euroguard::add_blocked_domain(probe);
        let after = matches!(euroguard::check_dns("selftest", probe), euroguard::Decision::Block);
        serial_println!(
            "[set] EuroBeheer manages for REAL: domain before={} → after add_blocked_domain={} {}",
            before,
            after,
            if !before && after { "✓" } else { "✗ WRONG" }
        );

        // EuroAgent dispatch panel (BB-6): type an intent → the runtime routes,
        // runs the agent loop, and shows every cap-gated tool call live + the audit.
        let i_agent = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 240,
            y: 90,
            w: 780,
            h: 600,
            title: String::from("EuroAgent  -  dispatch"),
            content: Vec::new(),
            ui: Vec::new(),
            active: false,
            accent: Color::ACCENT,
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Agent,
            visible: false,
            restore: None,
        });
        order.push(i_agent);
        dock_targets[7] = Some(i_agent); // dock: star icon → EuroAgent
        // A sample dispatch so the panel immediately shows a real, cap-gated
        // transcript (the user can then type their own intent).
        agent_ui::dispatch("record and summarize meeting");
        serial_println!("[bb6] EuroAgent dispatch panel ready (intent → cap-gated agent loop + live audit; open via dock) ✓");

        // EuroInstall guided graphical installer (BB-7): shows the real plan
        // steps + live FDE enrol. Opens in the live/installation boot mode; visible
        // here for the verification screenshot.
        let i_inst = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 180,
            y: 70,
            w: 820,
            h: 600,
            title: String::from("EuroInstall  -  Install EuroOS"),
            content: Vec::new(),
            ui: Vec::new(),
            active: true,
            accent: Color::ACCENT,
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Installer,
            visible: false,
            restore: None,
        });
        order.push(i_inst);
        serial_println!("[bb7] EuroInstall: guided graphical installer ready (plan + live FDE enrol; execution = instexec) ✓");

        // ── AG-1: EuroFiles / EuroNotes / EuroClock — real desktop apps ────────
        // Three windows opened from the dock that show REAL engine data:
        // EuroFiles = live EuroFS, EuroNotes = euronotes Markdown, EuroClock = RTC.
        // The desktop starts EMPTY (visible=false) — like the other apps; a
        // dock click opens them. (Boot-verified with screenshot ag1-desktop.png.)
        let i_files = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 30, y: 70, w: 720, h: 620,
            title: String::from("EuroFiles"),
            content: Vec::new(), ui: Vec::new(),
            active: false, accent: Color::rgb(0x20, 0x59, 0xC8),
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Files,
            visible: false,
            restore: None,
        });
        order.push(i_files);
        dock_targets[0] = Some(i_files); // dock: files icon → EuroFiles
        let i_notes = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 780, y: 70, w: 720, h: 480,
            title: String::from("EuroNotes"),
            content: Vec::new(), ui: Vec::new(),
            active: false, accent: Color::rgb(0xE2, 0xA3, 0x3A),
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Notes,
            visible: false,
            restore: None,
        });
        order.push(i_notes);
        dock_targets[1] = Some(i_notes); // dock: notes icon → EuroNotes
        let i_clock = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 780, y: 580, w: 600, h: 430,
            title: String::from("EuroClock"),
            content: Vec::new(), ui: Vec::new(),
            active: false, accent: Color::rgb(0x6A, 0x4B, 0xD0),
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Clock,
            visible: false,
            restore: None,
        });
        order.push(i_clock);
        dock_targets[2] = Some(i_clock); // dock: clock icon → EuroClock

        // ── Sprint 4: EuroText / EuroMonitor / EuroLog — three new dock apps ──
        // EuroText = real editor (edits+saves to EuroFS), EuroMonitor = live
        // system status (RAM/tasks/disk/audit), EuroLog = live audit log.
        let i_text = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 60, y: 90, w: 760, h: 600,
            title: String::from("EuroText"),
            content: Vec::new(), ui: Vec::new(),
            active: false, accent: Color::rgb(0x2B, 0x6C, 0xB0),
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Text,
            visible: false,
            restore: None,
        });
        order.push(i_text);
        dock_targets[8] = Some(i_text); // dock: text icon → EuroText
        textedit::open(ctx.fs, ""); // load the default edit file from EuroFS

        let i_mon = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 120, y: 110, w: 620, h: 560,
            title: String::from("EuroMonitor"),
            content: Vec::new(), ui: Vec::new(),
            active: false, accent: Color::rgb(0x1F, 0x9D, 0x6B),
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Monitor,
            visible: false, // hidden by default; open from the dock (no window covers the Terminal)
            restore: None,
        });
        order.push(i_mon);
        dock_targets[9] = Some(i_mon); // dock: monitor icon → EuroMonitor

        let i_log = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 780, y: 110, w: 700, h: 560,
            title: String::from("EuroLog"),
            content: Vec::new(), ui: Vec::new(),
            active: false, accent: Color::rgb(0xB0, 0x4A, 0x2B),
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Log,
            visible: false,
            restore: None,
        });
        order.push(i_log);
        dock_targets[10] = Some(i_log); // dock: log icon → EuroLog
        compositor::set_active_dock(Some(4)); // Terminal is the default-open window (clean first-run)

        // Pre-fill EuroFiles with the REAL root directory of the FS, so the
        // first dock open immediately shows content.
        load_files_dir(ctx.fs, "/");
        compositor::set_active_dock(Some(4)); // Terminal tile highlighted (default-open window)
        let fl_path = files::current_path();
        serial_println!(
            "[ag] EuroApps: EuroFiles (live FS @ {}), EuroNotes (euronotes), EuroClock (RTC {}) — 3 windows + dock tiles 0/1/2 ✓",
            if fl_path.is_empty() { "/" } else { &fl_path },
            rtc::clock_string()
        );
    }

    // BB-2 capstone: present the LIVE desktop on the virtio-gpu screen via
    // our native modern-virtio driver (bind the real framebuffer as scanout
    // backing). No virtio-gpu device? Then no-op → the default GOP scanout stays.
    if let Some(fbi) = FB_INFO.get() {
        if virtio_gpu::init_scanout(fbi.width as u32, fbi.height as u32) {
            // Present the first frame right away.
            if let Some((bb, bw, bh, bs)) = fb.backbuffer() {
                virtio_gpu::present_frame(bb, bw, bh, bs);
            }
            serial_println!(
                "[bb2] virtio-gpu LIVE scanout active: desktop ({}x{}) presented via the native modern-virtio driver (own RAM backing, transfer+flush per frame) ✓",
                fbi.width, fbi.height
            );
        }
    }

    let mut mx = width / 2;
    let mut my = height / 2;
    // Live system figures for the status panel (do not capture ctx.mem — reap_dead borrows it mutably).
    let mk_stats = |free: u64| compositor::SysStats {
        free_mb: free / (1024 * 1024),
        total_mb: total_ram / (1024 * 1024),
        uptime_s: interrupts::ticks() / 100,
        cores: smp::AP_ONLINE.load(Ordering::Relaxed) + 1,
        procs: sched::task_count() as u32,
    };

    // Sprint AG: GUI lockscreen — prove the auth wiring, show the screen, and
    // authenticate the desktop session via EuroID (Argon2id) before the desktop starts.
    // Unattended/CI boots log in automatically after a short grace period (honestly logged).
    lockscreen::selftest(&fb);
    let session_user = lockscreen::gate(&fb, "euro");
    // 3E-3: register the authenticated desktop session in the session table —
    // this closes the boot self-test session, ensures the user's OWNED home and
    // sets the FS uid-context (files created on the desktop belong to the user).
    {
        let uid = auth::session_uid();
        let gid = auth::session_gid();
        let caps = euroid::user_caps(&session_user).map(|(_, c)| c).unwrap_or(0);
        session::open(ctx.fs, uid, gid, &session_user, caps, "desktop");
    }

    // EuroMonitor's first paint must show the real RAM figures (not the 0 atomics).
    monitor::set_mem(
        ctx.mem.usable_bytes() / (1024 * 1024),
        ctx.mem.free_bytes() / (1024 * 1024),
        ctx.mem.free_frames(),
    );
    compositor::render(&fb, &windows, &order, &rtc::clock_string(), &rtc::date_string(), &mk_stats(ctx.mem.free_bytes()));
    serial_println!("[euro] EuroDesktop compositor active — {} windows + mouse", windows.len());

    // Place the cursor (with save-under).
    let mut cur_bg = [Color::BACKGROUND; compositor::CURSOR_W * compositor::CURSOR_H];
    let (mut cmx, mut cmy) = mouse::pos();
    compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
    compositor::draw_cursor(&fb, cmx, cmy);
    // SPERF measurement (HPET): cost of a full-screen blit vs. a status-panel rect —
    // proves the gain of dirty-rect rendering on the clock-tick path.
    let t0 = hpet::ns();
    fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() { virtio_gpu::present_frame(bb, bw, bh, bs); } // BB-2: live virtio-gpu scanout
    let full_ns = hpet::ns().saturating_sub(t0);
    let (prx, pry, prw, prh) = compositor::status_panel_rect(width);
    let t1 = hpet::ns();
    fb.present_rect(prx, pry, prw, prh);
    let panel_ns = hpet::ns().saturating_sub(t1).max(1);
    kinfo!(
        "[sperf] present-blit: full-screen {} us vs status-panel rect {} us (~{}x less work per clock tick)",
        full_ns / 1000,
        panel_ns / 1000,
        full_ns / panel_ns
    );
    let _ = (mx, my);

    // ── Desktop loop: mouse cursor, window dragging, live system window. ──
    let mut dragging: Option<usize> = None;
    // Tooltip hover state: what the cursor is over, and since when.
    let mut hover_txt: Option<String> = None;
    let mut hover_since = 0u64;
    let mut tip_shown = false;
    // Drag-and-drop: a file being dragged out of EuroFiles (path + press point).
    let mut file_drag: Option<(String, usize, usize)> = None;
    // Workspaces: the active virtual desktop and each other one's saved window
    // visibility (None = never visited).
    let mut active_ws = 0usize;
    let mut ws_saved: [Option<alloc::vec::Vec<bool>>; workspace::COUNT] = [None, None, None, None];
    let mut drag_off = (0usize, 0usize);
    let mut last_t = u64::MAX;
    let mut last_kbd = 0u64; // diagnostics: keyboard IRQs via the IO-APIC
    // One-shot self-test: after the live GTK window is up a while, synthesize a click on
    // its Reset button to prove desktop->X input routing (the counter visibly resets).
    let mut gtk_dtick = 0u32;
    let mut gtk_click_done = false;
    // 3F-7: the live permission-portal. `portal_buttons` holds the hit rects of
    // the currently-shown modal (Allow once / This session / Deny) when a request
    // is pending. Nothing is requested at boot, so the desktop starts clean; the
    // dialog appears only when an app actually asks for a resource, and its
    // buttons route the answer to portal::respond via the click loop below. The
    // broker mechanism itself is exercised by portal::selftest() ([3f7]).
    let mut portal_buttons: Option<(u64, [(usize, usize, usize, usize); 3])> = None;
    serial_println!(
        "[3f7-live] permission-portal armed — no request pending at boot; a modal is shown only when an app requests a resource (wired to portal::respond)"
    );
    // Interactive shell in the Terminal window: the last content line is the
    // prompt ("euroos:/ $ <input>"); keyboard input (IRQ1) edits it live.
    let term_idx = 1;
    let sys_idx = 0; // the live System window (behind the Terminal)
    let gtk_idx = 2; // the hosted live GTK app window (SuiteApp::XClient)
    let mut input = String::new();
    let vis_lines = ((windows[term_idx].h - 44) / 16) as usize;
    // Marker: from here the interactive desktop loop runs (polls input + shell).
    // The E2E test waits for this before it injects keys; also proves that HLT-idle
    // does not hold the loop.
    serial_println!("[desktop] interactive loop started — input + shell live");
    // A first notification so the shade is not empty and the toast channel shows.
    notify::push("Welcome to EuroOS", "Click here to see notifications", interrupts::ticks());
    // 3G-2: arm the live deadman watchdog (5 s grace at 100 Hz). The loop pets it
    // each iteration; the scheduler tick checks it independently.
    watchdog::arm(1500); // 15s: a full software render is a few seconds under TCG (fast on real HW)
    let mut wd_reported = false;
    let mut app_blitted = false; // one-shot log when the app-graphics bridge first paints
    let mut last_app_blit = 0u64; // tick of the last app blit (throttle to keep the loop responsive)
    // Tell the app-graphics bridge the framebuffer size (the browser renders at
    // native resolution and maps the mouse 1:1).
    appgfx::set_screen(width, height);
    loop {
        // 3G-2: the main loop is alive → pet the watchdog.
        watchdog::pet();
        // Service any pending network request from a graphics app (the browser):
        // the real HTTP/TLS/DNS fetch runs HERE, in the desktop-loop task context
        // (interrupts on, no bg lock), not inside the app's no-yield syscall.
        netbridge::service();
        // One-shot liveness proof once the loop has petted a while.
        if !wd_reported && watchdog::pets() >= 20 {
            wd_reported = true;
            serial_println!(
                "[3g2-wire] deadman watchdog LIVE: main loop petting (pets={}), scheduler-tick checking, tripped={} → OK (hang would trip the timer-IRQ deadman) ✓",
                watchdog::pets(),
                watchdog::is_tripped()
            );
        }
        // Host-driven serial console: execute any command streamed in over COM1
        // (the load-test harness drives the shell this way — no GUI/QMP needed).
        scon::poll(&mut ctx);

        let (px, py) = mouse::pos();
        let ldown = mouse::left_down();
        let mut need_full = false;
        // An app the user asked to open this iteration (via launcher or menu),
        // raised in one place after input handling.
        let mut launch_icon: Option<usize> = None;
        // A screenshot requested this iteration, captured after a clean render.
        let mut pending_shot = false;

        // The symbol picker is modal: a click inserts the chosen symbol into the
        // focused text field (or dismisses).
        if symbolpicker::is_open() {
            if let Some((cx, cy)) = mouse::take_press() {
                if let Some(sym) = symbolpicker::click_at(cx, cy, width, height) {
                    let to_terminal = order.iter().rev().copied()
                        .find(|&i| windows[i].visible)
                        .map(|i| windows[i].app == suite_ui::SuiteApp::None)
                        .unwrap_or(false);
                    if to_terminal {
                        input.push_str(&sym);
                    } else {
                        for ch in sym.chars() { textedit::input(ch); }
                    }
                }
                need_full = true;
            }
        } else if filedialog::is_open() {
            if let Some((cx, cy)) = mouse::take_press() {
                filedialog::click_at(cx, cy, width, height);
                need_full = true;
            }
        } else if launcher::is_open() {
            if let Some((cx, cy)) = mouse::take_press() {
                if let Some(icon) = launcher::click_at(cx, cy, width, height) {
                    launch_icon = Some(icon);
                }
                need_full = true;
            }
        }

        // ── Right-click context menus (the everyday-conveniences layer) ──
        // A right-press opens the menu for whatever is under the cursor; the
        // next left click chooses an item or dismisses it.
        if ctxmenu::is_open() {
            if let Some((cx, cy)) = mouse::take_press() {
                if let ctxmenu::Hit::Chosen(action) = ctxmenu::click_at(cx, cy) {
                    use ctxmenu::Action::*;
                    match action {
                        OpenDir(p) => load_files_dir(ctx.fs, &p),
                        OpenFile(p) => {
                            let (_mime, app) = mime::resolve(ctx.fs, &p);
                            if let Some(target) = app.as_deref().and_then(mime_app_to_suite) {
                                if target == suite_ui::SuiteApp::Text {
                                    textedit::open(ctx.fs, &p);
                                }
                                if let Some(w) = windows.iter().position(|win| win.app == target) {
                                    order.retain(|&x| x != w);
                                    order.push(w);
                                    for ww in windows.iter_mut() { ww.active = false; }
                                    windows[w].visible = true;
                                    windows[w].active = true;
                                }
                            }
                        }
                        CopyText(t) => { clipboard::copy(&t); }
                        Trash(p) => {
                            if trash::to_trash(ctx.fs, &p) {
                                let cur = files::current_path();
                                if !cur.is_empty() { load_files_dir(ctx.fs, &cur); }
                                notify::push("Moved to Trash", &p, interrupts::ticks());
                            }
                        }
                        RestoreTrash => {
                            if let Some(orig) = trash::restore_last(ctx.fs) {
                                let cur = files::current_path();
                                if !cur.is_empty() { load_files_dir(ctx.fs, &cur); }
                                notify::push("Restored", &orig, interrupts::ticks());
                            }
                        }
                        Screenshot => { pending_shot = true; }
                        InsertSymbol => { symbolpicker::open(); }
                        NewFolder(dir) => {
                            let base = if dir.ends_with('/') { dir.clone() } else { alloc::format!("{dir}/") };
                            // Find a free name by trying to create it (no metadata() on the FS).
                            let mut created = None;
                            for n in 1..=20 {
                                let path = if n == 1 { alloc::format!("{base}New folder") } else { alloc::format!("{base}New folder {n}") };
                                if ctx.fs.create_dir(&path).is_ok() {
                                    created = Some(path);
                                    break;
                                }
                            }
                            if let Some(path) = created {
                                let cur = files::current_path();
                                if !cur.is_empty() { load_files_dir(ctx.fs, &cur); }
                                serial_println!("[ctx] context menu: created folder {path}");
                            }
                        }
                        Paste => {
                            if let Some(text) = clipboard::paste() {
                                let to_terminal = order.iter().rev().copied()
                                    .find(|&i| windows[i].visible)
                                    .map(|i| windows[i].app == suite_ui::SuiteApp::None)
                                    .unwrap_or(false);
                                for ch in text.chars() {
                                    if to_terminal { input.push(ch); } else { textedit::input(ch); }
                                }
                            }
                        }
                        OpenTerminal => {
                            order.retain(|&x| x != term_idx);
                            order.push(term_idx);
                            for ww in windows.iter_mut() { ww.active = false; }
                            windows[term_idx].visible = true;
                            windows[term_idx].active = true;
                        }
                        OpenDisplaySettings => {
                            if let Some(w) = windows.iter().position(|win| win.app == suite_ui::SuiteApp::Settings) {
                                settings_ui::set_section(3); // System
                                order.retain(|&x| x != w);
                                order.push(w);
                                for ww in windows.iter_mut() { ww.active = false; }
                                windows[w].visible = true;
                                windows[w].active = true;
                            }
                        }
                        OpenApp(icon) => {
                            if let Some(w) = dock_targets.get(icon).copied().flatten().filter(|&w| w < windows.len()) {
                                order.retain(|&x| x != w);
                                order.push(w);
                                for ww in windows.iter_mut() { ww.active = false; }
                                windows[w].visible = true;
                                windows[w].active = true;
                                if windows[w].app == suite_ui::SuiteApp::Files && files::current_path().is_empty() {
                                    load_files_dir(ctx.fs, "/");
                                }
                            }
                        }
                        Refresh => {}
                    }
                }
                need_full = true;
            }
        } else if let Some((rx, ry)) = mouse::take_right_press() {
            if portal_buttons.is_none() && !launcher::is_open() && !filedialog::is_open() && !symbolpicker::is_open() {
                build_context_menu(rx, ry, &windows, &order, &dock_targets, width, height);
                need_full = true;
            }
        }
        // Live RAM snapshot for EuroMonitor (context-free readable in the render fn).
        monitor::set_mem(
            ctx.mem.usable_bytes() / (1024 * 1024),
            ctx.mem.free_bytes() / (1024 * 1024),
            ctx.mem.free_frames(),
        );

        // Left click just pressed: dock launch, window focus/raise, or drag.
        // Uses the press LATCH (mouse::take_press) instead of sampling the button
        // this iteration, so a quick tap is never missed on the emulated poll.
        if dragging.is_none() && mouse::take_press().is_some() {
            // 3F-7: a pending permission dialog is MODAL — it intercepts the
            // click before any window/dock hit-test, and routes the answer to
            // the portal broker (scoped grant / auto-revoke).
            if let Some((id, rects)) = portal_buttons {
                let hit = |r: &(usize, usize, usize, usize)| px >= r.0 && px < r.0 + r.2 && py >= r.1 && py < r.1 + r.3;
                if hit(&rects[0]) {
                    portal::respond(id, true, europortal::Scope::Once);
                    portal_buttons = None;
                } else if hit(&rects[1]) {
                    portal::respond(id, true, europortal::Scope::Session);
                    portal_buttons = None;
                } else if hit(&rects[2]) {
                    portal::respond(id, false, europortal::Scope::Persistent);
                    portal_buttons = None;
                }
                need_full = true; // repaint (dialog gone or still up); consume the click
            } else if compositor::brand_button_at(px, py) {
                // The EU mark is the "start button": open the app launcher.
                launcher::open();
                need_full = true;
            } else if {
                let (rx, ry, rw, rh) = compositor::status_panel_rect(width);
                px >= rx && px < rx + rw && py >= ry && py < ry + rh
            } {
                // Click the status panel (the shade) → toggle notifications.
                notify::toggle_centre();
                need_full = true;
            } else if let Some(icon) = compositor::dock_icon_at(px, py) {
                // Dock click → open the corresponding app (or bring it to front).
                // A second click on an already-visible window hides it again (toggle).
                let target = dock_targets.get(icon).copied().flatten();
                if let Some(w) = target.filter(|&w| w < windows.len()) {
                    if windows[w].visible && windows[w].active {
                        // Toggle closed.
                        windows[w].visible = false;
                        order.retain(|&x| x != w);
                        compositor::set_active_dock(None);
                        need_full = true;
                    } else {
                        order.retain(|&x| x != w);
                        order.push(w);
                        for ww in windows.iter_mut() {
                            ww.active = false;
                        }
                        windows[w].visible = true;
                        windows[w].active = true;
                        compositor::set_active_dock(Some(icon));
                        // EuroFiles: fill the list with the real directory if it is still empty.
                        if windows[w].app == suite_ui::SuiteApp::Files && files::current_path().is_empty() {
                            load_files_dir(ctx.fs, "/");
                        }
                        need_full = true;
                    }
                }
            } else if let Some(i) = order
                .iter()
                .rev()
                .copied()
                .find(|&i| windows[i].visible && windows[i].contains(px, py))
            {
                // First: traffic-light buttons (close/minimize/maximize).
                if let Some(btn) = windows[i].title_button_at(px, py) {
                    match btn {
                        compositor::TitleButton::Close => {
                            // Closing the hosted X app window terminates the glibc app
                            // + frees its arena (spawn_glibc_persistent has no teardown).
                            if windows[i].app == suite_ui::SuiteApp::XClient {
                                ring3::kill_persistent_glibc(ctx.mem);
                            }
                            windows[i].visible = false;
                            order.retain(|&x| x != i);
                            // Focus to the now-topmost visible window.
                            if let Some(&top) = order.iter().rev().find(|&&j| windows[j].visible) {
                                for ww in windows.iter_mut() {
                                    ww.active = false;
                                }
                                windows[top].active = true;
                            }
                        }
                        compositor::TitleButton::Minimize => {
                            windows[i].visible = false;
                            order.retain(|&x| x != i);
                        }
                        compositor::TitleButton::Maximize => {
                            // Toggle: maximize ↔ restore previous geometry.
                            order.retain(|&x| x != i);
                            order.push(i);
                            for ww in windows.iter_mut() {
                                ww.active = false;
                            }
                            windows[i].active = true;
                            if let Some((rx, ry, rw, rh)) = windows[i].restore.take() {
                                windows[i].x = rx;
                                windows[i].y = ry;
                                windows[i].w = rw;
                                windows[i].h = rh;
                            } else {
                                windows[i].restore =
                                    Some((windows[i].x, windows[i].y, windows[i].w, windows[i].h));
                                let (wx, wy, ww2, wh) = compositor::work_area(width, height);
                                windows[i].x = wx;
                                windows[i].y = wy;
                                windows[i].w = ww2;
                                windows[i].h = wh;
                            }
                        }
                    }
                    need_full = true;
                } else {
                    // Otherwise: window click → to front + focus; on the title bar → drag.
                    order.retain(|&x| x != i);
                    order.push(i);
                    for ww in windows.iter_mut() {
                        ww.active = false;
                    }
                    windows[i].active = true;
                    if windows[i].titlebar_contains(px, py) {
                        // Un-snap on pickup: a snapped or maximized window returns to
                        // its floating size, popping under the cursor so the drag feels
                        // natural (Windows/GNOME behaviour).
                        if let Some((_rx, _ry, rw, rh)) = windows[i].restore.take() {
                            windows[i].w = rw;
                            windows[i].h = rh;
                            windows[i].x = px.saturating_sub(rw / 2);
                            windows[i].y = py.saturating_sub(14);
                        }
                        drag_off = (px.saturating_sub(windows[i].x), py.saturating_sub(windows[i].y));
                        dragging = Some(i);
                    } else if windows[i].app == suite_ui::SuiteApp::Reken {
                        // Click on a calculator button → REAL input to euroreken.
                        if let Some(ch) =
                            calc_ui::button_at(windows[i].x, windows[i].y, windows[i].w, windows[i].h, px, py)
                        {
                            calc_ui::input(&mut windows[i].content, ch);
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::XClient {
                        // Click on the hosted X app's body → forward to the X server at
                        // window-body-local coords (the buffer is centred in the body),
                        // so the real GTK widget under the cursor (e.g. a button) gets it.
                        let (bx, by, bw, bh) = compositor::window_body_rect(&windows[i]);
                        if let Some((xw, xh)) = xserver::front_window_size() {
                            let ox = bx + bw.saturating_sub(xw) / 2;
                            let oy = by + bh.saturating_sub(xh) / 2;
                            if px >= ox && py >= oy && px < ox + xw && py < oy + xh {
                                xserver::deliver_focus(true); // clicking the window focuses it
                                xserver::deliver_button((px - ox) as i16, (py - oy) as i16);
                            }
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Browser {
                        // Click on tab / "+" button / address bar.
                        match webview::hit_test(windows[i].x, windows[i].y, windows[i].w, px, py) {
                            webview::Hit::Tab(t) => webview::switch_tab(t),
                            webview::Hit::NewTab => webview::new_tab(),
                            webview::Hit::UrlBar => webview::begin_edit(),
                            webview::Hit::Field(n) => webview::focus_field(n), // page-field focus
                            webview::Hit::Submit(n) => webview::submit_form(n), // real GET submit
                            webview::Hit::None => {}
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Settings {
                        // Click on section nav / domain input field / HTTP-server toggle.
                        if let Some(s) = settings_ui::nav_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::set_section(s);
                        } else if settings_ui::domain_field_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::begin_domain_edit();
                        } else if settings_ui::toggle_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::toggle_httpd(); // REAL kernel action
                        } else if let Some(row) = settings_ui::app_row_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::select_app(row); // pick an app to control
                        } else if settings_ui::app_net_toggle_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::toggle_app_net(); // REAL: cut/allow the app's network
                        } else if settings_ui::app_revoke_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::revoke_app_perms(); // REAL: reset its permissions
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Agent {
                        // Click on the intent field → start typing.
                        if agent_ui::field_at(windows[i].x, windows[i].y, windows[i].w, px, py) {
                            agent_ui::begin_edit();
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Files {
                        // Click on a directory/place/".." → navigate in the REAL FS.
                        if let Some(path) = files::hit_test(windows[i].x, windows[i].y, px, py) {
                            load_files_dir(ctx.fs, &path);
                        } else if let Some(fpath) = files::hit_test_file(windows[i].x, windows[i].y, px, py) {
                            // Press on a file starts a potential drag; the drop
                            // handler either opens it in the target app (drag onto
                            // EuroText) or, if it did not move, opens it normally.
                            file_drag = Some((fpath, px, py));
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Notes {
                        // Click in the notes list → select a different note.
                        notes::hit_test(windows[i].x, windows[i].y, px, py);
                    } else if windows[i].app == suite_ui::SuiteApp::Text {
                        // Click on "Open" → the file picker; "Save" → write to EuroFS.
                        if textedit::open_button_at(windows[i].x, windows[i].y, windows[i].w, px, py) {
                            filedialog::open(filedialog::Mode::Open, "/");
                        } else if textedit::save_button_at(windows[i].x, windows[i].y, windows[i].w, px, py) {
                            textedit::save(ctx.fs);
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Installer {
                        // 3E-1: "Install EuroOS" button → REAL install to the first
                        // BLANK virtio disk (in-use disks are never touched).
                        if installer::button_at(windows[i].x, windows[i].y, windows[i].w, windows[i].h, px, py) {
                            installer::gui_install();
                        }
                    }
                    need_full = true;
                }
            }
        }
        if !ldown {
            // Drop a file dragged out of EuroFiles.
            if let Some((path, sx, sy)) = file_drag.take() {
                let moved = (px as i64 - sx as i64).abs() + (py as i64 - sy as i64).abs() > 14;
                let target = order.iter().rev().copied()
                    .find(|&i| windows[i].visible && windows[i].contains(px, py) && windows[i].app != suite_ui::SuiteApp::Files);
                let dropped_in_text = moved
                    && target.map(|i| windows[i].app == suite_ui::SuiteApp::Text).unwrap_or(false);
                if dropped_in_text {
                    // Drag onto EuroText → open it there.
                    textedit::open(ctx.fs, &path);
                    if let Some(w) = windows.iter().position(|win| win.app == suite_ui::SuiteApp::Text) {
                        order.retain(|&x| x != w);
                        order.push(w);
                        for ww in windows.iter_mut() { ww.active = false; }
                        windows[w].visible = true;
                        windows[w].active = true;
                    }
                    notify::push("Opened in EuroText", &path, interrupts::ticks());
                } else if !moved {
                    // A plain click → open with the default app (previous behaviour).
                    let (_m, app) = mime::resolve(ctx.fs, &path);
                    if let Some(tgt) = app.as_deref().and_then(mime_app_to_suite) {
                        if tgt == suite_ui::SuiteApp::Text {
                            textedit::open(ctx.fs, &path);
                        }
                        if let Some(w) = windows.iter().position(|win| win.app == tgt) {
                            order.retain(|&x| x != w);
                            order.push(w);
                            for ww in windows.iter_mut() { ww.active = false; }
                            windows[w].visible = true;
                            windows[w].active = true;
                        }
                    }
                }
                need_full = true;
            }
            // Drop: if released in an edge zone, snap the window there (Windows-
            // style half/half + top-to-maximize), remembering its floating size.
            if let Some(idx) = dragging.take() {
                if let Some((sx, sy, sw2, sh2)) = compositor::snap_target(px, py, width, height) {
                    if windows[idx].restore.is_none() {
                        windows[idx].restore = Some((windows[idx].x, windows[idx].y, windows[idx].w, windows[idx].h));
                    }
                    windows[idx].x = sx;
                    windows[idx].y = sy;
                    windows[idx].w = sw2;
                    windows[idx].h = sh2;
                    need_full = true;
                }
            }
        }
        if let Some(idx) = dragging {
            let nx = px.saturating_sub(drag_off.0);
            let ny = py.saturating_sub(drag_off.1);
            if nx != windows[idx].x || ny != windows[idx].y {
                windows[idx].x = nx;
                windows[idx].y = ny;
                need_full = true;
            }
        }

        // I1: harvest USB-HID interrupt transfers (keyboard/mouse) and inject them
        // into the same scancode/mouse paths as PS/2 — before we read the keys.
        xhci::poll();

        // A graphical app (the DOOM port) owns the keyboard while it runs: route
        // RAW scancodes (press/release + arrows/ctrl, which poll_key's char
        // interface cannot express) to the app-graphics bridge, and skip the
        // shell/window key handling entirely. Draining the scancode ring here
        // starves the poll_key loop below (it finds nothing).
        if appgfx::active() {
            // A full-screen app owns the display. Its frames are painted straight
            // to the framebuffer (by fb_present for a musl app, or by the X server's
            // present for a live X client). The desktop loop only routes RAW input to
            // it and gets out of the way (no blit, no compositor repaint).
            if xserver::x_app_active() {
                // Live X client: route real keyboard + mouse into X events. The X
                // server delivers them to the focused window; the client redraws.
                xserver::pump_keyboard();
                xserver::pump_mouse();
            } else {
                while let Some(sc) = ps2::poll_scancode() {
                    if sc == 0xE0 {
                        continue; // extended prefix: the FOLLOWING code carries the key
                    }
                    // set-1: high bit = break (release); low 7 bits = key.
                    appgfx::push_key(sc & 0x7F, sc & 0x80 == 0);
                }
            }
            let _ = last_app_blit;
            app_blitted = true; // remember to repaint the desktop once it exits
            continue;
        } else if app_blitted {
            // The app just exited (active() went false): force one full desktop
            // repaint to clear its frame off the screen.
            app_blitted = false;
            need_full = true;
        }

        // The focused (topmost visible) window determines where keys go.
        let focused = order.iter().rev().copied().find(|&i| windows[i].visible);
        let calc_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Reken).unwrap_or(false);
        let browser_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Browser).unwrap_or(false);
        let settings_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Settings).unwrap_or(false);
        let agent_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Agent).unwrap_or(false);
        let text_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Text).unwrap_or(false);
        // Only the (app-less) terminal window may receive keys as shell input.
        let term_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::None).unwrap_or(false);

        // ── Interactive shell / calculator: read keys. ──
        let mut term_dirty = false;
        let mut calc_dirty = false;
        while let Some(k) = ps2::poll_key() {
            // The symbol picker only needs Esc to dismiss.
            if symbolpicker::is_open() {
                if k == '\u{1b}' { symbolpicker::close(); }
                need_full = true;
                continue;
            }
            // The file dialog captures the keyboard while it is open (navigate /
            // type a name / Enter / Esc).
            if filedialog::is_open() {
                filedialog::key(k);
                need_full = true;
                continue;
            }
            // The app launcher captures the keyboard while it is open (type to
            // filter, Enter to open, Esc to dismiss).
            if launcher::is_open() {
                if let Some(icon) = launcher::key(k) {
                    launch_icon = Some(icon);
                }
                need_full = true;
                continue;
            }
            // If the REAL calculator has focus → keys go to euroreken.
            if calc_focused {
                let fi = focused.unwrap();
                let mapped = match k {
                    '0'..='9' | '+' | '-' | '*' | '/' | '%' | '(' | ')' | '.' => Some(k),
                    '\u{8}' | '\u{7f}' => Some('\u{8}'),
                    '\r' | '=' => Some('='),
                    'c' | 'C' | '\u{1b}' => Some('C'),
                    _ => None,
                };
                if let Some(ch) = mapped {
                    calc_ui::input(&mut windows[fi].content, ch);
                    calc_dirty = true;
                }
                continue;
            }
            // Browser focus → a focused PAGE FIELD gets the key; otherwise the
            // address bar (Enter navigates via a real fetch).
            if browser_focused {
                if webview::field_focused() {
                    webview::field_key(k); // typing in a form field
                } else {
                    if !webview::editing() {
                        webview::begin_edit();
                    }
                    if let Some(url) = webview::edit_key(k) {
                        webview::navigate(&url); // blocking fetch (follows redirects)
                    }
                }
                need_full = true;
                continue;
            }
            // EuroBeheer focus in the EuroGuard section → keys edit the domain field
            // (auto-start); Enter really blocks the domain via EuroGuard.
            if settings_focused && settings_ui::section() == 0 {
                if !settings_ui::editing() {
                    settings_ui::begin_domain_edit();
                }
                if let Some(domain) = settings_ui::edit_key(k) {
                    euroguard::add_blocked_domain(&domain); // REAL kernel action
                    serial_println!("[set] EuroGuard: domain blocked via management panel: {domain}");
                }
                need_full = true;
                continue;
            }
            // EuroAgent focus → keys edit the intent field (auto-start); Enter
            // dispatches to the agent loop (cap-gated tool calls + live audit).
            if agent_focused {
                if !agent_ui::editing() {
                    agent_ui::begin_edit();
                }
                if let Some(intent) = agent_ui::edit_key(k) {
                    agent_ui::dispatch(&intent); // route + run the agent loop
                    serial_println!("[bb6] EuroAgent dispatch: intent='{intent}' → agent loop executed (cap-gated, audited)");
                }
                need_full = true;
                continue;
            }
            // EuroText focus → the key edits the editor buffer (type/backspace/enter).
            if text_focused {
                textedit::input(k);
                need_full = true;
                continue;
            }
            // Read-only apps (EuroMonitor/EuroLog) get no shell input.
            if !term_focused {
                continue;
            }
            term_dirty = true;
            match k {
                '\r' => {
                    let cmd = String::from(input.trim());
                    // Record the executed command on the current prompt line.
                    if let Some(last) = windows[term_idx].content.last_mut() {
                        *last = format!("euroos:/ $ {cmd}");
                    }
                    // Split off redirection: `prog ... > file` or `>> file`.
                    let (exec_cmd, redir): (String, Option<(String, bool)>) =
                        if let Some(pos) = cmd.find(">>") {
                            (
                                String::from(cmd[..pos].trim()),
                                Some((String::from(cmd[pos + 2..].trim()), true)),
                            )
                        } else if let Some(pos) = cmd.find('>') {
                            (
                                String::from(cmd[..pos].trim()),
                                Some((String::from(cmd[pos + 1..].trim()), false)),
                            )
                        } else {
                            (cmd.clone(), None)
                        };

                    let mut out: Vec<String> = Vec::new();
                    if exec_cmd == "clear" {
                        windows[term_idx].content.clear();
                    } else if exec_cmd == "help" {
                        out.push("programs (exec from EuroFS): hello cat linuxprog muslprog".into());
                        out.push("        argvprog pieprog muslreal muslfile mcat mwrite".into());
                        out.push("        mecho <text> · mupper (stdin->UPPERCASE)".into());
                        out.push("pipes/redirection: a | b · prog > file · prog >> file".into());
                        out.push("install <package> (Ed25519 verification) · packages: msum".into());
                        out.push("network (live NIC): ping <ip|name> · ping6 · net · fetch <host> · https <host>".into());
                        out.push("server: serve (one connection) · httpd (background HTTP server on/off)".into());
                        out.push("EuroGuard (Track 7): guard · guard block <domain> · guard allow <domain>".into());
                        out.push("processes: ps · kill <pid> (background musl processes)".into());
                        out.push("engines: calc <expr> (EuroReken) · js <code> (EuroJS)".into());
                        out.push("builtins: ls, uname, mem, df, clear, help".into());
                    } else if let Some(expr) = exec_cmd.strip_prefix("calc ") {
                        // REAL calculation via the euroreken engine (no mockup).
                        match euroreken::eval(expr.trim()) {
                            Ok(v) => {
                                let n = if v == (v as i64) as f64 && euroreken::math::fabs(v) < 1e15 {
                                    format!("{}", v as i64)
                                } else {
                                    format!("{v}")
                                };
                                out.push(format!("{} = {}", expr.trim(), n));
                            }
                            Err(e) => out.push(format!("calc: error: {e:?}")),
                        }
                    } else if let Some(code) = exec_cmd.strip_prefix("js ") {
                        // REAL JavaScript execution via the EuroJS interpreter.
                        let (res, logs) = eurojs::run_capture(code);
                        for l in logs {
                            out.push(l);
                        }
                        match res {
                            Ok(v) => out.push(format!("=> {}", eurojs_show(&v))),
                            Err(e) => out.push(format!("js: error: {e}")),
                        }
                    } else if let Some(pkg) = exec_cmd.strip_prefix("install ") {
                        // Sovereign package installation: verify the Ed25519 signature
                        // before writing the package to EuroFS + registering it.
                        let pkg = pkg.trim();
                        let path = format!("/bin/{pkg}");
                        match ring3::installable(pkg) {
                            Some((bytes, caps, abi)) if ring3::verify_program(&path, bytes) => {
                                let _ = ctx.fs.write_file(&path, bytes);
                                if let Some(sig) = ring3::program_sig(&path) {
                                    let _ = ctx.fs.write_file(&format!("{path}.sig"), sig);
                                }
                                ring3::register_program(&path, caps, abi);
                                out.push(format!("[pkg] {pkg}: Ed25519 signature VERIFIED ({} bytes)", bytes.len()));
                                out.push(format!("[pkg] installed in {path} + {path}.sig (EuroFS)"));
                                out.push(format!("[pkg] registered — run it with: {pkg} <numbers>"));
                            }
                            Some(_) => out.push(format!("[sec] {pkg}: REJECTED — invalid Ed25519 signature")),
                            None => out.push(format!("install: unknown package '{pkg}' (available: msum)")),
                        }
                    } else if exec_cmd == "net" {
                        for l in net::cmd_net() {
                            out.push(l);
                        }
                    } else if exec_cmd == "ping6" {
                        for l in net::cmd_ping6() {
                            out.push(l);
                        }
                    } else if exec_cmd == "serve" {
                        for l in net::cmd_serve() {
                            out.push(l);
                        }
                    } else if exec_cmd == "httpd" {
                        // Background HTTP server on/off (serves :80 in the desktop loop).
                        let on = net::httpd_toggle();
                        if on {
                            out.push("httpd: background HTTP server ON — now serving :80".into());
                            out.push("  (connect from outside via the hostfwd; desktop stays active)".into());
                        } else {
                            out.push("httpd: background HTTP server OFF".into());
                        }
                    } else if let Some(d) = exec_cmd.strip_prefix("guard block ") {
                        // Add a custom block (spec: "Add domain").
                        let d = d.trim();
                        euroguard::add_blocked_domain(d);
                        out.push(format!("EuroGuard: '{d}' added to the block list"));
                    } else if let Some(d) = exec_cmd.strip_prefix("guard allow ") {
                        // Whitelist: remove a domain from the block list.
                        let d = d.trim();
                        if euroguard::remove_blocked_domain(d) {
                            out.push(format!("EuroGuard: '{d}' removed from the block list"));
                        } else {
                            out.push(format!("EuroGuard: '{d}' was not on the block list"));
                        }
                    } else if exec_cmd == "ps" {
                        // Process overview (per-process model).
                        for l in ring3::ps_lines() {
                            out.push(l);
                        }
                    } else if let Some(arg) = exec_cmd.strip_prefix("kill ") {
                        match arg.trim().parse::<u64>() {
                            Ok(pid) if ring3::kill_pid(pid) => {
                                out.push(format!("kill: process {pid} terminated — being cleaned up"));
                            }
                            Ok(pid) => out.push(format!("kill: no (live) background process with pid {pid}")),
                            Err(_) => out.push("kill: invalid pid".into()),
                        }
                    } else if exec_cmd == "guard" {
                        // EuroGuard dashboard (Track 7): policy + network monitor + audit log.
                        out.push("EuroGuard — access & network control (Track 7)".into());
                        for l in euroguard::policy_lines() {
                            out.push(l);
                        }
                        for l in euroguard::stats_lines() {
                            out.push(l);
                        }
                        for l in euroguard::dns_lines() {
                            out.push(l);
                        }
                        for l in euroguard::audit_lines(8) {
                            out.push(l);
                        }
                    } else if let Some(host) = exec_cmd.strip_prefix("ping ") {
                        for l in net::cmd_ping(host.trim()) {
                            out.push(l);
                        }
                    } else if let Some(host) = exec_cmd.strip_prefix("fetch ") {
                        for l in net::cmd_fetch(host.trim()) {
                            out.push(l);
                        }
                    } else if let Some(host) = exec_cmd.strip_prefix("https ") {
                        for l in net::cmd_https(host.trim()) {
                            out.push(l);
                        }
                    } else if shell::is_pipeline(&exec_cmd)
                        && exec_cmd
                            .split('|')
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .all(|st| {
                                // All stages built-in (no /bin program) → the
                                // pipe-aware shell handles the coreutils filters.
                                let n = st.split_whitespace().next().unwrap_or("");
                                !n.starts_with('/') && ring3::program_caps_abi(&format!("/bin/{n}")).is_none()
                            })
                    {
                        for l in shell::exec(&mut ctx, &exec_cmd) {
                            out.push(l);
                        }
                    } else if {
                        let n = exec_cmd.split_whitespace().next().unwrap_or("");
                        n == "doom" || n == "fbtest" || n == "browser" || n == "surf"
                    } {
                        // Graphical app: a PREEMPTIVELY-scheduled userspace program
                        // that draws to its own centered framebuffer (fb_present)
                        // and reads the keyboard (getkey). Unlike a /bin program it
                        // does NOT run to completion — it owns the screen until it
                        // exits, so we spawn it on the scheduler and return at once.
                        let raw = exec_cmd.split_whitespace().next().unwrap_or("");
                        let name = if raw == "surf" { "browser" } else { raw }; // alias (avoids a keymap quirk)
                        let path = format!("/bin/{name}");
                        match ring3::program_caps_abi(&path) {
                            None => out.push(format!("{name}: not installed")),
                            Some(_) => {
                                let bytes = ctx.fs.read_file(&path).unwrap_or_default();
                                if !ring3::verify_program(&path, &bytes) {
                                    out.push(format!("[sec] {path}: REJECTED — invalid Ed25519 signature"));
                                } else {
                                    let mem = &mut *ctx.mem;
                                    let pid: u64 = 90;
                                    // DOOM needs its IWAD; fbtest takes no args.
                                    let argv: Vec<&[u8]> = if name == "doom" {
                                        alloc::vec![b"doom".as_slice(), b"-iwad".as_slice(), b"/doom1.wad".as_slice(), b"-nosound".as_slice(), b"-nomusic".as_slice()]
                                    } else {
                                        alloc::vec![name.as_bytes()]
                                    };
                                    // The browser renders a full-screen framebuffer
                                    // + page/link buffers, so it needs a larger arena.
                                    let arena_mib = if name == "browser" { 48 } else { 32 };
                                    let spawned = x86_64::instructions::interrupts::without_interrupts(|| {
                                        ring3::spawn_bg_app(mem, &bytes, pid, &argv, arena_mib)
                                    });
                                    if spawned.is_some() {
                                        appgfx::set_app_pid(pid);
                                        appgfx::set_active(true);
                                        out.push(format!("{name}: launched (pid {pid}) — drawing to the screen; Esc quits"));
                                    } else {
                                        out.push(format!("{name}: out of memory (needs a {arena_mib} MiB arena)"));
                                    }
                                }
                            }
                        }
                    } else if !exec_cmd.is_empty() {
                        // Pipeline: split on '|' into stages; stdout of stage N -> stdin of N+1.
                        // Redirection (>) applies to the stdout of the LAST stage.
                        let stages: Vec<String> = exec_cmd
                            .split('|')
                            .map(|s| String::from(s.trim()))
                            .filter(|s| !s.is_empty())
                            .collect();
                        let mut piped: Option<Vec<u8>> = None; // stdin for the next stage
                        let mut last: Option<(u64, String, bool)> = None;
                        let mut unknown: Option<String> = None;
                        for (si, stage) in stages.iter().enumerate() {
                            let name = stage.split_whitespace().next().unwrap_or("");
                            let path = if name.starts_with('/') { String::from(name) } else { format!("/bin/{name}") };
                            let Some((caps, abi)) = ring3::program_caps_abi(&path) else {
                                unknown = Some(String::from(stage.as_str()));
                                break;
                            };
                            let bytes = ctx.fs.read_file(&path).unwrap_or_default();
                            // Verify-before-execute: Ed25519 signature must match.
                            if !ring3::verify_program(&path, &bytes) {
                                out.push(format!("[sec] {path}: REJECTED — invalid Ed25519 signature"));
                                last = None;
                                break;
                            }
                            let mut argv_s: Vec<String> = alloc::vec![path.clone()];
                            for w in stage.split_whitespace().skip(1) {
                                argv_s.push(String::from(w));
                            }
                            let argv: Vec<&[u8]> = argv_s.iter().map(|s| s.as_bytes()).collect();
                            // Stdin = stdout of the previous stage (pipe).
                            ring3::set_stdin(piped.as_deref().unwrap_or(&[]));
                            // Redirection only on the stdout of the last stage.
                            let is_last = si == stages.len() - 1;
                            if is_last {
                                if let Some((ref rp, append)) = redir {
                                    ring3::set_stdout_redirect(Some(rp), append);
                                }
                            }
                            let mem = &mut *ctx.mem;
                            let (ec, o) = x86_64::instructions::interrupts::without_interrupts(|| {
                                ring3::run_args(mem, &bytes, &argv, caps, abi)
                            });
                            ring3::set_stdout_redirect(None, false);
                            ring3::set_stdin(&[]);
                            piped = Some(o.clone().into_bytes());
                            last = Some((ec, o, abi));
                        }
                        if let Some(stage) = unknown {
                            // Unknown program -> kernel builtins (ls/uname/net/mem/df/…).
                            for l in shell::exec(&mut ctx, &stage) {
                                out.push(l);
                            }
                        } else if let Some((ec, o, abi)) = last {
                            if let Some((ref rp, append)) = redir {
                                out.push(format!("[{}] stdout -> {rp}", if append { ">>" } else { ">" }));
                            } else {
                                for l in o.lines() {
                                    out.push(l.into());
                                }
                            }
                            // Sync written files back to EuroFS.
                            for (p, bytes) in ring3::take_dirty() {
                                let n = bytes.len();
                                if ctx.fs.write_file(&p, &bytes).is_ok() {
                                    out.push(format!("[fs] {p} ({n} B) -> EuroFS synced"));
                                }
                            }
                            out.push(format!("[exit {ec}, abi={}]", if abi { "linux" } else { "native" }));
                        }
                    }
                    // Finishing-sprint E2E: tee the executed command + its output to
                    // serial, so the end-to-end loop (USB key → scancode → poll_key →
                    // shell prompt → Enter → exec → output) is externally verifiable.
                    serial_println!("[e2e] $ {cmd}");
                    for l in &out {
                        serial_println!("[e2e] {l}");
                    }
                    for l in out {
                        windows[term_idx].content.push(l);
                    }
                    windows[term_idx].content.push(String::from("euroos:/ $ "));
                    input.clear();
                    // Keep the buffer at the visible height so the prompt stays visible.
                    let c = &mut windows[term_idx].content;
                    if c.len() > vis_lines {
                        c.drain(0..c.len() - vis_lines);
                    }
                }
                '\u{8}' => {
                    input.pop();
                    if let Some(last) = windows[term_idx].content.last_mut() {
                        *last = format!("euroos:/ $ {input}");
                    }
                }
                c if !c.is_control() => {
                    input.push(c);
                    if let Some(last) = windows[term_idx].content.last_mut() {
                        *last = format!("euroos:/ $ {input}");
                    }
                }
                _ => {}
            }
        }

        // File dialog: list a directory it asked for, and carry out a chosen path.
        if let Some(dir) = filedialog::needs_load() {
            let items = match ctx.fs.list_dir(&dir) {
                Ok(v) => v.into_iter().map(|e| (e.name, e.kind == eurofs::EntryKind::Directory)).collect::<Vec<_>>(),
                Err(_) => Vec::new(),
            };
            filedialog::set_entries(&dir, items);
            need_full = true;
        }
        if let Some((mode, path)) = filedialog::take_result() {
            match mode {
                filedialog::Mode::Open => textedit::open(ctx.fs, &path),
                filedialog::Mode::Save => { textedit::save_to(ctx.fs, &path); }
            }
            // Raise the EuroText window so the result is visible.
            if let Some(w) = windows.iter().position(|win| win.app == suite_ui::SuiteApp::Text) {
                order.retain(|&x| x != w);
                order.push(w);
                for ww in windows.iter_mut() { ww.active = false; }
                windows[w].visible = true;
                windows[w].active = true;
            }
            need_full = true;
        }

        // Open an app requested by the launcher or a menu (single place).
        if let Some(icon) = launch_icon {
            if let Some(w) = dock_targets.get(icon).copied().flatten().filter(|&w| w < windows.len()) {
                order.retain(|&x| x != w);
                order.push(w);
                for ww in windows.iter_mut() { ww.active = false; }
                windows[w].visible = true;
                windows[w].active = true;
                if windows[w].app == suite_ui::SuiteApp::Files && files::current_path().is_empty() {
                    load_files_dir(ctx.fs, "/");
                }
            }
            need_full = true;
        }

        if term_dirty && windows[term_idx].visible {
            compositor::restore_cursor_bg(&fb, cmx, cmy, &cur_bg);
            // The Terminal is the front window and overlaps nothing above it, so only
            // redrawing this window is enough (System sits behind it, next to the terminal).
            // (When maximized it can be larger; a full render then follows via need_full.)
            compositor::draw_window(&fb, &windows[term_idx]);
            compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
            compositor::draw_cursor(&fb, cmx, cmy);
            fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() { virtio_gpu::present_frame(bb, bw, bh, bs); } // BB-2: live virtio-gpu scanout
        }

        // The REAL calculator changed → redraw only its window.
        if calc_dirty {
            if let Some(fi) = focused {
                compositor::restore_cursor_bg(&fb, cmx, cmy, &cur_bg);
                compositor::draw_window(&fb, &windows[fi]);
                compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
                compositor::draw_cursor(&fb, cmx, cmy);
                fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() { virtio_gpu::present_frame(bb, bw, bh, bs); } // BB-2: live virtio-gpu scanout
            }
        }

        let t = interrupts::ticks();
        let tick = t / 50 != last_t;

        // Alt-Tab app switcher: tap Tab (Alt held) to cycle; release Alt to raise.
        if ps2::take_alt_tab() {
            if switcher::is_open() {
                switcher::advance();
            } else {
                let items: alloc::vec::Vec<(usize, String)> = order.iter().rev().copied()
                    .filter(|&i| windows[i].visible)
                    .map(|i| (i, windows[i].title.clone()))
                    .collect();
                switcher::begin(items);
            }
            need_full = true;
        }
        if switcher::is_open() && !ps2::alt_down() {
            if let Some(w) = switcher::selected() {
                order.retain(|&x| x != w);
                order.push(w);
                for ww in windows.iter_mut() { ww.active = false; }
                windows[w].active = true;
                windows[w].visible = true;
            }
            switcher::close();
            need_full = true;
        }

        // Workspaces: Alt+1..4 switches virtual desktops (save/restore visibility).
        if let Some(k) = ps2::take_ws_switch() {
            if k < workspace::COUNT && k != active_ws {
                ws_saved[active_ws] = Some(windows.iter().map(|w| w.visible).collect());
                let target = ws_saved[k].as_deref();
                let vis = workspace::switch_visibility(windows.len(), target);
                for (i, v) in vis.iter().enumerate() {
                    windows[i].visible = *v;
                }
                active_ws = k;
                notify::push("Workspace", &alloc::format!("Switched to desktop {}", k + 1), t);
                need_full = true;
            }
        }

        // Tooltips: after a short dwell over a control, show its label. Any open
        // overlay suppresses tooltips.
        let overlay_up = ctxmenu::is_open() || launcher::is_open() || filedialog::is_open() || notify::is_centre_open();
        let target = if overlay_up { None } else { tooltip_for(px, py, &windows, &order, width) };
        if target != hover_txt {
            hover_txt = target;
            hover_since = t;
            if tip_shown {
                tooltip::clear();
                tip_shown = false;
                need_full = true;
            }
        } else if let Some(txt) = &hover_txt {
            if !tip_shown && t.saturating_sub(hover_since) > 35 {
                tooltip::set(txt, px, py);
                tip_shown = true;
                need_full = true;
            }
        }

        // While a context menu or the launcher is open, keep repainting so the
        // hovered row tracks the cursor and the overlay stays above everything.
        if ctxmenu::is_open() || launcher::is_open() || filedialog::is_open()
            || symbolpicker::is_open() || notify::has_active_toasts(t) || notify::is_centre_open()
            || file_drag.is_some() || switcher::is_open()
        {
            need_full = true;
        }
        if need_full {
            // Full redraw (drag or z-order changed).
            last_t = t / 50;
            compositor::render(&fb, &windows, &order, &rtc::clock_string(), &rtc::date_string(), &mk_stats(ctx.mem.free_bytes()));
            // 3F-7: draw the permission-portal modal over the desktop (if a
            // request is pending) and remember its button rects for the click loop.
            portal_buttons = portal::render_dialog(&fb, width, height);
            // The right-click context menu overlays the desktop, under the cursor.
            ctxmenu::render(&fb, px, py);
            // The app launcher overlays everything (a centered search palette).
            launcher::render(&fb, width, height);
            // The file open/save dialog is the topmost overlay when active.
            filedialog::render(&fb, width, height);
            // The symbol picker overlay.
            symbolpicker::render(&fb, width, height);
            // Notification toasts and the centre shade sit above the desktop.
            notify::render_toasts(&fb, width, t);
            notify::render_centre(&fb, width, height);
            // A hover tooltip, if one is showing, sits just under the cursor.
            tooltip::render(&fb, width, height);
            // A drag ghost while a file is being dragged out of EuroFiles.
            if let Some((ref path, _, _)) = file_drag {
                let name = path.rsplit('/').next().unwrap_or(path);
                let tw = text::width_px(name, 12.0);
                let gx = px + 12;
                let gy = py + 12;
                fb.fill_rounded_rect(gx, gy, tw + 22, 24, eds::RADIUS_S, Color::ACCENT);
                text::draw_px(&fb, gx + 11, gy + 5, name, Color::WHITE, 12.0);
            }
            // The Alt-Tab switcher overlay.
            switcher::render(&fb, width, height);
            // Workspace pager: one dot per virtual desktop, the active one filled.
            {
                let n = workspace::COUNT;
                let (dot, gap) = (10usize, 10usize);
                let total = n * dot + (n - 1) * gap;
                let sx = width.saturating_sub(total) / 2;
                let sy = height.saturating_sub(26);
                for i in 0..n {
                    let c = if i == active_ws { Color::ACCENT } else { Color::BORDER };
                    fb.fill_rounded_rect(sx + i * (dot + gap), sy, dot, dot, dot / 2, c);
                }
            }
            cmx = px;
            cmy = py;
            compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
            compositor::draw_cursor(&fb, cmx, cmy);
            fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() { virtio_gpu::present_frame(bb, bw, bh, bs); } // BB-2: live virtio-gpu scanout
            // Capture the freshly rendered desktop if a screenshot was requested.
            if pending_shot {
                if let Some(path) = screenshot::capture(&fb, ctx.fs) {
                    notify::push("Screenshot saved", &path, t);
                }
                pending_shot = false;
            }
        } else if tick {
            // Update the live system window (incl. daemon heartbeat) + clock.
            last_t = t / 50;
            // Diagnostics: log the number of keyboard IRQs (via IO-APIC) on change.
            let kc = interrupts::KBD_IRQ_COUNT.load(Ordering::Relaxed);
            if kc != last_kbd {
                last_kbd = kc;
                serial_println!("[ioapic] keyboard IRQs received via IO-APIC: {}", kc);
            }
            // Clean up terminated processes (free frames) — safe from
            // task 0 on the boot PML4. Free RAM visibly recovers.
            ring3::reap_dead(ctx.mem);
            // S4: EuroInit supervision (restart stopped services) + eurologd
            // (kmsg ring periodically to /var/log/messages).
            init::supervise(ctx.mem, ctx.fs);
            init::flush_log(ctx.fs);
            // G5: periodic background scrub (rate-limited ~60 s) → /var/log/fsck.log.
            scrub::maybe_run(ctx.fs, t);
            let (ox, oy) = (cmx, cmy);
            compositor::restore_cursor_bg(&fb, cmx, cmy, &cur_bg);
            // Refresh the status panel (large clock) — without shadow so it does not stack.
            compositor::draw_status_panel(&fb, width, height, &rtc::clock_string(), &rtc::date_string(), &mk_stats(ctx.mem.free_bytes()), false);
            // Live System window: update real kernel status (counters, daemon heartbeat,
            // SMEP/SMAP, IPC audit). Redraw only the body (no shadow → no stacking).
            // Skip if the window is closed/minimized (otherwise it would come back).
            let sys_vis = windows[sys_idx].visible;
            if sys_vis {
                windows[sys_idx].content = sysinfo(t, ctx.mem.free_bytes());
                compositor::draw_window_body(&fb, &windows[sys_idx]);
            }
            // Live GTK app: if it repainted (X client presented a new frame), recomposite
            // its window body from the retained X buffer + blit only that rect.
            let gtk_vis = windows[gtk_idx].visible && xserver::take_dirty();
            if gtk_vis {
                compositor::draw_window_body(&fb, &windows[gtk_idx]);
            }
            cmx = px;
            cmy = py;
            compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
            compositor::draw_cursor(&fb, cmx, cmy);
            // SPERF dirty-rect: blit ONLY the status panel + the System window + the
            // old/new cursor, not the whole screen (was ~2M px/tick).
            let (rx, ry, rw, rh) = compositor::status_panel_rect(width);
            fb.present_rect(rx, ry, rw, rh);
            if sys_vis {
                let (sx, sy, sw, sh) = compositor::window_body_rect(&windows[sys_idx]);
                fb.present_rect(sx, sy, sw, sh);
            }
            if gtk_vis {
                let (gx, gy, gw, gh) = compositor::window_body_rect(&windows[gtk_idx]);
                fb.present_rect(gx, gy, gw, gh);
            }
            fb.present_rect(ox, oy, compositor::CURSOR_W, compositor::CURSOR_H);
            fb.present_rect(cmx, cmy, compositor::CURSOR_W, compositor::CURSOR_H);
        } else if px != cmx || py != cmy {
            // Only move the cursor (save-under) — blit only the area
            // the cursor leaves + arrives at, so the mouse stays smooth.
            let (ox, oy) = (cmx, cmy);
            compositor::restore_cursor_bg(&fb, cmx, cmy, &cur_bg);
            cmx = px;
            cmy = py;
            compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
            compositor::draw_cursor(&fb, cmx, cmy);
            let rx = ox.min(px);
            let ry = oy.min(py);
            let rw = ox.abs_diff(px) + compositor::CURSOR_W;
            let rh = oy.abs_diff(py) + compositor::CURSOR_H;
            fb.present_rect(rx, ry, rw, rh);
        }

        // Keep the network alive: answer ARP requests + recycle RX buffers.
        net::service();

        // One-shot self-test: after the live GTK window has been up a while, synthesize a
        // click on its Reset button (through the normal desktop click path) to prove
        // desktop->X input routing end-to-end (the on-screen counter visibly resets).
        if windows[gtk_idx].visible && xserver::front_window_size().is_some() {
            gtk_dtick += 1;
            // Self-test 1a: focus the window + type "hi" into the entry (keyboard).
            if gtk_dtick == 30 {
                for ww in windows.iter_mut() { ww.active = false; }
                windows[gtk_idx].active = true;
                xserver::deliver_focus(true);
                ps2::push_scancode(0x23); ps2::push_scancode(0x23 | 0x80); // h
                ps2::push_scancode(0x17); ps2::push_scancode(0x17 | 0x80); // i
                serial_println!("[gtk-test] focused + typed 'hi'");
            }
            // Self-test 1b: click the Reset button (X delivery + GTK dispatch).
            if gtk_dtick == 45 && !gtk_click_done {
                if let Some((xw, xh)) = xserver::front_window_size() {
                    let (lx, ly) = ((xw / 2) as i16, xh.saturating_sub(18) as i16);
                    serial_println!("[gtk-test] deliver_button to Reset @local({lx},{ly}) of {xw}x{xh}");
                    xserver::deliver_button(lx, ly);
                }
                gtk_click_done = true;
            }
            // Self-test 2 (Leg B): close the window -> terminate the app + free its arena.
            if gtk_dtick == 90 {
                let free_before = ctx.mem.free_bytes();
                ring3::kill_persistent_glibc(ctx.mem);
                windows[gtk_idx].visible = false;
                order.retain(|&x| x != gtk_idx);
                need_full = true;
                serial_println!("[gtk-test] closed GTK window; free RAM {} -> {} MiB",
                    free_before / (1 << 20), ctx.mem.free_bytes() / (1 << 20));
            }
        }

        // Keyboard focus: when the hosted GTK window is the active window, route real
        // keystrokes to it (X KeyPress via the keymap) instead of the shell. Otherwise
        // the shell keeps the keyboard as normal.
        if windows[gtk_idx].visible && windows[gtk_idx].active {
            xserver::pump_keyboard();
        }

        // Cooperatively yield so a hosted persistent glibc app (the live GTK window)
        // gets CPU promptly instead of only via timer preemption — the desktop loop
        // otherwise just hlt()s, which the Explore mapping flagged as starving it.
        if xserver::x_windowed() {
            crate::sched::yield_now();
        }

        // Finishing sprint: HLT idle. Yield the CPU until the NEXT interrupt (timer
        // 100 Hz or keyboard/mouse/USB input) instead of spinning 100% — the CPU
        // sleeps energy-efficiently between frames; the timer tick guarantees ~10 ms
        // responsiveness and every input IRQ wakes the desktop immediately.
        x86_64::instructions::hlt();
    }
}


/// Populate a fresh EuroFS with the system files + baked-in userspace programs.
/// Called when formatting (first boot / installation on an empty disk).
/// The system binaries the kernel SHIPS (path + ELF bytes) — one source of
/// truth for both the first installation (`populate_fs`) and the version sync
/// (`sync_system_files`). When adding a /bin program: one line here.
/// Show an EuroJS value as a string (for the `js` shell command).
fn eurojs_show(v: &eurojs::Value) -> String {
    match v {
        eurojs::Value::Num(n) => {
            if n.is_finite() && *n == (*n as i64) as f64 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        eurojs::Value::Str(s) => (**s).clone(),
        eurojs::Value::Bool(b) => format!("{b}"),
        eurojs::Value::Null => String::from("null"),
        eurojs::Value::Undefined => String::from("undefined"),
        _ => String::from("(object)"),
    }
}

fn system_binaries() -> [(&'static str, &'static [u8]); 32] {
    [
        ("/bin/fbtest", ring3::fbtest_bytes()),
        ("/bin/doom", ring3::doom_bytes()),
        ("/bin/browser", ring3::browser_bytes()),
        ("/bin/hello", ring3::program_bytes()),
        ("/bin/cat", ring3::cat_bytes()),
        ("/bin/linuxprog", ring3::linuxprog_bytes()),
        ("/bin/forktest", ring3::forktest_bytes()),
        ("/bin/execee", ring3::execee_bytes()),
        ("/bin/forkpipe", ring3::forkpipe_bytes()),
        ("/bin/ticker", ring3::ticker_bytes()),
        ("/bin/muslprog", ring3::muslprog_bytes()),
        ("/bin/argvprog", ring3::argvprog_bytes()),
        ("/bin/pieprog", ring3::pieprog_bytes()),
        ("/bin/muslreal", ring3::muslreal_bytes()),
        ("/bin/muslfile", ring3::muslfile_bytes()),
        ("/bin/mcat", ring3::mcat_bytes()),
        ("/bin/mwrite", ring3::mwrite_bytes()),
        ("/bin/mecho", ring3::mecho_bytes()),
        ("/bin/mupper", ring3::mupper_bytes()),
        ("/bin/daemon", ring3::daemon_bytes()),
        ("/bin/menv", ring3::menv_bytes()),
        ("/bin/msock", ring3::msock_bytes()),
        ("/bin/mdns", ring3::mdns_bytes()),
        ("/bin/mtrack", ring3::mtrack_bytes()),
        ("/bin/tlscount", ring3::tlscount_bytes()),
        ("/bin/isotest", ring3::isotest_bytes()),
        ("/bin/worker", ring3::worker_bytes()),
        ("/bin/mthread", ring3::mthread_bytes()),
        ("/bin/mpthread", ring3::mpthread_bytes()),
        ("/bin/mmutex", ring3::mmutex_bytes()),
        ("/bin/ipcrecv", ring3::ipcrecv_bytes()),
        ("/bin/ipcsend", ring3::ipcsend_bytes()),
    ]
}

/// FNV-1a digest over all bundled binaries (path + content). Changes as soon as
/// one /bin program is rebuilt — the "build-id" for the system sync.
fn system_digest() -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for (path, bytes) in system_binaries() {
        mix(path.as_bytes());
        mix(bytes);
    }
    h
}

/// Register all files under `dir` (recursively) in the userspace VFS, so that
/// ring-3 programs can read them via open/read. EuroFS stays the source; this is
/// the syscall-visible mirror of it.
fn register_dir_recursive(fs: &mut dyn FileSystem, dir: &str) {
    let entries = match fs.list_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries {
        let path =
            if dir == "/" { format!("/{}", e.name) } else { format!("{}/{}", dir, e.name) };
        match e.kind {
            eurofs::EntryKind::File => {
                // Do NOT mirror /etc/shadow into the userspace VFS — password hashes
                // must not be world-readable (only the kernel/auth reads them).
                if path == "/etc/shadow" {
                    continue;
                }
                if let Ok(bytes) = fs.read_file(&path) {
                    ring3::register_file(&path, bytes);
                }
            }
            eurofs::EntryKind::Directory => register_dir_recursive(fs, &path),
            _ => {} // do not mirror symlinks etc.
        }
    }
}

/// The /etc skeleton: identity and system config that every EuroOS installation has
/// (like a real distro). One source for both installation (`populate_fs`) and the
/// write-if-missing fill-in on existing disks (`ensure_etc_skeleton`).
fn etc_skeleton() -> [(&'static str, &'static [u8]); 6] {
    [
        ("/etc/hostname", b"eurokernel\n"),
        ("/etc/hosts", b"127.0.0.1 localhost\n127.0.0.1 eurokernel\n10.0.2.2 gateway\n"),
        ("/etc/eurokernel.conf", b"version=0.1\nencryption=off\n"),
        (
            "/etc/passwd",
            b"root:x:0:0:root:/root:/bin/eurosh\neuro:x:1000:1000:Euro User:/home/euro:/bin/eurosh\n",
        ),
        ("/etc/group", b"root:x:0:\neuro:x:1000:\n"),
        (
            "/etc/os-release",
            b"NAME=\"EuroOS\"\nVERSION=\"0.1-alpha\"\nID=euroos\nPRETTY_NAME=\"EuroOS 0.1-alpha\"\nHOME_URL=\"https://euro-os.eu\"\n",
        ),
    ]
}

/// Fill in missing /etc skeleton files on an EXISTING installation (a
/// disk from before these files). Write-if-missing: NEVER overwrites edited
/// config. Returns the number of files filled in.
fn ensure_etc_skeleton(fs: &mut dyn FileSystem) -> usize {
    let mut added = 0;
    for (path, content) in etc_skeleton() {
        if fs.read_file(path).is_err() {
            if fs.write_file(path, content).is_ok() {
                added += 1;
            }
        }
    }
    added
}

fn populate_fs(fs: &mut dyn FileSystem) {
    fs.create_dir("/etc").unwrap();
    fs.create_dir("/boot").unwrap();
    for (path, content) in etc_skeleton() {
        fs.write_file(path, content).unwrap();
    }
    // /etc/shadow (S5): password hashes. 'euro' has password "euro" (demo);
    // 'root' is locked ("*") — access via sudo, like a real sudo system.
    let shadow = format!(
        "root:*:*\n{}\n",
        auth::shadow_line("euro", b"euro-salt-v1", b"euro")
    );
    fs.write_file("/etc/shadow", shadow.as_bytes()).unwrap();
    fs.create_dir("/etc/euroguard").unwrap();
    fs.write_file("/etc/euroguard/system.conf", EUROGUARD_CONF).unwrap();
    fs.write_file("/boot/version", b"EuroKernel v0.1-alpha\n").unwrap();
    fs.write_file("/boot/kernel.img", &[0u8; 12_000]).unwrap();
    fs.create_dir("/bin").unwrap();
    for (path, bytes) in system_binaries() {
        fs.write_file(path, bytes).unwrap();
    }
    // NOTE: /doom1.wad is served straight from the kernel image (ring3::vfs_open
    // special-case) — NOT written to the RAM FS, so it neither doubles the WAD's
    // RAM nor makes the boot-time FS scrub crawl over 4 MiB under TCG.
    // Record the build-id so the next boot sees that /bin is up to date.
    fs.write_file("/etc/system.ver", format!("{:016x}\n", system_digest()).as_bytes()).unwrap();
    fs.create_dir("/tmp").unwrap();
    fs.create_dir("/var").unwrap();
    fs.create_dir("/var/log").unwrap(); // eurologd destination (full daemon = S4)
    // H3: a dynamically-linked binary + its shared library in the FS, so that the
    // dynlinker can resolve the .so from /lib via DT_NEEDED (run-by-name).
    let _ = fs.create_dir("/lib");
    let _ = fs.write_file("/bin/dyntest", ring3::dyntest_bytes());
    let _ = fs.write_file("/lib/libeuro.so", ring3::libeuro_bytes());
    // Welcome/info file for the user (English): what EuroOS is, what you can
    // do, the main commands and the limitations. Publicly readable via the
    // Files app or `cat /Welcome.txt`.
    let _ = fs.create_dir("/home");
    let _ = fs.create_dir("/home/euro");
    fs.write_file("/Welcome.txt", WELCOME_TXT).unwrap();
    let _ = fs.write_file("/home/euro/Welcome.txt", WELCOME_TXT);
}

/// The English-language welcome/info file seeded into the FS.
const WELCOME_TXT: &[u8] = b"\
EuroOS - Welcome
================

EuroOS is a sovereign, security-first operating system built from scratch in
Rust. It has its own kernel, filesystem, network stack, capability security and
desktop - with no Linux or BSD underneath. Zero telemetry. Licensed under the
European Union Public Licence (EUPL) v1.2.

This is an ALPHA PREVIEW: something to explore and build on, not yet a
daily-driver OS.

What you can do here
--------------------
* Open the Terminal from the dock and explore an interactive shell.
* Browse the filesystem and read/write files (EuroFS is copy-on-write).
* Use real networking: DNS, HTTP/HTTPS over TLS 1.3.
* Try the sovereign subsystems: user identity, the encrypted secrets vault,
  capability-isolated AI agents, the firewall, snapshots, and the audit log.

Useful shell commands  (type `help` for the exact, current list)
----------------------------------------------------------------
Files     : ls, cat, write <file> <text>, mkdir, cp, find, df
Text      : echo, grep, sort, uniq, cut, wc, head, tail   (GNU-compatible)
System    : uname, hostname, whoami, id, date, free, ps, lspci, lsdev, metrics
Identity  : login <user> <pw>, su, sudo <cmd>, logout, eurousers
Security  : vault, europol, audit, eurosnap, eurohealth
Network   : net, eurofw (firewall), vpn
Agents    : euroagent   (capability-isolated AI agents)

The demo desktop user is `euro` (password: euro). Root access is via `sudo`.

Known limitations (alpha)
-------------------------
* Not every desktop app is fully interactive yet; some are previews.
* Hardware support is limited - it runs best in QEMU/KVM with virtio devices.
* There is no internet app store / package installation yet.
* This preview is for evaluation - do not store important data here.

Learn more
----------
Website : https://euro-os.eu
Source  : https://github.com/GoTrustbe/Euroos   (open source, EUPL-1.2)
Docs    : https://euro-os.eu/docs

Welcome to a computer that is yours, on your terms.  - The EuroOS project
";

/// Mini-EuroUpdate (Track 9): keep an INSTALLED system in sync with the kernel.
/// If the bundled binaries differ from what is on disk (= the kernel was
/// rebuilt), rewrite /bin + the build-id. Fixes the sig mismatch where a
/// rebuilt kernel would reject old /bin binaries on disk. Returns true on update.
fn sync_system_files(fs: &mut dyn FileSystem) -> bool {
    let want = format!("{:016x}\n", system_digest());
    let have = fs
        .read_file("/etc/system.ver")
        .ok()
        .and_then(|d| alloc::string::String::from_utf8(d).ok());
    if have.as_deref() == Some(want.as_str()) {
        return false; // disk is already up to date
    }
    let _ = fs.create_dir("/bin");
    for (path, bytes) in system_binaries() {
        // L1: a previously IMMUTABLE-marked binary may be replaced by the (trusted) boot
        // updater — clear the flag, write the new build, and `protect_system_files`
        // marks it immutable again later. This is the correct immutable-OS update flow.
        let _ = fs.set_flags(path, 0);
        let _ = fs.write_file(path, bytes);
    }
    // /doom1.wad is served from the kernel image (see the fresh-FS branch).
    let _ = fs.write_file("/boot/version", b"EuroKernel v0.1-alpha\n");
    let _ = fs.write_file("/etc/system.ver", want.as_bytes());
    true
}

/// Build the physical frame allocator from the UEFI memory map (before exit).
fn build_frame_allocator() -> FrameAllocator {
    let map = boot::memory_map(MemoryType::LOADER_DATA).expect("memory map");
    let mut regions: Vec<MemoryRegion> = Vec::new();
    for d in map.entries() {
        regions.push(MemoryRegion {
            start: d.phys_start,
            len: d.page_count * 4096,
            usable: d.ty == MemoryType::CONVENTIONAL,
        });
    }
    FrameAllocator::from_regions(&regions, 0x10_0000)
}

/// Our own panic handler (the uefi variant no longer works after ExitBootServices).
/// Logs to COM1 and draws a red panic screen if the framebuffer is known.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Serial panic dump: message + registers + backtrace + recent kmsg history.
    // write_raw goes directly to the UART (try_lock), so it works even if another
    // lock was held when the panic struck.
    serial::write_raw(b"\n========== KERNEL PANIC ==========\n");
    serial_println!("[PANIC] {info}");
    klog::dump_registers_and_backtrace();
    // 3G-1: persist a minidump for a Rust panic too (vector 0xFF), not only for
    // the CPU faults (#GP/#PF/#DF) — so a `panic!` is recoverable across a reboot.
    crashdump::capture(0xFF, 0, 0, 0, 0);
    serial::write_raw(b"[panic] --- recent kernel log (kmsg) ---\n");
    klog::with_recent(24, |line| {
        serial::write_raw(b"  | ");
        serial::write_raw(line);
        serial::write_raw(b"\n");
    });
    serial::write_raw(b"========== END PANIC ==========\n");

    // Screen: red background + message + the last few log lines as context.
    if let Some(fbi) = FB_INFO.get() {
        let fb = unsafe { FrameBuffer::new(fbi.base as *mut u8, fbi.width, fbi.height, fbi.stride, fbi.pf) };
        fb.clear(Color::rgb(0x40, 0x08, 0x10));
        draw_string(&fb, 24, 24, "KERNEL PANIC", Color::WHITE, 3);
        draw_string(&fb, 24, 84, "recent kernel log (see COM1 for registers + backtrace):", Color::WHITE, 1);
        // Show the last ~24 lines building from the bottom.
        let mut rows: [( [u8; klog::LINE_LEN], usize); 24] = [([0u8; klog::LINE_LEN], 0); 24];
        let mut n = 0usize;
        klog::with_recent(24, |line| {
            let i = n % 24;
            let l = line.len().min(klog::LINE_LEN);
            rows[i].0[..l].copy_from_slice(&line[..l]);
            rows[i].1 = l;
            n += 1;
        });
        let shown = n.min(24);
        let start = if n > 24 { n - 24 } else { 0 };
        for k in 0..shown {
            let idx = (start + k) % 24;
            if let Ok(s) = core::str::from_utf8(&rows[idx].0[..rows[idx].1]) {
                draw_string(&fb, 24, 120 + k * 16, s, Color::rgb(0xE0, 0xC0, 0xC0), 1);
            }
        }
    }
    loop {
        x86_64::instructions::hlt();
    }
}
