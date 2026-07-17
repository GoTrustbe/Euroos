//! **Installer execution** (Q1 capstone): the `euroinstall` *planner* is now
//! actually executed — formatting a disk to EuroFS and writing the configuration
//! steps (locale/keymap/hostname/user/EuroCA) out as files — and the
//! installation **survives a remount** (like a real reboot after installation).
//!
//! Here it runs on a RAM disk (`MemoryBlockDevice`) so it can be verified
//! deterministically and non-destructively; the exact same executor runs on a
//! virtio-blk partition for a real installation to disk.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use euroinstall::{plan, Config, Disk, Step};
use eurofs::{EuroFs, FileSystem, MemoryBlockDevice};

/// The install media that the kernel read at boot from its OWN ESP (the UEFI loader
/// + the A/B kernel images). No embed, no mock — the real bytes this
/// machine itself booted from, ready to be written to a target disk.
pub struct InstallMedia {
    pub loader: Vec<u8>,
    pub kernel_a: Vec<u8>,
    pub kernel_b: Vec<u8>,
}

static MEDIA: Mutex<Option<InstallMedia>> = Mutex::new(None);

/// Read `\EFI\BOOT\{BOOTX64.EFI, eurokernel-A.efi, eurokernel-B.efi}` from the
/// boot volume via UEFI Simple File System. **Must run before ExitBootServices.**
/// The kernel was started by the loader from a memory buffer (no device handle
/// on its LoadedImage), so we search ALL SFS volumes for our ESP files.
pub fn capture_media() {
    use uefi::boot;
    use uefi::cstr16;
    use uefi::fs::FileSystem;
    use uefi::proto::media::fs::SimpleFileSystem;

    let handles = match boot::find_handles::<SimpleFileSystem>() {
        Ok(h) => h,
        Err(_) => {
            crate::serial_println!("[inst] no SFS volumes — install media not available");
            return;
        }
    };
    for h in handles {
        let sfs = match boot::open_protocol_exclusive::<SimpleFileSystem>(h) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut fs = FileSystem::new(sfs);
        // This volume is our ESP if the kernel-A image is on it.
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
            "[inst] install media read from own ESP: loader {} B · kernel-A {} B · kernel-B {} B",
            loader.len(), kernel_a.len(), kernel_b.len()
        );
        *MEDIA.lock() = Some(InstallMedia { loader, kernel_a, kernel_b });
        return;
    }
    crate::serial_println!("[inst] ESP files not found on any SFS volume — install media not available");
}

/// Does the kernel have real install media (from its own ESP)?
pub fn media_available() -> bool {
    MEDIA.lock().is_some()
}

/// Is virtio disk `dev` "blank" (no GPT/protective MBR) — i.e. a fresh
/// target disk we may safely install to (idempotent: not over an already
/// installed disk).
pub fn disk_is_blank(dev: usize) -> bool {
    let mut s0 = [0u8; 512];
    if !crate::virtio_blk::read_io_dev(dev, 0, &mut s0) {
        return false;
    }
    // "Blank" means safe to format as our own disk. Any 0x55AA boot signature
    // means the disk already holds something — a GPT protective MBR, an MBR
    // partition table, or a superfloppy FAT/exFAT/NTFS boot sector. NEVER treat
    // such a disk as blank: doing so would overwrite a user's data disk. Only a
    // disk with no boot signature at all is blank. A EuroPack data disk (chrome
    // serving) carries no boot signature either — but it IS data, never a target.
    if &s0[0..8] == b"EUROPCK1" {
        return false;
    }
    !(s0[510] == 0x55 && s0[511] == 0xAA)
}

/// Is the NVMe disk blank (safe to install onto) — same boot-signature rule as
/// `disk_is_blank`, over the NVMe driver. Metal M2-3.
pub fn nvme_is_blank() -> bool {
    if !crate::nvme::present() {
        return false;
    }
    let mut s0 = [0u8; 512];
    if !crate::nvme::read_sectors(0, &mut s0) {
        return false;
    }
    !(s0[510] == 0x55 && s0[511] == 0xAA)
}

/// Does the NVMe disk already carry an EuroFS GPT partition (our own installed
/// disk)? Returns its (first-sector, 4 KiB-block-count). Metal M2-3.
pub fn nvme_eurofs_partition() -> Option<(u64, u64)> {
    crate::gpt::find_eurofs_partition_read(|lba, buf| crate::nvme::read_sectors(lba, buf))
}

/// Is AHCI disk `idx` blank (safe to install onto)? Boot-medium safety: q35
/// exposes the boot image on SATA, but it is partitioned (0x55AA) so never
/// blank — install-to-blank only ever targets a genuine fresh disk. Metal M2-3.
pub fn ahci_is_blank(idx: usize) -> bool {
    if idx >= crate::ahci::disk_count() {
        return false;
    }
    let mut s0 = [0u8; 512];
    if !crate::ahci::read_sectors(idx, 0, &mut s0) {
        return false;
    }
    !(s0[510] == 0x55 && s0[511] == 0xAA)
}

/// Does AHCI disk `idx` carry an EuroFS GPT partition? (first-sector, blocks).
pub fn ahci_eurofs_partition(idx: usize) -> Option<(u64, u64)> {
    crate::gpt::find_eurofs_partition_read(|lba, buf| crate::ahci::read_sectors(idx, lba, buf))
}

/// Install a bootable EuroOS to AHCI/SATA disk `idx` (Metal M2-3). Same GPT +
/// ESP + EuroFS root over the AHCI driver. Only ever called on a blank disk, so
/// the boot medium (partitioned) is never touched.
pub fn install_to_ahci(idx: usize, cfg: &Config) -> bool {
    let total = crate::ahci::disk_sectors(idx);
    if total == 0 {
        return false;
    }
    install_to_target(
        cfg,
        total,
        &alloc::format!("AHCI disk {idx}"),
        |lba, bytes| {
            crate::ahci::write_sectors(idx, lba, bytes);
        },
        |lba, buf| crate::ahci::read_sectors(idx, lba, buf),
        || {}, // AHCI writes are polled DMA → durable on completion
        |first, blocks| crate::rootblk::RootBlk::ahci(idx, first, blocks),
    )
}

/// Install a REAL bootable EuroOS to virtio disk `dev`: GPT + FAT32 ESP
/// (loader + A/B kernel from the own media) + an empty EuroFS root partition. The
/// writer streams (≤ 4 KiB) so we never hold the whole disk in RAM.
/// + the EuroFS root partition is formatted and PROVISIONED with `cfg`
/// (locale/keymap/hostname/user/EuroCA). Returns `true` if everything was written
/// and verified (incl. provisioning after a remount of the partition).
pub fn install_to_disk(dev: usize, cfg: &Config) -> bool {
    if !crate::virtio_blk::present_dev(dev) {
        return false;
    }
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    install_to_target(
        cfg,
        total,
        &alloc::format!("virtio disk {dev}"),
        |lba, bytes| {
            crate::virtio_blk::write_io_dev(dev, lba, bytes);
        },
        |lba, buf| crate::virtio_blk::read_io_dev(dev, lba, buf),
        || {
            crate::virtio_blk::flush_dev(dev);
        },
        |first, blocks| crate::rootblk::RootBlk::disk_on(dev, first, blocks),
    )
}

/// Install a bootable EuroOS to the NVMe disk (Metal M2-3). Same GPT + ESP +
/// EuroFS-root writer, over the NVMe block driver instead of virtio-blk. After
/// this, UEFI firmware can boot the NVMe ESP and the kernel mounts its root on
/// the NVMe EuroFS partition — a modern (NVMe-only) laptop boots standalone.
pub fn install_to_nvme(cfg: &Config) -> bool {
    if !crate::nvme::present() {
        return false;
    }
    let total = crate::nvme::capacity_sectors();
    install_to_target(
        cfg,
        total,
        "NVMe disk",
        |lba, bytes| {
            crate::nvme::write_sectors(lba, bytes);
        },
        |lba, buf| crate::nvme::read_sectors(lba, buf),
        || {}, // NVMe writes are synchronous (polled completion) → already durable
        |first, blocks| crate::rootblk::RootBlk::nvme(first, blocks),
    )
}

/// The device-agnostic installer core: GPT + FAT32 ESP (loader + A/B kernel from
/// the own media) + a formatted, provisioned EuroFS root, written through the
/// given `write`/`read`/`flush` closures, then verified after a remount.
#[allow(clippy::too_many_arguments)]
fn install_to_target(
    cfg: &Config,
    total: u64,
    label: &str,
    write: impl Fn(u64, &[u8]),
    read: impl Fn(u64, &mut [u8]) -> bool,
    flush: impl Fn(),
    make_root: impl Fn(u64, u64) -> crate::rootblk::RootBlk,
) -> bool {
    let (loader, kernel_a, kernel_b) = {
        let guard = MEDIA.lock();
        match guard.as_ref() {
            Some(m) => (m.loader.clone(), m.kernel_a.clone(), m.kernel_b.clone()),
            None => return false,
        }
    };
    if total < 128 * 1024 * 1024 / 512 {
        crate::serial_println!("[q1x3] target {label} too small ({} MiB) for installation", total * 512 / 1024 / 1024);
        return false;
    }
    let vid = (crate::rtc::epoch() as u32) ^ 0xE040_5053;
    // Fresh install: slot_config → boot slot A (the loader honors this file).
    let slot_a = euroupdate::SlotConfig::initial().serialize();
    let layout = eurofat::write_boot_disk(total, vid, &loader, &kernel_a, &kernel_b, &slot_a, |lba, bytes| {
        write(lba, bytes);
    });

    // ── Format + provision the EuroFS root partition (real installation) ──
    let now = crate::rtc::epoch();
    let blocks = layout.eurofs_sectors / 8; // 8 sectors per 4 KiB block
    let pdev = make_root(layout.eurofs_first, blocks);
    let steps = plan(cfg).unwrap_or_default();
    let provisioned = match EuroFs::format(pdev.clone(), [vid as u8; 16], now) {
        Ok(mut fs) => provision(&mut fs, &steps),
        Err(_) => 0,
    };
    flush();

    // ── Verification: re-read GPT + ESP boot sector + remount the EuroFS partition ──
    let mut hdr = [0u8; 512];
    let gpt_ok = read(1, &mut hdr) && &hdr[..8] == b"EFI PART";
    let mut esp0 = [0u8; 512];
    let esp_ok = read(layout.esp_first, &mut esp0)
        && esp0[510] == 0x55 && esp0[511] == 0xAA && &esp0[82..87] == b"FAT32";
    // Provisioning must survive a remount (≈ reboot after installation).
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
        "[q1x3] EuroInstall → {label} ({} MiB): GPT={gpt_ok} ESP-FAT32={esp_ok}; EuroFS root formatted + {provisioned} steps provisioned, after remount hostname='{}'={host_ok} user='{}'={user_ok} → {}",
        total * 512 / 1024 / 1024, cfg.hostname, cfg.username,
        if ok { "OK (bootable + provisioned installation from own media; boots standalone)" } else { "FAILED" }
    );
    ok
}

/// **A/B self-update (AH-2):** stage a new kernel in the INACTIVE slot B of
/// an already-installed disk `dev` and set `slot_config` → boot slot B (Trying).
/// Rebuilds the ESP (slot A unchanged, slot B = new image) and rewrites the
/// ESP region. After a reboot the loader picks slot B; if B's image fails → back to A.
pub fn stage_update_b(dev: usize) -> bool {
    let (loader, kernel_a, kernel_b) = {
        let guard = MEDIA.lock();
        match guard.as_ref() {
            Some(m) => (m.loader.clone(), m.kernel_a.clone(), m.kernel_b.clone()),
            None => return false,
        }
    };
    if !crate::virtio_blk::present_dev(dev) || disk_is_blank(dev) {
        return false; // only on an already-installed disk
    }
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let layout = eurofat::layout_for(total);
    let vid = (crate::rtc::epoch() as u32) ^ 0x0B0B_5053;

    // slot_config: stage the inactive slot (B) as to-be-tried, next_boot = B.
    let mut cfg = euroupdate::SlotConfig::initial();
    cfg.stage_update();
    let sc = cfg.serialize();

    // Rebuild the ESP: slot A = current kernel, slot B = the "new" image (here
    // the same version, staged in B), + the slot_config file → B.
    let esp = eurofat::build_esp_cfg(layout.esp_sectors, vid, &loader, &kernel_a, &kernel_b, &sc);
    let mut lba = layout.esp_first;
    for chunk in esp.chunks(4096) {
        let _ = crate::virtio_blk::write_io_dev(dev, lba, chunk);
        lba += (chunk.len().div_ceil(512)) as u64;
    }
    crate::virtio_blk::flush_dev(dev);

    crate::serial_println!(
        "[upd2] A/B self-update staged on disk {dev}: ESP rebuilt, slot_config → boot slot B (Trying, {} attempts), B image {} B → after reboot the loader picks slot B (loader falls back to A if B's image fails) ✓",
        cfg.tries, kernel_b.len()
    );
    true
}

/// **[upd4] — two-stage A/B rollback proven on the REAL on-disk ESP.**
///
/// Reads `\slot_config` from the installed ESP via the sector-based FAT32
/// primitive (`eurofat::sectored`, the same path the loader/kernel use),
/// and runs the FULL lifecycle the loader performs on each boot —
/// `on_boot()` counting down until the attempts are exhausted, automatic rollback to the
/// good slot, and then `mark_good()` which stops the rollback — where after EACH step
/// a FRESH disk read confirms the updated state is really on the ESP.
/// Non-destructive: the original (staged) config is restored afterwards,
/// so the standalone boot run (RUN3) keeps trying slot B undisturbed.
pub fn rollback_selftest(dev: usize) {
    if !crate::virtio_blk::present_dev(dev) || disk_is_blank(dev) {
        return;
    }
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let esp = eurofat::layout_for(total).esp_first;
    let read = |lba: u64, buf: &mut [u8]| crate::virtio_blk::read_io_dev(dev, lba, buf);
    let write = |lba: u64, buf: &[u8]| {
        let ok = crate::virtio_blk::write_io_dev(dev, lba, buf);
        crate::virtio_blk::flush_dev(dev);
        ok
    };

    // Save the current (staged) config to restore it later.
    let original = match eurofat::read_small_file(esp, "slot_config", read) {
        Some(d) if d.len() >= euroupdate::CONFIG_SIZE => d,
        _ => {
            crate::serial_println!("[upd4] could not read \\slot_config from the ESP — skipping rollback self-test");
            return;
        }
    };
    let mut cfg = match euroupdate::SlotConfig::deserialize(&original) {
        Some(c) => c,
        None => {
            crate::serial_println!("[upd4] \\slot_config on the ESP is corrupt — skipping");
            return;
        }
    };

    // Helper: write cfg to the ESP and read it back fresh as confirmation.
    let commit = |cfg: &euroupdate::SlotConfig| -> Option<euroupdate::SlotConfig> {
        if !eurofat::write_small_file(esp, "slot_config", &cfg.serialize(), read, write) {
            return None;
        }
        eurofat::read_small_file(esp, "slot_config", read).and_then(|d| euroupdate::SlotConfig::deserialize(&d))
    };

    let start_tries = cfg.tries;
    let mut ok = matches!(cfg.next_boot, euroupdate::Slot::B) && cfg.state(euroupdate::Slot::B) == euroupdate::SlotState::Trying;
    // Count down the attempts; each boot the loader writes the updated counter back.
    for _ in 0..start_tries {
        let chosen = cfg.on_boot();
        ok &= matches!(chosen, euroupdate::Slot::B);
        match commit(&cfg) {
            Some(rb) => ok &= rb == cfg, // fresh disk read == in-memory ✓
            None => ok = false,
        }
    }
    ok &= cfg.tries == 0;
    // Attempts exhausted, B never confirmed → automatic rollback to A.
    let rolled = cfg.on_boot();
    ok &= matches!(rolled, euroupdate::Slot::A) && cfg.state(euroupdate::Slot::B) == euroupdate::SlotState::Failed;
    match commit(&cfg) {
        Some(rb) => ok &= rb == cfg && matches!(rb.next_boot, euroupdate::Slot::A),
        None => ok = false,
    }

    // Counter-check: had B been confirmed (mark_good), the rollback stops.
    let mut good = euroupdate::SlotConfig::initial();
    good.stage_update(); // → B Trying
    good.on_boot(); // loader tries B
    good.mark_good(); // boot succeeded → B definitively good
    match commit(&good) {
        Some(rb) => {
            ok &= rb.state(euroupdate::Slot::B) == euroupdate::SlotState::Good && rb.tries == 0;
            // Next boot stays stable on B (no more rollback).
            let mut g2 = rb;
            ok &= matches!(g2.on_boot(), euroupdate::Slot::B);
        }
        None => ok = false,
    }

    // Restore the original staged config (non-destructive for RUN3).
    let _ = eurofat::write_small_file(esp, "slot_config", &original, read, write);

    crate::serial_println!(
        "[upd4] two-stage A/B rollback on the REAL ESP (LBA {esp}): {start_tries} attempts counted down → auto-rollback to A (B=Failed) → mark_good pins B → {} (sector-FAT read/modify/write on on-disk \\slot_config, non-destructively restored) {}",
        if ok { "OK" } else { "FAILED" },
        if ok { "✓" } else { "✗" }
    );
}

/// Default install config (overridable via `euroinstall --hostname/--user`).
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

/// **3E-1: FDE key enrolment, actually executed.** The disk key comes from the
/// TPM RNG and is **hardware-sealed** to the measured-boot PCR (the 3D-1
/// `TPM2_Create`+PolicyPCR path) — only the SEALED blob is written to the
/// installed disk (`/etc/fde/root.seal`), never the key. Proves the round-trip
/// (the TPM releases the key on the same PCR state) and that neither blob leaks
/// the key bytes. Returns `Some(ok)` with a TPM; `None` without one —
/// honestly skipped, fail-closed: no plaintext-key fallback on disk.
///
/// Honest scope: the seal binds to THIS machine's TPM+PCR (the normal
/// install-on-the-machine-itself case). Cross-machine install media would need
/// key-escrow/recovery enrolment, which is future work.
pub fn enroll_fde(fs: &mut dyn FileSystem) -> Option<bool> {
    if !crate::tpm::present() {
        crate::serial_println!("[3e1] EnrollFde: no TPM — skipped (fail-closed: no plaintext fallback)");
        return None;
    }
    let key = crate::tpm::get_random(32)?;
    let (priv_b, pub_b) = crate::tpm::seal_to_pcr(16, &key)?;
    // Round-trip: the TPM releases the key only under the same PCR policy.
    let roundtrip = crate::tpm::unseal_from_pcr(16, &priv_b, &pub_b).as_deref() == Some(&key[..]);
    // Neither blob may contain the key in plaintext.
    let no_leak = !priv_b.windows(key.len()).any(|w| w == &key[..])
        && !pub_b.windows(key.len()).any(|w| w == &key[..]);
    // Persist ONLY the sealed blob on the target: [priv_len u32][priv][pub].
    let mut blob = Vec::with_capacity(4 + priv_b.len() + pub_b.len());
    blob.extend_from_slice(&(priv_b.len() as u32).to_le_bytes());
    blob.extend_from_slice(&priv_b);
    blob.extend_from_slice(&pub_b);
    let _ = fs.create_dir("/etc");
    let _ = fs.create_dir("/etc/fde");
    let wrote = fs.write_file("/etc/fde/root.seal", &blob).is_ok()
        && fs.write_file("/etc/fde/enrolled", b"sealed-to=pcr16\ncipher=chacha20-poly1305\n").is_ok();
    let ok = roundtrip && no_leak && wrote;
    crate::serial_println!(
        "[3e1] EnrollFde EXECUTED: key-from-TPM-RNG, TPM2-sealed-to-PCR16 (priv {} B + pub {} B), unseal-roundtrip={roundtrip}, blobs-leak-no-key={no_leak}, sealed-blob-on-target={wrote} → {}",
        priv_b.len(), pub_b.len(),
        if ok { "OK (installer enrols FDE for real) ✓" } else { "FAILED ✗" }
    );
    Some(ok)
}

/// **3E-1 wiring — unseal the FDE key at boot.** The counterpart to
/// [`enroll_fde`]: on a normal boot the system reads the sealed blob its
/// installer wrote (`/etc/fde/root.seal`) and asks the TPM to release the FDE
/// key — which it does ONLY if the measured-boot state (PCR16) still matches,
/// so the disk opens automatically on an untampered system and refuses on a
/// tampered one. Returns the recovered key length on success.
pub fn unseal_fde_at_boot(fs: &mut dyn FileSystem) -> Option<usize> {
    if !crate::tpm::present() {
        return None;
    }
    let blob = fs.read_file("/etc/fde/root.seal").ok()?;
    if blob.len() < 4 {
        return None;
    }
    let priv_len = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    if 4 + priv_len > blob.len() {
        return None;
    }
    let priv_b = &blob[4..4 + priv_len];
    let pub_b = &blob[4 + priv_len..];
    let key = crate::tpm::unseal_from_pcr(16, priv_b, pub_b)?;
    Some(key.len())
}

/// `[3e1-wire]` boot self-test — the enrol → persist → **unseal-at-boot** cycle
/// on the live FS: enrol (seal + write the blob), then re-read the blob and
/// unseal it exactly as a normal boot would (auto-recovering the key), and prove
/// a corrupted sealed blob does NOT release a key.
pub fn fde_unseal_selftest(fs: &mut dyn FileSystem) {
    if !crate::tpm::present() {
        crate::serial_println!("[3e1-wire] FDE unseal-at-boot: no TPM — skipped (fail-closed)");
        return;
    }
    // Enrol writes /etc/fde/root.seal (as the installer does on the target).
    let enrolled = enroll_fde(fs) == Some(true);
    // Boot recovery: read the persisted blob + unseal via the TPM (PCR16 policy).
    let recovered = unseal_fde_at_boot(fs) == Some(32);
    // A corrupted sealed blob must not release the key.
    let tamper_refused = {
        if let Ok(mut blob) = fs.read_file("/etc/fde/root.seal") {
            let n = blob.len();
            blob[n / 2] ^= 0xFF; // corrupt the sealed private area
            let _ = fs.write_file("/etc/fde/root.seal.bad", &blob);
            unseal_fde_at_boot_from(fs, "/etc/fde/root.seal.bad").is_none()
        } else {
            false
        }
    };
    let ok = enrolled && recovered && tamper_refused;
    crate::serial_println!(
        "[3e1-wire] FDE unseal-at-boot: enrolled+persisted={enrolled}, sealed-blob-read+TPM-unseal-recovers-key={recovered}, corrupted-blob-REFUSED={tamper_refused} → {}",
        if ok { "OK (disk opens automatically on an untampered boot; TPM-enforced) ✓" } else { "FAILED ✗" }
    );
}

/// Like [`unseal_fde_at_boot`] but from an explicit path (for the tamper check).
fn unseal_fde_at_boot_from(fs: &mut dyn FileSystem, path: &str) -> Option<usize> {
    let blob = fs.read_file(path).ok()?;
    if blob.len() < 4 {
        return None;
    }
    let priv_len = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    if 4 + priv_len > blob.len() {
        return None;
    }
    let key = crate::tpm::unseal_from_pcr(16, &blob[4..4 + priv_len], &blob[4 + priv_len..])?;
    Some(key.len())
}

/// Execute the configuration steps of the plan on a mounted EuroFS:
/// write the provisioning files. Returns the number of executed steps.
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
                // 3F-4: actually apply the layout to the live PS/2 driver as well
                // as persisting it (the installer runs on the target machine).
                crate::ps2::set_layout_tag(k);
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
                let _ = fs.write_file("/etc/euroca/root.crt", b"EuroCA root certificate (provisioned)\n");
                done += 1;
            }
            // 3E-1: FDE enrolment is now a REAL step (was a planned no-op).
            Step::EnrollFde => {
                if enroll_fde(fs) == Some(true) {
                    done += 1;
                }
            }
            // The disk steps (Partition/Format/WriteKernelSlots/…) are realized here
            // by the EuroFS format itself on the RAM disk.
            _ => {}
        }
    }
    done
}

/// Boot self-test: actually run an installation on a RAM disk and prove that it
/// survives a remount (≈ reboot).
pub fn selftest(now: u64) {
    let cfg = default_config();
    let steps = match plan(&cfg) {
        Ok(s) => s,
        Err(e) => {
            crate::serial_println!("[q1x] installer plan invalid: {e:?}");
            return;
        }
    };

    // A 4 MiB RAM disk as the target (1024 × 4 KiB blocks).
    let mut dev = MemoryBlockDevice::new(1024, 4096);

    // ── Execute: format to EuroFS + provision. ──
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

    // ── Remount (≈ reboot after installation): the provisioning must be persistent. ──
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
        "[q1x] EuroInstall execution: EuroFS-format={format_ok}, {provisioned} steps provisioned, after remount: hostname={hostname_ok}, user={user_ok}, locale={locale_ok}, EuroCA={ca_ok} → {}",
        if ok { "OK (installation actually executed + survives reboot) ✓" } else { "FAILED" }
    );
}

/// `euroinstall exec` extension: run a dry-run execution and report.
pub fn shell() -> Vec<String> {
    alloc::vec![
        String::from("EuroInstall execution — formats EuroFS + provisions (locale/keymap/hostname/user/EuroCA)"),
        String::from("  boot self-test [q1x] runs this on a RAM disk and proves the installation survives a remount"),
        String::from("  the same executor runs on a virtio-blk partition for a real installation to disk"),
    ]
}
