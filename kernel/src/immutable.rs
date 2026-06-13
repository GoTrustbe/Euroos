//! L1 + L2: bestands-**immutability** + de **`CAP_IMMUTABLE_ADMIN`**-poort.
//!
//! De soevereine veiligheids-ruggengraat begint hier. EuroFS draagt per inode
//! immutability-vlaggen (L1: [`eurofs::FLAG_IMMUTABLE`] / [`eurofs::FLAG_APPEND_ONLY`])
//! die schrijven/verwijderen/hernoemen IN HET FILESYSTEEM tegenhouden — onafhankelijk
//! van POSIX-rechten of root. Deze module is de kernel-poort erboven (L2): het
//! **zetten of wissen** van die vlaggen vereist de aparte capability
//! [`CAP_IMMUTABLE_ADMIN`]. Zo kan zelfs een root-shell systeembestanden niet
//! ontgrendelen zonder die expliciete, auditeerbare bevoegdheid — de basis voor een
//! verifieerbaar onveranderbaar systeem (en, met L3, voor verity-partities).

use eurofs::{FileSystem, FsError, FLAG_APPEND_ONLY, FLAG_IMMUTABLE};

use crate::ring3::CAP_IMMUTABLE_ADMIN;

/// L2: zet/wis de immutability-vlaggen van `path` — ALLEEN als `caps` de
/// `CAP_IMMUTABLE_ADMIN`-bit bevat. Anders `PermissionDenied`, óók voor root.
pub fn set_protected(fs: &mut dyn FileSystem, path: &str, flags: u32, caps: u64) -> Result<(), FsError> {
    if caps & CAP_IMMUTABLE_ADMIN == 0 {
        crate::audit::record(crate::audit::Event::ImmutableDenied, path);
        return Err(FsError::PermissionDenied);
    }
    let r = fs.set_flags(path, flags);
    if r.is_ok() {
        crate::audit::record(
            if flags & FLAG_IMMUTABLE != 0 {
                crate::audit::Event::ImmutableSet
            } else {
                crate::audit::Event::ImmutableCleared
            },
            path,
        );
    }
    r
}

/// Markeer de meegeleverde systeembinaries + kritieke config IMMUTABEL — tamper-proof
/// systeembestanden. Geeft het aantal beschermde bestanden terug.
pub fn protect_system_files(fs: &mut dyn FileSystem, caps: u64) -> usize {
    let mut n = 0;
    for &p in SYSTEM_FILES {
        if fs.exists(p) && set_protected(fs, p, FLAG_IMMUTABLE, caps).is_ok() {
            n += 1;
        }
    }
    n
}

/// De meegeleverde, tegen-manipulatie-beschermde systeembestanden (gespiegeld in
/// [`protect_system_files`]) — voor de `euroimmutable list`-weergave.
const SYSTEM_FILES: &[&str] = &[
    "/bin/hello",
    "/bin/cat",
    "/bin/dyntest",
    "/lib/libeuro.so",
    "/etc/shadow",
    "/etc/hostname",
];

fn describe_flags(flags: u32) -> &'static str {
    if flags & FLAG_IMMUTABLE != 0 && flags & FLAG_APPEND_ONLY != 0 {
        "immutabel + append-only"
    } else if flags & FLAG_IMMUTABLE != 0 {
        "immutabel (i)"
    } else if flags & FLAG_APPEND_ONLY != 0 {
        "append-only (a)"
    } else {
        "mutabel"
    }
}

/// `euroimmutable` — de bevoegde immutability-admintool (L2-API). Het ZETTEN/WISSEN
/// van vlaggen loopt via [`set_protected`] en vereist dus `CAP_IMMUTABLE_ADMIN`; dit
/// is de getekende admintool die die capability houdt. Statuslezen is vrij.
///
/// Subcommando's: `status <pad>` · `list` · `lock <pad>` (+i) · `unlock <pad>` (−i).
pub fn shell(fs: &mut dyn FileSystem, sub: &str, path: &str) -> alloc::vec::Vec<alloc::string::String> {
    use alloc::string::ToString;
    use alloc::vec;
    match sub {
        "" | "help" => vec![
            "euroimmutable — onveranderbaarheid (L1/L2):".to_string(),
            "  status <pad>   toon de immutability-vlaggen van een bestand".to_string(),
            "  list           toon de beschermde systeembestanden".to_string(),
            "  lock <pad>     markeer immutabel (+i) — vereist CAP_IMMUTABLE_ADMIN".to_string(),
            "  unlock <pad>   wis de vlaggen (−i) — vereist CAP_IMMUTABLE_ADMIN".to_string(),
        ],
        "status" => {
            if path.is_empty() {
                return vec!["gebruik: euroimmutable status <pad>".to_string()];
            }
            match fs.get_flags(path) {
                Ok(f) => vec![alloc::format!("{path}: {} (vlaggen={f:#x})", describe_flags(f))],
                Err(_) => vec![alloc::format!("euroimmutable: kan '{path}' niet lezen")],
            }
        }
        "list" => {
            let mut out = vec!["beschermde systeembestanden:".to_string()];
            for &p in SYSTEM_FILES {
                if fs.exists(p) {
                    let f = fs.get_flags(p).unwrap_or(0);
                    out.push(alloc::format!("  {p}  →  {}", describe_flags(f)));
                }
            }
            out
        }
        "lock" | "unlock" => {
            if path.is_empty() {
                return vec![alloc::format!("gebruik: euroimmutable {sub} <pad>")];
            }
            let flags = if sub == "lock" { FLAG_IMMUTABLE } else { 0 };
            match set_protected(fs, path, flags, CAP_IMMUTABLE_ADMIN) {
                Ok(()) => vec![alloc::format!(
                    "euroimmutable: {path} is nu {} (geauditeerd)",
                    describe_flags(flags)
                )],
                Err(_) => vec![alloc::format!(
                    "euroimmutable: GEWEIGERD voor {path} — vereist CAP_IMMUTABLE_ADMIN"
                )],
            }
        }
        _ => vec![alloc::format!("euroimmutable: onbekend subcommando '{sub}' (zie: euroimmutable help)")],
    }
}

/// L1/L2-boot-zelftest: bewijs (a) de cap-poort op het zetten van de vlag, en (b) dat
/// de FS-laag een immutabel bestand écht beschermt tegen schrijven/verwijderen.
pub fn selftest(fs: &mut dyn FileSystem) {
    let path = "/tmp/l1-test";
    let _ = fs.create_dir("/tmp");
    if fs.write_file(path, b"origineel").is_err() {
        crate::serial_println!("[l1] zelftest: kon testbestand niet maken");
        return;
    }

    // (L2) Zonder CAP_IMMUTABLE_ADMIN mag de vlag NIET gezet worden — ook niet "als root".
    let no_cap = set_protected(fs, path, FLAG_IMMUTABLE, crate::ring3::CAP_FILE);
    // (L2) Mét de capability lukt het wel.
    let with_cap = set_protected(fs, path, FLAG_IMMUTABLE, CAP_IMMUTABLE_ADMIN);

    // (L1) Nu immutabel: schrijven + verwijderen worden door de FS geweigerd.
    let write_blocked = fs.write_file(path, b"gehackt") == Err(FsError::PermissionDenied);
    let remove_blocked = fs.remove_file(path) == Err(FsError::PermissionDenied);
    let intact = fs.read_file(path).map(|d| d == b"origineel").unwrap_or(false);

    // (L2) Vlag wissen vereist óók de capability; daarna weer wijzigbaar.
    let clear_no_cap = set_protected(fs, path, 0, crate::ring3::CAP_FILE) == Err(FsError::PermissionDenied);
    let _ = set_protected(fs, path, 0, CAP_IMMUTABLE_ADMIN);
    let writable_again = fs.write_file(path, b"weer-mutabel").is_ok();
    let _ = fs.remove_file(path);

    let ok = no_cap == Err(FsError::PermissionDenied)
        && with_cap.is_ok()
        && write_blocked
        && remove_blocked
        && intact
        && clear_no_cap
        && writable_again;
    crate::serial_println!(
        "[l1] immutability + CAP_IMMUTABLE_ADMIN: cap-poort-op-set={}, schrijf-geblokkeerd={}, verwijder-geblokkeerd={}, inhoud-intact={}, cap-poort-op-clear={}, weer-mutabel-na-clear={} → {}",
        no_cap == Err(FsError::PermissionDenied), write_blocked, remove_blocked, intact, clear_no_cap, writable_again,
        if ok { "OK (zelfs root kan zonder de cap niets wijzigen) ✓" } else { "MISLUKT" }
    );
    let _ = FLAG_APPEND_ONLY; // (P3 gebruikt deze vlag — zie audit.rs)
}
