//! **3E-6: package-manager EXECUTION** — install/remove/upgrade on a
//! content-addressed store, with a signed repository index.
//!
//! What was missing after M2 (the resolver) and `eupkg` (build/verify): actually
//! carrying out an installation. This module does that, pure and host-tested:
//!
//! * **verify**: `.eupkg` = STORED ZIP → manifest + Ed25519 signature (over the
//!   manifest, checked via a caller-supplied verifier = the OS dev key) + the
//!   binary whose SHA-256 the signed manifest pins. Any mismatch → refusal.
//! * **content-addressed store**: binaries live under `<root>/store/<sha256hex>`
//!   — identical content is stored once; the hash IS the identity.
//! * **registry**: `<root>/installed` records name/version/hash/bin/deps.
//! * **link**: `<bin_dir>/<name>` points at the store blob (symlink when the FS
//!   supports it, copy otherwise).
//! * **remove** refuses while another installed package depends on the target;
//!   **gc** deletes unreferenced store blobs; **upgrade** installs the newer
//!   index version and relinks.
//! * **signed index**: the repository index itself is Ed25519-signed — a forged
//!   index is refused before anything is fetched (same model as EuroUpdate 3E-2).
//!
//! Crypto is injected (`Verifier`/`Hasher` callbacks): the kernel passes its
//! baked-in dev.pub Ed25519 verify + SHA-256; host tests use real ed25519-dalek.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::zipread;
use crate::{Constraint, Dep, PkgIndexEntry, Repo, Version};

/// Ed25519 verify: `(message, signature) -> valid?` (the OS developer key).
pub type Verifier<'a> = &'a dyn Fn(&[u8], &[u8]) -> bool;
/// SHA-256 of `data`.
pub type Hasher<'a> = &'a dyn Fn(&[u8]) -> [u8; 32];
/// Fetch repository file bytes by path (FS read, HTTP GET, …).
pub type Fetch<'a> = &'a mut dyn FnMut(&str) -> Option<Vec<u8>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PkgError {
    /// Not a valid STORED zip (or CRC mismatch).
    BadZip,
    BadManifest,
    /// Ed25519 signature over the manifest/index invalid.
    BadSignature,
    /// Binary SHA-256 differs from the signed manifest — tampered.
    HashMismatch,
    NotFound,
    /// Another installed package still depends on it.
    RequiredBy(String),
    /// Resolver failure (missing dep, conflict, cycle).
    Unresolvable,
    Io,
}

/// Minimal filesystem the engine needs; the kernel adapts `eurofs::FileSystem`,
/// host tests use an in-memory mock.
pub trait PkgFs {
    fn read(&self, path: &str) -> Option<Vec<u8>>;
    fn write(&mut self, path: &str, data: &[u8]) -> bool;
    fn exists(&self, path: &str) -> bool;
    fn remove(&mut self, path: &str) -> bool;
    fn mkdir(&mut self, path: &str) -> bool;
    /// Link `path` → `target` (symlink). Default: not supported → the engine
    /// writes a copy instead (the store blob stays authoritative either way).
    fn symlink(&mut self, path: &str, target: &str) -> bool {
        let _ = (path, target);
        false
    }
}

/// Parsed `MANIFEST.toml` (the fields the installer needs).
#[derive(Debug, Clone)]
pub struct PkgMeta {
    pub name: String,
    pub version: String,
    pub binary: String,
    pub binary_sha256: String,
    /// `"app"` for a launchable program, `"lib"` (default) for anything else.
    pub kind: String,
    /// Launcher title (defaults to the name).
    pub title: String,
    /// Comma-separated EuroGuard capabilities: console,file,net,proc.
    pub caps: String,
    /// Syscall profile: full (default), utility, renderer, strict.
    pub profile: String,
    /// `x11` (hosted window) or `none` (command line).
    pub display: String,
    /// Extra argv after argv[0], space separated.
    pub args: String,
    /// `linux` (glibc through the compat bridge, the default), `euro` (a native
    /// static PIE speaking the Euro API) or `wasm` (eurowasm, frame-driven).
    pub abi: String,
}

impl PkgMeta {
    pub fn is_app(&self) -> bool {
        self.kind == "app"
    }
}

fn toml_value<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    for line in s.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix('=') {
                return Some(v.trim().trim_matches('"'));
            }
        }
    }
    None
}

/// Parse the (already signature-verified) manifest. Deliberately minimal — the
/// format is produced by our own `eupkg build`.
pub fn parse_manifest(toml: &str) -> Option<PkgMeta> {
    let name = toml_value(toml, "name")?.to_string();
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None; // the name becomes a path component
    }
    let opt = |k: &str, d: &str| toml_value(toml, k).unwrap_or(d).to_string();
    let kind = opt("kind", "lib");
    let profile = opt("profile", "full");
    let display = opt("display", "none");
    let abi = opt("abi", "linux");
    if !matches!(kind.as_str(), "lib" | "app")
        || !matches!(profile.as_str(), "full" | "utility" | "renderer" | "strict" | "euro")
        || !matches!(display.as_str(), "none" | "x11")
        || !matches!(abi.as_str(), "linux" | "euro" | "wasm")
    {
        return None;
    }
    Some(PkgMeta {
        title: opt("title", &name),
        caps: opt("caps", ""),
        args: opt("args", ""),
        kind,
        profile,
        display,
        abi,
        name,
        version: toml_value(toml, "version")?.to_string(),
        binary: toml_value(toml, "binary")?.to_string(),
        binary_sha256: toml_value(toml, "binary_sha256")?.to_string(),
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Verify a `.eupkg` end-to-end: STORED-zip + CRC → Ed25519 over the manifest →
/// SHA-256 pin of the binary. Returns the metadata + the binary bytes.
pub fn verify_package(pkg: &[u8], verify: Verifier, sha256: Hasher) -> Result<(PkgMeta, Vec<u8>), PkgError> {
    let entries = zipread::parse(pkg).ok_or(PkgError::BadZip)?;
    let find = |n: &str| entries.iter().find(|e| e.name == n).map(|e| e.data.clone());
    let manifest = find("MANIFEST.toml").ok_or(PkgError::BadManifest)?;
    let sig = find("signature.ed25519").ok_or(PkgError::BadSignature)?;
    if !verify(&manifest, &sig) {
        return Err(PkgError::BadSignature);
    }
    let meta = parse_manifest(core::str::from_utf8(&manifest).map_err(|_| PkgError::BadManifest)?)
        .ok_or(PkgError::BadManifest)?;
    let bin = find(&meta.binary).ok_or(PkgError::BadManifest)?;
    if hex(&sha256(&bin)) != meta.binary_sha256 {
        return Err(PkgError::HashMismatch);
    }
    Ok((meta, bin))
}

/// One installed-registry row.
#[derive(Debug, Clone)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub hash: String,
    pub bin: String,
    pub deps: Vec<String>,
    pub kind: String,
    pub title: String,
    pub caps: String,
    pub profile: String,
    pub display: String,
    pub args: String,
    pub abi: String,
}

/// The package engine over an abstract FS + injected crypto.
pub struct PkgEngine<'a> {
    pub fs: &'a mut dyn PkgFs,
    pub verify: Verifier<'a>,
    pub sha256: Hasher<'a>,
    /// Store/registry root, e.g. `/pkg`.
    pub root: &'a str,
    /// Where binaries get linked, e.g. `/bin`.
    pub bin_dir: &'a str,
}

impl<'a> PkgEngine<'a> {
    fn registry_path(&self) -> String {
        format!("{}/installed", self.root)
    }
    /// Content-addressed blob path, split two levels deep (`<hash[..32]>/<hash[32..]>`)
    /// so every path component stays within a filesystem's name limit — EuroFS
    /// caps names at 48 chars, and a flat 64-hex sha256 name would be rejected.
    /// The 32/32 split keeps both halves ≤ 48.
    fn store_path(&self, hash: &str) -> String {
        if hash.len() > 32 {
            format!("{}/store/{}/{}", self.root, &hash[..32], &hash[32..])
        } else {
            format!("{}/store/{}", self.root, hash)
        }
    }
    fn store_dir(&self, hash: &str) -> String {
        if hash.len() > 32 {
            format!("{}/store/{}", self.root, &hash[..32])
        } else {
            format!("{}/store", self.root)
        }
    }

    pub fn registry(&self) -> Vec<Installed> {
        let data = match self.fs.read(&self.registry_path()) {
            Some(d) => d,
            None => return Vec::new(),
        };
        let text = String::from_utf8_lossy(&data).into_owned();
        let mut out = Vec::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() >= 5 {
                let g = |i: usize, d: &str| f.get(i).copied().unwrap_or(d).to_string();
                out.push(Installed {
                    name: f[0].to_string(),
                    version: f[1].to_string(),
                    hash: f[2].to_string(),
                    bin: f[3].to_string(),
                    deps: if f[4].is_empty() { Vec::new() } else { f[4].split(',').map(String::from).collect() },
                    kind: g(5, "lib"),
                    title: g(6, f[0]),
                    caps: g(7, ""),
                    profile: g(8, "full"),
                    display: g(9, "none"),
                    args: g(10, ""),
                    abi: g(11, "linux"),
                });
            }
        }
        out
    }

    fn save_registry(&mut self, rows: &[Installed]) -> Result<(), PkgError> {
        let mut text = String::new();
        for r in rows {
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                r.name, r.version, r.hash, r.bin, r.deps.join(","), r.kind, r.title, r.caps, r.profile, r.display, r.args, r.abi
            ));
        }
        if self.fs.write(&self.registry_path(), text.as_bytes()) {
            Ok(())
        } else {
            Err(PkgError::Io)
        }
    }

    fn ensure_dirs(&mut self) {
        let _ = self.fs.mkdir(self.root);
        let _ = self.fs.mkdir(&format!("{}/store", self.root));
        let _ = self.fs.mkdir(self.bin_dir);
    }

    /// Install ONE verified package (no dependency handling — see
    /// [`Self::install_from_index`] for the resolver-driven path).
    pub fn install_package(&mut self, pkg: &[u8], deps: &[String]) -> Result<PkgMeta, PkgError> {
        let (meta, bin) = verify_package(pkg, self.verify, self.sha256)?;
        self.ensure_dirs();
        // Content-addressed: the (verified) hash IS the identity — dedup for free.
        let blob = self.store_path(&meta.binary_sha256);
        let _ = self.fs.mkdir(&self.store_dir(&meta.binary_sha256));
        if !self.fs.exists(&blob) && !self.fs.write(&blob, &bin) {
            return Err(PkgError::Io);
        }
        self.track_blob(&meta.binary_sha256);
        // Link the binary: symlink into the store when supported, else a copy.
        let link = format!("{}/{}", self.bin_dir, meta.name);
        if self.fs.exists(&link) {
            let _ = self.fs.remove(&link);
        }
        if !self.fs.symlink(&link, &blob) && !self.fs.write(&link, &bin) {
            return Err(PkgError::Io);
        }
        // Registry upsert.
        let mut rows = self.registry();
        rows.retain(|r| r.name != meta.name);
        rows.push(Installed {
            name: meta.name.clone(),
            version: meta.version.clone(),
            hash: meta.binary_sha256.clone(),
            bin: link,
            deps: deps.to_vec(),
            kind: meta.kind.clone(),
            title: meta.title.clone(),
            caps: meta.caps.clone(),
            profile: meta.profile.clone(),
            display: meta.display.clone(),
            args: meta.args.clone(),
            abi: meta.abi.clone(),
        });
        self.save_registry(&rows)?;
        Ok(meta)
    }

    /// Install `name` from a **signed repository index**: verify the index
    /// signature, resolve the dependency closure (M2 resolver, topological),
    /// then fetch+verify+install every not-yet-installed package in order.
    pub fn install_from_index(
        &mut self,
        index: &[u8],
        index_sig: &[u8],
        name: &str,
        fetch: Fetch,
    ) -> Result<Vec<PkgMeta>, PkgError> {
        if !(self.verify)(index, index_sig) {
            return Err(PkgError::BadSignature); // forged index → nothing is fetched
        }
        let text = core::str::from_utf8(index).map_err(|_| PkgError::BadZip)?;
        let entries = parse_index(text).ok_or(PkgError::BadZip)?;
        // Feed the resolver (M2): topological order incl. missing/conflict/cycle detection.
        let mut repo = Repo::new();
        for e in &entries {
            let deps: Vec<Dep> = e.deps.iter().map(|d| Dep::new(d, Constraint::Any)).collect();
            let v = Version::parse(&e.version).ok_or(PkgError::Unresolvable)?;
            repo.add(&e.name, v, deps);
        }
        let order = repo.resolve(name).map_err(|_| PkgError::Unresolvable)?;
        let installed = self.registry();
        let mut done = Vec::new();
        for (pname, pver) in order {
            if installed.iter().any(|r| r.name == pname && r.version == pver.to_string_dotted()) {
                continue; // already present at this version
            }
            let entry = entries
                .iter()
                .find(|e| e.name == pname)
                .ok_or(PkgError::NotFound)?;
            let bytes = fetch(&entry.file).ok_or(PkgError::NotFound)?;
            let meta = self.install_package(&bytes, &entry.deps)?;
            done.push(meta);
        }
        Ok(done)
    }

    /// Remove `name`: refuse while a *different* installed package depends on it;
    /// unlink the binary, drop the registry row, and GC unreferenced blobs.
    pub fn remove(&mut self, name: &str) -> Result<(), PkgError> {
        let rows = self.registry();
        let target = rows.iter().find(|r| r.name == name).ok_or(PkgError::NotFound)?.clone();
        if let Some(dep) = rows.iter().find(|r| r.name != name && r.deps.iter().any(|d| d == name)) {
            return Err(PkgError::RequiredBy(dep.name.clone()));
        }
        let _ = self.fs.remove(&target.bin);
        let remaining: Vec<Installed> = rows.into_iter().filter(|r| r.name != name).collect();
        self.save_registry(&remaining)?;
        self.gc();
        Ok(())
    }

    fn blobs_path(&self) -> String {
        format!("{}/store/.blobs", self.root)
    }

    fn known_blobs(&self) -> Vec<String> {
        self.fs
            .read(&self.blobs_path())
            .map(|d| String::from_utf8_lossy(&d).lines().map(String::from).collect())
            .unwrap_or_default()
    }

    /// The engine has no directory listing on the abstract FS, so every written
    /// blob hash is tracked in a sidecar list — that list is what GC walks.
    fn track_blob(&mut self, hash: &str) {
        let mut known = self.known_blobs();
        if !known.iter().any(|h| h == hash) {
            known.push(String::from(hash));
            let _ = self.fs.write(&self.blobs_path(), known.join("\n").as_bytes());
        }
    }

    /// Delete store blobs no installed package references. Returns the count.
    pub fn gc(&mut self) -> usize {
        let live: Vec<String> = self.registry().iter().map(|r| r.hash.clone()).collect();
        let mut kept = Vec::new();
        let mut removed = 0;
        for h in self.known_blobs() {
            if live.contains(&h) {
                kept.push(h);
            } else {
                let p = self.store_path(&h);
                if self.fs.remove(&p) {
                    removed += 1;
                }
            }
        }
        let _ = self.fs.write(&self.blobs_path(), kept.join("\n").as_bytes());
        removed
    }

    /// Upgrade `name` to the (newer) version in the signed index. `Ok(None)` if
    /// already up to date.
    pub fn upgrade(
        &mut self,
        index: &[u8],
        index_sig: &[u8],
        name: &str,
        fetch: Fetch,
    ) -> Result<Option<PkgMeta>, PkgError> {
        if !(self.verify)(index, index_sig) {
            return Err(PkgError::BadSignature);
        }
        let text = core::str::from_utf8(index).map_err(|_| PkgError::BadZip)?;
        let entries = parse_index(text).ok_or(PkgError::BadZip)?;
        let entry = entries.iter().find(|e| e.name == name).ok_or(PkgError::NotFound)?;
        let cur = self
            .registry()
            .into_iter()
            .find(|r| r.name == name)
            .ok_or(PkgError::NotFound)?;
        let (curv, newv) = match (Version::parse(&cur.version), Version::parse(&entry.version)) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err(PkgError::BadManifest),
        };
        if newv <= curv {
            return Ok(None);
        }
        let bytes = fetch(&entry.file).ok_or(PkgError::NotFound)?;
        let meta = self.install_package(&bytes, &entry.deps)?;
        self.gc(); // the old version's blob is now unreferenced
        Ok(Some(meta))
    }
}

/// Parse the repository index (our controlled JSON subset):
/// `{"packages":[{"name":"a","version":"1.0.0","file":"/pkg/a.eupkg","deps":["b"]}]}`
pub fn parse_index(s: &str) -> Option<Vec<PkgIndexEntry>> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("{\"name\":\"") {
        let obj = &rest[i..];
        let name = field(obj, "name")?;
        let version = field(obj, "version")?;
        let file = field(obj, "file")?;
        let deps = match obj.find("\"deps\":[") {
            Some(d) => {
                let tail = &obj[d + 8..];
                let end = tail.find(']')?;
                tail[..end]
                    .split(',')
                    .map(|x| x.trim().trim_matches('"').to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            }
            None => Vec::new(),
        };
        let opt = |k: &str| field(obj, k).unwrap_or_default();
        out.push(PkgIndexEntry { title: opt("title"), abi: opt("abi"), summary: opt("summary"), key: opt("key"), name, version, file, deps });
        rest = &rest[i + 9..];
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn field(obj: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = obj.find(&pat)? + pat.len();
    let rest = &obj[i..];
    Some(rest[..rest.find('"')?].to_string())
}

#[cfg(test)]
mod app_meta_tests {
    use super::*;

    #[test]
    fn app_manifest_fields_parse_with_defaults_and_validation() {
        let m = parse_manifest("name = \"hello\"\nversion = \"1.0.0\"\nbinary = \"hello\"\nbinary_sha256 = \"ab\"\n").unwrap();
        assert_eq!((m.kind.as_str(), m.title.as_str(), m.profile.as_str(), m.display.as_str()), ("lib", "hello", "full", "none"));
        assert!(!m.is_app());
        let a = parse_manifest("name = \"hello-x11\"\nversion = \"1.0.0\"\nbinary = \"hello\"\nbinary_sha256 = \"ab\"\nkind = \"app\"\ntitle = \"Hello\"\ncaps = \"console,file\"\nprofile = \"utility\"\ndisplay = \"x11\"\nargs = \"--fast\"\n").unwrap();
        assert!(a.is_app());
        assert_eq!((a.title.as_str(), a.caps.as_str(), a.profile.as_str(), a.display.as_str(), a.args.as_str()), ("Hello", "console,file", "utility", "x11", "--fast"));
        // unknown profile / display / kind, or a name with a path in it, is refused
        assert!(parse_manifest("name = \"x\"\nversion = \"1\"\nbinary = \"x\"\nbinary_sha256 = \"a\"\nprofile = \"root\"\n").is_none());
        assert!(parse_manifest("name = \"x\"\nversion = \"1\"\nbinary = \"x\"\nbinary_sha256 = \"a\"\ndisplay = \"wayland\"\n").is_none());
        assert!(parse_manifest("name = \"../x\"\nversion = \"1\"\nbinary = \"x\"\nbinary_sha256 = \"a\"\n").is_none());
    }

    #[test]
    fn abi_field_parses_and_validates() {
        let base = "name = \"n\"\nversion = \"1\"\nbinary = \"b\"\nbinary_sha256 = \"ab\"\nkind = \"app\"\n";
        assert_eq!(parse_manifest(base).unwrap().abi, "linux");
        assert_eq!(parse_manifest(&format!("{base}abi = \"euro\"\nprofile = \"euro\"\n")).unwrap().abi, "euro");
        assert_eq!(parse_manifest(&format!("{base}abi = \"wasm\"\n")).unwrap().abi, "wasm");
        assert!(parse_manifest(&format!("{base}abi = \"java\"\n")).is_none());
    }
}
