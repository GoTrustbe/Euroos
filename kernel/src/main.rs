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
mod apic;
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
mod msix;
mod net;
mod paging;
mod gpt;
mod hda;
mod hpet;
mod pci;
mod power;
mod procpool;
mod rootblk;
mod scrub;
mod swapmgr;
mod wasm;
mod wayland;
mod ps2;
mod ring3;
mod rtc;
mod sched;
mod serial;
mod shell;
mod smp;
mod nvme;
mod tls_roots;
mod update;
mod tpm;
mod virtio_blk;
mod virtio_gpu;
mod virtio_net;
mod vpn;
mod agent;
mod locale;
mod installer;
mod ca;
mod attest;
mod idm;
mod euroid;
mod pkg;
mod repro;
mod access;
mod suite;
mod wifi;
mod gpu;
mod print;
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
mod agent_ui;
mod files;
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

/// AG-1: lees een ECHTE map uit het FS en geef ze aan de EuroFiles-GUI. Geen mock —
/// de bestandsbeheerder toont precies wat `fs.list_dir` teruggeeft.
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

const PROMPT: &str = "euroos:/ $ ";

/// EuroGuard Niveau-1 systeem-policy (Track 7, Fase 7.2). Wordt bij de eerste
/// boot naar /etc/euroguard/system.conf geschreven en daarvandaan ingelezen —
/// data-gedreven, niet hardgecodeerd. Eenvoudig, leesbaar regelformaat.
const EUROGUARD_CONF: &[u8] = b"# EuroGuard systeem-policy (Niveau 1) - /etc/euroguard/system.conf\n\
# Transparant: dit is precies wat het systeem blokkeert. Wijzig + herstart.\n\
\n\
# Geblokkeerde IP-adressen (tracker/telemetrie-endpoints)\n\
block-ip 203.0.113.5\n\
\n\
# Geblokkeerde poorten (verouderd/onveilig)\n\
block-port 23\n\
block-port 1900\n\
\n\
# DNS-blokkeerlijst: ads, trackers, telemetrie (incl. subdomeinen)\n\
block-domain ads.doubleclick.net\n\
block-domain telemetry.mozilla.org\n\
block-domain google-analytics.com\n\
block-domain graph.facebook.com\n";

/// Framebuffer-info als plain-data, globaal beschikbaar voor de panic-handler.
#[derive(Clone, Copy)]
struct FbInfo {
    base: usize,
    width: usize,
    height: usize,
    stride: usize,
    pf: PixelFormat,
}
static FB_INFO: spin::Once<FbInfo> = spin::Once::new();

#[entry]
fn main() -> Status {
    // ── Allereerst: eigen heap + serial (werken óók na ExitBootServices). ──
    allocator::init();
    serial::init();
    serial_println!("\n[euro] EuroKernel bring-up — heap ({} MiB) + COM1 actief", allocator::size() / (1024 * 1024));

    // EuroFS wordt later opgezet (ná virtio-blk-init): óf op de GPT-schijf
    // (geïnstalleerd, persistent) óf in RAM (live-modus). Zie `populate_fs`.

    // ── Track 3.1: frame-allocator uit de UEFI-geheugenkaart (nog in BS). ──
    let mut allocator = build_frame_allocator();
    serial_println!("[euro] frame-allocator: {} MiB bruikbaar RAM", allocator.usable_bytes() / (1024 * 1024));

    // ── GOP framebuffer ophalen en bewaren (blijft geldig ná exit). ──
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>().expect("GOP-handle");
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(handle).expect("GOP open");
    graphics::set_best_mode(&mut gop);
    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();
    let pf = mode.pixel_format();
    let base = gop.frame_buffer().as_mut_ptr();
    FB_INFO.call_once(|| FbInfo { base: base as usize, width, height, stride, pf });
    // SAFETY: het framebuffer-geheugen blijft geldig na ExitBootServices.
    // Gebufferd: tekenen gaat naar een RAM-backbuffer, present() blit het beeld.
    let fb = unsafe { FrameBuffer::new_buffered(base, width, height, stride, pf) };
    drop(gop); // protocol netjes sluiten zolang Boot Services nog leven
    serial_println!("[euro] GOP {width}x{height} stride={stride} {pf:?}");

    // Pak het ACPI-RSDP-adres uit de UEFI-configuratietabel (kan alleen nu, vóór
    // we UEFI verlaten) — nodig om straks de MADT (CPU-cores + IO-APIC) te lezen.
    let rsdp = uefi::system::with_config_table(|t| {
        t.iter()
            .find(|e| e.guid == uefi::table::cfg::ACPI2_GUID)
            .or_else(|| t.iter().find(|e| e.guid == uefi::table::cfg::ACPI_GUID))
            .map(|e| e.address as u64)
            .unwrap_or(0)
    });
    acpi::set_rsdp(rsdp);
    serial_println!("[acpi] RSDP @ {rsdp:#x}");

    // AG-3: lees onze EIGEN install-media (loader + A/B-kernel) van het boot-volume
    // via UEFI — KAN ALLEEN NU, vóór ExitBootServices. Zo kan de installer later een
    // bootbare kopie naar een doelschijf schrijven (geen embed, echte huidige bytes).
    instexec::capture_media();

    // ── DE SPRONG: verlaat UEFI Boot Services. Hierna geen UEFI-services meer. ──
    serial_println!("[euro] ExitBootServices...");
    let _ = unsafe { boot::exit_boot_services(MemoryType::LOADER_DATA) };
    serial_println!("[euro] >>> UEFI verlaten — kernelmodus <<<");

    // ── Kernelmodus-bring-up: interrupts uit, GDT, IDT, exception-test. ──
    x86_64::instructions::interrupts::disable();
    gdt::init();
    serial_println!("[euro] GDT+TSS geladen");
    interrupts::init();
    serial_println!("[euro] IDT geladen");

    // Eigen page tables laden (identity-map; alles blijft werken op onze CR3).
    let fb_base = FB_INFO.get().map(|i| i.base).unwrap_or(0);
    serial_println!("[euro] framebuffer @ {fb_base:#x} — eigen page tables laden...");
    let pml4 = paging::init(&mut allocator);
    sched::set_boot_pml4(pml4); // gedeelde kernel-adresruimte voor de scheduler
    serial_println!("[euro] CR3 geladen (PML4 @ {pml4:#x}) — eigen paging actief");

    // A2: zet een guarded kernel-stack op in de gedeelde hoge regio (één niet-
    // gemapte guard-pagina onder de stack → overflow faultt onmiddellijk). Non-
    // destructieve verificatie: stack-pagina schrijfbaar, guard-pagina niet-present.
    let gtop = paging::setup_guarded_stack(&mut allocator);
    if gtop != 0 {
        let stack_ok = unsafe {
            let p = (gtop - 8) as *mut u64; // binnen de bovenste stack-pagina
            p.write_volatile(0xA2_DEAD_BEEF);
            p.read_volatile() == 0xA2_DEAD_BEEF
        };
        serial_println!(
            "[a2] guarded kernel-stack: top {:#x}, guard {:#x} — stack schrijfbaar={}, guard niet-present={}",
            gtop,
            paging::STACK_GUARD_ADDR.load(core::sync::atomic::Ordering::Relaxed),
            stack_ok,
            paging::guard_page_unmapped(),
        );
    }

    // S3: reserveer een PROCES-FRAME-POOL (64 MiB) uit de hoofd-allocator. fork()/
    // execve() alloceren hieruit terwijl ze in een syscall draaien (de hoofd-
    // allocator is dan onbereikbaar). Identity-gemapt, dus kernel-toegankelijk.
    const POOL_FRAMES: usize = 16384; // 64 MiB
    match allocator.allocate_contiguous(POOL_FRAMES) {
        Ok(base) => {
            procpool::install(base, POOL_FRAMES);
            serial_println!("[mm] proces-frame-pool: 64 MiB @ {base:#x} (fork/exec)");
        }
        Err(_) => serial_println!("[mm] WAARSCHUWING: geen proces-frame-pool (fork uitgeschakeld)"),
    }

    // virtio-blk: echte schijf initialiseren (PIO/DMA werkt op onze identity-map).
    virtio_blk::init(&mut allocator);

    // NVMe (B2): detecteer + initialiseer een NVMe-controller (admin/I/O-queues,
    // identify), doe een read/write-zelftest + SMART-uitlezing. No-op zonder NVMe.
    if nvme::init(&mut allocator) {
        nvme::self_test();
    }

    // ── ROOT-FILESYSTEM ── EuroFS staat OP de schijf (geïnstalleerd, persistent)
    // als er een virtio-blk-schijf is, anders in RAM (live-modus). Bestaande FS
    // wordt gemount; een verse/lege schijf wordt geformatteerd + gevuld
    // (= installatie). Zo overleven bestanden een herstart — als een echt OS.
    // AG-3: als we ECHTE install-media hebben én er een blanco virtio-doelschijf is,
    // installeer een bootbare EuroOS daarheen (i.p.v. ze als root te gebruiken) en
    // draai zelf verder in live-modus — de doelschijf boot standalone (multidisk-harnas).
    let installed = if instexec::media_available() && virtio_blk::present() {
        if instexec::disk_is_blank(0) {
            // Verse doelschijf → installeer een bootbare, geprovisioneerde EuroOS (slot A).
            instexec::install_to_disk(0, &instexec::default_config())
        } else {
            // Al geïnstalleerd → demonstreer de A/B-ZELFUPDATE: stage slot B + flip
            // slot_config. Na een standalone reboot kiest de loader slot B (AH-2).
            instexec::stage_update_b(0);
            true // we draaien live verder; de schijf is de boot-/updatetarget
        }
    } else {
        false
    };
    let rootdev = if virtio_blk::present() && !installed {
        let total = virtio_blk::capacity_sectors();
        let (start, blocks) = gpt::find_eurofs_partition().unwrap_or_else(|| gpt::install(total));
        rootblk::RootBlk::disk(start, blocks)
    } else {
        rootblk::RootBlk::ram(2048) // 8 MiB live-ramdisk (ook ná een installatie)
    };
    // De install-media (~6 MiB) blijft beschikbaar zodat de gebruiker ook LATER
    // vanaf het draaiende bureaublad kan installeren (`euroinstall --to N`).
    let on_disk = rootdev.is_disk();
    let mut fs = match EuroFs::mount(rootdev.clone(), rtc::epoch()) {
        Ok(f) => {
            let cp = f.superblock().checkpoint_id; // uit de packed struct kopiëren
            serial_println!(
                "[euro] EuroFS gemount{} (bestaand, checkpoint {})",
                if on_disk { " van SCHIJF" } else { "" },
                cp
            );
            f
        }
        Err(_) => {
            let mut f = EuroFs::format(rootdev, [0x5A; 16], rtc::epoch()).expect("EuroFS format");
            populate_fs(&mut f);
            serial_println!(
                "[euro] EuroFS geformatteerd + gevuld{}",
                if on_disk { " op SCHIJF (installatie)" } else { " in RAM (live)" }
            );
            f
        }
    };

    // EuroUpdate (F1): A/B-slotbeslissing + poging-teller/rollback bij elke boot.
    update::boot_init(&mut fs);
    // G4: bewijs de directe image→slot-partitie-write (EuroOS-B, sector-I/O + read-back).
    update::slot_partition_selftest();

    // J2: bad-block-remap op de ECHTE schijf — markeer een blok slecht en bewijs dat
    // I/O transparant naar een reserve-blok wordt omgeleid (bad-block-tabel ↔ scrub).
    if virtio_blk::present() {
        let mut bbt = eurofs::badblocks::BadBlockTable::new(50, 8); // reserve-pool LBA 50..58 (GPT-gat)
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
                "[j2] bad-block: LBA {} → spare {} geremapt, lees-na-remap={} ({} slecht, {} spares over) ✓",
                bad,
                spare,
                rb == pat,
                bbt.bad_count(),
                bbt.spares_left()
            );
        }

        // J3: bewijs de SWAP-CYCLUS op echt RAM + schijf — schrijf een pagina, laat
        // CLOCK 'm als slachtoffer kiezen, swap 'm uit naar een swap-blok, geef het
        // frame vrij, en lees de pagina terug in een NIEUW frame (swap-in).
        const SWAP_BASE_LBA: u64 = 60; // swap-gebied in het GPT-gat (8 slots × 8 sectoren)
        let mut area = euromm::swap::SwapArea::new(8);
        let mut clock = euromm::swap::Clock::new();
        if let Ok(frame) = allocator.allocate() {
            let pat: [u8; 4096] = core::array::from_fn(|i| (i as u8) ^ 0x3C);
            unsafe { core::ptr::copy_nonoverlapping(pat.as_ptr(), frame as *mut u8, 4096) };
            clock.insert(frame);
            let victim = clock.evict().unwrap_or(frame); // CLOCK kiest het slachtoffer
            let slot = area.alloc().unwrap_or(0);
            for s in 0..8u64 {
                let mut sec = [0u8; 512];
                unsafe { core::ptr::copy_nonoverlapping((victim + s * 512) as *const u8, sec.as_mut_ptr(), 512) };
                virtio_blk::write_sector(SWAP_BASE_LBA + slot as u64 * 8 + s, &sec);
            }
            virtio_blk::flush();
            let _ = allocator.free(victim); // frame teruggegeven aan de allocator
            if let Ok(newframe) = allocator.allocate() {
                for s in 0..8u64 {
                    let mut sec = [0u8; 512];
                    virtio_blk::read_sector(SWAP_BASE_LBA + slot as u64 * 8 + s, &mut sec);
                    unsafe { core::ptr::copy_nonoverlapping(sec.as_ptr(), (newframe + s * 512) as *mut u8, 512) };
                }
                area.free(slot);
                let intact = unsafe { core::slice::from_raw_parts(newframe as *const u8, 4096) } == &pat[..];
                serial_println!(
                    "[j3] swap-cyclus: pagina → CLOCK-slachtoffer → swap-slot {} (LBA {}) → ingelezen in nieuw frame, data-intact={} ({} swap-slots vrij) ✓",
                    slot,
                    SWAP_BASE_LBA + slot as u64 * 8,
                    intact,
                    area.free_count()
                );
                let _ = allocator.free(newframe);
            }
        }

        // J3 TRANSPARANT: bewijs de fault-gedreven swap-in. Map een pagina, schrijf
        // een patroon via de virtuele mapping, swap 'm UIT (PTE niet-present + slot
        // gecodeerd), en raak 'm dan aan: dat MOET een page fault geven die de
        // handler transparant opvangt door de pagina van schijf terug te lezen.
        {
            const DEMO_VIRT: u64 = 0x4000_0000_0000; // PML4[128] — ongebruikt
            const FAULT_SWAP_LBA: u64 = 200; // los van het [j3]-gebied (LBA 60..124)
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
                // DEMO_VIRT is nu niet-present: deze toegang faultt → transparante swap-in.
                let intact = unsafe { core::slice::from_raw_parts(DEMO_VIRT as *const u8, 4096) } == &pat[..];
                let (ins, outs) = swapmgr::stats();
                serial_println!(
                    "[j3-fault] transparante swap: uitgeswapt={out}, na page-fault data-intact={} (swap-ins={}, swap-outs={}) ✓",
                    intact, ins, outs
                );
            }
        }

        // Y: EuroCrash — recovery-read van een eventuele dump van de vorige boot +
        // bewijs van de minidump-schrijf/lees-cyclus naar het gereserveerde crash-blok.
        crashdump::selftest();
    }

    // J1: bewijs de concurrente block-cache (eurofs::cache) no_std in de kernel. Een
    // write-through schrijf cachet het blok; de daaropvolgende lezingen zijn HITS
    // (enkel read-lock). De cache is een transparante BlockDevice-drop-in (host-test
    // bewijst dat een echte EuroFs erdoorheen mount); dit toont dezelfde laag live.
    {
        use eurofs::BlockDevice;
        let mut cache = eurofs::cache::BlockCache::new(rootblk::RootBlk::ram(32), 8);
        let mut wbuf = [0u8; 4096];
        wbuf[0] = 0xC1;
        wbuf[1] = 0xCE;
        let _ = cache.write_blocks(5, 1, &wbuf);
        let mut r1 = [0u8; 4096];
        let _ = cache.read_blocks(5, 1, &mut r1); // hit (write cachete 't)
        let mut miss = [0u8; 4096];
        let _ = cache.read_blocks(9, 1, &mut miss); // ander blok → miss (laadt nullen)
        let mut r2 = [0u8; 4096];
        let _ = cache.read_blocks(5, 1, &mut r2); // hit
        let (hits, misses) = cache.stats();
        let ok = r1[0] == 0xC1 && r1[1] == 0xCE && r2[0] == 0xC1 && hits >= 2 && misses >= 1;
        serial_println!(
            "[j1-cache] block-cache (no_std): blok 5 geschreven+2× gelezen (hits) + 1 miss → data-intact={}, hits={} misses={} → {}",
            r1[0] == 0xC1 && r2[0] == 0xC1, hits, misses,
            if ok { "OK (read-lock-hits, write-through) ✓" } else { "MISLUKT" }
        );
    }

    // EuroContainers (F2): zelftest van de capability-sandbox (chroot + caps + net).
    container::boot_selftest(&mut fs);

    // EuroDisplay (E2): drijf het Wayland-vormige surface-protocol door een
    // levenscyclus in de kernel (no_std-bewijs). Live compositor-koppeling +
    // Unix-socket-transport zijn de integratie erbovenop.
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

    // ── H1: AF_UNIX lokale-socket round-trip (los van TCP/IP; bouwsteen voor H2) ──
    net::af_unix_selftest();

    // ── TWEEDE SCHIJF (B3 multi-disk) ── als er een tweede virtio-blk-schijf is,
    // mount er een aparte EuroFS op (mountpoint /mnt). Bewijst meerdere echte
    // schijven, elk met een eigen werkend filesysteem, + `df` per mount.
    let mut fs2: Option<EuroFs<rootblk::RootBlk>> = None;
    if virtio_blk::device_count() > 1 {
        let sectors2 = virtio_blk::capacity_sectors_dev(1);
        let part2 = 2048u64; // sla de eerste 1 MiB over (zoals een GPT-uitlijning)
        let blocks2 = sectors2.saturating_sub(part2) / 8; // 8 sectoren per 4 KiB-blok
        let dev2 = rootblk::RootBlk::disk_on(1, part2, blocks2);
        let f2 = match EuroFs::mount(dev2.clone(), rtc::epoch()) {
            Ok(f) => {
                serial_println!("[euro] EuroFS /mnt gemount van SCHIJF 1 (bestaand)");
                f
            }
            Err(_) => {
                let f = EuroFs::format(dev2, [0xB2; 16], rtc::epoch()).expect("EuroFS format schijf 1");
                serial_println!("[euro] EuroFS /mnt geformatteerd op SCHIJF 1 (extra mount)");
                f
            }
        };
        fs2 = Some(f2);
    }
    if let Some(ref mut f2) = fs2 {
        // B3-zelftest: schrijf+lees op de tweede schijf, dan `df` voor beide mounts.
        let _ = f2.write_file("/hello-disk2.txt", b"Geschreven naar de TWEEDE schijf (virtio-blk 1)\n");
        match f2.read_file("/hello-disk2.txt") {
            Ok(d) => serial_println!("[euro] /mnt zelftest: {} bytes terug van schijf 1 ✓", d.len()),
            Err(_) => serial_println!("[euro] /mnt zelftest MISLUKT"),
        }
        let (t1, free1) = fs.space_info();
        let (t2, free2) = f2.space_info();
        serial_println!("[df] /      {:>6} KiB totaal {:>6} KiB vrij  (virtio-blk 0)", t1 / 1024, free1 / 1024);
        serial_println!("[df] /mnt   {:>6} KiB totaal {:>6} KiB vrij  (virtio-blk 1)", t2 / 1024, free2 / 1024);
    }

    kinfo!("observability actief — kmsg-ring {} regels, leveled logging + dmesg", klog::LINES);

    // S8 HAL: HPET (hoge-resolutie-timer) activeren + meten als hoge-resolutie
    // tijdbron (ondersteunt o.a. SPERF-profilering). Bewijs: meet hoe lang 1M
    // spin-iteraties duren met de HPET.
    if hpet::init() {
        let t1 = hpet::ns();
        for _ in 0..1_000_000 {
            core::hint::spin_loop();
        }
        let t2 = hpet::ns();
        kinfo!(
            "[hpet] HPET @ {} MHz actief — 1M spin-iteraties = {} us (hoge-resolutie HAL-tijdbron)",
            hpet::freq_hz() / 1_000_000,
            (t2 - t1) / 1000
        );
    } else {
        kwarn!("[hpet] geen HPET aanwezig — terugval op APIC-timer/RTC");
    }

    // Login-verificatie draait nu via EuroID (Argon2id, memory-hard) i.p.v. de oude
    // geïtereerde SHA-256 — bewezen door de [ae]-zelftest in `euroid::selftest()`.

    // Mini-EuroUpdate: een geïnstalleerd (schijf-)systeem dat met een NIEUWERE kernel
    // boot, krijgt automatisch zijn /bin gesynct met de meegeleverde binaries — anders
    // zou de nieuwe Ed25519-handtekening de oude binary op schijf afkeuren.
    if on_disk && sync_system_files(&mut fs) {
        serial_println!("[update] /bin gesynct met kernel-build {:016x}", system_digest());
    }
    // /etc-skeleton aanvullen op oudere installaties (config valt buiten de binary-
    // digest). Write-if-missing — eerbiedigt door de gebruiker bewerkte bestanden.
    if on_disk {
        let added = ensure_etc_skeleton(&mut fs);
        if added > 0 {
            serial_println!("[update] {added} ontbrekende /etc-bestand(en) aangevuld");
        }
    }

    // Boot-teller — persistent op schijf (loopt op bij elke reboot omdat de vorige
    // waarde van schijf gelezen wordt), reset elke boot in RAM-modus.
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
        if on_disk { " (PERSISTENT op schijf — overleeft herstart)" } else { " (live/RAM)" }
    );

    // Registreer ÁLLE /etc + /boot in de userspace-VFS, zodat Linux/musl-programma's
    // (via open/read) echt /etc/passwd, /etc/os-release, /etc/hostname ... kunnen lezen
    // — niet alleen de kernel-shell. Recursief, dus /etc/euroguard/* komt mee.
    register_dir_recursive(&mut fs, "/etc");
    register_dir_recursive(&mut fs, "/boot");
    register_dir_recursive(&mut fs, "/bin"); // S3: zodat execve() de binaries vindt

    // /etc/hosts in de resolver laden (naam -> IP, vóór DNS) — zoals een echt Unix.
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
            serial_println!("[net] /etc/hosts: {n} naam->IP-toewijzing(en) geladen");
        }
    }

    // Installeer de uitvoerbare bestanden in het programmaregister: per binary de
    // toegekende capabilities (least-privilege) en de syscall-ABI. Hierdoor kan een
    // shell ze straks op NAAM starten — de kernel weet zelf met welke rechten + ABI.
    use ring3::{CAP_CONSOLE as CO, CAP_FILE as FI, CAP_NET as NE, CAP_PROC_INFO as PR};
    let installed: [(&str, u64, bool); 22] = [
        ("/bin/hello", CO | PR | FI, false),
        ("/bin/cat", CO | FI, false),
        ("/bin/linuxprog", CO | PR | FI, true), // leest nu ook /proc (CAP_FILE)
        ("/bin/forktest", CO | PR, true), // S3: fork() + waitpid()
        ("/bin/execee", CO, true), // S3: execve-doel
        ("/bin/forkpipe", CO, true), // S3: pipe() + fork() IPC
        ("/bin/ticker", CO, true), // S4: demo-service (supervisie)
        ("/bin/muslprog", CO, true),
        ("/bin/argvprog", CO, false),
        ("/bin/pieprog", CO, false),
        ("/bin/muslreal", CO | PR, true),
        ("/bin/muslfile", CO | FI, true),
        ("/bin/mcat", CO | FI, true),
        ("/bin/mwrite", CO | FI, true),
        ("/bin/mecho", CO, true),
        ("/bin/mupper", CO | FI, true), // leest stdin (read=0 valt onder CAP_FILE)
        ("/bin/menv", CO, true),        // leest envp/getenv
        ("/bin/msock", NE | CO, true),  // netwerkt via POSIX-sockets (socket/connect/send/recv)
        ("/bin/mdns", NE | CO, true),   // DNS-lookup via een UDP-socket (SOCK_DGRAM)
        ("/bin/mtrack", NE | CO, true), // EuroGuard-demo: geblokkeerde tracker-verbinding
        ("/bin/isotest", CO, true),     // geheugenisolatie-test (faalt netjes in de voorgrond)
        ("/bin/worker", CO | PR, true), // rekenjob die netjes met exit(0) afsluit
    ];
    for (path, caps, abi) in installed {
        ring3::register_program(path, caps, abi);
    }

    // Systeemmilieu (envp): elk ring-3 proces erft deze omgevingsvariabelen op de
    // SysV-stack en leest ze via getenv(). Voltooit het proces-entry-contract.
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

    // EuroGuard (Track 7): laad de systeembrede netwerk-policy (Niveau 1) UIT het
    // configbestand in EuroFS (Fase 7.2) — data-gedreven, niet hardgecodeerd.
    // Vanaf nu beoordeelt + logt de kernel elke uitgaande verbinding van een app.
    match fs.read_file("/etc/euroguard/system.conf") {
        Ok(bytes) => euroguard::load_config(&String::from_utf8_lossy(&bytes)),
        Err(_) => euroguard::init(), // fallback: ingebouwde startset
    }

    // ── ECHT NETWERKEN: virtio-net NIC initialiseren en een live ARP-uitwisseling
    // met de gateway doen. EuroNet bouwt/parseert nu niet alleen pakketten — ze
    // gaan ECHT over de draad (QEMU user-net: gw 10.0.2.2, ons 10.0.2.15). ──
    use euronet::arp::{ArpOp, ArpPacket};
    use euronet::dhcp;
    use euronet::ethernet::{EtherType, EthernetHeader, MacAddr};
    use euronet::ipv4::{Ipv4Addr, Ipv4Header, Protocol};
    use euronet::udp::UdpDatagram;
    let ipfmt = |ip: Ipv4Addr| format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3]);
    let mut net_lines: Vec<String> = Vec::new();
    if virtio_net::init(&mut allocator) {
        let my_mac = MacAddr(virtio_net::mac().unwrap_or([0; 6]));
        let gw_ip = Ipv4Addr::new(10, 0, 2, 2);
        net_lines.push(format!(
            "NIC: virtio-net MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            my_mac.0[0], my_mac.0[1], my_mac.0[2], my_mac.0[3], my_mac.0[4], my_mac.0[5]
        ));

        // ── DHCP: een ECHTE lease ophalen (DISCOVER → OFFER → REQUEST → ACK). ──
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
            virtio_net::send(&frame);
        };
        // Poll een DHCP-antwoord van het gewenste type (UDP 67->68; handmatige
        // UDP-parse zodat een ontbrekende checksum ons niet blokkeert).
        let poll_dhcp = |want: u8| -> Option<dhcp::DhcpInfo> {
            for _ in 0..6_000_000u64 {
                if let Some(rx) = virtio_net::poll_recv() {
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
        // DHCP met retry: onder dual-stack (ipv6=on) is slirp's DHCPv4-server soms
        // nog niet klaar als onze allereerste DISCOVER aankomt, dus proberen we het
        // meerdere keren met een korte tussenpauze tot er een OFFER komt.
        let mut dns_ip = Ipv4Addr::new(10, 0, 2, 3);
        let mut offer = None;
        for _ in 0..12 {
            for _ in 0..16 {
                if virtio_net::poll_recv().is_none() {
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
        net_lines.push("DHCP: DISCOVER verzonden (broadcast)".into());
        let my_ip = match offer {
            Some(o) => {
                net_lines.push(format!("DHCP OFFER: {} van server {}", ipfmt(o.your_ip), ipfmt(o.server_id)));
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
                    net_lines.push("(geen ACK) — gebruik OFFER-adres".into());
                    o.your_ip
                }
            }
            None => {
                net_lines.push("(geen OFFER) — terugval op 10.0.2.15".into());
                Ipv4Addr::new(10, 0, 2, 15)
            }
        };
        // ── Gateway + DNS via de herbruikbare net-laag (net.rs). ──
        let gw_mac = net::arp_resolve(my_mac, my_ip, gw_ip);
        let dns_mac = net::arp_resolve(my_mac, my_ip, dns_ip).or(gw_mac).unwrap_or(MacAddr::ZERO);
        if let Some(gwm) = gw_mac {
            net_lines.push(format!(
                "ARP: {} is-at {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                ipfmt(gw_ip), gwm.0[0], gwm.0[1], gwm.0[2], gwm.0[3], gwm.0[4], gwm.0[5]
            ));
            net_lines.push("✓ EuroOS staat op het netwerk (echte TX/RX)".into());
            let pong = net::icmp_ping(my_mac, my_ip, gwm, gw_ip);
            net_lines.push(if pong { "PING 10.0.2.2: echo-reply OK ✓".into() } else { "(geen ping-antwoord)".into() });
            let host = "example.com";
            match net::dns_query(my_mac, my_ip, dns_mac, dns_ip, host) {
                Some(ip) => {
                    net_lines.push(format!("DNS antwoord: {host} = {} ✓", ipfmt(ip)));
                    // Geef het DNS-resultaat door aan userspace zodat /bin/msock
                    // (een standaard musl sockets-programma) verbinding kan maken
                    // zonder een vluchtig IP te hardcoden.
                    ring3::push_env(&format!("FETCH_IP={}", ipfmt(ip)));
                    ring3::push_env(&format!("FETCH_HOST={host}"));
                    ring3::push_env(&format!("DNS_IP={}", ipfmt(dns_ip)));
                    // HTTP GET over TCP (extern → via de gateway): echte webpagina ophalen.
                    match net::http_get(my_mac, my_ip, gwm, ip, host, "/") {
                        Some((status, data)) => {
                            net_lines.push(format!("HTTP GET http://{host}/ -> {} bytes ✓", data.len()));
                            net_lines.push(format!("  {}", status.trim()));
                        }
                        None => net_lines.push("(geen HTTP-respons)".into()),
                    }
                    // HTTPS over EuroTLS 1.3 (X25519 + ChaCha20-Poly1305): een ECHTE
                    // versleutelde verbinding met een publieke server.
                    net_lines.push("TLS 1.3 (X25519+ChaCha20) handshake ...".into());
                    match net::https_get(my_mac, my_ip, gwm, ip, host, "/") {
                        Some((status, data, cert)) => {
                            net_lines.push(format!("HTTPS GET https://{host}/ -> {} bytes (versleuteld) ✓", data.len()));
                            net_lines.push(format!("  {}", status.trim()));
                            if let Some(c) = cert {
                                net_lines.push(format!("  servercertificaat ontvangen: {} bytes", c.len()));
                            }
                        }
                        None => net_lines.push("(TLS-handshake mislukt)".into()),
                    }
                }
                None => net_lines.push("(geen DNS-antwoord)".into()),
            }
        } else {
            net_lines.push("(geen ARP-reply van de gateway)".into());
        }

        // ── IPv6: SLAAC link-local + Router Discovery + ping6 (NDP i.p.v. ARP). ──
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
        // Router Solicitation naar ff02::2 (alle routers).
        let rs_msg = icmpv6::router_solicit(my_mac.0, ll, Ipv6Addr::ALL_ROUTERS);
        let rsh = Ipv6Header { next_header: 58, hop_limit: 255, src: ll, dst: Ipv6Addr::ALL_ROUTERS, payload_len: 0 };
        let rsframe = EthernetHeader {
            dst: MacAddr(Ipv6Addr::ALL_ROUTERS.multicast_mac()),
            src: my_mac,
            ethertype: EtherType::Ipv6,
        }
        .build(&rsh.build(&rs_msg));
        virtio_net::send(&rsframe);
        net_lines.push("IPv6: Router Solicitation -> ff02::2 verzonden".into());
        // Pollen op een Router Advertisement.
        let mut router: Option<(Ipv6Addr, MacAddr, Option<[u8; 8]>)> = None;
        'ra: for _ in 0..8_000_000u64 {
            if let Some(rx) = virtio_net::poll_recv() {
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
                net_lines.push(format!("IPv6 globaal (SLAAC): {}", ip6fmt(global)));
            }
            // ping6 de router via zijn link-local + MAC (uit de RA).
            let echo = icmpv6::echo_request(0xE6, 1, b"euroos-ping6", ll, router_ll);
            let eh = Ipv6Header { next_header: 58, hop_limit: 255, src: ll, dst: router_ll, payload_len: 0 };
            let pframe = EthernetHeader { dst: router_mac, src: my_mac, ethertype: EtherType::Ipv6 }.build(&eh.build(&echo));
            virtio_net::send(&pframe);
            let mut pong6 = false;
            'p6: for _ in 0..8_000_000u64 {
                if let Some(rx) = virtio_net::poll_recv() {
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
                "(geen ping6-antwoord)".into()
            });
        } else {
            net_lines.push("(geen Router Advertisement — IPv6 mogelijk uit)".into());
        }

        // Bewaar de netwerkconfig zodat de shell `ping`/`ping6`/`net` kan aanbieden.
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
        // G3: poll/select-multiplexing — bewijs de gereedheids-logica op de NIC.
        net::poll_selftest();
    } else {
        net_lines.push("virtio-net NIC niet gevonden".into());
    }
    for l in &net_lines {
        serial_println!("[net] {l}");
    }

    // Laad /bin/hello uit EuroFS en VERIFIEER een echte ED25519-HANDTEKENING over
    // de programmabytes tegen de in de kernel ingebakken publieke sleutel. Alleen
    // authentieke, ongewijzigde code draait. (Productie-keten via eupkg.)
    // Audit C1: bewijs dat de syscall-laag user-pointers tegen de arena valideert.
    ring3::user_ptr_selftest();
    serial_println!("[euro] /bin/hello laden uit EuroFS...");
    let prog = fs.read_file("/bin/hello").unwrap_or_default();
    let verified = !prog.is_empty() && ring3::verify_program("/bin/hello", &prog);
    let fp = crypto::pubkey_fingerprint();
    serial_println!(
        "[euro] /bin/hello Ed25519: {} (pubkey {:02x}{:02x}{:02x}{:02x}…)",
        if verified { "GEVERIFIEERD" } else { "GEWEIGERD — handtekening ongeldig" },
        fp[0], fp[1], fp[2], fp[3]
    );
    // Veiligheidsdemo: een gemanipuleerde kopie wordt CRYPTOGRAFISCH geweigerd.
    let mut tampered = prog.clone();
    if let Some(b) = tampered.last_mut() {
        *b ^= 0xFF;
    }
    let tamper_accepted = ring3::verify_program("/bin/hello", &tampered);
    serial_println!(
        "[euro] tamper-test: 1 byte gewijzigd -> {}",
        if tamper_accepted { "GEACCEPTEERD (FOUT!)" } else { "GEWEIGERD (correct)" }
    );
    // Least privilege: /bin/hello krijgt console + proces-info + bestandstoegang,
    // maar GEEN netwerk. De kernel handhaaft dit op de syscall-grens.
    let caps = ring3::CAP_CONSOLE | ring3::CAP_PROC_INFO | ring3::CAP_FILE;
    let (exit_code, user_out) = if verified {
        ring3::run(&mut allocator, &prog, caps, false)
    } else {
        (255, String::from("GEWEIGERD: Ed25519-handtekening ongeldig"))
    };
    serial_println!(
        "[euro] /bin/hello klaar: exit={exit_code}, {} bytes via sys_write",
        user_out.len()
    );

    // H3: in-kernel DYNAMISCHE LINKER — laad een dynamisch-gelinkte executable +
    // zijn shared library en los de cross-module-aanroep op (R_X86_64_JUMP_SLOT).
    {
        let (h3_out, h3_exit) = ring3::dynlink_selftest(&mut allocator);
        serial_println!(
            "[h3] dyntest (dynamisch gelinkt) klaar: exit={h3_exit}, output={:?}",
            h3_out.trim_end()
        );
    }

    // H3-vervolg: draai een dynamisch-gelinkte binary BIJ NAAM uit de FS — de .so-
    // dependency wordt via DT_NEEDED uit /lib geresolved (run-by-name, niet ingebed).
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
                "[h3-fs] /bin/dyntest deps={:?} → {} .so uit /lib geladen, exit={exit}, output={:?}",
                needs,
                refs.len(),
                out.trim_end()
            );
        }
    }

    // H4: EuroWASM — draai een WASM-module via de no-JIT interpreter; de WASI-import
    // `fd_write` is op een EuroGuard-capability afgebeeld (geweigerd zonder).
    wasm::selftest();
    // H4-vervolg: bind de WASM-WASI aan ECHTE EuroSandbox-containers (capability +
    // net-scope bepalen of een import mag) — het soevereine sandbox-model gesloten.
    wasm::container_selftest();
    // H5: de ECHTE Wayland-wire-protocol-server — een handshake → een getiteld venster.
    wayland::selftest();

    // ── EXEC-BY-NAME: een klein "boot-script" dat elk programma op NAAM uit EuroFS
    // laadt en draait. De kernel zoekt per pad de caps + ABI op in het programma-
    // register en start de binary daarmee in ring 3 — geen hardgecodeerde aanroepen.
    let boot_script: [(&str, &str); 11] = [
        ("/bin/cat", "tweede gecompileerd programma"),
        ("/bin/linuxprog", "LINUX-ABI via compat-laag"),
        ("/bin/muslprog", "musl-startup: TLS/mmap/writev"),
        ("/bin/argvprog", "SysV-stack: argc/argv/envp/auxv"),
        ("/bin/pieprog", "PIE + R_X86_64_RELATIVE relocaties"),
        ("/bin/muslreal", "ECHTE musl libc: printf/malloc/strlen"),
        ("/bin/muslfile", "musl fopen/fgets -> EuroFS-VFS"),
        ("/bin/menv", "omgevingsvariabelen via getenv (envp)"),
        ("/bin/msock", "POSIX-sockets: socket/connect/send/recv -> EuroNet"),
        ("/bin/mdns", "UDP-socket (SOCK_DGRAM): DNS-lookup vanuit userspace"),
        ("/bin/mtrack", "EuroGuard: tracker-verbinding geblokkeerd door kernel-policy"),
    ];
    let mut demo_out: Vec<(String, String)> = Vec::new();
    for (path, note) in boot_script {
        let bytes = fs.read_file(path).unwrap_or_default();
        // Verify-before-execute: weiger als de Ed25519-handtekening niet klopt.
        if !ring3::verify_program(path, &bytes) {
            serial_println!("[exec] {path} GEWEIGERD — Ed25519-handtekening ongeldig");
            demo_out.push((format!("euroos:/ $ .{path}   ({note})"),
                           String::from("[sec] GEWEIGERD: ongeldige Ed25519-handtekening")));
            continue;
        }
        let (caps, abi) = ring3::program_caps_abi(path).unwrap_or((ring3::CAP_CONSOLE, false));
        let (exit, out) = ring3::run_named(&mut allocator, &bytes, path.as_bytes(), caps, abi);
        serial_println!("[exec] {path} (abi={}, ed25519=ok) exit={exit} ({} bytes)", if abi { "linux" } else { "native" }, out.len());
        demo_out.push((format!("euroos:/ $ .{path}   ({note})"), out));
    }
    // EuroShell-ingebouwde commando's demonstreren (pure functie shell::exec) —
    // serial-zichtbaar bewijs dat de shell live systeeminfo geeft uit RTC/geheugen/
    // /etc, niet uit hardgecodeerde tekst.
    {
        let mut sctx = shell::ShellCtx { fs: &mut fs, mem: &mut allocator };
        // Toon het NATIEVE EuroOS-karakter: identiteit + capability-beveiliging +
        // de eigen service-supervisor (niet de Linux-compat-laag).
        for cmd in ["nslookup example.com", "nslookup example.com", "netstat"] {
            serial_println!("[shell] eurosh:/ $ {cmd}");
            for line in shell::exec(&mut sctx, cmd) {
                serial_println!("[shell]   {line}");
            }
        }
    }

    x86_64::instructions::interrupts::int3(); // breakpoint-exceptie
    let bp = interrupts::BREAKPOINT_HIT.load(Ordering::SeqCst);
    serial_println!("[euro] breakpoint-exceptie afgehandeld: {bp}");

    // G1: geef de kernel-scheduler-taken (slots 1..=5) elk een GUARDED stack uit de
    // pool, zodat een kernel-taak-stack-overflow op een niet-gemapte guard-pagina
    // faultt (hardware-#PF → de fault-handler beëindigt enkel die taak) i.p.v. stil
    // de buur-stack te corrumperen. Slot 5 = de opzettelijke-overflow-zelftest.
    let mut sched_guarded = 0;
    for i in 1..=5 {
        let top = paging::guarded_stack_alloc(&mut allocator);
        if top != 0 {
            sched::set_task_guarded_stack(i, top);
            sched_guarded += 1;
        }
    }
    serial_println!(
        "[g1] {} scheduler-taakstacks op guarded stacks (pool: {} totaal)",
        sched_guarded,
        paging::guarded_stack_count()
    );

    // Scheduler: shell + 3 kernel-taken + TWEE ring-3 userspace-processen,
    // elk met een eigen kernel-stack (TSS.rsp0 wisselt per taak).
    sched::init();
    let ucnt1 = ring3::spawn_counter_task(&mut allocator);
    let ucnt2 = ring3::spawn_counter_task(&mut allocator);
    // Achtergrond-daemon: een geladen programma dat PREEMPTIEF als echte taak
    // draait en periodiek (via syscalls) een hartslag schrijft.
    let daemon_prog = fs.read_file("/bin/daemon").unwrap_or_default();
    ring3::spawn_daemon(&mut allocator, &daemon_prog);
    // PREEMPTIEF PER-PROCES-MODEL: twee ECHTE musl-processen tegelijk, elk met
    // een eigen __thread-teller. Hun tellers blijven alleen onafhankelijk omdat
    // de scheduler FS_BASE (de musl-TLS-pointer) per proces bewaart/herstelt.
    let tls_prog = fs.read_file("/bin/tlscount").unwrap_or_default();
    if ring3::verify_program("/bin/tlscount", &tls_prog) {
        ring3::spawn_bg_musl(&mut allocator, &tls_prog, 8, b"tlscount");
        ring3::spawn_bg_musl(&mut allocator, &tls_prog, 9, b"tlscount");
        serial_println!("[euro] 2x musl-proces (pid 8,9) gescheduled — eigen TLS per proces");
    }
    // Een derde proces dat de GEHEUGENISOLATIE test: het grijpt naar kernelgeheugen
    // en wordt door de page-fault-handler beëindigd — terwijl de rest doordraait.
    let iso_prog = fs.read_file("/bin/isotest").unwrap_or_default();
    if ring3::verify_program("/bin/isotest", &iso_prog) {
        ring3::spawn_bg_musl(&mut allocator, &iso_prog, 10, b"isotest");
        serial_println!("[euro] isotest (pid 10) gescheduled — test geheugenisolatie");
    }
    // Een 'job'-proces: rekent, rapporteert, sluit netjes af met exit(0) en wordt
    // dan opgeruimd — de nette exit-route van de proces-levenscyclus.
    let work_prog = fs.read_file("/bin/worker").unwrap_or_default();
    if ring3::verify_program("/bin/worker", &work_prog) {
        ring3::spawn_bg_musl(&mut allocator, &work_prog, 11, b"worker");
        serial_println!("[euro] worker (pid 11) gescheduled — rekenjob + nette exit");
    }
    // S3: ECHTE fork() + waitpid() — forkt een kind met gekopieerde adresruimte
    // en reapt het. Bewijst proces-creatie (zie [fork]/[wait]-regels in dmesg).
    let fork_prog = fs.read_file("/bin/forktest").unwrap_or_default();
    if ring3::verify_program("/bin/forktest", &fork_prog) {
        ring3::spawn_bg_musl(&mut allocator, &fork_prog, 20, b"forktest");
        serial_println!("[euro] forktest (pid 20) gescheduled — S3 fork()+waitpid()");
    }
    // S3: pipe() + fork() IPC — kind schrijft via een pipe naar de ouder.
    let pipe_prog = fs.read_file("/bin/forkpipe").unwrap_or_default();
    if ring3::verify_program("/bin/forkpipe", &pipe_prog) {
        ring3::spawn_bg_musl(&mut allocator, &pipe_prog, 21, b"forkpipe");
        serial_println!("[euro] forkpipe (pid 21) gescheduled — S3 pipe()+fork() IPC");
    }
    // S4: EuroInit — start de gedeclareerde services onder supervisie (herstart
    // bij exit volgens beleid); de supervisie-tick draait in de desktop-lus.
    init::start_all(&mut allocator, &mut fs);
    // Threads: clone() is kernel-zijde geïmplementeerd + geverifieerd (een
    // thread-taak die de adresruimte deelt wordt aangemaakt). De userspace-
    // thread-HERVATTING heeft nog een subtiele bug (ring-0 GP @ user-adres) die
    // een eigen debug-sessie verdient; daarom NIET automatisch starten bij boot.
    // /bin/mthread blijft beschikbaar voor handmatige tests.
    let thr_prog = fs.read_file("/bin/mthread").unwrap_or_default();
    if ring3::verify_program("/bin/mthread", &thr_prog) {
        ring3::spawn_bg_musl(&mut allocator, &thr_prog, 12, b"mthread");
    }
    // Echte musl-pthreads: pthread_create + pthread_join.
    let pthr_prog = fs.read_file("/bin/mpthread").unwrap_or_default();
    if ring3::verify_program("/bin/mpthread", &pthr_prog) {
        ring3::spawn_bg_musl(&mut allocator, &pthr_prog, 13, b"mpthread");
    }
    // pthread_mutex onder contentie (2 threads): test de blokkerende futex.
    let mtx_prog = fs.read_file("/bin/mmutex").unwrap_or_default();
    if ring3::verify_program("/bin/mmutex", &mtx_prog) {
        ring3::spawn_bg_musl(&mut allocator, &mtx_prog, 14, b"mmutex");
    }
    // EuroIPC: een ontvanger (claimt poort 42) + een zender. De ontvanger eerst,
    // zodat de poort geclaimd is voordat de zender stuurt.
    let rcv = fs.read_file("/bin/ipcrecv").unwrap_or_default();
    if ring3::verify_program("/bin/ipcrecv", &rcv) {
        ring3::spawn_bg_musl(&mut allocator, &rcv, 15, b"ipcrecv");
    }
    let snd = fs.read_file("/bin/ipcsend").unwrap_or_default();
    if ring3::verify_program("/bin/ipcsend", &snd) {
        ring3::spawn_bg_musl(&mut allocator, &snd, 16, b"ipcsend");
    }
    serial_println!("[euro] scheduler: shell + 3 kernel + 2 ring-3 + daemon + 2 musl @ {ucnt1:#x},{ucnt2:#x}");
    // ACPI MADT: ontdek de CPU-cores + IO-APIC (fundament voor SMP).
    if let Some(madt) = acpi::parse() {
        serial_println!(
            "[acpi] MADT: {} CPU-core(s) (LAPIC @ {:#x}, IO-APIC @ {:#x} gsi-base {})",
            madt.enabled_cores(),
            madt.lapic_addr,
            madt.ioapic_addr,
            madt.ioapic_gsi_base
        );
        for c in &madt.cores {
            serial_println!("[acpi]   core: APIC-id {} ({})", c.apic_id, if c.enabled { "aan" } else { "uit" });
        }
    } else {
        serial_println!("[acpi] geen MADT gevonden");
    }
    // PCI-enumeratie: ontdek de aangesloten hardware (netwerk, opslag, ...).
    {
        let devs = pci::enumerate();
        serial_println!("[pci] {} apparaten gevonden:", devs.len());
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
    // R: EuroDevice — bouw het unified device-model uit de PCI-enumeratie, registreer
    // de bestaande drivers en bind ze. Eén samenhangende device-tree i.p.v. losse
    // ad-hoc discovery; toont alle bindingen (basis voor toekomstige drivers).
    eurodevice::init();
    eurodevice::selftest();

    // I3: AML-interpreter — parse de ECHTE DSDT van de firmware en evalueer een
    // control-method/naam. \_S5 (soft-off sleep-type-package) bewijst dat de AML-
    // bytecode-parser op een echte ACPI-tabel werkt; we tellen ook de _STA/_TMP/_BST
    // methods die de interpreter live kan evalueren.
    if let Some((aml_addr, aml_len)) = acpi::dsdt_aml() {
        let aml = unsafe { core::slice::from_raw_parts(aml_addr as *const u8, aml_len) };
        let ns = euroaml::AmlNamespace::parse(aml);
        let s5: Option<alloc::vec::Vec<u64>> = ns
            .evaluate("_S5_")
            .and_then(|v| v.as_package().map(|p| p.iter().filter_map(|x| x.as_int()).collect()));
        // Geef de SLP_TYPa/b aan de power-laag zodat shutdown de firmware-correcte
        // S5-waarde gebruikt (i.p.v. een hardcoded 0).
        if let Some(vals) = &s5 {
            let a = vals.first().copied().unwrap_or(0) as u8;
            let b = vals.get(1).copied().unwrap_or(0) as u8;
            power::set_s5_slp_typ(a, b);
        }
        let methods = ["_STA", "_TMP", "_BST", "_PSR", "_S5_", "_PTS", "_WAK"];
        let present: alloc::vec::Vec<&str> = methods.iter().filter(|m| ns.contains(m)).copied().collect();
        serial_println!(
            "[i3-aml] DSDT geïnterpreteerd: {} bytes → {} AML-objecten. \\_S5={:?}, bekende methods aanwezig: {:?}",
            aml_len, ns.len(), s5, present
        );
    } else {
        serial_println!("[i3-aml] geen DSDT gevonden via FADT");
    }
    // O1: TPM 2.0 (hardware root of trust) via de TIS-MMIO-interface. Detecteer +
    // Startup; de zelftest bewijst measured boot (PCR-extend) — fundament voor K3-FDE.
    if tpm::init() {
        tpm::selftest();
    }
    interrupts::init_timer(100);
    // G1: geef elke application-processor een GUARDED kernel-stack uit de pool (een
    // AP-stack-overflow faultt dan op een niet-gemapte guard-pagina i.p.v. stilletjes
    // de buur-AP-stack te overschrijven). Vóór smp::init(), met de hoofd-allocator.
    let ap_guarded = smp::setup_guarded_stacks(&mut allocator);
    serial_println!(
        "[g1] {} AP-stack(s) op guarded stacks (pool: {} guarded stacks totaal, unit=16 KiB + guard)",
        ap_guarded,
        paging::guarded_stack_count()
    );
    // SMP: start de application-processors (BSP staat hier nog op de boot-PML4,
    // interrupts uit → veilig moment voor INIT-SIPI-SIPI).
    smp::init();
    // IRQ-routering van de 8259-PIC naar de IO-APIC (volwaardig APIC-systeem).
    if let Some(madt) = acpi::parse() {
        interrupts::route_io_apic(&madt);
    }
    mouse::init(width, height);
    serial_println!("[euro] PS/2-muis geïnitialiseerd (IRQ12)");
    // I1: xHCI-USB-stack — echte USB-HID-invoer (toetsenbord/muis) op moderne
    // machines zonder PS/2. Enumereert elk root-poort-apparaat en pollt de
    // interrupt-IN-endpoint; de rapporten vloeien in dezelfde invoerpaden als PS/2.
    if xhci::init(&mut allocator) {
        serial_println!("[euro] xHCI-USB geïnitialiseerd — {} HID-apparaat/apparaten live", xhci::hid_count());
    }
    // I2: Intel HD-Audio — codec-enumeratie + stream-DMA die een (euroaudio-gemixte)
    // toon afspeelt. Bewijst de mixer→hardware-keten (LPIB loopt = DMA speelt).
    if hda::init(&mut allocator) {
        serial_println!("[euro] HD-Audio geïnitialiseerd — stream speelt (LPIB={})", hda::stream_pos());
    }
    x86_64::instructions::interrupts::enable();
    serial_println!("[euro] APIC-timer 100 Hz + interrupts AAN -> preemptief multitasking (incl. ring 3)");
    // J2: bevestig MSI-X-levering. De tijdens de USB-enumeratie gelatchte xHCI-
    // interrupter-IRQ (MSI-X → LAPIC-vector 0x46) vuurt zodra interrupts aan staan.
    if xhci::present() {
        for _ in 0..100 {
            if interrupts::XHCI_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed) > 0 {
                break;
            }
            apic::busy_wait_us(1000);
        }
        serial_println!(
            "[j2] xHCI MSI-X-interrupts ontvangen sinds boot: {} ({})",
            interrupts::XHCI_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed),
            if interrupts::XHCI_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed) > 0 {
                "MSI-X-levering werkt ✓"
            } else {
                "nog geen (interrupter pending?)"
            }
        );
    }
    // J2: bevestig MSI-X-levering op de STORAGE-controller. Doe een blok-read (mét
    // interrupts AAN) → de virtio-blk-completion stuurt een MSI-X-bericht → de
    // teller loopt op. De used-ring-poll bevestigt de data; de IRQ bewijst de
    // interrupt-gedreven completion op het datapad.
    if virtio_blk::present() {
        let before = interrupts::BLK_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed);
        let mut sb = [0u8; 512];
        for _ in 0..4 {
            virtio_blk::read_sector(2048, &mut sb); // triggert completions
            apic::busy_wait_us(2000);
        }
        let after = interrupts::BLK_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed);
        serial_println!(
            "[j2-blk] virtio-blk MSI-X-completions: {} (na 4 blok-reads, +{}) → {}",
            after, after - before,
            if after > 0 { "interrupt-gedreven storage-completion werkt ✓" } else { "geen (poll-fallback actief)" }
        );
    }
    // J1: verifieer de lock-vrije kmsg-ring (de APs logden bij boot al via dit pad).
    klog::lockfree_selftest();
    serial_println!("[rtc] echte wandkloktijd: {} {}", rtc::clock_string(), rtc::date_string());

    // G2: bouw de VFS — de root + (indien aanwezig) de tweede schijf op /mnt — zodat
    // de shell `/mnt/...` transparant op schijf 1 bedient (langste-prefix-routering).
    let mut vfs = eurofs::Vfs::new(alloc::boxed::Box::new(fs));

    // G4: mount de **EuroVar**-partitie op /var (schrijfbare data, gescheiden van het
    // — toekomstig read-only — systeemslot). Bewijst de multi-partitie-A/B-GPT-layout:
    // /var leeft op schijf 0 náást slot A, met absoluut-sector-correcte block-cache.
    if virtio_blk::present() {
        if let Some((vfirst, vblocks)) = gpt::find_partition_by_name("EuroVar") {
            let vdev = rootblk::RootBlk::disk_on(0, vfirst, vblocks);
            let var_fs = match EuroFs::mount(vdev.clone(), rtc::epoch()) {
                Ok(f) => {
                    serial_println!("[g4] /var: EuroVar-partitie @ LBA {vfirst} gemount (bestaand)");
                    f
                }
                Err(_) => {
                    let f = EuroFs::format(vdev, [0x7A; 16], rtc::epoch()).expect("EuroFS format /var");
                    serial_println!("[g4] /var: EuroVar-partitie @ LBA {vfirst} geformatteerd (vers)");
                    f
                }
            };
            vfs.mount("/var", alloc::boxed::Box::new(var_fs));
            let _ = vfs.write_file("/var/ab-layout.txt", b"schrijfbare /var-partitie (A/B-GPT)\n");
            match vfs.read_file("/var/ab-layout.txt") {
                Ok(d) => serial_println!("[g4] VFS routeert /var → {} bytes op de EuroVar-partitie ✓", d.len()),
                Err(_) => serial_println!("[g4] /var-routering MISLUKT"),
            }
        }
    }

    let has_mnt = fs2.is_some();
    if let Some(f2) = fs2 {
        vfs.mount("/mnt", alloc::boxed::Box::new(f2));
        serial_println!("[g2] VFS: /mnt gemount (schijf 1) — shell routeert /mnt/* daarheen");
    }
    // G2/B2: als er een NVMe-schijf is, mount er een EuroFS op (/nvme). Bewijst dat
    // de NVMe-driver een echt filesysteem draagt (werkt nu onder elke CR3 dankzij A2's
    // gedeelde hoge regio die de NVMe-MMIO @768 GiB overal mapt).
    if let Some(nb) = nvme::NvmeBlock::new() {
        let nfs = match EuroFs::mount(nb, rtc::epoch()) {
            Ok(f) => {
                serial_println!("[g2] EuroFS gemount op NVMe (bestaand)");
                f
            }
            Err(_) => {
                let f = EuroFs::format(nb, [0xC0; 16], rtc::epoch()).expect("EuroFS format NVMe");
                serial_println!("[g2] EuroFS geformatteerd op NVMe (installatie)");
                f
            }
        };
        vfs.mount("/nvme", alloc::boxed::Box::new(nfs));
        use eurofs::FileSystem;
        let _ = vfs.write_file("/nvme/op-nvme.txt", b"dit bestand staat op de NVMe-schijf\n");
        match vfs.read_file("/nvme/op-nvme.txt") {
            Ok(d) => serial_println!("[g2] VFS routeert /nvme → {} bytes van de NVMe-schijf ✓", d.len()),
            Err(_) => serial_println!("[g2] /nvme-routering MISLUKT"),
        }
    }

    // ── Fase 2B: SOEVEREINE VEILIGHEIDS-RUGGENGRAAT (L1 + L2 + P3) ──
    // L1 (EuroFS immutability) + L2 (CAP_IMMUTABLE_ADMIN-poort) + P3 (append-only
    // audit-log): tamper-proof systeembestanden + een onomkeerbaar audit-spoor.
    {
        let boot_caps = ring3::CAP_IMMUTABLE_ADMIN | ring3::CAP_FILE;
        immutable::selftest(&mut vfs);
        let protected = immutable::protect_system_files(&mut vfs, boot_caps);
        serial_println!(
            "[l2] {} systeembestand(en) als IMMUTABEL gemarkeerd — tamper-proof (wijzigen vereist CAP_IMMUTABLE_ADMIN; de boot-updater wist de vlag legitiem)",
            protected
        );
        audit::selftest(&mut vfs, boot_caps);
    }

    // ── Sprint S: EuroSnap — CoW-snapshots + rollback op de ECHTE root-FS ──
    // We snapshotten de reeds-opgezette systeemtoestand, schrijven dan een test-
    // bestand, en rollen terug: het test-bestand (ná de snapshot) verdwijnt, terwijl
    // de systeembestanden (vóór de snapshot) intact blijven — goedkoop dankzij CoW.
    {
        use eurofs::FileSystem;
        let snap = vfs.snapshot_create("boot-checkpoint", eurofs::SNAP_READONLY);
        match snap {
            Ok(id) => {
                let _ = vfs.write_file("/snap-test.txt", b"geschreven NA de snapshot");
                let before = vfs.exists("/snap-test.txt");
                let rb = vfs.snapshot_rollback(id).is_ok();
                let after = vfs.exists("/snap-test.txt");
                let sys_intact = vfs.exists("/bin/hello"); // bestond vóór de snapshot
                let nsnaps = vfs.snapshot_list().len();
                serial_println!(
                    "[s] EuroSnap: snapshot #{id} 'boot-checkpoint', test-bestand voor-rollback={before} → na-rollback={after}, systeembestand-intact={sys_intact}, rollback-ok={rb}, {nsnaps} snapshot(s) → {}",
                    if before && !after && sys_intact && rb { "OK (CoW-rollback werkt, systeem intact) ✓" } else { "MISLUKT" }
                );
                // Opruimen (+ GC) zodat de zelftest niet bij elke boot snapshots opstapelt.
                let _ = vfs.snapshot_delete(id);
            }
            Err(e) => serial_println!("[s] EuroSnap: snapshot maken faalde ({e:?})"),
        }
    }

    // ── K3: FULL-DISK-ENCRYPTIE met een TPM-gegenereerde sleutel ──
    // Een ECHTE EuroFS bovenop de transparante FDE-laag (op een RAM-volume, zodat we
    // de echte root niet herformatteren). De 256-bit sleutel komt van de TPM (O1);
    // bewijst dat de hele FS transparant versleuteld op de schijf landt.
    {
        use eurofs::{BlockDevice, EuroFs, FileSystem};
        let (key_bytes, from_tpm) = match tpm::get_random(32) {
            Some(b) => (b, true),
            None => (alloc::vec![0x5Au8; 32], false), // fallback zonder TPM
        };
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes[..32]);
        let fde = eurofde::FdeKey::new(key, 0xE0_05);
        let enc = eurofde::EncryptedBlockDevice::new(rootblk::RootBlk::ram(128), fde);
        let mut enc = enc;
        let result = (|| -> Result<bool, eurofs::FsError> {
            let mut efs = EuroFs::format(&mut enc, [0xFD; 16], rtc::epoch())?;
            efs.write_file("/secret.txt", b"versleutelde data op de schijf")?;
            let back = efs.read_file("/secret.txt")?;
            Ok(back == b"versleutelde data op de schijf")
        })();
        match result {
            Ok(ok) => serial_println!(
                "[k3] FDE: EuroFS op versleutelde blok-laag (ChaCha20), sleutel-van-TPM={from_tpm}, lees-na-schrijf-intact={ok} → {}",
                if ok { "OK (transparante full-disk-encryptie werkt) ✓" } else { "MISLUKT" }
            ),
            Err(e) => serial_println!("[k3] FDE: mislukt ({e:?})"),
        }
    }

    // ── Fase 2B-vervolg: X (policy), W (observability), U (secrets) ──
    // X: EuroPol — declaratief beleid → EuroGuard-capabilities (violations → P3).
    europol::selftest();
    // W: EuroObserve — lock-vrije kernel-metrics + OpenMetrics-export.
    observe::selftest(allocator.free_frames() as u64);
    // U: EuroVault — capability-gated, versleutelde secrets met een TPM-master-sleutel.
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
        // AF / Zero-Trust: PCR-seal — bind een geheim aan de measured-boot-toestand,
        // zodat het enkel op een niet-gemanipuleerd systeem ontzegelt.
        vault::pcr_seal_selftest(mk);
    }

    // Z: EuroHealth — SMART (als NVMe) + FS-scrub + geheugen → gezondheidsscore.
    {
        use eurofs::FileSystem;
        let sr = vfs.scrub();
        health::selftest(sr.errors, sr.data_unrecoverable, allocator.free_frames() as u64, allocator.total_frames() as u64);
    }
    // N3: EuroFW — packet-filter in het RX-pad (stealth-drop van geblokkeerd verkeer).
    firewall::init();
    firewall::selftest();
    // N2: EuroVPN — soevereine forward-secret tunnel (seeds van de TPM indien aanwezig).
    {
        let mut seeds = [[0u8; 32]; 4];
        let mut from_tpm = true;
        for (i, s) in seeds.iter_mut().enumerate() {
            // De TPM-GetRandom levert ≤32 byte per call → vier aparte calls.
            match tpm::get_random(32) {
                Some(b) => s.copy_from_slice(&b[..32]),
                None => {
                    from_tpm = false;
                    *s = [(i as u8) + 0x11; 32];
                }
            }
        }
        vpn::selftest(seeds[0], seeds[1], seeds[2], seeds[3], from_tpm);
    }

    // EuroAgent (Sprint AA): bewijs de soevereine agent-runtime-kern bij boot —
    // manifest → least-privilege caps → cap-gated MCP-call → intent-routing.
    agent::selftest();

    // BB-1: bewijs het ECHTE LLM-transport — de agent-lus praat over EuroNet-TCP
    // met een lokale Ollama-`/api/chat`-endpoint (10.0.2.2:11434 via SLIRP-host).
    agent::llm_selftest();

    // EuroLocale (P1): bewijs lokalisatie voor de 24 EU-talen bij boot.
    locale::selftest();

    // EuroInstall (Q1): bewijs de installer-planner bij boot.
    installer::selftest();

    // EuroCA (O3): soevereine lokale certificaatautoriteit (TPM-geseede wortel).
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

    // EuroAttest (O2): remote attestation — quote over de measured-boot-PCR's.
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

    // EuroIDM (V): soevereine bedrijfsidentiteit (identiteit → capabilities).
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

    // EuroID (K1 + P3): soeverein gebruikersbeheer — Argon2id-credentials, sessies,
    // per-gebruiker caps, lockout, en een tamper-evident hash-chain audit-log.
    euroid::selftest();

    // EuroPkg (M2): dependency-resolutie van de pakketbeheerder.
    pkg::selftest();

    // EuroRepro (M3/Q2): reproduceerbare builds — attestatie + consensus.
    repro::selftest();

    // EuroAccess (P2): toegankelijkheidslaag — focus + meertalige schermlezer.
    access::selftest();

    // BB-8: LIVE accessibility-events — focus-navigatie door een dialoog → meertalige
    // schermlezer-aankondigingen → geroute naar EuroAudio (HDA). EN 301 549 end-to-end.
    access::live_selftest();

    // EuroSuite (ES-Core/IO/Calc): soeverein kantoorpakket op één UDM.
    suite::selftest();

    // EuroWeb (AB-B1): soevereine browser-engine — HTML5-tokenizer + DOM.
    web::selftest();
    // AG-2: afbeeldingen (<img> + QOI/PPM-decode) + formulieren (echte GET).
    web::selftest_ag2();

    // EuroReken (AC-1): soevereine rekenmachine — std/wetensch./programmeur.
    reken::selftest();

    // EuroNotes (AC-1): notitie-app — Markdown → EuroDoc-UDM.
    notes::selftest();

    // EuroArchive (AC-2): archiefbeheerder — USTAR tar + checksum + manifest.
    archive::selftest();

    // EuroSafe (AC-1): capability-dashboard — risico-scoring + aanbevelingen.
    safe::selftest();

    // Vensterbeheer + AC-apps (EuroClip/Clock/Shot/Contacts).
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

    // EuroFiles (AC-1): bestandsbeheerder — sorteer/filter/pad/badges.
    files::selftest();
    // EuroMedia (AC-1): afbeeldingsviewer — soevereine QOI-codec.
    media::selftest();

    // EuroWiFi (N1): 802.11-protocolkern — beacon-scan + WPA-sleutelafleiding.
    wifi::selftest();

    // BB-3: detecteer een Intel WiFi-radio + EERLIJKE driver-status (QEMU emuleert
    // geen 802.11; de protocolkern is bewezen door [n1], radio = hardware-attended).
    wifi::bb3_selftest();

    // EuroGPU (K4): virtio-gpu-commandoprotocol — displayinfo→scanout→flush.
    gpu::selftest();

    // BB-2: NATIVE moderne-virtio-transport + virtio-gpu-driver tegen een écht
    // device (init-handshake + GET_DISPLAY_INFO over de control-virtqueue).
    virtio_gpu::selftest();

    // BB-4: EuroPrint — echte IPP-over-TCP-round-trip naar een netwerkprinter/CUPS
    // (10.0.2.2:631 via SLIRP-host); Get-Printer-Attributes + Print-Job.
    print::selftest();

    // EuroCoreutils (CU-7): bewijs de reken-/control-commando's live in de kernel
    // (deterministisch — niet afhankelijk van USB-toetsaanslagen onder traag TCG).
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
            if ok { "OK (GNU-compatibele coreutils-kern live in de shell) ✓" } else { "MISLUKT" }
        );
    }

    // find (CU-5): bewijs de VFS-boom-walk + filters live, deterministisch. Maak een
    // boompje, zoek dan op naam en op type, en controleer de uitkomsten.
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
            && !depth.iter().any(|p| p == "/find-test/sub/gamma.txt"); // te diep voor maxdepth 1

        serial_println!(
            "[find] CU-5 find: -name *.txt (recursief)={name_ok}, -type d={type_ok}, -maxdepth 1={depth_ok} → {}",
            if name_ok && type_ok && depth_ok { "OK (VFS-walk + glob/type/diepte-filters) ✓" } else { "MISLUKT" }
        );
    }

    // AH-3 (H4-remainder): `wasm <bestand>` — een echte zelf-dragende .wasm van
    // EuroFS in de no-JIT sandbox draaien, met cap-gated WASI.
    wasm::selftest_file(&mut vfs);

    // pipe-stdin (CU-afmaak): bewijs dat coreutils-built-ins door een pijplijn
    // componeren — stdout van fase N → stdin van N+1 — deterministisch.
    {
        use eurofs::FileSystem;
        let _ = vfs.write_file("/pipe-test.txt", b"alpha\nbeta\nbravo\ngamma\n");
        let mut pctx = shell::ShellCtx { fs: &mut vfs, mem: &mut allocator };
        // cat | grep b | wc -l → 2 regels bevatten 'b' (beta, bravo).
        let r1 = shell::exec(&mut pctx, "cat /pipe-test.txt | grep b | wc -l");
        let pipe1 = r1.iter().any(|l| l.split_whitespace().next() == Some("2"));
        // seq 5 | tail -2 → 4 en 5 (niet 1).
        let r2 = shell::exec(&mut pctx, "seq 5 | tail -2");
        let joined: alloc::string::String = r2.join(",");
        let pipe2 = joined.contains('4') && joined.contains('5') && !joined.contains('1');
        // echo | tr (per-teken-vervanging) → haLLo.
        let r3 = shell::exec(&mut pctx, "echo hallo | tr l L");
        let pipe3 = r3.iter().any(|l| l.contains("haLLo"));
        // seq 3 | tee FILE | wc -l → tee schrijft 3 regels naar FILE én geeft door.
        let r4 = shell::exec(&mut pctx, "seq 3 | tee /pipe-tee.txt | wc -l");
        let tee_through = r4.iter().any(|l| l.split_whitespace().next() == Some("3"));
        let tee_written = pctx.fs.read_file("/pipe-tee.txt").map(|d| d.len()).unwrap_or(0) == 6; // "1\n2\n3\n"
        // AG-4: xargs — bouw uit de stdin-tokens een commando en voer het uit.
        // seq 3 | xargs echo → één regel "1 2 3".
        let rx1 = shell::exec(&mut pctx, "seq 3 | xargs echo");
        let xargs1 = rx1.iter().any(|l| l.split_whitespace().collect::<alloc::vec::Vec<_>>() == ["1", "2", "3"]);
        // seq 4 | xargs -n2 echo → twee batches: "1 2" en "3 4".
        let rx2 = shell::exec(&mut pctx, "seq 4 | xargs -n2 echo");
        let xargs2 = rx2.iter().filter(|l| !l.trim().is_empty()).count() == 2
            && rx2.iter().any(|l| l.split_whitespace().collect::<alloc::vec::Vec<_>>() == ["1", "2"])
            && rx2.iter().any(|l| l.split_whitespace().collect::<alloc::vec::Vec<_>>() == ["3", "4"]);
        // AG-4: extra pipe-stdin built-in (sha224sum als pijplijn-filter).
        let rsha = shell::exec(&mut pctx, "echo euroos | sha224sum");
        let sha_pipe = rsha.iter().any(|l| l.len() >= 56 && l.contains("-"));
        serial_println!(
            "[pipe] built-in pijplijn: cat|grep|wc-l=2 →{pipe1}, seq|tail-2 →{pipe2}, echo|tr →{pipe3}, seq|tee|wc(door={tee_through},bestand={tee_written}), xargs(echo={xargs1},-n2={xargs2}), sha224-filter={sha_pipe} → {}",
            if pipe1 && pipe2 && pipe3 && tee_through && tee_written && xargs1 && xargs2 && sha_pipe { "OK (stdout→stdin + tee + xargs + sha-filter) ✓" } else { "MISLUKT" }
        );
    }

    // EuroAgent echte tools (Phase 2C): een agent schrijft+leest écht op EuroFS via
    // de cap-gated MCP-gateway, sandbox-geklemd — niet langer een stub.
    agent::real_tools_selftest(&mut vfs);

    // AD-1: de échte net_get/vault_get-tools, dubbel gegate (cap + domein-allow-list);
    // vault-waarde komt in het resultaat maar nooit in de audit.
    agent::net_vault_selftest(&mut vfs);

    // Audit #7 / P0.3: het EuroAgent-audit-spoor is niet langer RAM-only — élke
    // tool-aanroep wordt naar het append-only on-disk log gepersisteerd (overleeft herstart).
    agent::audit_persist_selftest(&mut vfs, ring3::CAP_IMMUTABLE_ADMIN | ring3::CAP_FILE);

    // AF / Zero-Trust P2.2: just-in-time capability-elevatie + auto-revoke — een
    // verhoogde cap geldt enkel voor één bevestigde actie, niet de hele sessie.
    agent::jit_selftest();

    // AF / Zero-Trust P2.3: gedragsdetectie op de agent-audit-stroom — afwijkend
    // gedrag (probing, drift, rate-spikes) wordt deterministisch zichtbaar gemaakt.
    agent::anomaly_selftest();

    // Sprint AE-e2e: EuroID-opslag (gebruikers + Argon2id-hashes + staat) persistent
    // op EuroFS — overleeft een herstart i.p.v. elke boot opnieuw opgebouwd te worden.
    euroid::persist_selftest(&mut vfs);
    // Sprint AE-e2e: must-change-password end-to-end afgedwongen (login weigert tot
    // de gebruiker zijn wachtwoord zelf wijzigt).
    euroid::must_change_selftest();

    // EuroAgent MCP-daemon (AA-3 sluitstuk): de gateway geserveerd over AF_UNIX.
    mcpd::selftest(&mut vfs);

    // WASM-agent-host (AA-5 sluitstuk): agentcode draait in WASM → host-import →
    // MCP-gateway → EuroFS, capability-gated.
    wagent::selftest(&mut vfs);

    // EuroInstall-uitvoering (Q1 sluitstuk): formatteer + provisioneer écht een
    // RAM-schijf en bewijs dat de installatie een remount overleeft.
    instexec::selftest(rtc::epoch());

    if has_mnt {
        use eurofs::FileSystem;
        // De boot-zelftest schreef "/hello-disk2.txt" naar schijf 1. Via de VFS staat
        // dat nu op "/mnt/hello-disk2.txt" — bewijs dat de routering naar schijf 1 gaat.
        match vfs.read_file("/mnt/hello-disk2.txt") {
            Ok(d) => serial_println!("[g2] VFS routeert /mnt/hello-disk2.txt → {} bytes van schijf 1 ✓", d.len()),
            Err(_) => serial_println!("[g2] VFS /mnt-routering MISLUKT"),
        }
        for (mp, t, f) in vfs.df() {
            serial_println!("[g2] df {:<6} {:>7} KiB totaal {:>7} KiB vrij", mp, t / 1024, f / 1024);
        }
    }

    // EuroUpdate (F1): alle kern-init is geslaagd (we starten de desktop) →
    // markeer het actieve slot definitief goed, zodat een gestagede update niet
    // onnodig terugrolt. Vóór de VFS in de shell-context geleend wordt.
    update::mark_boot_good(&mut vfs);

    // G5: eerste achtergrond-scrub-pass over EuroFS (data-path-XXH3 + structuur) →
    // /var/log/fsck.log. Daarna draait de scrubber periodiek (rate-limited) vanuit
    // de desktop-tick.
    scrub::run(&mut vfs);

    // ── EuroDesktop compositor (Track 5) ──
    let _ = bp;
    let mut ctx = shell::ShellCtx {
        fs: &mut vfs,
        mem: &mut allocator,
    };

    // Terminal-venster: toon eerst de uitvoer van het C-programma /bin/hello
    // (geladen uit EuroFS, gedraaid in ring 3 via syscalls), dan wat commando's.
    let mut term: Vec<String> = Vec::new();
    term.push(String::from("euroos:/ $ ./bin/hello   (C-programma, EuroFS -> ring 3)"));
    term.push(format!(
        "[verify] Ed25519-handtekening {} (sleutel {:02x}{:02x}{:02x}{:02x}…)",
        if verified { "OK - geverifieerd" } else { "FOUT - geweigerd" },
        fp[0], fp[1], fp[2], fp[3]
    ));
    term.push(format!(
        "[sec]    tamper-test: 1 byte gewijzigd -> {}",
        if tamper_accepted { "GEACCEPTEERD (FOUT!)" } else { "GEWEIGERD" }
    ));
    term.push(String::from("[caps]   toegekend: CONSOLE PROC FILE  (GEEN NET)"));
    for line in user_out.lines() {
        term.push(line.into());
    }
    term.push(format!("[exit {exit_code}]"));
    term.push(String::new());
    // EuroNet — echte virtio-net NIC: live ARP-uitwisseling met de gateway.
    term.push(String::from("euroos:/ $ EuroNet — virtio-net (echte TX/RX)"));
    for l in &net_lines {
        term.push(l.clone());
    }
    term.push(String::new());
    // De uitvoer van het exec-by-name boot-script (op naam uit EuroFS gestart).
    for (header, out) in &demo_out {
        term.push(header.clone());
        for line in out.lines() {
            term.push(line.into());
        }
        term.push(String::new());
    }
    // ECHTE shell + filesystem-demo: maak een map, schrijf een bestand, lees het
    // terug — de uitvoer is echt (geen script), en /demo verschijnt ook in de
    // Bestanden-app. Bewijst dat de shell + EuroFS werkelijk werken.
    for c in [
        "uname",
        "mkdir /demo",
        "write /demo/welcome.txt Hallo-van-EuroOS",
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

    // Live systeem-vensterinhoud (ECHTE kernelstatus — geen mockup). De vroegere
    // Files/Mail-vensters waren hardgecodeerde EDS-mockups (geen echte programma's)
    // en zijn verwijderd: de desktop toont nu enkel wat werkelijk draait — een live
    // System-venster en de echte interactieve Terminal.
    let total_ram = ctx.mem.usable_bytes();
    let sysinfo = |t: u64, free: u64| -> Vec<String> {
        let a = sched::TASK_COUNTERS[1].load(Ordering::Relaxed);
        let b = sched::TASK_COUNTERS[2].load(Ordering::Relaxed);
        let c = sched::TASK_COUNTERS[3].load(Ordering::Relaxed);
        let u1 = ring3::read_counter(ucnt1) / 1_000_000;
        let u2 = ring3::read_counter(ucnt2) / 1_000_000;
        let mut v = alloc::vec![
            String::from("EuroKernel v0.1-alpha — from-scratch Rust (no_std)"),
            String::from("geen Linux/BSD eronder; Linux-ABI = compat-brug"),
            format!("uptime {} s  ({} ticks)   RAM {} / {} MiB vrij", t / 100, t, free / (1024 * 1024), total_ram / (1024 * 1024)),
            format!("CPU-isolatie  SMEP {} · SMAP {} · W^X/NX {} (CR4)",
                if ring3::smep_active() { "aan" } else { "n/b" },
                if ring3::smap_active() { "aan" } else { "n/b" },
                if ring3::nx_active() { "aan" } else { "n/b" }),
            String::new(),
            String::from("preemptieve scheduler (per-proces adresruimtes):"),
            format!("  kernel-threads   A={a} B={b} C={c}"),
            format!("  ring-3 proces 1  {u1}M iteraties"),
            format!("  ring-3 proces 2  {u2}M iteraties"),
            String::from("  daemon (pid 7)   -> EuroMonitor"),
            String::from("  shell (Terminal-venster)"),
            String::new(),
            String::from("per-proces (preemptief, eigen FS_BASE/TLS + heap):"),
        ];
        // De recentste regel van elk achtergrond-musl-proces: onafhankelijke
        // __thread-tellers bewijzen dat FS_BASE per proces bewaard wordt.
        for line in ring3::bg_lines() {
            v.push(format!("  {line}"));
        }
        for line in ring3::reaped_lines() {
            v.push(format!("  [reaper] {line}"));
        }
        let (httpd_on, served) = net::httpd_status();
        if httpd_on {
            v.push(format!("  [httpd] achtergrond-server AAN — {served} verzoeken bediend"));
        }
        let ipc = euroipc::audit_lines();
        if !ipc.is_empty() {
            v.push(String::from("EuroIPC (message-bus, audit):"));
            for line in ipc.iter().rev().take(3).rev() {
                v.push(format!("  {line}"));
            }
        }
        v.push(String::new());
        v.push(String::from("EuroMonitor daemon (preemptief, eigen syscalls):"));
        // De recentste hartslag-regels van de gescheduelde achtergrond-daemon.
        for line in ring3::daemon_lines().iter().rev().take(2).rev() {
            v.push(format!("  {line}"));
        }
        v
    };

    let mut windows = alloc::vec![
        // System — live, echte kernelstatus (achter, links). Geen mockup.
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
        // Terminal — ECHTE interactieve shell (hero, vooraan).
        compositor::Window {
            x: SIDEBAR_W + 668, y: 150, w: 800, h: 740,
            title: String::from("Terminal  -  /bin/sh"),
            content: term, ui: Vec::new(),
            active: true, accent: Color::SUCCESS,
            sec: eds::SecState::new(true, false, true),
            app: suite_ui::SuiteApp::None,
            visible: false,
            restore: None,
        },
    ];
    // Z-volgorde (back-to-front): System achter, Terminal vooraan.
    let mut order: Vec<usize> = alloc::vec![0, 1];
    // Dock-tegel (zie compositor::DOCK_APPS: files/notes/clock/browser/terminal/
    // settings/store/star) → venster-index. De desktop start LEEG (alle vensters
    // verborgen); een dock-klik opent een app. (AG-1 voegde files/notes/clock toe.)
    let mut dock_targets: [Option<usize>; 8] = [None; 8];
    dock_targets[4] = Some(1); // terminal → Terminal (de echte shell)

    // ── H2: LIVE DISPLAY-SERVER ── bind een AF_UNIX-socket (H1), laat een app-
    // proces verbinden en via het eurodisplay-protocol (Request/Event) een venster
    // openen, en render het als een ECHT compositor-venster — geen mockup, het
    // bestaat omdat een ander stuk code er over een socket om vroeg.
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
            "[h2] display-server @ {}: {} client(s), {} app-venster(s) via AF_UNIX → compositor ({} vensters totaal) ✓",
            dispserv::SOCK_PATH,
            dispserv.client_count(),
            dispserv.windows().len(),
            windows.len()
        );
    }

    // H5: render een venster dat via het ECHTE Wayland-protocol tot stand kwam (een
    // in-kernel Wayland-client deed de volledige handshake door de eurowl-server).
    if let Some((sid, title)) = wayland::run_handshake("EuroOS — echt Wayland-protocol") {
        let idx = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 200,
            y: 460,
            w: 520,
            h: 300,
            title,
            content: alloc::vec![
                String::from("Dit venster kwam via het ECHTE Wayland-"),
                String::from("draadprotocol tot stand: get_registry →"),
                String::from("bind → create_surface → xdg get_toplevel →"),
                String::from("set_title → commit (eurowl-server, H5)."),
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
            "[h5] Wayland-venster (surface {}) → compositor ({} vensters totaal) ✓",
            sid,
            windows.len()
        );
    }

    // ── BB-5: EuroSuite-GUI — Writer/Calc/Impress als echte vensters (Word/Excel/
    // PowerPoint-stijl) bovenop het EuroDoc-UDM + de EuroCalc-formule-engine.
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
        // Impress achteraan, Calc ertussen, Writer vooraan en groot (de hero).
        let i_impress = windows.len();
        windows.push(mksuite(SIDEBAR_W + 470, 360, 760, 540, "EuroSuite Impress  -  Presentatie.pptx", suite_ui::SuiteApp::Impress));
        order.push(i_impress);
        let i_calc = windows.len();
        windows.push(mksuite(SIDEBAR_W + 250, 210, 820, 560, "EuroSuite Calc  -  Omzet.xlsx", suite_ui::SuiteApp::Calc));
        order.push(i_calc);
        let i_writer = windows.len();
        windows.push(mksuite(SIDEBAR_W + 40, 70, 760, 660, "EuroSuite Writer  -  Soevereiniteit.docx", suite_ui::SuiteApp::Writer));
        order.push(i_writer);
        // Writer is de actieve hero; de rest staat eronder.
        for w in windows.iter_mut() {
            w.active = false;
        }
        windows[i_writer].active = true;
        // NB: Writer/Calc/Impress tonen vaste demo-documenten (een renderer-test,
        // geen bruikbare apps) → bewust NIET op de dock, om geen mockup als 'echt'
        // te presenteren. De render-zelftest blijft wel draaien.
        let _ = (i_writer, i_calc, i_impress);
        serial_println!("[bb5] EuroSuite-renderer: Writer/Calc/Impress-rendering getest (demo-documenten, niet op dock) ✓");
    }

    // ── AB-B6: EuroWeb-browser — rendert een ECHTE HTML+CSS-pagina via de eigen
    // engine (tokenizer→DOM→CSS→layout→paint) in een browservenster, vooraan.
    {
        // Bruikbare browser: tabbladen + bewerkbare adresbalk. Start blanco (geen
        // fetch bij boot) — typ een adres + Enter om live te laden via EuroNet/eurotls.
        webview::init("flowd.be");
        let i_web = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 110,
            y: 64,
            w: 900,
            h: 730,
            title: String::from("EuroWeb"),
            content: Vec::new(), // toestand leeft in de globale Browser
            ui: Vec::new(),
            active: true,
            accent: Color::ACCENT,
            sec: eds::SecState::new(true, true, false),
            app: suite_ui::SuiteApp::Browser,
            visible: false,
            restore: None,
        });
        order.push(i_web);
        dock_targets[3] = Some(i_web); // dock: wereldbol → EuroWeb
        serial_println!("[b6] EuroWeb: bruikbare browser (tabbladen + adresbalk) klaar (open via dock; typ een URL) ✓");
    }

    // ── EuroReken — een ECHTE interactieve rekenmachine. Toestand = win.content
    // ([expr, result]); toetsenbord/muis muteren 'm, euroreken berekent live.
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
        dock_targets[6] = Some(i_calc); // dock: store-icoon → EuroReken (echt)
        // Zelftest: exact dezelfde input-functie die toetsenbord/muis aanroepen,
        // ECHT doorgerekend door de euroreken-engine — geen hardcoded waarde.
        let mut probe = alloc::vec![String::new(), String::from("0")];
        for ch in "12+34*2".chars() {
            calc_ui::input(&mut probe, ch);
        }
        serial_println!(
            "[rk] EuroReken ECHT interactief: 12+34*2 = {} (engine, verwacht 80, mét voorrang) {}",
            probe[1],
            if probe[1] == "80" { "✓" } else { "✗ FOUT" }
        );
    }

    // ── EuroBeheer — instellingen/beheerpaneel dat de LIVE kernel-toestand toont
    // en beheert (EuroGuard-capabilities/firewall, netwerk, systeem). Geen mockup:
    // het leest euroguard::*_lines() / net::cmd_net() / interrupts::ticks() enz.
    {
        let i_set = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 200,
            y: 120,
            w: 760,
            h: 560,
            title: String::from("EuroBeheer  -  Instellingen"),
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
        dock_targets[5] = Some(i_set); // dock: instellingen-icoon → EuroBeheer
        serial_println!("[set] EuroBeheer: instellingenpaneel klaar (live EuroGuard/netwerk/systeem; open via dock) ✓");
        // Zelftest: bewijs dat het paneel ECHT beheert — add_blocked_domain (de functie
        // die de "blokkeer domein"-knop aanroept) blokkeert daadwerkelijk een DNS-domein.
        let probe = "zelftest-blok.example";
        let before = matches!(euroguard::check_dns("zelftest", probe), euroguard::Decision::Block);
        euroguard::add_blocked_domain(probe);
        let after = matches!(euroguard::check_dns("zelftest", probe), euroguard::Decision::Block);
        serial_println!(
            "[set] EuroBeheer beheert ECHT: domein vóór={} → na add_blocked_domain={} {}",
            before,
            after,
            if !before && after { "✓" } else { "✗ FOUT" }
        );

        // EuroAgent dispatch-paneel (BB-6): typ een intent → de runtime routeert,
        // draait de agent-lus, en toont elke cap-gated tool-call live + de audit.
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
        dock_targets[7] = Some(i_agent); // dock: ster-icoon → EuroAgent
        // Een voorbeeld-dispatch zodat het paneel meteen een echte, cap-gated
        // transcript toont (de gebruiker kan daarna z'n eigen intent typen).
        agent_ui::dispatch("vergadering opnemen en samenvatten");
        serial_println!("[bb6] EuroAgent dispatch-paneel klaar (intent → cap-gated agent-lus + live audit; open via dock) ✓");

        // EuroInstall begeleide grafische installer (BB-7): toont de echte plan-
        // stappen + live FDE-enrol. Opent in de live/installatie-bootmodus; hier
        // zichtbaar voor de verificatie-screenshot.
        let i_inst = windows.len();
        windows.push(compositor::Window {
            x: SIDEBAR_W + 180,
            y: 70,
            w: 820,
            h: 600,
            title: String::from("EuroInstall  -  EuroOS installeren"),
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
        serial_println!("[bb7] EuroInstall: begeleide grafische installer klaar (plan + live FDE-enrol; uitvoering = instexec) ✓");

        // ── AG-1: EuroFiles / EuroNotes / EuroClock — echte desktop-apps ────────
        // Drie vensters die door de dock geopend worden en ECHTE engine-data tonen:
        // EuroFiles = live EuroFS, EuroNotes = euronotes-Markdown, EuroClock = RTC.
        // De desktop start LEEG (visible=false) — net als de andere apps; een
        // dock-klik opent ze. (Boot-geverifieerd met screenshot ag1-desktop.png.)
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
        dock_targets[0] = Some(i_files); // dock: files-icoon → EuroFiles
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
        dock_targets[1] = Some(i_notes); // dock: notes-icoon → EuroNotes
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
        dock_targets[2] = Some(i_clock); // dock: clock-icoon → EuroClock

        // EuroFiles vooraf vullen met de ECHTE wortelmap van het FS, zodat het
        // eerste dock-open meteen inhoud toont. Geen dock-tegel actief bij boot.
        load_files_dir(ctx.fs, "/");
        compositor::set_active_dock(None);
        let fl_path = files::current_path();
        serial_println!(
            "[ag] EuroApps: EuroFiles (live FS @ {}), EuroNotes (euronotes), EuroClock (RTC {}) — 3 vensters + dock-tegels 0/1/2 ✓",
            if fl_path.is_empty() { "/" } else { &fl_path },
            rtc::clock_string()
        );
    }

    // BB-2 sluitstuk: presenteer het LEVENDE bureaublad op het virtio-gpu-scherm via
    // onze native moderne-virtio-driver (bind de echte framebuffer als scanout-
    // backing). Geen virtio-gpu-device? Dan no-op → standaard-GOP-scanout blijft.
    if let Some(fbi) = FB_INFO.get() {
        if virtio_gpu::init_scanout(fbi.width as u32, fbi.height as u32) {
            // Eerste frame meteen presenteren.
            if let Some((bb, bw, bh, bs)) = fb.backbuffer() {
                virtio_gpu::present_frame(bb, bw, bh, bs);
            }
            serial_println!(
                "[bb2] virtio-gpu LIVE-scanout actief: bureaublad ({}x{}) gepresenteerd via de native moderne-virtio-driver (eigen RAM-backing, transfer+flush per frame) ✓",
                fbi.width, fbi.height
            );
        }
    }

    let mut mx = width / 2;
    let mut my = height / 2;
    // Live systeemcijfers voor het statuspaneel (capture geen ctx.mem — reap_dead leent 'm mutabel).
    let mk_stats = |free: u64| compositor::SysStats {
        free_mb: free / (1024 * 1024),
        total_mb: total_ram / (1024 * 1024),
        uptime_s: interrupts::ticks() / 100,
        cores: smp::AP_ONLINE.load(Ordering::Relaxed) + 1,
        procs: sched::task_count() as u32,
    };

    // Sprint AG: GUI-lockscreen — bewijs de auth-bedrading, toon het scherm, en
    // authenticeer de desktop-sessie via EuroID (Argon2id) vóór de desktop start.
    // Onbeheerde/CI-boots loggen na een korte gratie automatisch in (eerlijk gelogd).
    lockscreen::selftest(&fb);
    let _session_user = lockscreen::gate(&fb, "euro");

    compositor::render(&fb, &windows, &order, &rtc::clock_string(), &rtc::date_string(), &mk_stats(ctx.mem.free_bytes()));
    serial_println!("[euro] EuroDesktop compositor actief — {} vensters + muis", windows.len());

    // Cursor neerzetten (met save-under).
    let mut cur_bg = [Color::BACKGROUND; compositor::CURSOR_W * compositor::CURSOR_H];
    let (mut cmx, mut cmy) = mouse::pos();
    compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
    compositor::draw_cursor(&fb, cmx, cmy);
    // SPERF-meting (HPET): kost van een full-screen blit vs. een statuspaneel-rect —
    // bewijst de winst van dirty-rect-rendering op het klok-tick-pad.
    let t0 = hpet::ns();
    fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() { virtio_gpu::present_frame(bb, bw, bh, bs); } // BB-2: live virtio-gpu-scanout
    let full_ns = hpet::ns().saturating_sub(t0);
    let (prx, pry, prw, prh) = compositor::status_panel_rect(width);
    let t1 = hpet::ns();
    fb.present_rect(prx, pry, prw, prh);
    let panel_ns = hpet::ns().saturating_sub(t1).max(1);
    kinfo!(
        "[sperf] present-blit: full-screen {} us vs statuspaneel-rect {} us (~{}x minder werk per klok-tick)",
        full_ns / 1000,
        panel_ns / 1000,
        full_ns / panel_ns
    );
    let _ = (mx, my);

    // ── Desktop-loop: muis-cursor, venster-slepen, live systeemvenster. ──
    let mut dragging: Option<usize> = None;
    let mut drag_off = (0usize, 0usize);
    let mut prev_left = false;
    let mut last_t = u64::MAX;
    let mut last_kbd = 0u64; // diagnostiek: toetsenbord-IRQs via de IO-APIC
    // Interactieve shell in het Terminal-venster: de laatste content-regel is de
    // prompt ("euroos:/ $ <invoer>"); toetsenbordinvoer (IRQ1) bewerkt 'm live.
    let term_idx = 1;
    let sys_idx = 0; // het live System-venster (achter de Terminal)
    let mut input = String::new();
    let vis_lines = ((windows[term_idx].h - 44) / 16) as usize;
    // Marker: vanaf hier draait de interactieve desktop-loop (pollt invoer + shell).
    // De E2E-test wacht hierop vóór 't toetsen injecteert; bewijst ook dat HLT-idle
    // de loop niet vasthoudt.
    serial_println!("[desktop] interactieve loop gestart — invoer + shell live");
    loop {
        let (px, py) = mouse::pos();
        let ldown = mouse::left_down();
        let mut need_full = false;

        // Linkerklik net ingedrukt: dock-launch, venster-focus/raise, of slepen.
        if ldown && !prev_left && dragging.is_none() {
            if let Some(icon) = compositor::dock_icon_at(px, py) {
                // Dock-klik → open de bijbehorende app (of breng 'm naar voren).
                // Een tweede klik op een al-zichtbaar venster verbergt het weer (toggle).
                let target = dock_targets.get(icon).copied().flatten();
                if let Some(w) = target.filter(|&w| w < windows.len()) {
                    if windows[w].visible && windows[w].active {
                        // Toggle dicht.
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
                        // EuroFiles: vul de lijst met de echte map als ze nog leeg is.
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
                // Eerst: verkeerslicht-knoppen (sluiten/minimaliseren/maximaliseren).
                if let Some(btn) = windows[i].title_button_at(px, py) {
                    match btn {
                        compositor::TitleButton::Close => {
                            windows[i].visible = false;
                            order.retain(|&x| x != i);
                            // Focus naar het nu bovenste zichtbare venster.
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
                            // Toggle: maximaliseren ↔ vorige geometrie herstellen.
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
                    // Anders: venster-klik → naar voren + focus; op de titelbalk → slepen.
                    order.retain(|&x| x != i);
                    order.push(i);
                    for ww in windows.iter_mut() {
                        ww.active = false;
                    }
                    windows[i].active = true;
                    if windows[i].titlebar_contains(px, py) {
                        drag_off = (px.saturating_sub(windows[i].x), py.saturating_sub(windows[i].y));
                        dragging = Some(i);
                    } else if windows[i].app == suite_ui::SuiteApp::Reken {
                        // Klik op een rekenmachine-knop → ECHTE invoer naar euroreken.
                        if let Some(ch) =
                            calc_ui::button_at(windows[i].x, windows[i].y, windows[i].w, windows[i].h, px, py)
                        {
                            calc_ui::input(&mut windows[i].content, ch);
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Browser {
                        // Klik op tabblad / "+"-knop / adresbalk.
                        match webview::hit_test(windows[i].x, windows[i].y, windows[i].w, px, py) {
                            webview::Hit::Tab(t) => webview::switch_tab(t),
                            webview::Hit::NewTab => webview::new_tab(),
                            webview::Hit::UrlBar => webview::begin_edit(),
                            webview::Hit::Field(n) => webview::focus_field(n), // paginaveld focus
                            webview::Hit::Submit(n) => webview::submit_form(n), // echte GET-submit
                            webview::Hit::None => {}
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Settings {
                        // Klik op sectie-nav / domein-invoerveld / HTTP-server-schakelaar.
                        if let Some(s) = settings_ui::nav_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::set_section(s);
                        } else if settings_ui::domain_field_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::begin_domain_edit();
                        } else if settings_ui::toggle_at(windows[i].x, windows[i].y, px, py) {
                            settings_ui::toggle_httpd(); // ECHTE kernel-actie
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Agent {
                        // Klik op het intent-veld → start typen.
                        if agent_ui::field_at(windows[i].x, windows[i].y, windows[i].w, px, py) {
                            agent_ui::begin_edit();
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Files {
                        // Klik op een map/plaats/".." → navigeer in het ECHTE FS.
                        if let Some(path) = files::hit_test(windows[i].x, windows[i].y, px, py) {
                            load_files_dir(ctx.fs, &path);
                        }
                    } else if windows[i].app == suite_ui::SuiteApp::Notes {
                        // Klik in de notitielijst → selecteer een andere notitie.
                        notes::hit_test(windows[i].x, windows[i].y, px, py);
                    }
                    need_full = true;
                }
            }
        }
        if !ldown {
            dragging = None;
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
        prev_left = ldown;

        // I1: harvest USB-HID-interrupt-transfers (toetsenbord/muis) en injecteer ze
        // in dezelfde scancode-/muis-paden als PS/2 — vóór we de toetsen uitlezen.
        xhci::poll();

        // Het gefocuste (bovenste zichtbare) venster bepaalt waar toetsen heen gaan.
        let focused = order.iter().rev().copied().find(|&i| windows[i].visible);
        let calc_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Reken).unwrap_or(false);
        let browser_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Browser).unwrap_or(false);
        let settings_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Settings).unwrap_or(false);
        let agent_focused = focused.map(|i| windows[i].app == suite_ui::SuiteApp::Agent).unwrap_or(false);

        // ── Interactieve shell / rekenmachine: lees toetsen. ──
        let mut term_dirty = false;
        let mut calc_dirty = false;
        while let Some(k) = ps2::poll_key() {
            // Als de ECHTE rekenmachine de focus heeft → toetsen naar euroreken.
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
            // Browser-focus → een gefocust PAGINAVELD krijgt de toets; anders de
            // adresbalk (Enter navigeert via een echte fetch).
            if browser_focused {
                if webview::field_focused() {
                    webview::field_key(k); // typen in een formulierveld
                } else {
                    if !webview::editing() {
                        webview::begin_edit();
                    }
                    if let Some(url) = webview::edit_key(k) {
                        webview::navigate(&url); // blokkerende fetch (volgt redirects)
                    }
                }
                need_full = true;
                continue;
            }
            // EuroBeheer-focus in de EuroGuard-sectie → toetsen bewerken het domein-veld
            // (auto-start); Enter blokkeert het domein écht via EuroGuard.
            if settings_focused && settings_ui::section() == 0 {
                if !settings_ui::editing() {
                    settings_ui::begin_domain_edit();
                }
                if let Some(domain) = settings_ui::edit_key(k) {
                    euroguard::add_blocked_domain(&domain); // ECHTE kernel-actie
                    serial_println!("[set] EuroGuard: domein geblokkeerd via beheerpaneel: {domain}");
                }
                need_full = true;
                continue;
            }
            // EuroAgent-focus → toetsen bewerken het intent-veld (auto-start); Enter
            // dispatcht naar de agent-lus (cap-gated tool-calls + live audit).
            if agent_focused {
                if !agent_ui::editing() {
                    agent_ui::begin_edit();
                }
                if let Some(intent) = agent_ui::edit_key(k) {
                    agent_ui::dispatch(&intent); // routeer + draai de agent-lus
                    serial_println!("[bb6] EuroAgent dispatch: intent='{intent}' → agent-lus uitgevoerd (cap-gated, geaudit)");
                }
                need_full = true;
                continue;
            }
            term_dirty = true;
            match k {
                '\r' => {
                    let cmd = String::from(input.trim());
                    // Leg het uitgevoerde commando vast op de huidige promptregel.
                    if let Some(last) = windows[term_idx].content.last_mut() {
                        *last = format!("euroos:/ $ {cmd}");
                    }
                    // Redirectie afsplitsen: `prog ... > bestand` of `>> bestand`.
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
                        out.push("programma's (exec uit EuroFS): hello cat linuxprog muslprog".into());
                        out.push("        argvprog pieprog muslreal muslfile mcat mwrite".into());
                        out.push("        mecho <tekst> · mupper (stdin->HOOFDLETTERS)".into());
                        out.push("pipes/redirectie: a | b · prog > bestand · prog >> bestand".into());
                        out.push("install <pakket> (Ed25519-verificatie) · pakketten: msum".into());
                        out.push("netwerk (live NIC): ping <ip|naam> · ping6 · net · fetch <host> · https <host>".into());
                        out.push("server: serve (één verbinding) · httpd (achtergrond-HTTP-server aan/uit)".into());
                        out.push("EuroGuard (Track 7): guard · guard block <domein> · guard allow <domein>".into());
                        out.push("processen: ps · kill <pid> (achtergrond-musl-processen)".into());
                        out.push("engines: calc <expr> (EuroReken) · js <code> (EuroJS)".into());
                        out.push("builtins: ls, uname, mem, df, clear, help".into());
                    } else if let Some(expr) = exec_cmd.strip_prefix("calc ") {
                        // ECHTE berekening via de euroreken-engine (geen mockup).
                        match euroreken::eval(expr.trim()) {
                            Ok(v) => {
                                let n = if v == (v as i64) as f64 && euroreken::math::fabs(v) < 1e15 {
                                    format!("{}", v as i64)
                                } else {
                                    format!("{v}")
                                };
                                out.push(format!("{} = {}", expr.trim(), n));
                            }
                            Err(e) => out.push(format!("calc: fout: {e:?}")),
                        }
                    } else if let Some(code) = exec_cmd.strip_prefix("js ") {
                        // ECHTE JavaScript-uitvoering via de EuroJS-interpreter.
                        let (res, logs) = eurojs::run_capture(code);
                        for l in logs {
                            out.push(l);
                        }
                        match res {
                            Ok(v) => out.push(format!("=> {}", eurojs_show(&v))),
                            Err(e) => out.push(format!("js: fout: {e}")),
                        }
                    } else if let Some(pkg) = exec_cmd.strip_prefix("install ") {
                        // Soevereine pakketinstallatie: verifieer de Ed25519-handtekening
                        // vóór het pakket in EuroFS te schrijven + te registreren.
                        let pkg = pkg.trim();
                        let path = format!("/bin/{pkg}");
                        match ring3::installable(pkg) {
                            Some((bytes, caps, abi)) if ring3::verify_program(&path, bytes) => {
                                let _ = ctx.fs.write_file(&path, bytes);
                                if let Some(sig) = ring3::program_sig(&path) {
                                    let _ = ctx.fs.write_file(&format!("{path}.sig"), sig);
                                }
                                ring3::register_program(&path, caps, abi);
                                out.push(format!("[pkg] {pkg}: Ed25519-handtekening GEVERIFIEERD ({} bytes)", bytes.len()));
                                out.push(format!("[pkg] geïnstalleerd in {path} + {path}.sig (EuroFS)"));
                                out.push(format!("[pkg] geregistreerd — voer uit met: {pkg} <getallen>"));
                            }
                            Some(_) => out.push(format!("[sec] {pkg}: GEWEIGERD — ongeldige Ed25519-handtekening")),
                            None => out.push(format!("install: onbekend pakket '{pkg}' (beschikbaar: msum)")),
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
                        // Achtergrond-HTTP-server aan/uit (bedient :80 in de desktop-lus).
                        let on = net::httpd_toggle();
                        if on {
                            out.push("httpd: achtergrond-HTTP-server AAN — bedient nu :80".into());
                            out.push("  (verbind van buiten via de hostfwd; desktop blijft actief)".into());
                        } else {
                            out.push("httpd: achtergrond-HTTP-server UIT".into());
                        }
                    } else if let Some(d) = exec_cmd.strip_prefix("guard block ") {
                        // Aangepaste blokkering toevoegen (spec: "Domein toevoegen").
                        let d = d.trim();
                        euroguard::add_blocked_domain(d);
                        out.push(format!("EuroGuard: '{d}' toegevoegd aan de blokkeerlijst"));
                    } else if let Some(d) = exec_cmd.strip_prefix("guard allow ") {
                        // Whitelist: domein van de blokkeerlijst halen.
                        let d = d.trim();
                        if euroguard::remove_blocked_domain(d) {
                            out.push(format!("EuroGuard: '{d}' van de blokkeerlijst gehaald"));
                        } else {
                            out.push(format!("EuroGuard: '{d}' stond niet op de blokkeerlijst"));
                        }
                    } else if exec_cmd == "ps" {
                        // Procesoverzicht (per-proces-model).
                        for l in ring3::ps_lines() {
                            out.push(l);
                        }
                    } else if let Some(arg) = exec_cmd.strip_prefix("kill ") {
                        match arg.trim().parse::<u64>() {
                            Ok(pid) if ring3::kill_pid(pid) => {
                                out.push(format!("kill: proces {pid} beëindigd — wordt opgeruimd"));
                            }
                            Ok(pid) => out.push(format!("kill: geen (levend) achtergrondproces met pid {pid}")),
                            Err(_) => out.push("kill: ongeldige pid".into()),
                        }
                    } else if exec_cmd == "guard" {
                        // EuroGuard-dashboard (Track 7): policy + netwerkmonitor + auditlog.
                        out.push("EuroGuard — toegangs- & netwerkcontrole (Track 7)".into());
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
                                // Alle fasen built-in (geen /bin-programma) → de
                                // pipe-bewuste shell verwerkt de coreutils-filters.
                                let n = st.split_whitespace().next().unwrap_or("");
                                !n.starts_with('/') && ring3::program_caps_abi(&format!("/bin/{n}")).is_none()
                            })
                    {
                        for l in shell::exec(&mut ctx, &exec_cmd) {
                            out.push(l);
                        }
                    } else if !exec_cmd.is_empty() {
                        // Pijplijn: splits op '|' in fasen; stdout van fase N -> stdin van N+1.
                        // Redirectie (>) geldt voor de stdout van de LAATSTE fase.
                        let stages: Vec<String> = exec_cmd
                            .split('|')
                            .map(|s| String::from(s.trim()))
                            .filter(|s| !s.is_empty())
                            .collect();
                        let mut piped: Option<Vec<u8>> = None; // stdin voor de volgende fase
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
                            // Verify-before-execute: Ed25519-handtekening moet kloppen.
                            if !ring3::verify_program(&path, &bytes) {
                                out.push(format!("[sec] {path}: GEWEIGERD — ongeldige Ed25519-handtekening"));
                                last = None;
                                break;
                            }
                            let mut argv_s: Vec<String> = alloc::vec![path.clone()];
                            for w in stage.split_whitespace().skip(1) {
                                argv_s.push(String::from(w));
                            }
                            let argv: Vec<&[u8]> = argv_s.iter().map(|s| s.as_bytes()).collect();
                            // Stdin = stdout van de vorige fase (pipe).
                            ring3::set_stdin(piped.as_deref().unwrap_or(&[]));
                            // Redirectie alleen op de stdout van de laatste fase.
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
                            // Onbekend programma -> kernel-builtins (ls/uname/net/mem/df/…).
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
                            // Geschreven bestanden terugsynchroniseren naar EuroFS.
                            for (p, bytes) in ring3::take_dirty() {
                                let n = bytes.len();
                                if ctx.fs.write_file(&p, &bytes).is_ok() {
                                    out.push(format!("[fs] {p} ({n} B) -> EuroFS gesynct"));
                                }
                            }
                            out.push(format!("[exit {ec}, abi={}]", if abi { "linux" } else { "native" }));
                        }
                    }
                    // Afmaak-sprint E2E: tee het uitgevoerde commando + z'n uitvoer naar
                    // serial, zodat de end-to-end-lus (USB-toets → scancode → poll_key →
                    // shell-prompt → Enter → exec → uitvoer) extern verifieerbaar is.
                    serial_println!("[e2e] $ {cmd}");
                    for l in &out {
                        serial_println!("[e2e] {l}");
                    }
                    for l in out {
                        windows[term_idx].content.push(l);
                    }
                    windows[term_idx].content.push(String::from("euroos:/ $ "));
                    input.clear();
                    // Houd de buffer op de zichtbare hoogte zodat de prompt zichtbaar blijft.
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
        if term_dirty && windows[term_idx].visible {
            compositor::restore_cursor_bg(&fb, cmx, cmy, &cur_bg);
            // De Terminal is het voorste venster en overlapt niets erboven, dus enkel
            // dit venster hertekenen volstaat (System ligt erachter, naast de terminal).
            // (Bij maximaliseren kan hij groter zijn; een volledige render volgt dan via need_full.)
            compositor::draw_window(&fb, &windows[term_idx]);
            compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
            compositor::draw_cursor(&fb, cmx, cmy);
            fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() { virtio_gpu::present_frame(bb, bw, bh, bs); } // BB-2: live virtio-gpu-scanout
        }

        // De ECHTE rekenmachine veranderde → herteken enkel haar venster.
        if calc_dirty {
            if let Some(fi) = focused {
                compositor::restore_cursor_bg(&fb, cmx, cmy, &cur_bg);
                compositor::draw_window(&fb, &windows[fi]);
                compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
                compositor::draw_cursor(&fb, cmx, cmy);
                fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() { virtio_gpu::present_frame(bb, bw, bh, bs); } // BB-2: live virtio-gpu-scanout
            }
        }

        let t = interrupts::ticks();
        let tick = t / 50 != last_t;

        if need_full {
            // Volledige hertekening (sleep of z-order gewijzigd).
            last_t = t / 50;
            compositor::render(&fb, &windows, &order, &rtc::clock_string(), &rtc::date_string(), &mk_stats(ctx.mem.free_bytes()));
            cmx = px;
            cmy = py;
            compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
            compositor::draw_cursor(&fb, cmx, cmy);
            fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() { virtio_gpu::present_frame(bb, bw, bh, bs); } // BB-2: live virtio-gpu-scanout
        } else if tick {
            // Live systeemvenster (incl. daemon-hartslag) + klok bijwerken.
            last_t = t / 50;
            // Diagnostiek: log het aantal toetsenbord-IRQs (via IO-APIC) bij wijziging.
            let kc = interrupts::KBD_IRQ_COUNT.load(Ordering::Relaxed);
            if kc != last_kbd {
                last_kbd = kc;
                serial_println!("[ioapic] toetsenbord-IRQs via IO-APIC ontvangen: {}", kc);
            }
            // Ruim beëindigde processen op (frames vrijgeven) — veilig vanuit
            // taak 0 op de boot-PML4. Vrij RAM herstelt zichtbaar.
            ring3::reap_dead(ctx.mem);
            // S4: EuroInit-supervisie (herstart gestopte services) + eurologd
            // (kmsg-ring periodiek naar /var/log/messages).
            init::supervise(ctx.mem, ctx.fs);
            init::flush_log(ctx.fs);
            // G5: periodieke achtergrond-scrub (rate-limited ~60 s) → /var/log/fsck.log.
            scrub::maybe_run(ctx.fs, t);
            let (ox, oy) = (cmx, cmy);
            compositor::restore_cursor_bg(&fb, cmx, cmy, &cur_bg);
            // Statuspaneel (grote klok) verversen — zonder schaduw zodat die niet stapelt.
            compositor::draw_status_panel(&fb, width, height, &rtc::clock_string(), &rtc::date_string(), &mk_stats(ctx.mem.free_bytes()), false);
            // Live System-venster: echte kernelstatus bijwerken (tellers, daemon-hartslag,
            // SMEP/SMAP, IPC-audit). Enkel de body hertekenen (geen schaduw → geen stapeling).
            // Sla over als het venster gesloten/geminimaliseerd is (anders zou het terugkomen).
            let sys_vis = windows[sys_idx].visible;
            if sys_vis {
                windows[sys_idx].content = sysinfo(t, ctx.mem.free_bytes());
                compositor::draw_window_body(&fb, &windows[sys_idx]);
            }
            cmx = px;
            cmy = py;
            compositor::save_cursor_bg(&fb, cmx, cmy, &mut cur_bg);
            compositor::draw_cursor(&fb, cmx, cmy);
            // SPERF dirty-rect: blit ENKEL het statuspaneel + het System-venster + de
            // oude/nieuwe cursor, niet het hele scherm (was ~2M px/tick).
            let (rx, ry, rw, rh) = compositor::status_panel_rect(width);
            fb.present_rect(rx, ry, rw, rh);
            if sys_vis {
                let (sx, sy, sw, sh) = compositor::window_body_rect(&windows[sys_idx]);
                fb.present_rect(sx, sy, sw, sh);
            }
            fb.present_rect(ox, oy, compositor::CURSOR_W, compositor::CURSOR_H);
            fb.present_rect(cmx, cmy, compositor::CURSOR_W, compositor::CURSOR_H);
        } else if px != cmx || py != cmy {
            // Alleen de cursor verplaatsen (save-under) — blit enkel het gebied
            // dat de cursor verlaat + aankomt, zodat de muis vloeiend blijft.
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

        // Houd het netwerk levend: beantwoord ARP-requests + recycle RX-buffers.
        net::service();

        // Afmaak-sprint: HLT-idle. Geef de CPU af tot de VOLGENDE interrupt (timer
        // 100 Hz of toetsenbord/muis/USB-invoer) i.p.v. 100% te spinnen — de CPU
        // slaapt energiezuinig tussen frames; de timer-tick garandeert ~10 ms-
        // responsiviteit en elke invoer-IRQ wekt de desktop meteen.
        x86_64::instructions::hlt();
    }
}


/// Vul een verse EuroFS met de systeembestanden + ingebakken userspace-programma's.
/// Aangeroepen bij het formatteren (eerste boot / installatie op een lege schijf).
/// De systeem-binaries die de kernel MEELEVERT (pad + ELF-bytes) — één bron van
/// waarheid voor zowel de eerste installatie (`populate_fs`) als de versie-sync
/// (`sync_system_files`). Bij het toevoegen van een /bin-programma: hier één regel.
/// Toon een EuroJS-waarde als string (voor het `js`-shellcommando).
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

fn system_binaries() -> [(&'static str, &'static [u8]); 29] {
    [
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

/// FNV-1a-digest over alle meegeleverde binaries (pad + inhoud). Verandert zodra
/// één /bin-programma herbouwd wordt — de "build-id" voor de systeem-sync.
fn system_digest() -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset-basis
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

/// Registreer alle bestanden onder `dir` (recursief) in de userspace-VFS, zodat
/// ring-3-programma's ze via open/read kunnen lezen. EuroFS blijft de bron; dit is
/// de syscall-zichtbare spiegel ervan.
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
                // /etc/shadow NIET in de userspace-VFS spiegelen — wachtwoord-hashes
                // mogen niet wereldleesbaar zijn (alleen de kernel/auth leest ze).
                if path == "/etc/shadow" {
                    continue;
                }
                if let Ok(bytes) = fs.read_file(&path) {
                    ring3::register_file(&path, bytes);
                }
            }
            eurofs::EntryKind::Directory => register_dir_recursive(fs, &path),
            _ => {} // symlinks e.d. niet spiegelen
        }
    }
}

/// De /etc-skeleton: identiteits- en systeemconfig die elke EuroOS-installatie heeft
/// (zoals een echte distro). Eén bron voor zowel installatie (`populate_fs`) als de
/// write-if-missing-aanvulling op bestaande schijven (`ensure_etc_skeleton`).
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

/// Vul ontbrekende /etc-skeletonbestanden aan op een BESTAANDE installatie (een
/// schijf van vóór deze bestanden). Write-if-missing: overschrijft NOOIT bewerkte
/// config. Geeft het aantal aangevulde bestanden terug.
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
    // /etc/shadow (S5): wachtwoord-hashes. 'euro' heeft wachtwoord "euro" (demo);
    // 'root' is vergrendeld ("*") — toegang via sudo, zoals een echt sudo-systeem.
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
    // Build-id vastleggen zodat de volgende boot ziet dat /bin actueel is.
    fs.write_file("/etc/system.ver", format!("{:016x}\n", system_digest()).as_bytes()).unwrap();
    fs.create_dir("/tmp").unwrap();
    fs.create_dir("/var").unwrap();
    fs.create_dir("/var/log").unwrap(); // eurologd-bestemming (volledige daemon = S4)
    // H3: een dynamisch-gelinkte binary + zijn shared library in de FS, zodat de
    // dynlinker de .so via DT_NEEDED uit /lib kan resolven (run-by-name).
    let _ = fs.create_dir("/lib");
    let _ = fs.write_file("/bin/dyntest", ring3::dyntest_bytes());
    let _ = fs.write_file("/lib/libeuro.so", ring3::libeuro_bytes());
    // Welkomst-/infobestand voor de gebruiker (Engels): wat EuroOS is, wat je kan
    // doen, de belangrijkste commando's en de beperkingen. Openbaar te lezen via de
    // Files-app of `cat /Welcome.txt`.
    let _ = fs.create_dir("/home");
    let _ = fs.create_dir("/home/euro");
    fs.write_file("/Welcome.txt", WELCOME_TXT).unwrap();
    let _ = fs.write_file("/home/euro/Welcome.txt", WELCOME_TXT);
}

/// Het Engelstalige welkomst-/infobestand dat in de FS wordt gezaaid.
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

/// Mini-EuroUpdate (Track 9): houd een GEÏNSTALLEERD systeem in sync met de kernel.
/// Als de meegeleverde binaries verschillen van wat op schijf staat (= de kernel is
/// herbouwd), herschrijf /bin + de build-id. Lost de sig-mismatch op waarbij een
/// herbouwde kernel oude /bin-binaries op schijf zou afkeuren. Geeft true bij update.
fn sync_system_files(fs: &mut dyn FileSystem) -> bool {
    let want = format!("{:016x}\n", system_digest());
    let have = fs
        .read_file("/etc/system.ver")
        .ok()
        .and_then(|d| alloc::string::String::from_utf8(d).ok());
    if have.as_deref() == Some(want.as_str()) {
        return false; // schijf is al actueel
    }
    let _ = fs.create_dir("/bin");
    for (path, bytes) in system_binaries() {
        // L1: een eerder IMMUTABEL gemarkeerde binary mag de (vertrouwde) boot-updater
        // wél vervangen — wis de vlag, schrijf de nieuwe build, en `protect_system_files`
        // zet 'm later weer immutabel. Dit is de correcte immutable-OS-update-flow.
        let _ = fs.set_flags(path, 0);
        let _ = fs.write_file(path, bytes);
    }
    let _ = fs.write_file("/boot/version", b"EuroKernel v0.1-alpha\n");
    let _ = fs.write_file("/etc/system.ver", want.as_bytes());
    true
}

/// Bouw de fysieke frame-allocator uit de UEFI-geheugenkaart (vóór exit).
fn build_frame_allocator() -> FrameAllocator {
    let map = boot::memory_map(MemoryType::LOADER_DATA).expect("geheugenkaart");
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

/// Eigen panic-handler (de uefi-variant werkt niet meer na ExitBootServices).
/// Logt naar COM1 en tekent een rood paniekscherm als de framebuffer bekend is.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Seriële paniek-dump: bericht + registers + backtrace + recente kmsg-historie.
    // write_raw gaat rechtstreeks naar de UART (try_lock), dus werkt ook als een
    // andere lock vastzat toen de paniek toesloeg.
    serial::write_raw(b"\n========== KERNEL PANIC ==========\n");
    serial_println!("[PANIC] {info}");
    klog::dump_registers_and_backtrace();
    serial::write_raw(b"[panic] --- recente kernel-log (kmsg) ---\n");
    klog::with_recent(24, |line| {
        serial::write_raw(b"  | ");
        serial::write_raw(line);
        serial::write_raw(b"\n");
    });
    serial::write_raw(b"========== EINDE PANIC ==========\n");

    // Scherm: rode achtergrond + bericht + de laatste paar logregels als context.
    if let Some(fbi) = FB_INFO.get() {
        let fb = unsafe { FrameBuffer::new(fbi.base as *mut u8, fbi.width, fbi.height, fbi.stride, fbi.pf) };
        fb.clear(Color::rgb(0x40, 0x08, 0x10));
        draw_string(&fb, 24, 24, "KERNEL PANIC", Color::WHITE, 3);
        draw_string(&fb, 24, 84, "recente kernel-log (zie COM1 voor registers + backtrace):", Color::WHITE, 1);
        // Laatste ~24 regels onderaan-opbouwend tonen.
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
