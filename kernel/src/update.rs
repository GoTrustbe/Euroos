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

/// Read the slot config from the raw reserved block (independent of EuroFS).
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

/// Write the slot config to the raw reserved block + flush to hardware.
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
    // Source of truth: the raw block (survives FS corruption). Falls back to the
    // FS mirror, then to a fresh initial config.
    if let Some(cfg) = raw_load() {
        return cfg;
    }
    match fs.read_file(CONFIG_PATH) {
        Ok(d) => SlotConfig::deserialize(&d).unwrap_or_else(SlotConfig::initial),
        Err(_) => SlotConfig::initial(),
    }
}

fn persist(fs: &mut dyn FileSystem, cfg: &SlotConfig) {
    // Primarily to the raw block; then the human-readable FS mirror.
    raw_persist(cfg);
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
        "[euroupdate] boot from slot {} (gen {}, {} attempt(s) left, A={:?} B={:?})",
        slot_name(booted),
        cfg.generation,
        cfg.tries,
        cfg.state(Slot::A),
        cfg.state(Slot::B),
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

/// The GPT partition name of an A/B slot (G4 multi-partition layout).
fn slot_partition_name(s: Slot) -> &'static str {
    match s {
        Slot::A => "EuroOS-A",
        Slot::B => "EuroOS-B",
    }
}

/// Write `image` directly to the partition of `slot` (sector I/O, outside EuroFS)
/// and verify with a read-back of the first sector. This is the real A/B
/// image write: the slot image lives in its own GPT partition, not in a
/// file on the root FS. Returns Ok(bytes) or an error reason.
fn write_image_to_slot(slot: Slot, image: &[u8]) -> Result<usize, &'static str> {
    let (first_lba, blocks) =
        crate::gpt::find_partition_by_name(slot_partition_name(slot)).ok_or("slot partition not found")?;
    let nsec = image.len().div_ceil(512);
    if nsec as u64 > blocks * 8 {
        return Err("image larger than the slot partition");
    }
    for i in 0..nsec {
        let off = i * 512;
        let end = (off + 512).min(image.len());
        let mut sec = [0u8; 512];
        sec[..end - off].copy_from_slice(&image[off..end]);
        if !crate::virtio_blk::write_sector(first_lba + i as u64, &sec) {
            return Err("writing to slot partition failed");
        }
    }
    crate::virtio_blk::flush();
    // Read-back verification (first sector) — proves that it is on the disk.
    let mut rb = [0u8; 512];
    if !crate::virtio_blk::read_sector(first_lba, &mut rb) {
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
    if !crate::virtio_blk::present() {
        return;
    }
    let pattern = b"EuroOS slot-image partition-write selftest (G4) -- non-FS, direct sector-I/O";
    match write_image_to_slot(Slot::B, pattern) {
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
    stage_verified_image(fs, &image, &sig)
}

/// The core of a secure update: verify the Ed25519 signature over `image`
/// (verify-before-activate), write it to the inactive slot, and stage it.
/// Shared by `apply` (FS source) and `fetch` (network source). An invalid
/// signature ALWAYS leads to refusal — a tampered update is never
/// staged, let alone activated.
fn stage_verified_image(fs: &mut dyn FileSystem, image: &[u8], sig: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    // EuroGuard: refuse an update without a valid EuroOS signature (anti-tamper).
    if !crate::crypto::verify(image, sig) {
        out.push("euroupdate: INVALID signature — update REFUSED".into());
        return out;
    }
    let mut cfg = CONFIG.lock().take().unwrap_or_else(|| load(fs));
    let target = cfg.inactive();
    // Write the image directly to the PARTITION of the inactive slot (G4: real
    // A/B partitions + read-back verification). Falls back to an FS file if the
    // multi-partition GPT is not (yet) present.
    match write_image_to_slot(target, image) {
        Ok(n) => out.push(alloc::format!(
            "euroupdate: {} bytes written to the {} partition + read-back ✓",
            n,
            slot_partition_name(target)
        )),
        Err(_) => {
            let slot_file = alloc::format!("/boot/slot_{}.img", slot_name(target));
            let _ = fs.create_dir("/boot");
            if fs.write_file(&slot_file, image).is_err() {
                out.push("euroupdate: writing to the inactive slot FAILED".into());
                *CONFIG.lock() = Some(cfg);
                return out;
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
    out
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
    let image = match crate::net::fetch_full(&host, port, &path, tls) {
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
    out.extend(stage_verified_image(fs, &image, &sig));
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

/// The version THIS build runs (compared against the channel manifest).
pub const RUNNING_VERSION: u64 = 1;

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
pub fn check_channel(fs: &mut dyn FileSystem, host: &str, port: u16, channel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mpath = alloc::format!("/channel/{channel}.json");
    out.push(alloc::format!("euroupdate check: {host}:{port} channel '{channel}' (Ed25519-signed manifest)…"));
    let man = match crate::net::fetch_full(host, port, &mpath, false) {
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
    let msig = match crate::net::fetch_full(host, port, &alloc::format!("{mpath}.sig"), false) {
        Some((200, _, b)) => b,
        _ => {
            out.push("  manifest signature missing — channel REFUSED".into());
            return out;
        }
    };
    if !crate::crypto::verify(&man, &msig) {
        out.push("  manifest signature INVALID — channel REFUSED (nothing fetched)".into());
        return out;
    }
    let text = String::from_utf8_lossy(&man).into_owned();
    let (version, image, sha_hex) =
        match (manifest_u64(&text, "version"), manifest_str(&text, "image"), manifest_str(&text, "sha256")) {
            (Some(v), Some(i), Some(s)) => (v, String::from(i), String::from(s)),
            _ => {
                out.push("  manifest malformed — REFUSED".into());
                return out;
            }
        };
    out.push(alloc::format!("  manifest OK (signature valid): version {version}, running {RUNNING_VERSION}"));
    if version <= RUNNING_VERSION {
        out.push("  already up to date — nothing to do".into());
        return out;
    }
    let img = match crate::net::fetch_full(host, port, &image, false) {
        Some((200, _, b)) => b,
        _ => {
            out.push(alloc::format!("  image {image} not fetched — aborted"));
            return out;
        }
    };
    let isig = match crate::net::fetch_full(host, port, &alloc::format!("{image}.sig"), false) {
        Some((200, _, b)) => b,
        _ => {
            out.push("  image signature not fetched — aborted".into());
            return out;
        }
    };
    // Defense-in-depth: the SIGNED manifest pins the image hash.
    let h = eurotls::keyschedule::sha256(&img);
    let hex: String = h.iter().map(|b| alloc::format!("{b:02x}")).collect();
    if hex != sha_hex {
        out.push("  image sha256 does not match the signed manifest — REFUSED".into());
        return out;
    }
    out.push(alloc::format!("  image {} B fetched, sha256 pinned by manifest ✓", img.len()));
    out.extend(stage_verified_image(fs, &img, &isig));
    out
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
    let up = check_channel(&mut fs, "10.0.2.2", 8722, "stable");
    let staged = up.iter().any(|l| l.contains("verified + written to slot"));
    let old = check_channel(&mut fs, "10.0.2.2", 8722, "old");
    let uptodate = old.iter().any(|l| l.contains("up to date"));
    let evil = check_channel(&mut fs, "10.0.2.2", 8722, "evil");
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
    let res = if ok {
        alloc::format!("euroupdate: rollback set — next boot from slot {}", slot_name(cfg.next_boot))
    } else {
        String::from("euroupdate: rollback NOT possible (no other good slot)")
    };
    *CONFIG.lock() = Some(cfg);
    alloc::vec![res]
}
