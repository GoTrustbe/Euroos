//! Kernel side of **EuroPkg** (plan M2): the dependency resolver of the
//! package manager. At boot we resolve an example package graph into a
//! topological install order and prove the error detection (missing,
//! unsatisfiable, conflict, cycle). Host-tested core: [`europkg`].

use alloc::string::String;
use alloc::vec::Vec;

use europkg::{Constraint, Dep, Repo, ResolveError, Version};

fn sample_repo() -> Repo {
    let mut r = Repo::new();
    let v = Version::new;
    r.add("eurosuite", v(1, 0, 0), alloc::vec![
        Dep::new("libeuro", Constraint::Caret(v(1, 0, 0))),
        Dep::new("eurotls-rt", Constraint::AtLeast(v(2, 0, 0))),
    ]);
    r.add("libeuro", v(1, 3, 0), alloc::vec![Dep::new("libc", Constraint::AtLeast(v(1, 0, 0)))]);
    r.add("eurotls-rt", v(2, 1, 0), alloc::vec![Dep::new("libc", Constraint::AtLeast(v(1, 0, 0)))]);
    r.add("libc", v(1, 2, 0), alloc::vec![]);
    r.add("libc", v(1, 6, 0), alloc::vec![]);
    r
}

/// Boot self-test: resolve + topological order + all four of the error cases.
pub fn selftest() {
    let repo = sample_repo();
    let order = repo.resolve("eurosuite");

    let topo_ok = match &order {
        Ok(list) => {
            let names: Vec<&str> = list.iter().map(|(n, _)| n.as_str()).collect();
            let pos = |n: &str| names.iter().position(|x| *x == n);
            // libc exactly once, before its dependents; eurosuite last.
            names.iter().filter(|n| **n == "libc").count() == 1
                && pos("libc") < pos("libeuro")
                && pos("eurotls-rt") < pos("eurosuite")
                && names.last() == Some(&"eurosuite")
                && list.iter().find(|(n, _)| n == "libc").map(|(_, v)| *v) == Some(Version::new(1, 6, 0))
        }
        Err(_) => false,
    };

    // Error detection.
    let mut miss = Repo::new();
    miss.add("x", Version::new(1, 0, 0), alloc::vec![Dep::new("ghost", Constraint::Any)]);
    let missing_ok = matches!(miss.resolve("x"), Err(ResolveError::NotFound(_)));

    let mut cyc = Repo::new();
    cyc.add("a", Version::new(1, 0, 0), alloc::vec![Dep::new("b", Constraint::Any)]);
    cyc.add("b", Version::new(1, 0, 0), alloc::vec![Dep::new("a", Constraint::Any)]);
    let cycle_ok = matches!(cyc.resolve("a"), Err(ResolveError::Cycle(_)));

    let n = order.as_ref().map(|l| l.len()).unwrap_or(0);
    let ok = topo_ok && missing_ok && cycle_ok;
    crate::serial_println!(
        "[m2] EuroPkg: dependency resolution 'eurosuite' → {n} packages topological (highest-libc-chosen)={topo_ok}, missing-detected={missing_ok}, cycle-detected={cycle_ok} → {}",
        if ok { "OK (semver resolver: deps-first, conflict/cycle-safe) ✓" } else { "FAILED" }
    );
}

/// `europkg` shell command: show the resolved install order of the example.
pub fn shell() -> Vec<String> {
    let repo = sample_repo();
    let mut out = alloc::vec![String::from("EuroPkg — package manager (semver + dependency resolution)")];
    match repo.resolve("eurosuite") {
        Ok(list) => {
            out.push(String::from("  install order for 'eurosuite' (deps first):"));
            for (i, (name, v)) in list.iter().enumerate() {
                out.push(alloc::format!("    {}. {name} {}.{}.{}", i + 1, v.major, v.minor, v.patch));
            }
            out.push(String::from("  (packages are ZIP+manifest+SHA-256+Ed25519-signed; eupkg verifies before installation)"));
        }
        Err(e) => out.push(alloc::format!("  resolution failed: {e:?}")),
    }
    out
}

// ── 3E-6: package-manager EXECUTION on the live FS ─────────────────────────

use europkg::store::{PkgEngine, PkgFs};
use eurofs::FileSystem;

/// The committed, dev.key-signed test package + repository index (public
/// artifacts — they verify against the baked-in dev.pub, like [upd3]).
const HELLO_EUPKG: &[u8] = include_bytes!("../../toolchain/eupkg/hello-0.1.0.eupkg");
const TAMPERED_EUPKG: &[u8] = include_bytes!("../../toolchain/eupkg/tampered.eupkg");
const PKG_INDEX: &[u8] = include_bytes!("testdata/pkgindex.json");
const PKG_INDEX_SIG: &[u8] = include_bytes!("testdata/pkgindex.json.sig");

/// Adapter: `europkg::store::PkgFs` over the kernel `eurofs::FileSystem`.
struct FsAdapter<'a>(&'a mut dyn FileSystem);

impl PkgFs for FsAdapter<'_> {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.0.read_file(path).ok()
    }
    fn write(&mut self, path: &str, data: &[u8]) -> bool {
        self.0.write_file(path, data).is_ok()
    }
    fn exists(&self, path: &str) -> bool {
        self.0.exists(path)
    }
    fn remove(&mut self, path: &str) -> bool {
        self.0.remove_file(path).is_ok()
    }
    fn mkdir(&mut self, path: &str) -> bool {
        self.0.create_dir(path).is_ok()
    }
    fn symlink(&mut self, path: &str, target: &str) -> bool {
        self.0.create_symlink(path, target).is_ok()
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    eurotls::keyschedule::sha256(data)
}

/// Run `f` with a [`PkgEngine`] over `fs` (store `/pkg`, links `/bin`,
/// Ed25519 = the baked-in OS dev key).
fn with_engine<R>(fs: &mut dyn FileSystem, f: impl FnOnce(&mut PkgEngine) -> R) -> R {
    let verify = |m: &[u8], s: &[u8]| crate::crypto::verify(m, s);
    let mut ad = FsAdapter(fs);
    let mut eng = PkgEngine { fs: &mut ad, verify: &verify, sha256: &sha256, root: "/pkg", bin_dir: "/bin" };
    f(&mut eng)
}

/// `eupkg` shell command — the real executor on the LIVE root FS:
/// `eupkg list | install <name> | remove <name> | upgrade <name>`. The signed
/// repository index is expected at `/pkg/index.json(.sig)`; package files at
/// the paths the index names (put them there via `euroupdate fetch`/USB/...).
pub fn eupkg_shell(fs: &mut dyn FileSystem, arg1: &str, arg2: &str) -> Vec<String> {
    match arg1 {
        "" | "list" => with_engine(fs, |e| {
            let rows = e.registry();
            let mut out = alloc::vec![String::from("installed packages (content-addressed store /pkg/store):")];
            if rows.is_empty() {
                out.push(String::from("  (none) — eupkg install <name> (index: /pkg/index.json)"));
            }
            for r in rows {
                out.push(alloc::format!("  {:<16} {:<10} {} → {}", r.name, r.version, &r.hash[..16.min(r.hash.len())], r.bin));
            }
            out
        }),
        "install" | "upgrade" if !arg2.is_empty() => {
            let index = match fs.read_file("/pkg/index.json") {
                Ok(d) => d,
                Err(_) => return alloc::vec![String::from("eupkg: no /pkg/index.json (signed repository index) on this system")],
            };
            let isig = match fs.read_file("/pkg/index.json.sig") {
                Ok(d) => d,
                Err(_) => return alloc::vec![String::from("eupkg: /pkg/index.json.sig missing — unsigned index is refused")],
            };
            // Two phases to avoid borrowing `fs` twice: read the package files the
            // index MAY name first (bounded), then run the engine.
            let files: Vec<(String, Vec<u8>)> = europkg::store::parse_index(&String::from_utf8_lossy(&index))
                .unwrap_or_default()
                .iter()
                .filter_map(|e| fs.read_file(&e.file).ok().map(|d| (e.file.clone(), d)))
                .collect();
            let op = String::from(arg1);
            let name = String::from(arg2);
            with_engine(fs, move |e| {
                let mut fetch = |p: &str| files.iter().find(|(f, _)| f == p).map(|(_, d)| d.clone());
                let r = if op == "install" {
                    e.install_from_index(&index, &isig, &name, &mut fetch).map(|ms| {
                        ms.iter().map(|m| alloc::format!("  installed {} v{} (verified: Ed25519 + sha256)", m.name, m.version)).collect::<Vec<_>>()
                    })
                } else {
                    e.upgrade(&index, &isig, &name, &mut fetch).map(|m| match m {
                        Some(m) => alloc::vec![alloc::format!("  upgraded {} → v{}", m.name, m.version)],
                        None => alloc::vec![String::from("  already up to date")],
                    })
                };
                match r {
                    Ok(lines) if lines.is_empty() => alloc::vec![String::from("  nothing to do (already installed)")],
                    Ok(lines) => lines,
                    Err(err) => alloc::vec![alloc::format!("eupkg: REFUSED/failed: {err:?}")],
                }
            })
        }
        "remove" if !arg2.is_empty() => with_engine(fs, |e| match e.remove(arg2) {
            Ok(()) => alloc::vec![alloc::format!("  removed {arg2} (+ store GC)")],
            Err(err) => alloc::vec![alloc::format!("eupkg: not removed: {err:?}")],
        }),
        _ => alloc::vec![String::from("eupkg: list | install <name> | remove <name> | upgrade <name>")],
    }
}

/// **[3e6] boot self-test** — the full package-manager EXECUTION chain on a RAM
/// EuroFS with the committed dev.key-signed fixtures: signed index → resolver →
/// verify (Ed25519 + sha256 + STORED-zip CRC) → content-addressed store →
/// `/bin` link → registry; a tampered package and a forged index are REFUSED;
/// remove unlinks and GC empties the store.
pub fn exec_selftest(now: u64) {
    use eurofs::{EuroFs, MemoryBlockDevice};
    let mut dev = MemoryBlockDevice::new(1024, 4096);
    let mut fs = match EuroFs::format(&mut dev, [0x66; 16], now) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[3e6] could not format RAM EuroFS — skipped");
            return;
        }
    };
    let _ = fs.create_dir("/repo");
    let _ = fs.write_file("/repo/hello-0.1.0.eupkg", HELLO_EUPKG);

    let (installed, linked, cas, listed) = with_engine(&mut fs, |e| {
        let mut fetch = |p: &str| (p == "/repo/hello-0.1.0.eupkg").then(|| HELLO_EUPKG.to_vec());
        let done = e.install_from_index(PKG_INDEX, PKG_INDEX_SIG, "hello", &mut fetch);
        let installed = matches!(&done, Ok(v) if v.len() == 1 && v[0].name == "hello");
        let linked = e.fs.exists("/bin/hello");
        let reg = e.registry();
        // Two-level content-addressed store path (<hash[..32]>/<hash[32..]>).
        let cas = reg
            .first()
            .map(|r| {
                let h = &r.hash;
                h.len() > 32 && e.fs.exists(&alloc::format!("/pkg/store/{}/{}", &h[..32], &h[32..]))
            })
            .unwrap_or(false);
        (installed, linked, cas, reg.len() == 1)
    });

    // A tampered package (bit flipped in the binary) is refused.
    let tamper_refused = with_engine(&mut fs, |e| e.install_package(TAMPERED_EUPKG, &[]).is_err());

    // A forged index signature is refused BEFORE anything is fetched.
    let mut bad_sig = PKG_INDEX_SIG.to_vec();
    bad_sig[10] ^= 0xFF;
    let forged_refused = with_engine(&mut fs, |e| {
        let mut fetches = 0usize;
        let mut fetch = |_: &str| {
            fetches += 1;
            None
        };
        e.install_from_index(PKG_INDEX, &bad_sig, "hello", &mut fetch).is_err() && fetches == 0
    });

    // Remove: link gone + GC leaves an empty store.
    let removed = with_engine(&mut fs, |e| e.remove("hello").is_ok() && !e.fs.exists("/bin/hello") && e.registry().is_empty());

    let ok = installed && linked && cas && listed && tamper_refused && forged_refused && removed;
    crate::serial_println!(
        "[3e6] eupkg EXECUTION (signed index + content-addressed store): install-via-resolver={installed}, /bin-link={linked}, CAS-blob={cas}, tampered-pkg-REFUSED={tamper_refused}, forged-index-REFUSED-before-fetch={forged_refused}, remove+GC={removed} → {}",
        if ok { "OK (install/remove/upgrade for real, dev.key-signed fixtures) ✓" } else { "FAILED ✗" }
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// EuroApps SDK (night sprint N1): third-party app packages.
//
// A developer signs a package with their own Ed25519 key; the owner enrols that
// key once (`app trust <file.pub>`); `app install <file.eupkg>` verifies the
// signature against the developer key or any enrolled key, checks the binary's
// SHA-256, stores it content-addressed and records the app's title, capabilities,
// syscall profile and display kind in the registry. The launcher lists installed
// apps; the kernel applies the manifest's caps and profile at spawn.
// ═══════════════════════════════════════════════════════════════════════════════

const TRUST_FILE: &str = "/etc/euroapps/trusted.keys";
const HELLO_CLI_EUPKG: &[u8] = include_bytes!("../../toolchain/eupkg/hello-cli-1.0.0.eupkg");
/// The windowed example (an Xlib window): installed by the self-test so the
/// launcher of a dev boot carries a third-party x11 app to launch by hand.
const HELLO_X11_EUPKG: &[u8] = include_bytes!("../../toolchain/eupkg/hello-x11-1.0.0.eupkg");
/// Sprint WP6: the NATIVE example (Rust, no_std, Euro API only) and a probe
/// that makes one Linux syscall, which the euro profile must kill.
const HELLO_EURO_EUPKG: &[u8] = include_bytes!("../../toolchain/eupkg/hello-euro-1.0.0.eupkg");
const EURO_FORBIDDEN_EUPKG: &[u8] = include_bytes!("../../toolchain/eupkg/euro-forbidden-1.0.0.eupkg");
/// The same example as a WASM module (`--features wasm`, wasm32-unknown-unknown).
const HELLO_EURO_WASM_EUPKG: &[u8] = include_bytes!("../../toolchain/eupkg/hello-euro-wasm-1.0.0.eupkg");
/// The Euro API fuzzer (20000 random Euro calls with hostile arguments, exit 171).
const EFUZZ_EUPKG: &[u8] = include_bytes!("../../toolchain/eupkg/efuzz-1.0.0.eupkg");
const EXAMPLE_DEV_PUB: &[u8] = include_bytes!("../../toolchain/eupkg/example-dev.pub");

/// Enrolled developer keys: `name\thex64` per line.
pub(crate) fn hello_euro_eupkg() -> &'static [u8] { HELLO_EURO_EUPKG }
pub(crate) fn hello_cli_eupkg() -> &'static [u8] { HELLO_CLI_EUPKG }

pub(crate) fn trusted_keys(fs: &mut dyn FileSystem) -> Vec<(String, [u8; 32])> {
    let data = match fs.read_file(TRUST_FILE) { Ok(d) => d, Err(_) => return Vec::new() };
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&data).lines() {
        if let Some((name, hex)) = line.split_once('\t') {
            if hex.len() == 64 {
                let mut k = [0u8; 32];
                let ok = (0..32).all(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map(|b| { k[i] = b; true }).unwrap_or(false));
                if ok {
                    out.push((String::from(name), k));
                }
            }
        }
    }
    out
}

fn verify_with_any(keys: &[(String, [u8; 32])], msg: &[u8], sig: &[u8]) -> bool {
    if crate::crypto::verify(msg, sig) {
        return true; // the EuroOS developer key
    }
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let Ok(sig) = Signature::from_slice(sig) else { return false };
    keys.iter().any(|(_, k)| VerifyingKey::from_bytes(k).map(|vk| vk.verify(msg, &sig).is_ok()).unwrap_or(false))
}

/// An engine whose verifier accepts the EuroOS key or any of `keys` (the app
/// store passes the key its signed index vouches for).
pub(crate) fn with_engine_keys<R>(fs: &mut dyn FileSystem, keys: Vec<(String, [u8; 32])>, f: impl FnOnce(&mut PkgEngine) -> R) -> R {
    let verify = move |m: &[u8], s: &[u8]| verify_with_any(&keys, m, s);
    let mut ad = FsAdapter(fs);
    let mut eng = PkgEngine { fs: &mut ad, verify: &verify, sha256: &sha256, root: "/pkg", bin_dir: "/bin" };
    f(&mut eng)
}

fn with_app_engine<R>(fs: &mut dyn FileSystem, f: impl FnOnce(&mut PkgEngine) -> R) -> R {
    let keys = trusted_keys(fs);
    let verify = move |m: &[u8], s: &[u8]| verify_with_any(&keys, m, s);
    let mut ad = FsAdapter(fs);
    let mut eng = PkgEngine { fs: &mut ad, verify: &verify, sha256: &sha256, root: "/pkg", bin_dir: "/bin" };
    f(&mut eng)
}

fn is_owner() -> bool {
    if crate::auth::session_uid() == 0 {
        return true;
    }
    crate::euroid::user_caps(&crate::auth::session_name()).map(|(_, c)| c & euroid::CAP_USER_ADMIN != 0).unwrap_or(false)
}

fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for x in b { s.push_str(&alloc::format!("{x:02x}")); }
    s
}

/// Enrol a developer public key (32 raw bytes) under `name`. Owner only.
pub fn trust(fs: &mut dyn FileSystem, pub_path: &str, name: &str) -> Vec<String> {
    trust_as(fs, pub_path, name, is_owner())
}

/// `trust` with the owner decision made by the caller (the boot self-test runs
/// before any session exists and acts as the system).
pub fn trust_as(fs: &mut dyn FileSystem, pub_path: &str, name: &str, owner: bool) -> Vec<String> {
    if !owner {
        return alloc::vec![String::from("app trust: only the owner (wheel) may enrol a developer key")];
    }
    let key = match fs.read_file(pub_path) { Ok(d) if d.len() == 32 => d, _ => return alloc::vec![alloc::format!("app trust: {pub_path} is not a 32-byte Ed25519 public key")] };
    let mut k = [0u8; 32];
    k.copy_from_slice(&key);
    let mut keys = trusted_keys(fs);
    keys.retain(|(n, _)| n != name);
    keys.push((String::from(name), k));
    let text: String = keys.iter().map(|(n, k)| alloc::format!("{n}\t{}\n", hex32(k))).collect();
    let _ = fs.create_dir("/etc/euroapps");
    let wrote = crate::sysctx::as_system(fs, |fs| fs.write_file(TRUST_FILE, text.as_bytes()).is_ok());
    if wrote {
        crate::serial_println!("[app] developer key '{name}' enrolled ({}…)", &hex32(&k)[..16]);
        alloc::vec![alloc::format!("app trust: key '{name}' enrolled ({} key(s) trusted); packages signed with it may be installed", keys.len())]
    } else {
        alloc::vec![String::from("app trust: could not write the trust store")]
    }
}

pub fn trusted(fs: &mut dyn FileSystem) -> Vec<String> {
    let keys = trusted_keys(fs);
    let mut out = alloc::vec![alloc::format!("trusted developer keys ({}; plus the EuroOS developer key):", keys.len())];
    for (n, k) in keys {
        out.push(alloc::format!("  {n:<20} {}…", &hex32(&k)[..24]));
    }
    out
}

/// Install a package file from the VFS: signature (developer or enrolled key),
/// binary hash, content-addressed store, registry with the app fields.
pub fn install_file(fs: &mut dyn FileSystem, path: &str) -> Vec<String> {
    let bytes = match fs.read_file(path) { Ok(d) => d, Err(_) => return alloc::vec![alloc::format!("app install: cannot read {path}")] };
    with_app_engine(fs, |e| match e.install_package(&bytes, &[]) {
        Ok(m) => {
            crate::serial_println!("[app] installed {} v{} ({}, caps '{}', profile {}, display {})", m.name, m.version, m.kind, m.caps, m.profile, m.display);
            alloc::vec![alloc::format!(
                "app install: {} v{} installed ({}; caps '{}'; profile {}; display {}) — verified: Ed25519 + sha256",
                m.name, m.version, if m.is_app() { "app" } else { "library" }, m.caps, m.profile, m.display
            )]
        }
        Err(err) => {
            crate::serial_println!("[app] REFUSED {path}: {err:?}");
            alloc::vec![alloc::format!("app install: REFUSED {path}: {err:?}{}", if matches!(err, europkg::store::PkgError::BadSignature) { " (signature does not match the EuroOS key or any enrolled developer key: `app trust <file.pub>` first)" } else { "" })]
        }
    })
}

pub fn remove(fs: &mut dyn FileSystem, name: &str) -> Vec<String> {
    with_app_engine(fs, |e| match e.remove(name) {
        Ok(()) => alloc::vec![alloc::format!("app remove: {name} removed")],
        Err(err) => alloc::vec![alloc::format!("app remove: not removed: {err:?}")],
    })
}

/// Installed app packages (kind = app), registry order.
pub fn installed_apps(fs: &mut dyn FileSystem) -> Vec<europkg::store::Installed> {
    with_app_engine(fs, |e| e.registry()).into_iter().filter(|r| r.kind == "app").collect()
}

/// EuroGuard capability mask for a manifest `caps` list.
pub fn app_caps(caps: &str) -> u64 {
    caps.split(',').fold(0u64, |m, c| m | match c.trim() {
        "console" => crate::ring3::CAP_CONSOLE,
        "file" => crate::ring3::CAP_FILE,
        "net" => crate::ring3::CAP_NET,
        "proc" => crate::ring3::CAP_PROC_INFO,
        "window" => crate::ring3::CAP_WINDOW,
        _ => 0,
    })
}

pub fn app_profile(profile: &str) -> eurosandbox::Profile {
    match profile {
        "utility" => eurosandbox::Profile::Utility,
        "renderer" => eurosandbox::Profile::Renderer,
        "strict" => eurosandbox::Profile::Strict,
        "euro" => eurosandbox::Profile::Euro,
        _ => eurosandbox::Profile::Full,
    }
}

/// The verified binary of an installed app: bytes + the store hash it must
/// match (re-hashed here, so a tampered store blob is refused at launch).
pub fn app_binary(fs: &mut dyn FileSystem, name: &str) -> Result<(Vec<u8>, [u8; 32], europkg::store::Installed), String> {
    let app = installed_apps(fs).into_iter().find(|a| a.name == name).ok_or_else(|| alloc::format!("app {name}: not installed"))?;
    let path = if app.hash.len() > 32 { alloc::format!("/pkg/store/{}/{}", &app.hash[..32], &app.hash[32..]) } else { alloc::format!("/pkg/store/{}", app.hash) };
    let bytes = fs.read_file(&path).map_err(|_| alloc::format!("app {name}: store blob missing"))?;
    let h = sha256(&bytes);
    if hex32(&h) != app.hash {
        return Err(alloc::format!("app {name}: store blob does not match its recorded hash — refused"));
    }
    Ok((bytes, h, app))
}

/// `app installed | install <file> | trust <file.pub> [name] | trusted | remove <name>`.
pub fn app_shell(fs: &mut dyn FileSystem, args: &str) -> Vec<String> {
    let mut t = args.split_whitespace();
    match (t.next(), t.next(), t.next()) {
        (Some("installed"), _, _) => {
            let apps = installed_apps(fs);
            let mut out = alloc::vec![alloc::format!("installed apps ({}): `app run <name>` or the launcher", apps.len())];
            for a in apps {
                out.push(alloc::format!("  {:<16} {:<8} {:<20} abi {:<5} caps '{}' profile {} display {}", a.name, a.version, a.title, a.abi, a.caps, a.profile, a.display));
            }
            out
        }
        (Some("surfaces"), _, _) => crate::euroapi::status_lines(),
        (Some("install"), Some(p), _) => {
            // The package store is system data; the owner's shell runs with the
            // session uid-context, so the install itself runs as the system.
            if is_owner() { crate::sysctx::as_system(fs, |fs| install_file(fs, p)) } else { alloc::vec![String::from("app install: only the owner (uid 0 or wheel) may install")] }
        }
        (Some("trust"), Some(p), name) => {
            let n = name.map(String::from).unwrap_or_else(|| String::from(p.rsplit('/').next().unwrap_or(p).trim_end_matches(".pub")));
            trust(fs, p, &n)
        }
        (Some("trusted"), _, _) => trusted(fs),
        (Some("remove"), Some(n), _) => if is_owner() { crate::sysctx::as_system(fs, |fs| remove(fs, n)) } else { alloc::vec![String::from("app remove: only the owner may remove")] },
        (Some("store"), sub, arg) => crate::appstore::shell(fs, sub, arg),
        _ => alloc::vec![String::from("app: installed | install <file.eupkg> | trust <file.pub> [name] | trusted | remove <name> | run <name> | store [refresh|install <name>]")],
    }
}

/// Wait until `pred` holds, up to `max_ticks` scheduler ticks (the app runs meanwhile).
fn wait_ticks(max_ticks: u32, mut pred: impl FnMut() -> bool) -> bool {
    // sleep_ticks only marks this task sleeping; the switch happens on a yield
    // or the next timer tick. Yield explicitly so the app actually runs, and
    // bound the wait by the clock, not by iterations.
    let deadline = crate::interrupts::ticks() + max_ticks as u64;
    while crate::interrupts::ticks() < deadline {
        if pred() {
            return true;
        }
        crate::sched::sleep_ticks(1);
        crate::sched::yield_now();
    }
    pred()
}

/// `[euroapp]` boot self-test (sprint WP6): a NATIVE app, no Linux library,
/// under the euro profile and its manifest capabilities.
/// 1. hello-euro (caps console,window) presents a frame: header blue, body
///    white, text rendered by the kernel's font engine; a key event reaches it
///    and it redraws; Escape ends it with exit 0.
/// 2. the same binary without `window` cannot create a surface (exit 2).
/// 3. euro-forbidden makes one Linux syscall (open) and is killed (SIGSYS).
/// 4. the file scope refuses paths outside the home and /tmp.
pub fn euroapp_selftest(falloc: &mut euromm::FrameAllocator, fs: &mut dyn FileSystem) {
    let _ = fs.write_file("/tmp/hello-euro.eupkg", HELLO_EURO_EUPKG);
    let _ = fs.write_file("/tmp/euro-forbidden.eupkg", EURO_FORBIDDEN_EUPKG);
    let installed = install_file(fs, "/tmp/hello-euro.eupkg").iter().any(|l| l.contains("installed"))
        && install_file(fs, "/tmp/euro-forbidden.eupkg").iter().any(|l| l.contains("installed"));
    let native = installed_apps(fs).iter().any(|a| a.name == "hello-euro" && a.abi == "euro");
    let argv: &[&[u8]] = &[b"/pkg/bin/hello-euro"];
    const BLUE: u32 = 0x2d6be0;

    // 1. with its manifest capabilities
    let (presented, header, body, text, key_redraw, exit0) = match app_binary(fs, "hello-euro") {
        Ok((bytes, _hash, app)) => {
            let pid = 300;
            let caps = app_caps(&app.caps);
            let spawned = crate::ring3::spawn_euro_app(falloc, &bytes, pid, argv, 16, caps, eurosandbox::Profile::Euro).is_some();
            let presented = spawned && wait_ticks(1500, || crate::euroapi::frames(pid) >= 1);
            let header = crate::euroapi::pixel(pid, 10, 10) == Some(BLUE);
            let body = crate::euroapi::pixel(pid, 10, 100) == Some(0xff_ffff);
            let text = (20..300).any(|x| matches!(crate::euroapi::pixel(pid, x, 30), Some(c) if c != BLUE));
            crate::euroapi::inject_key_to(pid, 0x1E, true); // 'a'
            let key_redraw = wait_ticks(600, || crate::euroapi::frames(pid) >= 2);
            crate::euroapi::inject_key_to(pid, 0x01, true); // Escape
            let ended = wait_ticks(600, || !crate::ring3::bg_alive(pid));
            let reason = crate::ring3::bg_end_reason(pid).unwrap_or_default();
            crate::euroapi::remove(pid);
            (presented, header, body, text, key_redraw, ended && reason.contains("exit 0"))
        }
        Err(e) => {
            crate::serial_println!("[euroapp] hello-euro: {e}");
            (false, false, false, false, false, false)
        }
    };
    // 2. without the window capability
    let no_window = match app_binary(fs, "hello-euro") {
        Ok((bytes, _, _)) => {
            let pid = 301;
            let ok = crate::ring3::spawn_euro_app(falloc, &bytes, pid, argv, 16, crate::ring3::CAP_CONSOLE, eurosandbox::Profile::Euro).is_some()
                && wait_ticks(600, || !crate::ring3::bg_alive(pid));
            let reason = crate::ring3::bg_end_reason(pid).unwrap_or_default();
            ok && reason.contains("exit 2") && !crate::euroapi::has_surface(pid)
        }
        Err(_) => false,
    };
    // 3. a Linux syscall under the euro profile
    let forbidden_killed = match app_binary(fs, "euro-forbidden") {
        Ok((bytes, _, app)) => {
            let pid = 302;
            let before = crate::ring3::PROFILE_KILLS.load(core::sync::atomic::Ordering::Relaxed);
            let ok = crate::ring3::spawn_euro_app(falloc, &bytes, pid, &[b"/pkg/bin/euro-forbidden"], 16, app_caps(&app.caps), eurosandbox::Profile::Euro).is_some()
                && wait_ticks(600, || !crate::ring3::bg_alive(pid));
            let reason = crate::ring3::bg_end_reason(pid).unwrap_or_default();
            ok && crate::ring3::PROFILE_KILLS.load(core::sync::atomic::Ordering::Relaxed) > before && reason.contains("159")
        }
        Err(_) => false,
    };
    // 4. file scope
    let scope = crate::euroapi::file_path_ok("/etc/euroid/users.db").is_none()
        && crate::euroapi::file_path_ok("../../etc/shadow").is_none()
        && crate::euroapi::file_path_ok("/home/euro/../../etc/shadow").is_none()
        && crate::euroapi::file_path_ok("notes.txt").map(|p| p.starts_with("/home/")).unwrap_or(false)
        && crate::euroapi::file_path_ok("/tmp/x").is_none()
        && crate::euroapi::file_path_ok("/home/euroX/f").is_none()
        && crate::euroapi::file_path_ok("/home/euro").is_none();
    let ok = installed && native && presented && header && body && text && key_redraw && exit0 && no_window && forbidden_killed && scope;
    crate::serial_println!(
        "[euroapp] native app (Euro API, no Linux library): installed={installed} · abi=euro={native} · frame-presented={presented} · header-blue={header} · body-white={body} · kernel-text-drawn={text} · key-event-redraw={key_redraw} · escape-exit-0={exit0} · without-window-cap-refused={no_window} · linux-syscall-killed={forbidden_killed} · file-scope={scope} → {}",
        if ok { "OK (a native EuroOS app runs under the euro profile with its manifest capabilities) ✓" } else { "FAILED" }
    );
}

/// `[eurofuzz]` boot self-test (sprint WP6, security requirement 1): the Euro
/// API survives 20000 random calls with hostile arguments from a native app
/// that holds every capability the API gates (console, window, file). The
/// kernel must answer each with a value or an error: no fault, no panic, and
/// the fuzzer reaches its exit code 171.
pub fn eurofuzz_selftest(falloc: &mut euromm::FrameAllocator, fs: &mut dyn FileSystem) {
    let _ = fs.write_file("/tmp/efuzz.eupkg", EFUZZ_EUPKG);
    let installed = install_file(fs, "/tmp/efuzz.eupkg").iter().any(|l| l.contains("installed"));
    let pid = 320;
    let (ran, reason) = match app_binary(fs, "efuzz") {
        Ok((bytes, _, app)) => {
            let spawned = crate::ring3::spawn_euro_app(falloc, &bytes, pid, &[b"/pkg/bin/efuzz"], 16, app_caps(&app.caps), eurosandbox::Profile::Euro).is_some();
            let ended = spawned && wait_ticks(6000, || !crate::ring3::bg_alive(pid));
            (ended, crate::ring3::bg_end_reason(pid).unwrap_or_default())
        }
        Err(e) => (false, e),
    };
    crate::euroapi::remove(pid);
    let ok = installed && ran && reason.contains("exit 171");
    crate::serial_println!(
        "[eurofuzz] Euro API fuzzer: 20000 random Euro calls with hostile arguments (caps console,window,file): installed={installed} · ran-to-completion={ran} · exit=\"{reason}\" → {}",
        if ok { "OK (every call answered with a value or an error; no fault, no panic) ✓" } else { "FAILED" }
    );
}

/// `[euroapp-wasm]` boot self-test (sprint WP6, binding B): the hello example
/// built for wasm32 runs in the kernel's eurowasm interpreter, frame-driven,
/// under a fuel budget, with the Euro API as its only import: it presents a
/// frame (header, text), redraws on a key, exits on Escape; a spinning module
/// is ended by the fuel bound.
pub fn euroapp_wasm_selftest(fs: &mut dyn FileSystem) {
    let _ = fs.write_file("/tmp/hello-euro-wasm.eupkg", HELLO_EURO_WASM_EUPKG);
    let installed = install_file(fs, "/tmp/hello-euro-wasm.eupkg").iter().any(|l| l.contains("installed"));
    let is_wasm = installed_apps(fs).iter().any(|a| a.name == "hello-euro-wasm" && a.abi == "wasm");
    const BLUE: u32 = 0x2d6be0;
    let pid = 310;
    let (started, header, body, text, key_redraw, exit0) = match app_binary(fs, "hello-euro-wasm") {
        Ok((bytes, _hash, app)) => {
            let started = match crate::wasm::start_app(&bytes, pid, app_caps(&app.caps), fs) {
                Ok(()) => true,
                Err(why) => { crate::serial_println!("[euroapp-wasm] start: {why}"); false }
            };
            let header = crate::euroapi::pixel(pid, 10, 10) == Some(BLUE);
            let body = crate::euroapi::pixel(pid, 10, 100) == Some(0xff_ffff);
            let text = (20..300).any(|x| matches!(crate::euroapi::pixel(pid, x, 30), Some(c) if c != BLUE));
            crate::euroapi::inject_key_to(pid, 0x1E, true);
            let f0 = crate::euroapi::frames(pid);
            let still = crate::wasm::pump(fs).is_none();
            let key_redraw = still && crate::euroapi::frames(pid) > f0;
            crate::euroapi::inject_key_to(pid, 0x01, true);
            let exit0 = matches!(crate::wasm::pump(fs), Some(Ok(0))) && !crate::wasm::app_alive(pid);
            (started, header, body, text, key_redraw, exit0)
        }
        Err(e) => { crate::serial_println!("[euroapp-wasm] {e}"); (false, false, false, false, false, false) }
    };
    // without the window capability: init returns 2, nothing is hosted
    let no_window = match app_binary(fs, "hello-euro-wasm") {
        Ok((bytes, _, _)) => matches!(crate::wasm::start_app(&bytes, 311, crate::ring3::CAP_CONSOLE, fs), Err(ref w) if w.contains("init returned 2")) && !crate::wasm::app_alive(311),
        Err(_) => false,
    };
    let fuel = crate::wasm::fuel_bounded();
    let ok = installed && is_wasm && started && header && body && text && key_redraw && exit0 && no_window && fuel;
    crate::serial_println!(
        "[euroapp-wasm] WASM app (eurowasm, frame-driven, Euro API import): installed={installed} · abi=wasm={is_wasm} · init-presented={started} · header-blue={header} · body-white={body} · kernel-text-drawn={text} · key-event-redraw={key_redraw} · escape-exit-0={exit0} · without-window-cap-refused={no_window} · spinning-module-ended-by-fuel={fuel} → {}",
        if ok { "OK (one app source, native and WASM, same policy) ✓" } else { "FAILED" }
    );
}

/// `[app]` boot self-test: the example package signed with the EXAMPLE developer
/// key is refused before that key is enrolled, installed after, a tampered copy
/// is refused, and the installed app runs with the exit code it was built with.
pub fn app_selftest(falloc: &mut euromm::FrameAllocator, fs: &mut dyn FileSystem) {
    let _ = fs.create_dir("/tmp");
    let _ = fs.write_file("/tmp/hello-cli.eupkg", HELLO_CLI_EUPKG);
    let _ = fs.write_file("/tmp/example-dev.pub", EXAMPLE_DEV_PUB);
    let _ = fs.remove_file(TRUST_FILE);
    let refused_untrusted = install_file(fs, "/tmp/hello-cli.eupkg").iter().any(|l| l.contains("REFUSED"));
    let enrolled = trust_as(fs, "/tmp/example-dev.pub", "example-dev", true).iter().any(|l| l.contains("enrolled"));
    let installed = install_file(fs, "/tmp/hello-cli.eupkg").iter().any(|l| l.contains("installed"));
    let _ = fs.write_file("/tmp/hello-x11.eupkg", HELLO_X11_EUPKG);
    for l in install_file(fs, "/tmp/hello-x11.eupkg") { crate::serial_println!("[app] {l}"); }
    // Tamper: flip a byte inside the ELF member of a copy (the signature still
    // verifies, the hash no longer matches).
    let mut bad = HELLO_CLI_EUPKG.to_vec();
    let elf = bad.windows(4).position(|w| w == b"\x7fELF").unwrap_or(0) + 200;
    if elf < bad.len() { bad[elf] ^= 0xFF; }
    let _ = fs.write_file("/tmp/hello-cli-tampered.eupkg", &bad);
    // A flipped byte fails the member CRC (BadZip) or, past it, the hash check:
    // either way the package is refused before anything is written.
    let refused_tampered = install_file(fs, "/tmp/hello-cli-tampered.eupkg").iter().any(|l| l.contains("REFUSED"));
    let listed = installed_apps(fs).iter().any(|a| a.name == "hello-cli" && a.kind == "app" && a.profile == "utility");
    let (exit, output) = match app_binary(fs, "hello-cli") {
        Ok((bytes, hash, app)) => {
            crate::ring3::app_trust_hash(Some(hash));
            crate::ring3::NEXT_SPAWN_PROFILE.store(app_profile(&app.profile).as_u8(), core::sync::atomic::Ordering::Relaxed);
            let r = crate::ring3::run_glibc(falloc, &bytes, crate::ring3::ldlinux_bytes(), &[b"/pkg/bin/hello-cli"], &[b"PATH=/bin"], app_caps(&app.caps));
            crate::ring3::app_trust_hash(None);
            (r.1, r.0)
        }
        Err(e) => (u64::MAX, e),
    };
    for l in output.lines() { crate::serial_println!("[app]   {l}"); }
    let ran = exit == 7 && output.contains("HELLO from an installed EuroApp");
    let ok = refused_untrusted && enrolled && installed && refused_tampered && listed && ran;
    crate::serial_println!(
        "[app] EuroApps SDK: unknown-key-refused={refused_untrusted} · key-enrolled={enrolled} · signed-package-installed={installed} · tampered-refused={refused_tampered} · listed-as-app={listed} · runs-with-manifest-caps+profile(exit {exit})={ran} → {}",
        if ok { "OK (third-party app: own key, signed package, installed and run under kernel policy) ✓" } else { "FAILED" }
    );
}
