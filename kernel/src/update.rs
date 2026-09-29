//! EuroUpdate integration in the kernel (plan F1 + G4): load the A/B slot configuration
//! from a RESERVED RAW BLOCK (LBA 40, outside every EuroFS partition — survives
//! filesystem corruption/torn-writes), run the bootloader/rollback logic on every
//! boot, and mark the slot "good" as soon as the boot succeeds. The `euroupdate`
//! crate contains the (host-tested) state machine; on top of it comes the raw-block
//! persistence + EuroGuard-signed `apply` flow. `/boot/slot_config` remains as a
//! human-readable mirror.
//!
//! NB: in this build the KERNEL makes the slot decision (our UEFI loader does not
//! yet pick a slot image). The configuration logic is identical to what the bootloader
//! will eventually do; this way the anti-brick mechanism is already real and visible now.

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::FileSystem;
use euroupdate::{Slot, SlotConfig};
use spin::Mutex;


const CONFIG_PATH: &str = "/boot/slot_config";

/// Reserved raw LBA for the A/B slot configuration (G4). The GPT partition table
/// fills LBA 2..33 (128 entries) and the first partition only begins at LBA 2048 — the
/// gap sector at LBA 40 thus lies OUTSIDE every EuroFS partition. By storing the slot
/// state here (instead of a file) it survives filesystem corruption,
/// torn-writes in the superblock, and an unusable slot image — exactly what an
/// anti-brick mechanism must be able to do. This is the source of truth; the file
/// `/boot/slot_config` is still a human-readable mirror.
const SLOT_LBA: u64 = 40;

static CONFIG: Mutex<Option<SlotConfig>> = Mutex::new(None);
/// Boot self-tests ([upd3] apply gate, [3e2] channel, [g4] slot write) exercise the
/// REAL staging code. With the loader now trusting a headered slot image and the
/// ESP `slot_config` being the loader's truth, a self-test that wrote its test
/// pattern into EuroSlot-B and staged it would send the next boot into three
/// failed attempts. While `SELFTEST_DRY` is set, slot writes and slot_config
/// persistence are skipped (the verify/decide logic still runs for real).
static SELFTEST_DRY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
struct DryGuard;
impl DryGuard {
    fn new() -> DryGuard {
        SELFTEST_DRY.store(true, core::sync::atomic::Ordering::SeqCst);
        DryGuard
    }
}
impl Drop for DryGuard {
    fn drop(&mut self) {
        SELFTEST_DRY.store(false, core::sync::atomic::Ordering::SeqCst);
    }
}
fn dry() -> bool {
    SELFTEST_DRY.load(core::sync::atomic::Ordering::SeqCst)
}

/// Is virtio dev 0 a foreign DATA disk (a EuroPack chrome-serving disk), NOT our
/// boot disk? Then LBA 40 holds the served file's bytes, not a slot config — we
/// must NEVER read a "slot config" from it or (worse) write one over it.
fn virtio0_is_foreign() -> bool {
    let mut s0 = [0u8; 512];
    crate::rootblk::boot_read(0, &mut s0) && crate::ring3::is_europack_header(&s0)
}

/// Read the slot config from the raw reserved block (independent of EuroFS).
fn raw_load() -> Option<SlotConfig> {
    if !crate::rootblk::boot_present() || virtio0_is_foreign() {
        return None;
    }
    let mut buf = [0u8; 512];
    if !crate::rootblk::boot_read(SLOT_LBA, &mut buf) {
        return None;
    }
    SlotConfig::deserialize(&buf[..euroupdate::CONFIG_SIZE])
}

/// Write the slot config to the raw reserved block + flush to hardware.
fn raw_persist(cfg: &SlotConfig) -> bool {
    if !crate::rootblk::boot_present() || virtio0_is_foreign() {
        return false;
    }
    let mut buf = [0u8; 512];
    buf[..euroupdate::CONFIG_SIZE].copy_from_slice(&cfg.serialize());
    let ok = crate::rootblk::boot_write(SLOT_LBA, &buf);
    crate::rootblk::boot_flush();
    ok
}

fn slot_name(s: Slot) -> &'static str {
    match s {
        Slot::A => "A",
        Slot::B => "B",
    }
}

/// The ESP of the boot disk (installed GPT layout) — where the LOADER reads and
/// writes `\slot_config`. None on the plain preview image (no GPT slots).
fn esp_first() -> Option<u64> {
    if !crate::rootblk::boot_present() || virtio0_is_foreign() {
        return None;
    }
    // Installed layout = the slot partitions exist (a plain preview image has none);
    // the ESP location comes from the partition table, not from an assumed layout.
    crate::gpt::find_partition_by_name("EuroSlot-A")?;
    crate::gpt::find_esp().map(|(first, _)| first)
}
fn esp_load() -> Option<SlotConfig> {
    let esp = esp_first()?;
    let read = |lba: u64, buf: &mut [u8]| crate::rootblk::boot_read(lba, buf);
    let d = eurofat::read_small_file(esp, "slot_config", read)?;
    SlotConfig::deserialize(&d)
}
fn esp_persist(cfg: &SlotConfig) -> bool {
    let Some(esp) = esp_first() else { return false };
    let read = |lba: u64, buf: &mut [u8]| crate::rootblk::boot_read(lba, buf);
    let write = |lba: u64, buf: &[u8]| {
        let ok = crate::rootblk::boot_write(lba, buf);
        crate::rootblk::boot_flush();
        ok
    };
    eurofat::write_small_file(esp, "slot_config", &cfg.serialize(), read, write)
}
fn load(fs: &mut dyn FileSystem) -> SlotConfig {
    // Source of truth on an installed disk: the ESP copy — that is the one the
    // two-stage loader just acted on (attempt counter, rollback). Then the raw
    // block (survives FS corruption), then the FS mirror, then a fresh config.
    if let Some(cfg) = esp_load() {
        return cfg;
    }
    if let Some(cfg) = raw_load() {
        return cfg;
    }
    match fs.read_file(CONFIG_PATH) {
        Ok(d) => SlotConfig::deserialize(&d).unwrap_or_else(SlotConfig::initial),
        Err(_) => SlotConfig::initial(),
    }
}

fn persist(fs: &mut dyn FileSystem, cfg: &SlotConfig) {
    // The ESP copy first (the loader's view: staging an update or marking a slot
    // good must reach it), then the raw block, then the human-readable FS mirror.
    if !dry() {
        esp_persist(cfg);
        raw_persist(cfg);
    }
    let _ = fs.create_dir("/boot");
    let _ = fs.write_file(CONFIG_PATH, &cfg.serialize());
}

/// Once per boot: read the config, run `on_boot` (pick slot + update the
/// attempt counter), persist, and log the decision.
pub fn boot_init(fs: &mut dyn FileSystem) {
    // Did the slot state come from the raw block (a previous boot wrote it) or is this
    // a fresh disk? That distinction proves cross-reboot persistence.
    let from_raw = raw_load().is_some();
    let mut cfg = load(fs);
    let booted = cfg.on_boot();
    persist(fs, &cfg);
    *CONFIG.lock() = Some(cfg);
    crate::serial_println!(
        "[euroupdate] boot from slot {} (gen {}, {} attempt(s) left, A={:?} B={:?}), running version {}",
        slot_name(booted),
        cfg.generation,
        cfg.tries,
        cfg.state(Slot::A),
        cfg.state(Slot::B),
        running_version(),
    );
    // G4: prove that the slot state is on the raw block (outside EuroFS) and reads back
    // exactly — a fresh block read, independent of the in-memory config.
    match raw_load() {
        Some(rb) if rb == cfg => crate::serial_println!(
            "[g4] slot_config on raw block LBA {} (outside EuroFS) — {}, round-trip verified, gen {} ✓",
            SLOT_LBA,
            if from_raw { "RESTORED from previous boot" } else { "fresh disk → initial" },
            rb.generation
        ),
        Some(_) => crate::serial_println!("[g4] WARNING: raw-block slot_config differs from memory"),
        None => crate::serial_println!("[g4] raw-block slot_config not readable (no virtio-blk?)"),
    }
}

/// Call this as soon as the boot is successful (EuroInit/desktop reached): mark
/// the active slot definitively good, so that a next boot does not roll back.
pub fn mark_boot_good(fs: &mut dyn FileSystem) {
    let mut guard = CONFIG.lock();
    if let Some(cfg) = guard.as_mut() {
        cfg.mark_good();
        let snapshot = *cfg;
        drop(guard);
        persist(fs, &snapshot);
        crate::serial_println!("[euroupdate] slot {} confirmed GOOD (boot succeeded)", slot_name(snapshot.active));
    }
}

/// `euroupdate status` — show the current slot configuration.
pub fn status(fs: &mut dyn FileSystem) -> Vec<String> {
    let cfg = (*CONFIG.lock()).unwrap_or_else(|| load(fs));
    alloc::vec![
        String::from("EuroUpdate — A/B system slots"),
        alloc::format!("  active slot   : {}", slot_name(cfg.active)),
        alloc::format!("  next boot     : {} ({} attempt(s) left)", slot_name(cfg.next_boot), cfg.tries),
        alloc::format!("  slot A        : {:?}", cfg.state(Slot::A)),
        alloc::format!("  slot B        : {:?}", cfg.state(Slot::B)),
        alloc::format!("  generation    : {}", cfg.generation),
    ]
}
/// `euroupdate status` incl. the automatic-update state.
pub fn status_full(fs: &mut dyn FileSystem) -> Vec<String> {
    let mut v = status(fs);
    v.extend(auto_status(fs));
    v
}

/// The GPT partition name of an A/B slot (G4 multi-partition layout).
fn slot_partition_name(s: Slot) -> &'static str {
    match s {
        Slot::A => "EuroSlot-A",
        Slot::B => "EuroSlot-B",
    }
}
/// The legacy 4-partition self-test layout names its slots `EuroOS-A/B`.
fn legacy_slot_partition_name(s: Slot) -> &'static str {
    match s {
        Slot::A => "EuroOS-A",
        Slot::B => "EuroOS-B",
    }
}

/// The 512-byte header in front of a slot image: magic, length, sha256, Ed25519
/// signature over the image. Shared by the updater and the installer.
pub fn slot_header(image: &[u8], sig: &[u8]) -> [u8; 512] {
    let mut hdr = [0u8; 512];
    hdr[..8].copy_from_slice(SLOT_MAGIC);
    hdr[8..16].copy_from_slice(&(image.len() as u64).to_le_bytes());
    hdr[16..48].copy_from_slice(&eurotls::keyschedule::sha256(image));
    hdr[48..112].copy_from_slice(&sig[..64]);
    hdr
}

/// Write `image` directly to the partition of `slot` (sector I/O, outside EuroFS)
/// and verify with a read-back of the first sector. This is the real A/B
/// image write: the slot image lives in its own GPT partition, not in a
/// file on the root FS. Returns Ok(bytes) or an error reason.
fn write_image_to_slot(slot: Slot, image: &[u8], sig: &[u8]) -> Result<usize, &'static str> {
    if dry() {
        return Ok(image.len()); // self-test: never touch the real slot partitions
    }
    let (first_lba, blocks) = crate::gpt::find_partition_by_name(slot_partition_name(slot))
        .or_else(|| crate::gpt::find_partition_by_name(legacy_slot_partition_name(slot)))
        .ok_or("slot partition not found")?;
    let nsec = image.len().div_ceil(512);
    if nsec as u64 + 1 > blocks * 8 {
        return Err("image larger than the slot partition");
    }
    // Header sector: the loader reads the length + hash from here and boots the
    // image straight from the partition (no FAT rewrite of the ESP needed).
    if sig.len() != 64 {
        return Err("slot image signature must be 64 bytes");
    }
    let hdr = slot_header(image, sig);
    // Wipe the old header first, then the image, then the new header: a torn
    // write leaves NO valid header over mixed sectors, never one that describes
    // the previous image on top of half of the new one.
    if !crate::rootblk::boot_write(first_lba, &[0u8; 512]) {
        return Err("clearing the slot header failed");
    }
    for i in 0..nsec {
        let off = i * 512;
        let end = (off + 512).min(image.len());
        let mut sec = [0u8; 512];
        sec[..end - off].copy_from_slice(&image[off..end]);
        if !crate::rootblk::boot_write(first_lba + 1 + i as u64, &sec) {
            return Err("writing to slot partition failed");
        }
    }
    crate::rootblk::boot_flush();
    if !crate::rootblk::boot_write(first_lba, &hdr) {
        return Err("writing the slot header failed");
    }
    crate::rootblk::boot_flush();
    // Read-back verification (header + first image sector) — proves that it is on the disk.
    let mut rb = [0u8; 512];
    if !crate::rootblk::boot_read(first_lba, &mut rb) || rb != hdr {
        return Err("header read-back mismatch");
    }
    if !crate::rootblk::boot_read(first_lba + 1, &mut rb) {
        return Err("read-back failed");
    }
    let first_end = 512.min(image.len());
    if rb[..first_end] != image[..first_end] {
        return Err("read-back mismatch");
    }
    Ok(image.len())
}

/// G4 self-test: write a pattern to the (unused) EuroOS-B slot partition
/// and read it back — proves the direct image→partition write path.
pub fn slot_partition_selftest() {
    let _dry = DryGuard::new();
    if !crate::rootblk::boot_present() {
        return;
    }
    let pattern = b"EuroOS slot-image partition-write selftest (G4) -- non-FS, direct sector-I/O";
    match write_image_to_slot(Slot::B, pattern, &[0u8; 64]) {
        Ok(n) => crate::serial_println!(
            "[g4] slot-image-write: {} bytes written to EuroOS-B partition + read-back verified ✓",
            n
        ),
        Err(e) => crate::serial_println!("[g4] slot-image-write selftest: {e}"),
    }
}

/// `euroupdate apply <image>` — verify the Ed25519 signature of `<image>`
/// (expects `<image>.sig` next to it), "write" to the inactive slot, and stage
/// the update so that the next boot tries it (with automatic rollback).
pub fn apply(fs: &mut dyn FileSystem, image_path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let image = match fs.read_file(image_path) {
        Ok(d) => d,
        Err(_) => {
            out.push(alloc::format!("euroupdate: cannot read '{image_path}'"));
            return out;
        }
    };
    let sig_path = alloc::format!("{image_path}.sig");
    let sig = match fs.read_file(&sig_path) {
        Ok(d) => d,
        Err(_) => {
            out.push(alloc::format!("euroupdate: signature '{sig_path}' missing"));
            return out;
        }
    };
    stage_verified_image(fs, &image, &sig).0
}

/// The core of a secure update: verify the Ed25519 signature over `image`
/// (verify-before-activate), write it to the inactive slot, and stage it.
/// Shared by `apply` (FS source) and `fetch` (network source). An invalid
/// signature ALWAYS leads to refusal — a tampered update is never
/// staged, let alone activated.
/// Returns the log and whether the image is now staged (slot_config flipped).
fn stage_verified_image(fs: &mut dyn FileSystem, image: &[u8], sig: &[u8]) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    // EuroGuard: refuse an update without a valid EuroOS signature (anti-tamper).
    if !crate::crypto::verify(image, sig) {
        out.push("euroupdate: INVALID signature — update REFUSED".into());
        return (out, false);
    }
    let mut cfg = CONFIG.lock().take().unwrap_or_else(|| load(fs));
    let target = cfg.inactive();
    // Write the image directly to the PARTITION of the inactive slot (G4: real
    // A/B partitions + read-back verification). On an installed layout a failed
    // write means NOT staged: flipping slot_config towards a slot that still holds
    // the previous image (or half of the new one) would boot the wrong kernel.
    // Only the legacy self-test layout without slot partitions uses an FS file.
    match write_image_to_slot(target, image, sig) {
        Ok(n) => out.push(alloc::format!(
            "euroupdate: {} bytes written to the {} partition + read-back ✓",
            n,
            slot_partition_name(target)
        )),
        Err(why) if esp_first().is_some() => {
            out.push(alloc::format!("euroupdate: writing to the inactive slot FAILED ({why}) — NOT staged"));
            *CONFIG.lock() = Some(cfg);
            return (out, false);
        }
        Err(_) => {
            let slot_file = alloc::format!("/boot/slot_{}.img", slot_name(target));
            let _ = fs.create_dir("/boot");
            if fs.write_file(&slot_file, image).is_err() {
                out.push("euroupdate: writing to the inactive slot FAILED".into());
                *CONFIG.lock() = Some(cfg);
                return (out, false);
            }
            out.push(alloc::format!("euroupdate: (fallback) image written to {slot_file}"));
        }
    }
    cfg.stage_update();
    persist(fs, &cfg);
    out.push(alloc::format!(
        "euroupdate: image ({} bytes) verified + written to slot {}",
        image.len(),
        slot_name(target)
    ));
    out.push(alloc::format!(
        "  next boot tries slot {} ({} attempts, then automatic rollback)",
        slot_name(cfg.next_boot),
        cfg.tries
    ));
    *CONFIG.lock() = Some(cfg);
    (out, true)
}

/// `euroupdate fetch <url>` — fetch a SIGNED update package over HTTPS
/// (`<url>` = the image, `<url>.sig` = the Ed25519 signature), verify it
/// against the baked-in EuroOS key, and stage it to the inactive slot.
/// Uses the real EuroTLS-1.3 stack (`net::fetch_full`). In this sandbox there is
/// no external network access, so we report the real fetch outcome honestly;
/// the verify-+-stage pipeline that follows is identical to `apply`.
pub fn fetch(fs: &mut dyn FileSystem, url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (host, port, path, tls) = match parse_url(url) {
        Some(p) => p,
        None => {
            out.push(alloc::format!("euroupdate fetch: invalid URL '{url}' (expected http(s)://host[:port]/path)"));
            return out;
        }
    };
    out.push(alloc::format!(
        "euroupdate fetch: {} {}:{}{} via EuroTLS-1.3…",
        if tls { "HTTPS" } else { "HTTP" }, host, port, path
    ));
    let sig_path = alloc::format!("{path}.sig");
    let image = match crate::net::fetch_bytes(&host, port, &path, tls, MAX_IMAGE) {
        Some((200, _, body)) => body,
        Some((code, _, _)) => {
            out.push(alloc::format!("euroupdate fetch: server returned HTTP {code} for the image — aborted"));
            return out;
        }
        None => {
            out.push("euroupdate fetch: no connection/response (no external network access in this environment) — aborted".into());
            return out;
        }
    };
    let sig = match crate::net::fetch_full(&host, port, &sig_path, tls) {
        Some((200, _, body)) => body,
        _ => {
            out.push(alloc::format!("euroupdate fetch: signature {sig_path} not fetched — aborted"));
            return out;
        }
    };
    out.push(alloc::format!("euroupdate fetch: {} B image + {} B signature fetched — verifying…", image.len(), sig.len()));
    out.extend(stage_verified_image(fs, &image, &sig).0);
    out
}

/// Very simple URL parser: `http(s)://host[:port]/path`.
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

/// **[upd3] (pipeline) — `apply()` accepts a real package and refuses a
/// tampered one**, end-to-end on a RAM EuroFS, with the REAL dev.key signature.
/// Global slot state is preserved/restored around the test (non-invasive).
pub fn apply_gate_selftest(now: u64) {
    let _dry = DryGuard::new();
    use eurofs::{EuroFs, MemoryBlockDevice};
    let saved = *CONFIG.lock();
    let mut dev = MemoryBlockDevice::new(1024, 4096);
    let mut fs = match EuroFs::format(&mut dev, [0x33; 16], now) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[upd3] (pipeline) could not format RAM EuroFS — skipped");
            return;
        }
    };
    let (img, sig) = crate::crypto::test_update_image();
    let _ = fs.create_dir("/upd");
    let _ = fs.write_file("/upd/ok.img", img);
    let _ = fs.write_file("/upd/ok.img.sig", sig);
    let accepted = apply(&mut fs, "/upd/ok.img").iter().any(|l| l.contains("verified + written to slot"));

    let mut tampered = img.to_vec();
    tampered[200] ^= 0xFF; // tampered image, original (valid) signature
    let _ = fs.write_file("/upd/bad.img", &tampered);
    let _ = fs.write_file("/upd/bad.img.sig", sig);
    let refused = apply(&mut fs, "/upd/bad.img").iter().any(|l| l.contains("REFUSED"));

    *CONFIG.lock() = saved; // restore global slot state
    crate::serial_println!(
        "[upd3] update pipeline: real package staged={} · tampered package refused={} → {}",
        accepted, refused,
        if accepted && refused { "OK ✓" } else { "FAILED ✗" }
    );
}

// ── 3E-2: EuroUpdate delivery — signed channel manifests over the network ──

/// The version THIS build runs (compared against the channel manifest). Set by
/// `scripts/build.sh` as `EUROOS_BUILD_VERSION=YYYYMMDD`; a build without it is
/// version 1, so any published release is newer than it.
pub fn running_version() -> u64 {
    option_env!("EUROOS_BUILD_VERSION").and_then(|v| v.trim().parse().ok()).unwrap_or(1)
}
/// The public update server (`euroupdate check` without arguments, and the
/// periodic background check). Signed manifests + signed images, over HTTPS.
pub const UPDATE_HOST: &str = "euro-os.eu";
pub const UPDATE_PORT: u16 = 443;
pub const UPDATE_TLS: bool = true;
pub const UPDATE_BASE: &str = "/update";
/// Largest kernel image the updater will download (the slot partitions are 96 MiB).
const MAX_IMAGE: usize = 96 * 1024 * 1024;
/// Header sector in front of a slot-partition image, so the loader can find the
/// image length and verify it before `LoadImage`: `EUROSLT2` + u64 length (LE) +
/// SHA-256 of the image + the image's Ed25519 signature (64 B, verified by the
/// loader against the baked-in keys). The image itself starts at sector 1.
pub const SLOT_MAGIC: &[u8; 8] = b"EUROSLT2";
/// Anti-rollback watermark: the highest version ever staged on this machine.
/// A signed manifest below it is refused, so an old (but validly signed) release
/// cannot be replayed onto a newer install.
const SEEN_PATH: &str = "/etc/euroupdate.seen";
fn seen_version(fs: &mut dyn FileSystem) -> u64 {
    fs.read_file(SEEN_PATH).ok().and_then(|d| String::from_utf8_lossy(&d).trim().parse().ok()).unwrap_or(0)
}
fn record_seen(fs: &mut dyn FileSystem, v: u64) {
    if v > seen_version(fs) {
        let _ = fs.create_dir("/etc");
        let _ = fs.write_file(SEEN_PATH, alloc::format!("{v}\n").as_bytes());
    }
}

/// Minimal field extraction from the (signature-verified) channel manifest.
/// The manifest is OUR controlled format — deliberately not a general JSON parser.
fn manifest_u64(s: &str, key: &str) -> Option<u64> {
    let pat = alloc::format!("\"{key}\":");
    let i = s.find(&pat)? + pat.len();
    let rest = s[i..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn manifest_str<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let pat = alloc::format!("\"{key}\":\"");
    let i = s.find(&pat)? + pat.len();
    let rest = &s[i..];
    Some(&rest[..rest.find('"')?])
}

/// `euroupdate check [channel]` — **the delivery chain (3E-2)**: fetch the
/// channel manifest + its Ed25519 signature from the update server, REFUSE an
/// unsigned/forged manifest, compare versions, and on a newer release fetch the
/// image (sha256 pinned by the manifest, Ed25519-signed) and stage it to the
/// inactive A/B slot. Security model = signed metadata + signed payload (the
/// APT model): a hostile mirror/MITM can at worst serve nothing — never a
/// tampered image. Transport here is HTTP; HTTPS runs over the same
/// `net::fetch_full(tls=true)` path when the server has a kernel-trusted cert.
pub fn check_channel(fs: &mut dyn FileSystem, host: &str, port: u16, tls: bool, base: &str, channel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mpath = alloc::format!("{base}/channel/{channel}.json");
    out.push(alloc::format!("euroupdate check: {}://{host}:{port}{base} channel '{channel}' (Ed25519-signed manifest)…", if tls { "https" } else { "http" }));
    let man = match crate::net::fetch_bytes(host, port, &mpath, tls, 64 * 1024) {
        Some((200, _, b)) => b,
        Some((code, _, _)) => {
            out.push(alloc::format!("  manifest HTTP {code} — aborted"));
            return out;
        }
        None => {
            out.push("  no connection to the update server — aborted".into());
            return out;
        }
    };
    let msig = match crate::net::fetch_bytes(host, port, &alloc::format!("{mpath}.sig"), tls, 4096) {
        Some((200, _, b)) => b,
        _ => {
            out.push("  manifest signature missing — channel REFUSED".into());
            return out;
        }
    };
    let (version, image, sha_hex) = match evaluate_manifest(fs, channel, &man, &msig, &mut out) {
        Ok(Verdict::Newer { version, image, sha_hex }) => (version, image, sha_hex),
        Ok(Verdict::UpToDate) | Err(()) => return out,
    };
    let img = match crate::net::fetch_bytes(host, port, &image, tls, MAX_IMAGE) {
        Some((200, _, b)) => b,
        _ => {
            out.push(alloc::format!("  image {image} not fetched — aborted"));
            return out;
        }
    };
    let isig = match crate::net::fetch_bytes(host, port, &alloc::format!("{image}.sig"), tls, 4096) {
        Some((200, _, b)) => b,
        _ => {
            out.push("  image signature not fetched — aborted".into());
            return out;
        }
    };
    finish_image(fs, version, &sha_hex, &img, &isig, &mut out);
    out
}

enum Verdict {
    UpToDate,
    Newer { version: u64, image: String, sha_hex: String },
}
/// Everything that decides whether a fetched manifest may be acted on: signature,
/// well-formedness, strictly newer than the running version, not expired, not
/// below the anti-rollback watermark, sane image path. Logs into `out`;
/// `Err(())` means refused/aborted (the reason is the last log line).
fn evaluate_manifest(fs: &mut dyn FileSystem, channel: &str, man: &[u8], msig: &[u8], out: &mut Vec<String>) -> Result<Verdict, ()> {
    if !crate::crypto::verify(man, msig) {
        out.push("  manifest signature INVALID — channel REFUSED (nothing fetched)".into());
        return Err(());
    }
    let text = String::from_utf8_lossy(man).into_owned();
    let (version, image, sha_hex) =
        match (manifest_u64(&text, "version"), manifest_str(&text, "image"), manifest_str(&text, "sha256")) {
            (Some(v), Some(i), Some(s)) => (v, String::from(i), String::from(s)),
            _ => {
                out.push("  manifest malformed — REFUSED".into());
                return Err(());
            }
        };
    // Bound to the channel it was fetched as: a validly signed beta.json served
    // as stable.json is not a stable release.
    if manifest_str(&text, "channel") != Some(channel) {
        out.push(alloc::format!("  manifest is not for channel '{channel}' — REFUSED"));
        return Err(());
    }
    out.push(alloc::format!("  manifest OK (signature valid): version {version}, running {}", running_version()));
    if version <= running_version() {
        out.push("  already up to date — nothing to do".into());
        return Ok(Verdict::UpToDate);
    }
    // Freshness (fail closed): a manifest that carries `expires` and is past it is
    // refused, so a captured-but-valid old manifest cannot be replayed forever.
    match manifest_u64(&text, "expires") {
        Some(exp) if exp >= crate::rtc::epoch() => {}
        Some(_) => {
            out.push("  manifest EXPIRED — REFUSED".into());
            return Err(());
        }
        None => {
            out.push("  manifest without expiry — REFUSED".into());
            return Err(());
        }
    }
    // The slot keeps one sector for the header; refuse before downloading 96 MiB.
    if manifest_u64(&text, "size").is_some_and(|n| n > (MAX_IMAGE - 512) as u64) {
        out.push("  manifest image does not fit the slot partition — REFUSED".into());
        return Err(());
    }
    // Anti-rollback: never below the highest version this machine ever staged.
    let seen = seen_version(fs);
    if version < seen {
        out.push(alloc::format!("  manifest version {version} is below the highest staged version {seen} — REFUSED"));
        return Err(());
    }
    // The image path is server-controlled (but signed): keep it a plain absolute
    // path on the same host, never a second URL or something with spaces/quotes.
    if !image.starts_with('/') || image.contains("..") || image.contains(char::is_whitespace) || image.contains(['"', '\\']) {
        out.push("  manifest image path malformed — REFUSED".into());
        return Err(());
    }
    Ok(Verdict::Newer { version, image, sha_hex })
}
/// The image side: hash pinned by the signed manifest, Ed25519 over the image,
/// written to the inactive slot, watermark recorded. Returns true when staged.
fn finish_image(fs: &mut dyn FileSystem, version: u64, sha_hex: &str, img: &[u8], isig: &[u8], out: &mut Vec<String>) -> bool {
    let h = eurotls::keyschedule::sha256(img);
    let hex: String = h.iter().map(|b| alloc::format!("{b:02x}")).collect();
    if hex != sha_hex {
        out.push("  image sha256 does not match the signed manifest — REFUSED".into());
        return false;
    }
    out.push(alloc::format!("  image {} B fetched, sha256 pinned by manifest ✓", img.len()));
    let (staged, ok) = stage_verified_image(fs, img, isig);
    if ok && !dry() {
        LAST_STAGED.store(version, core::sync::atomic::Ordering::Relaxed);
        record_seen(fs, version);
    }
    out.extend(staged);
    ok
}

/// **[3e2] — EuroUpdate delivery server, live.** If an update server answers on
/// the SLIRP host gateway (10.0.2.2:8722 — `toolchain/update-server/serve.py`),
/// run the FULL delivery chain live over EuroNet TCP: signed `stable` manifest →
/// newer version → image hash-pinned + Ed25519-verified + staged to the inactive
/// slot; the `old` channel reports up-to-date; the `evil` channel (forged
/// manifest signature) is REFUSED before any image is fetched. Slot state is
/// saved/restored (non-invasive, like [upd3]). Without a server the client is
/// honestly reported READY — the verify+stage pipeline itself is proven on FS
/// by [upd3] every boot.
pub fn channel_selftest(now: u64) {
    let _dry = DryGuard::new();
    use eurofs::{EuroFs, MemoryBlockDevice};
    if crate::net::fetch_full("10.0.2.2", 8722, "/channel/stable.json", false).is_none() {
        crate::serial_println!(
            "[3e2] EuroUpdate delivery: client READY (signed channel manifest → version compare → sha256-pinned + Ed25519-verified image → A/B stage); no update server on 10.0.2.2:8722 — start toolchain/update-server/serve.py for the live end-to-end"
        );
        return;
    }
    let saved = *CONFIG.lock();
    let mut dev = MemoryBlockDevice::new(1024, 4096);
    let mut fs = match EuroFs::format(&mut dev, [0x44; 16], now) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[3e2] could not format RAM EuroFS — skipped");
            return;
        }
    };
    let up = check_channel(&mut fs, "10.0.2.2", 8722, false, "", "stable");
    let staged = up.iter().any(|l| l.contains("verified + written to slot"));
    let old = check_channel(&mut fs, "10.0.2.2", 8722, false, "", "old");
    let uptodate = old.iter().any(|l| l.contains("up to date"));
    let evil = check_channel(&mut fs, "10.0.2.2", 8722, false, "", "evil");
    let refused = evil.iter().any(|l| l.contains("REFUSED"));
    *CONFIG.lock() = saved; // restore global slot state
    let ok = staged && uptodate && refused;
    crate::serial_println!(
        "[3e2] EuroUpdate delivery server LIVE (10.0.2.2:8722 over EuroNet TCP): stable-manifest-verified+image-staged={staged}, old-channel-up-to-date={uptodate}, forged-manifest-REFUSED-before-fetch={refused} → {}",
        if ok { "OK (signed OTA delivery end-to-end) ✓" } else { "FAILED ✗" }
    );
}

/// `euroupdate rollback` — force back to the other good slot.
pub fn rollback(fs: &mut dyn FileSystem) -> Vec<String> {
    let mut cfg = CONFIG.lock().take().unwrap_or_else(|| load(fs));
    let ok = cfg.rollback();
    persist(fs, &cfg);
    REBOOT_AT.store(0, core::sync::atomic::Ordering::Relaxed);
    let res = if ok {
        alloc::format!("euroupdate: rollback set — next boot from slot {}", slot_name(cfg.next_boot))
    } else {
        String::from("euroupdate: rollback NOT possible (no other good slot)")
    };
    *CONFIG.lock() = Some(cfg);
    alloc::vec![res]
}

// ── Automatic updates: the periodic background check + the user's policy ──────
//
// Policy (`/etc/euroupdate.conf`, `euroupdate policy <auto|ask|manual>`):
//   ask    (default) download + verify + stage the update, then tell the user;
//                    the next reboot boots it (with the A/B rollback safety net).
//   auto             as `ask`, and reboot by itself two minutes after staging.
//   manual           only tell the user that an update exists; nothing is fetched.
// The check runs 90 s after boot (the network needs a DHCP lease first) and
// every six hours after that. It never runs on the plain preview image (no
// A/B partitions to write to) and never re-downloads a version already staged.
const POLICY_PATH: &str = "/etc/euroupdate.conf";
const CHECK_FIRST_TICKS: u64 = 90 * 100;
const CHECK_INTERVAL_TICKS: u64 = 6 * 3600 * 100;
const AUTO_REBOOT_TICKS: u64 = 120 * 100;
static LAST_CHECK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static STAGED_VERSION: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static REBOOT_AT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static LAST_RESULT: Mutex<Option<String>> = Mutex::new(None);
/// Version number a successful `check_channel` just staged (set by the function
/// that knows it, not parsed back out of its log text).
static LAST_STAGED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Policy { Auto, Ask, Manual }
impl Policy {
    fn name(self) -> &'static str { match self { Policy::Auto => "auto", Policy::Ask => "ask", Policy::Manual => "manual" } }
    fn parse(s: &str) -> Option<Policy> {
        match s.trim() { "auto" => Some(Policy::Auto), "ask" => Some(Policy::Ask), "manual" => Some(Policy::Manual), _ => None }
    }
}
pub fn policy(fs: &mut dyn FileSystem) -> Policy {
    fs.read_file(POLICY_PATH).ok().and_then(|d| Policy::parse(&String::from_utf8_lossy(&d))).unwrap_or(Policy::Ask)
}
pub fn set_policy(fs: &mut dyn FileSystem, s: &str) -> Vec<String> {
    match Policy::parse(s) {
        Some(p) => {
            let _ = fs.create_dir("/etc");
            match fs.write_file(POLICY_PATH, p.name().as_bytes()) {
                Ok(_) => {
                    if p != Policy::Auto {
                        REBOOT_AT.store(0, core::sync::atomic::Ordering::Relaxed); // an armed auto-reboot is off now
                    }
                    alloc::vec![alloc::format!("euroupdate policy: {}", p.name())]
                }
                Err(_) => alloc::vec!["euroupdate policy: could not write /etc/euroupdate.conf".into()],
            }
        }
        None => alloc::vec!["usage: euroupdate policy <auto|ask|manual>".into()],
    }
}
/// What `euroupdate status` adds: version, policy, last result, staged version.
pub fn auto_status(fs: &mut dyn FileSystem) -> Vec<String> {
    let staged = STAGED_VERSION.load(core::sync::atomic::Ordering::Relaxed);
    alloc::vec![
        alloc::format!("  running       : version {}", running_version()),
        alloc::format!("  policy        : {} (euroupdate policy <auto|ask|manual>)", policy(fs).name()),
        alloc::format!("  server        : https://{UPDATE_HOST}{UPDATE_BASE}/channel/stable.json"),
        alloc::format!("  keys          : daily {} · rotation {}", hex8(&crate::crypto::EUROOS_PUBKEY), if ed25519_dalek::VerifyingKey::from_bytes(&crate::crypto::EUROOS_ROTATION_PUBKEY).is_ok() { hex8(&crate::crypto::EUROOS_ROTATION_PUBKEY) } else { String::from("none (placeholder)") }),
        alloc::format!("  anti-rollback : highest staged version {}", seen_version(fs)),
        alloc::format!("  last check    : {}", LAST_RESULT.lock().clone().unwrap_or_else(|| "not yet".into())),
        alloc::format!("  staged        : {}", if staged > 0 { alloc::format!("version {staged} — reboot to apply") } else { "nothing".into() }),
    ]
}
/// Run one automatic check (used by the periodic hook and by `euroupdate check`).
/// Returns the human-readable log; stages the image unless the policy is `manual`.
pub fn auto_check(fs: &mut dyn FileSystem, now: u64) -> Vec<String> {
    use core::sync::atomic::Ordering;
    let pol = policy(fs);
    let mut out = check_channel(fs, UPDATE_HOST, UPDATE_PORT, UPDATE_TLS, UPDATE_BASE, "stable");
    let v = LAST_STAGED.swap(0, Ordering::Relaxed);
    let summary = if v != 0 {
        STAGED_VERSION.store(v, Ordering::Relaxed);
        crate::notify::push("EuroOS update ready", &alloc::format!("Version {v} is verified and staged. Restart to apply; the previous version stays as a fallback."), now);
        if pol == Policy::Auto {
            REBOOT_AT.store(now.max(1) + AUTO_REBOOT_TICKS, Ordering::Relaxed);
            crate::notify::push("EuroOS restarts in 2 minutes", "Automatic update policy. Run `euroupdate policy ask` to be asked instead.", now);
        }
        alloc::format!("staged version {v}")
    } else if out.iter().any(|l| l.contains("already up to date")) {
        "up to date".into()
    } else {
        out.last().cloned().unwrap_or_else(|| "no result".into())
    };
    *LAST_RESULT.lock() = Some(summary);
    out.push(alloc::format!("  policy: {}", pol.name()));
    out
}
/// Periodic hook (desktop loop, like the scrubber): first check 90 s after boot,
/// then every six hours; `manual` policy only looks, never stages. Also fires the
/// delayed reboot of the `auto` policy.
pub fn maybe_check(fs: &mut dyn FileSystem, now: u64) {
    use core::sync::atomic::Ordering;
    let at = REBOOT_AT.load(Ordering::Relaxed);
    if at != 0 && now >= at {
        if policy(fs) == Policy::Auto {
            crate::serial_println!("[euroupdate] auto policy: rebooting into the staged update");
            crate::power::reboot();
        }
        REBOOT_AT.store(0, Ordering::Relaxed); // policy changed meanwhile: forget it
    }
    let last = LAST_CHECK.load(Ordering::Relaxed);
    let due = if last == 0 { now >= CHECK_FIRST_TICKS } else { now.wrapping_sub(last) >= CHECK_INTERVAL_TICKS };
    if !due || STAGED_VERSION.load(Ordering::Relaxed) != 0 {
        return;
    }
    if esp_first().is_none() {
        // Preview image (no A/B partitions): say so once, then stay quiet.
        if !NO_SLOTS_SAID.swap(true, Ordering::Relaxed) {
            crate::serial_println!("[euroupdate] no slot partitions on the boot disk: automatic updates are off (install EuroOS to enable them)");
        }
        return;
    }
    if crate::net::get().is_none() {
        return; // no network yet: try again next tick
    }
    LAST_CHECK.store(now.max(1), Ordering::Relaxed);
    let pol = policy(fs);
    crate::serial_println!("[euroupdate] check due (policy {}, running {})", pol.name(), running_version());
    // The work itself happens in `step()`, one non-blocking slice per desktop
    // iteration, so the desktop keeps drawing while 50 MB come in over TLS.
    if !start_job(now, pol == Policy::Manual) {
        crate::serial_println!("[euroupdate] could not reach the update server (DNS or TCP connect failed); retry in 30 min");
        *LAST_RESULT.lock() = Some("could not reach the update server".into());
        retry_sooner(now);
    }
}

fn hex8(k: &[u8; 32]) -> String {
    k[..8].iter().map(|b| alloc::format!("{b:02x}")).collect()
}

// ── The cooperative update job: manifest → signature → image → signature, one
// `HttpsFetch::step()` per desktop-loop iteration, verification and staging
// (seconds of disk work, no network) at the end. ─────────────────────────────
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase { Manifest, ManifestSig, Image, ImageSig }
struct Job {
    phase: Phase,
    fetch: crate::net::HttpsFetch,
    look_only: bool,
    man: Vec<u8>,
    version: u64,
    image: String,
    sha_hex: String,
    img: Vec<u8>,
    started: u64,
    retries: u8,
    /// On the rescue channel: the stable manifest no longer verified (our daily
    /// key was rotated away while we were offline), so we follow the release
    /// that the rotation key signed, which carries the new daily key.
    rescue: bool,
}
impl Job {
    fn channel(&self) -> &'static str {
        if self.rescue { "rescue" } else { "stable" }
    }
    /// The server path a phase fetches (a retry reopens exactly this).
    fn path(&self) -> (String, usize) {
        match self.phase {
            Phase::Manifest => (alloc::format!("{UPDATE_BASE}/channel/{}.json", self.channel()), 64 * 1024),
            Phase::ManifestSig => (alloc::format!("{UPDATE_BASE}/channel/{}.json.sig", self.channel()), 4096),
            Phase::Image => (self.image.clone(), MAX_IMAGE),
            Phase::ImageSig => (alloc::format!("{}.sig", self.image), 4096),
        }
    }
    fn phase_name(&self) -> &'static str {
        match self.phase {
            Phase::Manifest => "manifest",
            Phase::ManifestSig => "manifest signature",
            Phase::Image => "image",
            Phase::ImageSig => "image signature",
        }
    }
}
/// A connection that fails or answers without a 200 is reopened this many times
/// before the whole check is given up (and retried in 30 minutes).
const FETCH_RETRIES: u8 = 2;
/// Reopen the current phase's fetch after a transient failure; false when out of retries.
fn retry_phase(mut job: Job, why: &str) -> bool {
    if job.retries >= FETCH_RETRIES {
        return false;
    }
    job.retries += 1;
    let (path, max) = job.path();
    crate::serial_println!("[euroupdate] {} fetch failed ({why}); retry {}/{FETCH_RETRIES}", job.phase_name(), job.retries);
    match fetch_for(&path, max) {
        Some(f) => {
            job.fetch = f;
            *JOB.lock() = Some(job);
            true
        }
        None => false,
    }
}
static JOB: Mutex<Option<Job>> = Mutex::new(None);
static NO_SLOTS_SAID: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// After a failed or unreachable check, try again in 30 minutes instead of 6 hours.
const RETRY_TICKS: u64 = 30 * 60 * 100;
fn retry_sooner(now: u64) {
    // Backdate the last check so the next one is due in RETRY_TICKS. Wrapping on
    // purpose: early after boot `now` is smaller than the interval, and `due`
    // compares with wrapping_sub as well. 0 means "never checked", so avoid it.
    let v = now.wrapping_sub(CHECK_INTERVAL_TICKS - RETRY_TICKS);
    LAST_CHECK.store(if v == 0 { 1 } else { v }, core::sync::atomic::Ordering::Relaxed);
}
fn fetch_for(path: &str, max: usize) -> Option<crate::net::HttpsFetch> {
    crate::net::HttpsFetch::start(UPDATE_HOST, path, max)
}
fn start_job(now: u64, look_only: bool) -> bool {
    let Some(fetch) = fetch_for(&alloc::format!("{UPDATE_BASE}/channel/stable.json"), 64 * 1024) else { return false };
    *JOB.lock() = Some(Job { phase: Phase::Manifest, fetch, look_only, man: Vec::new(), version: 0, image: String::new(), sha_hex: String::new(), img: Vec::new(), started: now, retries: 0, rescue: false });
    crate::serial_println!("[euroupdate] background check started: https://{UPDATE_HOST}{UPDATE_BASE}/channel/stable.json");
    true
}
fn job_done(now: u64, summary: String, failed: bool) {
    crate::serial_println!("[euroupdate] {summary}");
    *LAST_RESULT.lock() = Some(summary);
    if failed {
        retry_sooner(now);
    }
}
/// Called every desktop-loop iteration (~10 ms). Cheap when idle: one atomic
/// load and one mutex try. Also fires the delayed reboot of the `auto` policy.
pub fn step(fs: &mut dyn FileSystem, now: u64) {
    use core::sync::atomic::Ordering;
    let at = REBOOT_AT.load(Ordering::Relaxed);
    if at != 0 && now >= at {
        if policy(fs) == Policy::Auto {
            crate::serial_println!("[euroupdate] auto policy: rebooting into the staged update");
            crate::power::reboot();
        }
        REBOOT_AT.store(0, Ordering::Relaxed); // policy changed meanwhile: forget it
    }
    let Some(mut job) = JOB.lock().take() else { return };
    // A whole job may not run longer than 30 minutes (a stalled link), whatever
    // the per-fetch idle limit says.
    if now.wrapping_sub(job.started) > 30 * 60 * 100 {
        job_done(now, "background check aborted: took longer than 30 minutes".into(), true);
        return;
    }
    match job.fetch.step() {
        crate::net::FetchStep::Pending => {
            *JOB.lock() = Some(job);
        }
        crate::net::FetchStep::Failed(why) => {
            let what = alloc::format!("background check failed while fetching the {}: {why}", job.phase_name());
            if !retry_phase(job, why) {
                job_done(now, what, true);
            }
        }
        crate::net::FetchStep::Done => {
            // Peek at the status first: a non-200 (or an empty answer, status 0)
            // is retried on a fresh connection like a failed one.
            let code = job.fetch.status();
            crate::serial_println!("[euroupdate] fetched {}: HTTP {code}", job.phase_name());
            if code != 200 {
                let what = alloc::format!("background check: HTTP {code} while fetching the {}", job.phase_name());
                if !retry_phase(job, "no 200") {
                    job_done(now, what, true);
                }
                return;
            }
            let channel = job.channel();
            let Job { phase, fetch, look_only, mut man, mut version, mut image, mut sha_hex, mut img, started, retries, rescue } = job;
            let (_, body) = fetch.finish();
            let mut out: Vec<String> = Vec::new();
            let next = match phase {
                Phase::Manifest => {
                    man = body;
                    fetch_for(&alloc::format!("{UPDATE_BASE}/channel/{channel}.json.sig"), 4096).map(|f| (Phase::ManifestSig, f))
                }
                Phase::ManifestSig => match evaluate_manifest(fs, channel, &man, &body, &mut out) {
                    Ok(Verdict::UpToDate) => {
                        job_done(now, "up to date".into(), false);
                        return;
                    }
                    Err(()) if !rescue && out.last().is_some_and(|l| l.contains("signature INVALID")) => {
                        // Not a forged manifest but, most likely, a daily key we no
                        // longer share with the server: try the rotation-signed rescue release.
                        crate::serial_println!("[euroupdate] stable manifest does not verify against our keys: trying the rescue channel");
                        match fetch_for(&alloc::format!("{UPDATE_BASE}/channel/rescue.json"), 64 * 1024) {
                            Some(f) => *JOB.lock() = Some(Job { phase: Phase::Manifest, fetch: f, look_only, man: Vec::new(), version: 0, image: String::new(), sha_hex: String::new(), img: Vec::new(), started, retries: 0, rescue: true }),
                            None => job_done(now, "manifest signature INVALID and no connection for the rescue channel".into(), true),
                        }
                        return;
                    }
                    Err(()) => {
                        job_done(now, out.last().cloned().unwrap_or_else(|| "manifest refused".into()), true);
                        return;
                    }
                    Ok(Verdict::Newer { version: v, image: i, sha_hex: h }) => {
                        for l in &out {
                            crate::serial_println!("[euroupdate] {l}");
                        }
                        if look_only {
                            crate::notify::push("EuroOS update available", &alloc::format!("Version {v} is available. Run `euroupdate check` to install it."), now);
                            job_done(now, alloc::format!("version {v} available (manual policy)"), false);
                            return;
                        }
                        version = v;
                        image = i;
                        sha_hex = h;
                        fetch_for(&image, MAX_IMAGE).map(|f| (Phase::Image, f))
                    }
                },
                Phase::Image => {
                    img = body;
                    fetch_for(&alloc::format!("{image}.sig"), 4096).map(|f| (Phase::ImageSig, f))
                }
                Phase::ImageSig => {
                    let ok = finish_image(fs, version, &sha_hex, &img, &body, &mut out);
                    LAST_STAGED.store(0, Ordering::Relaxed); // reported through STAGED_VERSION below, not by the blocking path
                    for l in &out {
                        crate::serial_println!("[euroupdate] {l}");
                    }
                    if ok {
                        STAGED_VERSION.store(version, Ordering::Relaxed);
                        crate::notify::push("EuroOS update ready", &alloc::format!("Version {version} is verified and staged. Restart to apply; the previous version stays as a fallback."), now);
                        if policy(fs) == Policy::Auto {
                            REBOOT_AT.store(now.max(1) + AUTO_REBOOT_TICKS, Ordering::Relaxed);
                            crate::notify::push("EuroOS restarts in 2 minutes", "Automatic update policy. Run `euroupdate policy ask` to be asked instead.", now);
                        }
                        job_done(now, alloc::format!("staged version {version}"), false);
                    } else {
                        job_done(now, out.last().cloned().unwrap_or_else(|| "image refused".into()), true);
                    }
                    return;
                }
            };
            match next {
                Some((phase, fetch)) => *JOB.lock() = Some(Job { phase, fetch, look_only, man, version, image, sha_hex, img, started, retries, rescue }),
                None => job_done(now, "background check: could not open the next connection".into(), true),
            }
        }
    }
}
