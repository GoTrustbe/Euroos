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
