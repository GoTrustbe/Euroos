//! TPM 2.0 via de **TIS** (TPM Interface Specification) MMIO-interface — plan O1,
//! hardware root of trust.
//!
//! De TPM-chip hangt op de vaste MMIO-basis `0xFED4_0000` (locality 0, identity-
//! mapped). Deze module is de transportlaag onder de host-geteste [`eurotpm`]-
//! commando-codering: ze vraagt de locality op, schrijft een TPM-commando in de
//! FIFO, geeft de chip de `go`, en leest de respons terug (met burst-count-flow-
//! control). Daarbovenop draait een boot-zelftest: `Startup`, `GetRandom` (bewijst
//! dat de TPM leeft), en **PCR-extend** — de meet-operatie die measured boot en
//! (met K3) een aan de boot-toestand gesealde schijfsleutel mogelijk maakt.

use alloc::vec::Vec;

const TIS_BASE: u64 = 0xFED4_0000; // locality 0
const REG_ACCESS: u64 = 0x00;
const REG_STS: u64 = 0x18;
const REG_DATA_FIFO: u64 = 0x24;
const REG_DID_VID: u64 = 0xF00;

const ACCESS_ACTIVE_LOCALITY: u8 = 1 << 5;
const ACCESS_REQUEST_USE: u8 = 1 << 1;
const STS_VALID: u32 = 1 << 7;
const STS_COMMAND_READY: u32 = 1 << 6;
const STS_TPM_GO: u32 = 1 << 5;
const STS_DATA_AVAIL: u32 = 1 << 4;
const STS_EXPECT: u32 = 1 << 3;

static mut PRESENT: bool = false;

#[inline]
unsafe fn r8(off: u64) -> u8 {
    ((TIS_BASE + off) as *const u8).read_volatile()
}
#[inline]
unsafe fn w8(off: u64, v: u8) {
    ((TIS_BASE + off) as *mut u8).write_volatile(v);
}
#[inline]
unsafe fn sts() -> u32 {
    ((TIS_BASE + REG_STS) as *const u32).read_volatile()
}
#[inline]
unsafe fn burst_count() -> usize {
    ((sts() >> 8) & 0xFFFF) as usize
}

fn spin() {
    for _ in 0..200 {
        core::hint::spin_loop();
    }
}

/// Vraag locality 0 op (request → wacht op activeLocality).
unsafe fn request_locality() -> bool {
    w8(REG_ACCESS, ACCESS_REQUEST_USE);
    for _ in 0..100_000 {
        if r8(REG_ACCESS) & ACCESS_ACTIVE_LOCALITY != 0 {
            return true;
        }
        spin();
    }
    false
}

/// Geef locality 0 weer vrij.
unsafe fn release_locality() {
    w8(REG_ACCESS, ACCESS_ACTIVE_LOCALITY);
}

unsafe fn wait_sts(mask: u32) -> bool {
    for _ in 0..1_000_000 {
        let s = sts();
        if s & mask == mask {
            return true;
        }
        spin();
    }
    false
}

/// Voer één volledige TPM-transactie uit: stuur `cmd`, lees de respons. None bij
/// een transport-fout/timeout.
pub fn transact(cmd: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        if !PRESENT {
            return None;
        }
        if !request_locality() {
            return None;
        }
        // Zet de TPM in command-ready.
        w8(REG_STS, STS_COMMAND_READY as u8);
        if !wait_sts(STS_COMMAND_READY) {
            release_locality();
            return None;
        }
        // Schrijf het commando via de FIFO, met burst-count-flow-control. Het laatste
        // byte pas schrijven als Expect nog gezet is (de TPM weet zo wanneer 't af is).
        let mut sent = 0;
        while sent < cmd.len() {
            let mut burst = burst_count();
            if burst == 0 {
                if !wait_sts(STS_VALID) {
                    break;
                }
                burst = burst_count().max(1);
            }
            let chunk = burst.min(cmd.len() - sent);
            for &b in &cmd[sent..sent + chunk] {
                w8(REG_DATA_FIFO, b);
            }
            sent += chunk;
        }
        // Geef de chip de go.
        w8(REG_STS, STS_TPM_GO as u8);
        if !wait_sts(STS_VALID | STS_DATA_AVAIL) {
            release_locality();
            return None;
        }
        // Lees de respons: eerst de 10-byte header (voor de size), dan de rest.
        let mut resp = Vec::new();
        let mut guard = 0;
        loop {
            if sts() & STS_DATA_AVAIL == 0 {
                break;
            }
            let burst = burst_count().max(1);
            for _ in 0..burst {
                if sts() & STS_DATA_AVAIL == 0 {
                    break;
                }
                resp.push(r8(REG_DATA_FIFO));
                // Stop zodra we de volledige respons (size uit de header) hebben.
                if resp.len() >= 6 {
                    let size = u32::from_be_bytes([resp[2], resp[3], resp[4], resp[5]]) as usize;
                    if resp.len() >= size {
                        w8(REG_STS, STS_COMMAND_READY as u8);
                        release_locality();
                        return Some(resp);
                    }
                }
            }
            guard += 1;
            if guard > 100_000 {
                break;
            }
        }
        w8(REG_STS, STS_COMMAND_READY as u8);
        release_locality();
        if resp.len() >= 10 {
            Some(resp)
        } else {
            None
        }
    }
}

/// Detecteer de TPM + doe Startup. Geeft true als er een werkende TPM is.
pub fn init() -> bool {
    unsafe {
        // DID_VID lezen: 0xFFFFFFFF / 0 = geen chip.
        let didvid = ((TIS_BASE + REG_DID_VID) as *const u32).read_volatile();
        if didvid == 0xFFFF_FFFF || didvid == 0 {
            crate::serial_println!("[tpm] geen TPM-TIS @ {:#x} (DID_VID={:#010x})", TIS_BASE, didvid);
            return false;
        }
        PRESENT = true;
        // TPM2_Startup(CLEAR). De firmware (OVMF) startte 'm meestal al → dan geeft
        // dit TPM_RC_INITIALIZE (0x100), wat prima is.
        if let Some(r) = transact(&eurotpm::startup()) {
            if let Some(h) = eurotpm::parse_header(&r) {
                crate::serial_println!(
                    "[tpm] TPM 2.0 TIS @ {:#x} (DID_VID={:#010x}), Startup rc={:#x}{}",
                    TIS_BASE, didvid, h.rc,
                    if h.rc == 0 { " (vers gestart)" } else { " (al door firmware gestart)" }
                );
            }
        }
        true
    }
}

pub fn present() -> bool {
    unsafe { PRESENT }
}

/// Vraag `n` echte willekeurige bytes aan de TPM (voor sleutelgeneratie, K3-FDE).
pub fn get_random(n: u16) -> Option<Vec<u8>> {
    let r = transact(&eurotpm::get_random(n))?;
    eurotpm::parse_random(&r).filter(|b| b.len() >= n as usize)
}

/// Lees een PCR-waarde (SHA-256, 32 bytes) — de measured-boot-toestand die
/// remote attestation (O2) in een quote opneemt.
pub fn read_pcr(index: u32) -> Option<[u8; 32]> {
    let r = transact(&eurotpm::pcr_read(index))?;
    let v = eurotpm::parse_pcr_read(&r)?;
    if v.len() >= 32 {
        let mut out = [0u8; 32];
        out.copy_from_slice(&v[..32]);
        Some(out)
    } else {
        None
    }
}

/// O1-boot-zelftest: bewijs een levende TPM (GetRandom) + measured boot (PCR-extend
/// verandert de PCR-waarde, precies zoals de boot-keten meet).
pub fn selftest() {
    if !present() {
        return;
    }
    // (1) GetRandom — een levende TPM levert echte willekeur.
    let rnd = transact(&eurotpm::get_random(16)).and_then(|r| eurotpm::parse_random(&r));
    let rnd_ok = rnd.as_ref().map(|b| b.len() >= 8 && b.iter().any(|&x| x != 0)).unwrap_or(false);

    // (2) PCR 16 (debug-PCR) lezen vóór de extend.
    let before = transact(&eurotpm::pcr_read(16)).and_then(|r| eurotpm::parse_pcr_read(&r));

    // (3) Extend PCR 16 met een meet-digest → de PCR verandert (SHA256(oud || digest)).
    let digest = [0x42u8; eurotpm::SHA256_LEN];
    let extended = transact(&eurotpm::pcr_extend(16, &digest))
        .and_then(|r| eurotpm::parse_header(&r))
        .map(|h| h.ok())
        .unwrap_or(false);

    // (4) PCR 16 opnieuw lezen → moet VERSCHILLEN van vóór de extend.
    let after = transact(&eurotpm::pcr_read(16)).and_then(|r| eurotpm::parse_pcr_read(&r));
    let changed = matches!((&before, &after), (Some(b), Some(a)) if b != a);

    let rb: Vec<u8> = rnd.unwrap_or_default().into_iter().take(8).collect();
    crate::serial_println!(
        "[o1] TPM measured boot: GetRandom-OK={} ({:02x?}), PCR16-extend-OK={}, PCR16-veranderd-na-extend={} → {}",
        rnd_ok, rb, extended, changed,
        if rnd_ok && extended && changed { "OK (hardware root of trust werkt) ✓" } else { "MISLUKT/onvolledig" }
    );
}
