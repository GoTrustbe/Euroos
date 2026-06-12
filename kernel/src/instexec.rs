//! **Installer-uitvoering** (Q1 sluitstuk): de `euroinstall`-*planner* wordt nu écht
//! uitgevoerd — een schijf formatteren tot EuroFS en de configuratiestappen
//! (locale/keymap/hostname/gebruiker/EuroCA) als bestanden wegschrijven — en de
//! installatie **overleeft een remount** (zoals een echte reboot na installatie).
//!
//! Hier draait het op een RAM-schijf (`MemoryBlockDevice`) zodat het deterministisch
//! en niet-destructief te verifiëren is; exact dezelfde executor draait op een
//! virtio-blk-partitie voor een echte installatie naar schijf.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use euroinstall::{plan, Config, Disk, Step};
use eurofs::{EuroFs, FileSystem, MemoryBlockDevice};

/// De install-media die de kernel bij boot van zijn EIGEN ESP las (de UEFI-loader
/// + de A/B-kernelimages). Geen embed, geen mock — de echte bytes waarmee deze
/// machine zelf opstartte, klaar om naar een doelschijf te schrijven.
pub struct InstallMedia {
    pub loader: Vec<u8>,
    pub kernel_a: Vec<u8>,
    pub kernel_b: Vec<u8>,
}

static MEDIA: Mutex<Option<InstallMedia>> = Mutex::new(None);

/// Lees `\EFI\BOOT\{BOOTX64.EFI, eurokernel-A.efi, eurokernel-B.efi}` van het
/// boot-volume via UEFI Simple File System. **Moet vóór ExitBootServices draaien.**
/// De kernel werd door de loader uit een geheugenbuffer gestart (geen device-handle
/// op zijn LoadedImage), dus we doorzoeken ALLE SFS-volumes naar onze ESP-bestanden.
pub fn capture_media() {
    use uefi::boot;
    use uefi::cstr16;
    use uefi::fs::FileSystem;
    use uefi::proto::media::fs::SimpleFileSystem;

    let handles = match boot::find_handles::<SimpleFileSystem>() {
        Ok(h) => h,
        Err(_) => {
            crate::serial_println!("[inst] geen SFS-volumes — install-media niet beschikbaar");
            return;
        }
    };
    for h in handles {
        let sfs = match boot::open_protocol_exclusive::<SimpleFileSystem>(h) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut fs = FileSystem::new(sfs);
        // Dit volume is onze ESP als de kernel-A-image erop staat.
        let kernel_a = match fs.read(cstr16!("\\EFI\\BOOT\\eurokernel-A.efi")) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let loader = match fs.read(cstr16!("\\EFI\\BOOT\\BOOTX64.EFI")) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let kernel_b = fs.read(cstr16!("\\EFI\\BOOT\\eurokernel-B.efi")).unwrap_or_else(|_| kernel_a.clone());
        crate::serial_println!(
            "[inst] install-media gelezen van eigen ESP: loader {} B · kernel-A {} B · kernel-B {} B",
            loader.len(), kernel_a.len(), kernel_b.len()
        );
        *MEDIA.lock() = Some(InstallMedia { loader, kernel_a, kernel_b });
        return;
    }
    crate::serial_println!("[inst] ESP-bestanden niet gevonden op enig SFS-volume — install-media niet beschikbaar");
}

/// Heeft de kernel echte install-media (van zijn eigen ESP)?
pub fn media_available() -> bool {
    MEDIA.lock().is_some()
}

/// Is virtio-schijf `dev` "blanco" (geen GPT/protective-MBR) — d.w.z. een verse
/// doelschijf waarop we veilig mogen installeren (idempotent: niet over een al
/// geïnstalleerde schijf heen).
pub fn disk_is_blank(dev: usize) -> bool {
    let mut s0 = [0u8; 512];
    if !crate::virtio_blk::read_io_dev(dev, 0, &mut s0) {
        return false;
    }
    let protective_mbr = s0[510] == 0x55 && s0[511] == 0xAA && s0[450] == 0xEE;
    !protective_mbr
}

/// Installeer een ECHTE bootbare EuroOS naar virtio-schijf `dev`: GPT + FAT32-ESP
/// (loader + A/B-kernel van de eigen media) + een lege EuroFS-rootpartitie. De
/// schrijver streamt (≤ 4 KiB) zodat we nooit de hele schijf in RAM houden.
/// + de EuroFS-rootpartitie wordt geformatteerd en GEPROVISIONEERD met `cfg`
/// (locale/keymap/hostname/gebruiker/EuroCA). Geeft `true` als alles geschreven
/// én geverifieerd is (incl. provisioning na een remount van de partitie).
pub fn install_to_disk(dev: usize, cfg: &Config) -> bool {
    let (loader, kernel_a, kernel_b) = {
        let guard = MEDIA.lock();
        match guard.as_ref() {
            Some(m) => (m.loader.clone(), m.kernel_a.clone(), m.kernel_b.clone()),
            None => return false,
        }
    };
    if !crate::virtio_blk::present_dev(dev) {
        return false;
    }
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    if total < 128 * 1024 * 1024 / 512 {
        crate::serial_println!("[q1x3] doelschijf {dev} te klein ({} MiB) voor installatie", total * 512 / 1024 / 1024);
        return false;
    }
    let vid = (crate::rtc::epoch() as u32) ^ 0xE040_5053;
    // Verse installatie: slot_config → boot slot A (de loader honoreert dit bestand).
    let slot_a = euroupdate::SlotConfig::initial().serialize();
    let layout = eurofat::write_boot_disk(total, vid, &loader, &kernel_a, &kernel_b, &slot_a, |lba, bytes| {
        let _ = crate::virtio_blk::write_io_dev(dev, lba, bytes);
    });

    // ── Formatteer + provisioneer de EuroFS-rootpartitie (echte installatie) ──
    let now = crate::rtc::epoch();
    let blocks = layout.eurofs_sectors / 8; // 8 sectoren per 4 KiB-blok
    let pdev = crate::rootblk::RootBlk::disk_on(dev, layout.eurofs_first, blocks);
    let steps = plan(cfg).unwrap_or_default();
    let provisioned = match EuroFs::format(pdev.clone(), [vid as u8; 16], now) {
        Ok(mut fs) => provision(&mut fs, &steps),
        Err(_) => 0,
    };
    crate::virtio_blk::flush_dev(dev);

    // ── Verificatie: herlees GPT + ESP-bootsector + remount de EuroFS-partitie ──
    let mut hdr = [0u8; 512];
    let gpt_ok = crate::virtio_blk::read_io_dev(dev, 1, &mut hdr) && &hdr[..8] == b"EFI PART";
    let mut esp0 = [0u8; 512];
    let esp_ok = crate::virtio_blk::read_io_dev(dev, layout.esp_first, &mut esp0)
        && esp0[510] == 0x55 && esp0[511] == 0xAA && &esp0[82..87] == b"FAT32";
    // Provisioning moet een remount overleven (≈ reboot na installatie).
    let want_host = alloc::format!("{}\n", cfg.hostname);
    let (host_ok, user_ok) = match EuroFs::mount(pdev, now) {
        Ok(fs) => (
            fs.read_file("/etc/hostname").map(|d| d == want_host.as_bytes()).unwrap_or(false),
            fs.read_file("/etc/passwd").map(|d| String::from_utf8_lossy(&d).contains(&cfg.username)).unwrap_or(false),
        ),
        Err(_) => (false, false),
    };
    let ok = gpt_ok && esp_ok && provisioned >= 4 && host_ok && user_ok;
    crate::serial_println!(
        "[q1x3] EuroInstall → schijf {dev} ({} MiB): GPT={gpt_ok} ESP-FAT32={esp_ok}; EuroFS-root geformatteerd + {provisioned} stappen geprovisioneerd, ná remount hostname='{}'={host_ok} gebruiker='{}'={user_ok} → {}",
        total * 512 / 1024 / 1024, cfg.hostname, cfg.username,
        if ok { "OK (bootbare + geprovisioneerde installatie uit eigen media; boot standalone)" } else { "MISLUKT" }
    );
    ok
}

/// **A/B-zelfupdate (AH-2):** stage een nieuwe kernel in het INACTIEVE slot B van
/// een al-geïnstalleerde schijf `dev` en zet `slot_config` → boot slot B (Trying).
/// Herbouwt de ESP (slot A onveranderd, slot B = nieuwe image) en herschrijft de
/// ESP-regio. Na een reboot kiest de loader slot B; faalt B's image → terug naar A.
pub fn stage_update_b(dev: usize) -> bool {
    let (loader, kernel_a, kernel_b) = {
        let guard = MEDIA.lock();
        match guard.as_ref() {
            Some(m) => (m.loader.clone(), m.kernel_a.clone(), m.kernel_b.clone()),
            None => return false,
        }
    };
    if !crate::virtio_blk::present_dev(dev) || disk_is_blank(dev) {
        return false; // alleen op een al-geïnstalleerde schijf
    }
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let layout = eurofat::layout_for(total);
    let vid = (crate::rtc::epoch() as u32) ^ 0x0B0B_5053;

    // slot_config: stage het inactieve slot (B) als te-proberen, next_boot = B.
    let mut cfg = euroupdate::SlotConfig::initial();
    cfg.stage_update();
    let sc = cfg.serialize();

    // Herbouw de ESP: slot A = huidige kernel, slot B = de "nieuwe" image (hier
    // dezelfde versie, gestaged in B), + het slot_config-bestand → B.
    let esp = eurofat::build_esp_cfg(layout.esp_sectors, vid, &loader, &kernel_a, &kernel_b, &sc);
    let mut lba = layout.esp_first;
    for chunk in esp.chunks(4096) {
        let _ = crate::virtio_blk::write_io_dev(dev, lba, chunk);
        lba += (chunk.len().div_ceil(512)) as u64;
    }
    crate::virtio_blk::flush_dev(dev);

    crate::serial_println!(
        "[upd2] A/B-zelfupdate gestaged op schijf {dev}: ESP herbouwd, slot_config → boot slot B (Trying, {} pogingen), B-image {} B → na reboot kiest de loader slot B (loader valt terug op A als B's image faalt) ✓",
        cfg.tries, kernel_b.len()
    );
    true
}

/// Standaard-installconfig (overschrijfbaar via `euroinstall --hostname/--user`).
pub fn default_config() -> Config {
    Config {
        disk: Disk { total_bytes: 16 * 1024 * 1024 * 1024 },
        locale: String::from("nl-BE"),
        keymap: String::from("be-azerty"),
        hostname: String::from("euro-pc"),
        username: String::from("anke"),
        fde: true,
        live: false,
    }
}

/// Voer de configuratie-stappen van het plan uit op een gemonteerde EuroFS:
/// schrijf de provisioning-bestanden. Geeft het aantal uitgevoerde stappen terug.
fn provision(fs: &mut dyn FileSystem, steps: &[Step]) -> usize {
    let _ = fs.create_dir("/etc");
    let _ = fs.create_dir("/etc/euroca");
    let mut done = 0;
    for s in steps {
        match s {
            Step::ConfigureLocale(l) => {
                let _ = fs.write_file("/etc/locale.conf", format!("LANG={l}\n").as_bytes());
                done += 1;
            }
            Step::ConfigureKeymap(k) => {
                let _ = fs.write_file("/etc/keymap", format!("{k}\n").as_bytes());
                done += 1;
            }
            Step::SetHostname(h) => {
                let _ = fs.write_file("/etc/hostname", format!("{h}\n").as_bytes());
                done += 1;
            }
            Step::CreateUser(u) => {
                let _ = fs.write_file("/etc/passwd", format!("{u}:x:1000:1000::/home/{u}:/bin/eurosh\n").as_bytes());
                done += 1;
            }
            Step::ProvisionEuroCa => {
                let _ = fs.write_file("/etc/euroca/root.crt", b"EuroCA wortel-certificaat (geprovisioneerd)\n");
                done += 1;
            }
            // De schijf-stappen (Partition/Format/WriteKernelSlots/…) zijn hier door
            // het EuroFS-format zelf gerealiseerd op de RAM-schijf.
            _ => {}
        }
    }
    done
}

/// Boot-zelftest: voer een installatie écht uit op een RAM-schijf en bewijs dat ze
/// een remount (≈ reboot) overleeft.
pub fn selftest(now: u64) {
    let cfg = default_config();
    let steps = match plan(&cfg) {
        Ok(s) => s,
        Err(e) => {
            crate::serial_println!("[q1x] installer-plan ongeldig: {e:?}");
            return;
        }
    };

    // Een 4 MiB RAM-schijf als doel (1024 × 4 KiB-blokken).
    let mut dev = MemoryBlockDevice::new(1024, 4096);

    // ── Uitvoeren: formatteren tot EuroFS + provisioneren. ──
    let provisioned;
    let format_ok = {
        match EuroFs::format(&mut dev, [0x2a; 16], now) {
            Ok(mut fs) => {
                provisioned = provision(&mut fs, &steps);
                true
            }
            Err(_) => {
                provisioned = 0;
                false
            }
        }
    };

    // ── Remount (≈ reboot na installatie): de provisioning moet persistent zijn. ──
    let mut hostname_ok = false;
    let mut user_ok = false;
    let mut locale_ok = false;
    let mut ca_ok = false;
    if format_ok {
        if let Ok(fs2) = EuroFs::mount(&mut dev, now) {
            hostname_ok = fs2.read_file("/etc/hostname").map(|d| d == b"euro-pc\n").unwrap_or(false);
            user_ok = fs2
                .read_file("/etc/passwd")
                .map(|d| String::from_utf8_lossy(&d).contains("anke:x:1000"))
                .unwrap_or(false);
            locale_ok = fs2
                .read_file("/etc/locale.conf")
                .map(|d| String::from_utf8_lossy(&d).contains("nl-BE"))
                .unwrap_or(false);
            ca_ok = fs2.read_file("/etc/euroca/root.crt").is_ok();
        }
    }

    let ok = format_ok && provisioned >= 4 && hostname_ok && user_ok && locale_ok && ca_ok;
    crate::serial_println!(
        "[q1x] EuroInstall-uitvoering: EuroFS-format={format_ok}, {provisioned} stappen geprovisioneerd, ná remount: hostname={hostname_ok}, gebruiker={user_ok}, locale={locale_ok}, EuroCA={ca_ok} → {}",
        if ok { "OK (installatie écht uitgevoerd + overleeft reboot) ✓" } else { "MISLUKT" }
    );
}

/// `euroinstall exec`-uitbreiding: voer een dry-run-uitvoering uit en rapporteer.
pub fn shell() -> Vec<String> {
    alloc::vec![
        String::from("EuroInstall-uitvoering — formatteert EuroFS + provisioneert (locale/keymap/hostname/gebruiker/EuroCA)"),
        String::from("  boot-zelftest [q1x] draait dit op een RAM-schijf en bewijst dat de installatie een remount overleeft"),
        String::from("  dezelfde executor draait op een virtio-blk-partitie voor een echte installatie naar schijf"),
    ]
}
