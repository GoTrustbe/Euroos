//! System-integrity verification: the files under /bin and /lib on disk are
//! compared byte-for-byte against the reference copies embedded in the kernel
//! image. The kernel image itself is Ed25519-verified by the boot loader
//! (A/B slots), so this closes the trust chain: loader verifies kernel,
//! kernel verifies the system files it once wrote. Any on-disk tampering —
//! even with the immutable flag somehow bypassed — is detected and reported.

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::fs::FileSystem;

/// One verification sweep. Returns (files checked, mismatched paths).
/// Runs in system context: integrity checking is a kernel service.
pub fn verify(fs: &mut dyn FileSystem, binaries: &[(&'static str, &'static [u8])]) -> (usize, Vec<String>) {
    crate::sysctx::as_system(fs, |fs| {
        let mut checked = 0usize;
        let mut bad: Vec<String> = Vec::new();
        for (path, want) in binaries {
            match fs.read_file(path) {
                Ok(have) => {
                    checked += 1;
                    if have.as_slice() != *want {
                        bad.push(String::from(*path));
                    }
                }
                Err(_) => {
                    // Missing IS a mismatch: someone removed a system file.
                    checked += 1;
                    bad.push(String::from(*path));
                }
            }
        }
        (checked, bad)
    })
}

/// Boot + periodic entry: verify, report, and journal a mismatch.
pub fn check_and_report(fs: &mut dyn FileSystem, binaries: &[(&'static str, &'static [u8])], when: &str) -> bool {
    let (checked, bad) = verify(fs, binaries);
    if bad.is_empty() {
        crate::serial_println!("[intg] system integrity {when}: {checked}/{checked} files match the signed kernel image \u{2713}");
        true
    } else {
        for p in &bad {
            crate::serial_println!("[intg] INTEGRITY MISMATCH {when}: {p} differs from the signed reference!");
            crate::journal::log(eurojournal::Severity::Err, "integrity", p);
        }
        crate::notify::push(
            "System integrity",
            &alloc::format!("{} system file(s) tampered or missing", bad.len()),
            crate::interrupts::ticks(),
        );
        false
    }
}

/// `integrity` shell command: run a sweep now and print the verdict.
pub fn shell(fs: &mut dyn FileSystem, binaries: &[(&'static str, &'static [u8])]) -> Vec<String> {
    let (checked, bad) = verify(fs, binaries);
    let mut out = Vec::new();
    if bad.is_empty() {
        out.push(alloc::format!("system integrity: {checked}/{checked} files match the signed kernel image"));
        out.push(String::from("trust chain: loader verifies kernel (Ed25519, A/B) -> kernel verifies /bin + /lib"));
    } else {
        out.push(alloc::format!("system integrity: {} of {checked} files DO NOT MATCH:", bad.len()));
        for p in &bad {
            out.push(alloc::format!("  TAMPERED/MISSING: {p}"));
        }
    }
    out
}

/// `[intg]` boot self-test: a REAL end-to-end tamper detection — lift the
/// immutable flag with the boot capability, tamper a system binary on disk,
/// prove the sweep catches it, then restore the original and the flag.
pub fn selftest(fs: &mut dyn FileSystem, binaries: &[(&'static str, &'static [u8])], caps: u64) {
    let victim = "/bin/cat";
    let orig = binaries.iter().find(|(p, _)| *p == victim).map(|(_, b)| *b);
    let Some(orig) = orig else {
        crate::serial_println!("[intg] selftest skipped: {victim} not bundled");
        return;
    };
    let clean_before = verify(fs, binaries).1.is_empty();
    // Tamper (legitimately: boot capability lifts the immutable flag).
    let _ = crate::immutable::set_protected(fs, victim, 0, caps);
    let wrote = crate::sysctx::as_system(fs, |fs| fs.write_file(victim, b"EVIL").is_ok());
    let detected = verify(fs, binaries).1.iter().any(|p| p == victim);
    // Restore.
    let restored = crate::sysctx::as_system(fs, |fs| fs.write_file(victim, orig).is_ok());
    let _ = crate::immutable::set_protected(fs, victim, eurofs::FLAG_IMMUTABLE, caps);
    let clean_after = verify(fs, binaries).1.is_empty();
    let ok = clean_before && wrote && detected && restored && clean_after;
    crate::serial_println!(
        "[intg] tamper-detection: clean-before={clean_before}, tamper-detected={detected}, restored+clean={clean_after} \u{2192} {}",
        if ok { "OK (integrity sweep is live) \u{2713}" } else { "FAILED \u{2717}" }
    );
}
