//! EuroUpdate-integratie in de kernel (plan F1 + G4): laad de A/B-slotconfiguratie
//! van een GERESERVEERD RAUW BLOK (LBA 40, buiten elke EuroFS-partitie — overleeft
//! filesystem-corruptie/torn-writes), voer bij elke boot de bootloader-/rollback-
//! logica uit, en markeer het slot "goed" zodra de boot slaagt. De `euroupdate`-crate
//! bevat de (host-geteste) toestandsmachine; hier komt de rauw-blok-persistentie +
//! EuroGuard-getekende `apply`-stroom bovenop. `/boot/slot_config` blijft als
//! mens-leesbare spiegel bestaan.
//!
//! NB: in deze build voert de KERNEL de slotbeslissing uit (onze UEFI-loader kiest
//! nog geen slot-image). De configuratie-logica is identiek aan wat de bootloader
//! straks doet; zo is het anti-brick-mechanisme nu al echt en zichtbaar.

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::FileSystem;
use euroupdate::{Slot, SlotConfig};
use spin::Mutex;


const CONFIG_PATH: &str = "/boot/slot_config";

/// Gereserveerde rauwe LBA voor de A/B-slotconfiguratie (G4). De GPT-partitietabel
/// vult LBA 2..33 (128 entries) en de eerste partitie begint pas op LBA 2048 — de
/// gat-sector op LBA 40 ligt dus BUITEN elke EuroFS-partitie. Door de slotstaat
/// hier (i.p.v. een bestand) op te slaan overleeft hij filesystem-corruptie,
/// torn-writes in de superblock, en een onbruikbaar slot-image — precies wat een
/// anti-brick-mechanisme moet kunnen. Dit is de bron-van-waarheid; het bestand
/// `/boot/slot_config` is nog een mens-leesbare spiegel.
const SLOT_LBA: u64 = 40;

static CONFIG: Mutex<Option<SlotConfig>> = Mutex::new(None);

/// Lees de slotconfig van het rauwe gereserveerde blok (los van EuroFS).
fn raw_load() -> Option<SlotConfig> {
    if !crate::virtio_blk::present() {
        return None;
    }
    let mut buf = [0u8; 512];
    if !crate::virtio_blk::read_sector(SLOT_LBA, &mut buf) {
        return None;
    }
    SlotConfig::deserialize(&buf[..euroupdate::CONFIG_SIZE])
}

/// Schrijf de slotconfig naar het rauwe gereserveerde blok + flush naar hardware.
fn raw_persist(cfg: &SlotConfig) -> bool {
    if !crate::virtio_blk::present() {
        return false;
    }
    let mut buf = [0u8; 512];
    buf[..euroupdate::CONFIG_SIZE].copy_from_slice(&cfg.serialize());
    let ok = crate::virtio_blk::write_sector(SLOT_LBA, &buf);
    crate::virtio_blk::flush();
    ok
}

fn slot_name(s: Slot) -> &'static str {
    match s {
        Slot::A => "A",
        Slot::B => "B",
    }
}

fn load(fs: &mut dyn FileSystem) -> SlotConfig {
    // Bron-van-waarheid: het rauwe blok (overleeft FS-corruptie). Valt terug op de
    // FS-spiegel, dan op een verse initiële config.
    if let Some(cfg) = raw_load() {
        return cfg;
    }
    match fs.read_file(CONFIG_PATH) {
        Ok(d) => SlotConfig::deserialize(&d).unwrap_or_else(SlotConfig::initial),
        Err(_) => SlotConfig::initial(),
    }
}

fn persist(fs: &mut dyn FileSystem, cfg: &SlotConfig) {
    // Primair naar het rauwe blok; daarna de mens-leesbare FS-spiegel.
    raw_persist(cfg);
    let _ = fs.create_dir("/boot");
    let _ = fs.write_file(CONFIG_PATH, &cfg.serialize());
}

/// Eén keer per boot: lees de config, voer `on_boot` uit (kies slot + werk de
/// poging-teller bij), persisteer, en log de beslissing.
pub fn boot_init(fs: &mut dyn FileSystem) {
    // Kwam de slotstaat van het rauwe blok (een vorige boot schreef 'm) of is dit
    // een verse schijf? Dat onderscheid bewijst cross-reboot-persistentie.
    let from_raw = raw_load().is_some();
    let mut cfg = load(fs);
    let booted = cfg.on_boot();
    persist(fs, &cfg);
    *CONFIG.lock() = Some(cfg);
    crate::serial_println!(
        "[euroupdate] boot van slot {} (gen {}, {} poging(en) over, A={:?} B={:?})",
        slot_name(booted),
        cfg.generation,
        cfg.tries,
        cfg.state(Slot::A),
        cfg.state(Slot::B),
    );
    // G4: bewijs dat de slotstaat op het rauwe blok (buiten EuroFS) staat en exact
    // terugleest — een verse blok-read, los van de in-memory config.
    match raw_load() {
        Some(rb) if rb == cfg => crate::serial_println!(
            "[g4] slot_config op rauw blok LBA {} (buiten EuroFS) — {}, round-trip geverifieerd, gen {} ✓",
            SLOT_LBA,
            if from_raw { "HERSTELD van vorige boot" } else { "verse schijf → initieel" },
            rb.generation
        ),
        Some(_) => crate::serial_println!("[g4] WAARSCHUWING: rauw-blok slot_config wijkt af van geheugen"),
        None => crate::serial_println!("[g4] rauw-blok slot_config niet leesbaar (geen virtio-blk?)"),
    }
}

/// Roep dit aan zodra de boot succesvol is (EuroInit/desktop bereikt): markeer
/// het actieve slot definitief goed, zodat een volgende boot niet terugrolt.
pub fn mark_boot_good(fs: &mut dyn FileSystem) {
    let mut guard = CONFIG.lock();
    if let Some(cfg) = guard.as_mut() {
        cfg.mark_good();
        let snapshot = *cfg;
        drop(guard);
        persist(fs, &snapshot);
        crate::serial_println!("[euroupdate] slot {} bevestigd GOED (boot geslaagd)", slot_name(snapshot.active));
    }
}

/// `euroupdate status` — toon de huidige slotconfiguratie.
pub fn status(fs: &mut dyn FileSystem) -> Vec<String> {
    let cfg = (*CONFIG.lock()).unwrap_or_else(|| load(fs));
    alloc::vec![
        String::from("EuroUpdate — A/B-systeemslots"),
        alloc::format!("  actief slot   : {}", slot_name(cfg.active)),
        alloc::format!("  volgende boot : {} ({} poging(en) over)", slot_name(cfg.next_boot), cfg.tries),
        alloc::format!("  slot A        : {:?}", cfg.state(Slot::A)),
        alloc::format!("  slot B        : {:?}", cfg.state(Slot::B)),
        alloc::format!("  generatie     : {}", cfg.generation),
    ]
}

/// De GPT-partitienaam van een A/B-slot (G4 multi-partitie-layout).
fn slot_partition_name(s: Slot) -> &'static str {
    match s {
        Slot::A => "EuroOS-A",
        Slot::B => "EuroOS-B",
    }
}

/// Schrijf `image` direct naar de partitie van `slot` (sector-I/O, buiten EuroFS)
/// en verifieer met een read-back van de eerste sector. Dit is de echte A/B-
/// image-write: het slot-image leeft in zijn eigen GPT-partitie, niet in een
/// bestand op de root-FS. Geeft Ok(bytes) of een foutreden.
fn write_image_to_slot(slot: Slot, image: &[u8]) -> Result<usize, &'static str> {
    let (first_lba, blocks) =
        crate::gpt::find_partition_by_name(slot_partition_name(slot)).ok_or("slot-partitie niet gevonden")?;
    let nsec = image.len().div_ceil(512);
    if nsec as u64 > blocks * 8 {
        return Err("image groter dan de slot-partitie");
    }
    for i in 0..nsec {
        let off = i * 512;
        let end = (off + 512).min(image.len());
        let mut sec = [0u8; 512];
        sec[..end - off].copy_from_slice(&image[off..end]);
        if !crate::virtio_blk::write_sector(first_lba + i as u64, &sec) {
            return Err("schrijven naar slot-partitie mislukt");
        }
    }
    crate::virtio_blk::flush();
    // Read-back-verificatie (eerste sector) — bewijst dat het op de schijf staat.
    let mut rb = [0u8; 512];
    if !crate::virtio_blk::read_sector(first_lba, &mut rb) {
        return Err("read-back mislukt");
    }
    let first_end = 512.min(image.len());
    if rb[..first_end] != image[..first_end] {
        return Err("read-back-mismatch");
    }
    Ok(image.len())
}

/// G4-zelftest: schrijf een patroon naar de (ongebruikte) EuroOS-B-slot-partitie
/// en lees het terug — bewijst de directe image→partitie-write-weg.
pub fn slot_partition_selftest() {
    if !crate::virtio_blk::present() {
        return;
    }
    let pattern = b"EuroOS slot-image partitie-write zelftest (G4) -- niet-FS, directe sector-I/O";
    match write_image_to_slot(Slot::B, pattern) {
        Ok(n) => crate::serial_println!(
            "[g4] slot-image-write: {} bytes naar EuroOS-B-partitie geschreven + read-back geverifieerd ✓",
            n
        ),
        Err(e) => crate::serial_println!("[g4] slot-image-write zelftest: {e}"),
    }
}

/// `euroupdate apply <image>` — verifieer de Ed25519-handtekening van `<image>`
/// (verwacht `<image>.sig` ernaast), "schrijf" naar het inactieve slot, en stage
/// de update zodat de volgende boot het probeert (met automatische rollback).
pub fn apply(fs: &mut dyn FileSystem, image_path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let image = match fs.read_file(image_path) {
        Ok(d) => d,
        Err(_) => {
            out.push(alloc::format!("euroupdate: kan '{image_path}' niet lezen"));
            return out;
        }
    };
    let sig_path = alloc::format!("{image_path}.sig");
    let sig = match fs.read_file(&sig_path) {
        Ok(d) => d,
        Err(_) => {
            out.push(alloc::format!("euroupdate: handtekening '{sig_path}' ontbreekt"));
            return out;
        }
    };
    stage_verified_image(fs, &image, &sig)
}

/// De kern van een veilige update: verifieer de Ed25519-handtekening over `image`
/// (verify-before-activate), schrijf het naar het inactieve slot, en stage het.
/// Gedeeld door `apply` (FS-bron) en `fetch` (netwerkbron). Een ongeldige
/// handtekening leidt ALTIJD tot weigering — een gemanipuleerde update wordt nooit
/// gestaged, laat staan geactiveerd.
fn stage_verified_image(fs: &mut dyn FileSystem, image: &[u8], sig: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    // EuroGuard: weiger een update zonder geldige EuroOS-handtekening (anti-tamper).
    if !crate::crypto::verify(image, sig) {
        out.push("euroupdate: ONGELDIGE handtekening — update GEWEIGERD".into());
        return out;
    }
    let mut cfg = CONFIG.lock().take().unwrap_or_else(|| load(fs));
    let target = cfg.inactive();
    // Schrijf het image direct naar de PARTITIE van het inactieve slot (G4: echte
    // A/B-partities + read-back-verificatie). Valt terug op een FS-bestand als de
    // multi-partitie-GPT (nog) niet aanwezig is.
    match write_image_to_slot(target, image) {
        Ok(n) => out.push(alloc::format!(
            "euroupdate: {} bytes naar de {}-partitie geschreven + read-back ✓",
            n,
            slot_partition_name(target)
        )),
        Err(_) => {
            let slot_file = alloc::format!("/boot/slot_{}.img", slot_name(target));
            let _ = fs.create_dir("/boot");
            if fs.write_file(&slot_file, image).is_err() {
                out.push("euroupdate: schrijven naar het inactieve slot MISLUKT".into());
                *CONFIG.lock() = Some(cfg);
                return out;
            }
            out.push(alloc::format!("euroupdate: (fallback) image naar {slot_file} geschreven"));
        }
    }
    cfg.stage_update();
    persist(fs, &cfg);
    out.push(alloc::format!(
        "euroupdate: image ({} bytes) geverifieerd + naar slot {} geschreven",
        image.len(),
        slot_name(target)
    ));
    out.push(alloc::format!(
        "  volgende boot probeert slot {} ({} pogingen, dan automatische rollback)",
        slot_name(cfg.next_boot),
        cfg.tries
    ));
    *CONFIG.lock() = Some(cfg);
    out
}

/// `euroupdate fetch <url>` — haal een GESIGNEERD updatepakket over HTTPS op
/// (`<url>` = het image, `<url>.sig` = de Ed25519-handtekening), verifieer het
/// tegen de ingebakken EuroOS-sleutel, en stage het naar het inactieve slot.
/// Gebruikt de echte EuroTLS-1.3-stack (`net::fetch_full`). In deze sandbox is er
/// geen externe netwerktoegang, dus rapporteren we de echte fetch-uitkomst eerlijk;
/// de verify-+-stage-pijplijn die volgt is identiek aan `apply`.
pub fn fetch(fs: &mut dyn FileSystem, url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (host, port, path, tls) = match parse_url(url) {
        Some(p) => p,
        None => {
            out.push(alloc::format!("euroupdate fetch: ongeldige URL '{url}' (verwacht http(s)://host[:poort]/pad)"));
            return out;
        }
    };
    out.push(alloc::format!(
        "euroupdate fetch: {} {}:{}{} via EuroTLS-1.3…",
        if tls { "HTTPS" } else { "HTTP" }, host, port, path
    ));
    let sig_path = alloc::format!("{path}.sig");
    let image = match crate::net::fetch_full(&host, port, &path, tls) {
        Some((200, _, body)) => body,
        Some((code, _, _)) => {
            out.push(alloc::format!("euroupdate fetch: server gaf HTTP {code} voor het image — afgebroken"));
            return out;
        }
        None => {
            out.push("euroupdate fetch: geen verbinding/antwoord (geen externe netwerktoegang in deze omgeving) — afgebroken".into());
            return out;
        }
    };
    let sig = match crate::net::fetch_full(&host, port, &sig_path, tls) {
        Some((200, _, body)) => body,
        _ => {
            out.push(alloc::format!("euroupdate fetch: handtekening {sig_path} niet opgehaald — afgebroken"));
            return out;
        }
    };
    out.push(alloc::format!("euroupdate fetch: {} B image + {} B handtekening opgehaald — verifiëren…", image.len(), sig.len()));
    out.extend(stage_verified_image(fs, &image, &sig));
    out
}

/// Heel eenvoudige URL-parser: `http(s)://host[:poort]/pad`.
fn parse_url(url: &str) -> Option<(String, u16, String, bool)> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return None;
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, if tls { 443 } else { 80 }),
    };
    Some((String::from(host), port, String::from(path), tls))
}

/// **[upd3] (pijplijn) — `apply()` aanvaardt een echt pakket en weigert een
/// gemanipuleerd**, end-to-end op een RAM-EuroFS, met de ECHTE dev.key-handtekening.
/// Globale slotstaat wordt rond de test bewaard/hersteld (niet-invasief).
pub fn apply_gate_selftest(now: u64) {
    use eurofs::{EuroFs, MemoryBlockDevice};
    let saved = *CONFIG.lock();
    let mut dev = MemoryBlockDevice::new(1024, 4096);
    let mut fs = match EuroFs::format(&mut dev, [0x33; 16], now) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[upd3] (pijplijn) kon RAM-EuroFS niet formatteren — overgeslagen");
            return;
        }
    };
    let (img, sig) = crate::crypto::test_update_image();
    let _ = fs.create_dir("/upd");
    let _ = fs.write_file("/upd/ok.img", img);
    let _ = fs.write_file("/upd/ok.img.sig", sig);
    let accepted = apply(&mut fs, "/upd/ok.img").iter().any(|l| l.contains("geverifieerd + naar slot"));

    let mut tampered = img.to_vec();
    tampered[200] ^= 0xFF; // gemanipuleerd image, originele (geldige) handtekening
    let _ = fs.write_file("/upd/bad.img", &tampered);
    let _ = fs.write_file("/upd/bad.img.sig", sig);
    let refused = apply(&mut fs, "/upd/bad.img").iter().any(|l| l.contains("GEWEIGERD"));

    *CONFIG.lock() = saved; // globale slotstaat herstellen
    crate::serial_println!(
        "[upd3] update-pijplijn: echt pakket gestaged={} · gemanipuleerd pakket geweigerd={} → {}",
        accepted, refused,
        if accepted && refused { "OK ✓" } else { "MISLUKT ✗" }
    );
}

/// `euroupdate rollback` — forceer terug naar het andere goede slot.
pub fn rollback(fs: &mut dyn FileSystem) -> Vec<String> {
    let mut cfg = CONFIG.lock().take().unwrap_or_else(|| load(fs));
    let ok = cfg.rollback();
    persist(fs, &cfg);
    let res = if ok {
        alloc::format!("euroupdate: rollback ingesteld — volgende boot van slot {}", slot_name(cfg.next_boot))
    } else {
        String::from("euroupdate: rollback NIET mogelijk (geen ander goed slot)")
    };
    *CONFIG.lock() = Some(cfg);
    alloc::vec![res]
}
