//! EuroContainers (plan F2): lightweight sandboxes on top of the EuroGuard capability
//! model — no Linux namespaces. A container chroots an execution to
//! `/containers/<name>` and restricts capabilities + network. The security-critical
//! path resolution (no `..` escape) lives in the host-tested `eurosandbox` crate.

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::FileSystem;
use eurosandbox::{Container, NetScope};
use spin::Mutex;

use crate::ring3::{CAP_CONSOLE, CAP_FILE, CAP_NET, CAP_PROC_INFO};

static CONTAINERS: Mutex<Vec<Container>> = Mutex::new(Vec::new());

fn find(name: &str) -> Option<Container> {
    CONTAINERS.lock().iter().find(|c| c.name == name).cloned()
}

/// Create a container: register it and lay out its file root.
pub fn create(fs: &mut dyn FileSystem, name: &str, caps: u64, net: NetScope) -> Vec<String> {
    if find(name).is_some() {
        return alloc::vec![alloc::format!("container '{name}' already exists")];
    }
    let con = Container::new(name, caps, net);
    let _ = fs.create_dir("/containers");
    let _ = fs.create_dir(&con.root);
    CONTAINERS.lock().push(con.clone());
    alloc::vec![alloc::format!("container '{name}' created — root {}, caps {:#06b}", con.root, caps & 0xF)]
}

/// List the registered containers.
pub fn list() -> Vec<String> {
    let cs = CONTAINERS.lock();
    if cs.is_empty() {
        return alloc::vec!["(no containers)".into()];
    }
    let mut out = alloc::vec![String::from("CONTAINER         ROOT                   CAPS   NET")];
    for c in cs.iter() {
        let net = match &c.net {
            NetScope::None => String::from("none"),
            NetScope::Any => String::from("any"),
            NetScope::Allow(l) => alloc::format!("{} rule(s)", l.len()),
        };
        out.push(alloc::format!("  {:<15} {:<22} {:#06b} {}", c.name, c.root, c.caps & 0xF, net));
    }
    out
}

/// `container run <name> <path>` — demonstrate the sandbox: write a file via
/// a container path, and show that an escape attempt (`..`) stays within the root.
/// Proves the chroot semantics with the real filesystem.
pub fn run(fs: &mut dyn FileSystem, name: &str, path: &str) -> Vec<String> {
    let con = match find(name) {
        Some(c) => c,
        None => return alloc::vec![alloc::format!("container '{name}' does not exist")],
    };
    let mut out = Vec::new();
    let resolved = con.resolve(path);
    out.push(alloc::format!("container '{name}': path '{path}' → '{resolved}'"));
    if con.contains(&resolved) || resolved == con.root {
        out.push("  ✓ within the container root (no escape)".into());
    } else {
        out.push("  ✗ ESCAPED — this would be a bug".into());
    }
    // Write+read via the resolved path (real FS proof).
    if fs.write_file(&resolved, b"hello from the container\n").is_ok() {
        if let Ok(d) = fs.read_file(&resolved) {
            out.push(alloc::format!("  {} bytes written+read on the sandboxed path ✓", d.len()));
        }
    }
    out
}

/// **[3f1] boot self-test** — the container-runtime pieces that were missing:
/// a **signed image** (tampered manifest refused), **ResourceLimits** enforced
/// (allocation refused at the ceiling), and a **CoW overlay** (write copies up,
/// delete whiteouts the lower image). Complements the chroot+caps `[container]`.
pub fn runtime_selftest() {
    use eurosandbox::image::{verify_image, ImageManifest, SignedImage};
    use eurosandbox::{Layer, Overlay, ResourceLimits, Usage};

    // (1) Signed image: the committed dev.key-signed manifest fixture verifies
    // against the baked-in dev.pub; then tamper (elevate caps) and prove refusal.
    // The manifest bytes are the exact `ImageManifest::to_bytes()` canonical form.
    let bytes: &[u8] = include_bytes!("testdata/container-image.manifest");
    let sig: &[u8] = include_bytes!("testdata/container-image.sig");
    let verify = |m: &[u8], s: &[u8]| crate::crypto::verify(m, s);
    let good = verify_image(&SignedImage { manifest: bytes.to_vec(), signature: sig.to_vec() }, &verify, Some("euroos-rootfs-v1")).is_ok();
    // Sanity: the parsed manifest carries the constrained caps, not everything.
    let manifest = ImageManifest::from_bytes(bytes);
    let caps_ok = manifest.as_ref().map(|m| m.caps == 0b0111).unwrap_or(false);
    let mut evil = manifest.unwrap_or(ImageManifest {
        name: String::new(),
        caps: 0,
        limits: ResourceLimits::default(),
        net: Vec::new(),
        rootfs_sha256: String::new(),
    });
    evil.caps = u64::MAX; // grab every capability, keep the old signature
    let tampered_refused = verify_image(&SignedImage { manifest: evil.to_bytes(), signature: sig.to_vec() }, &verify, None).is_err();

    // (2) ResourceLimits: a 4 KiB memory ceiling admits 3 KiB, refuses the next 2 KiB.
    let lim = ResourceLimits::new(4096, 0, 0, 0);
    let mut usage = Usage::default();
    let within = usage.check_alloc(&lim, 3072, 0).is_ok();
    usage.charge(3072, 0);
    let over_refused = usage.check_alloc(&lim, 2048, 0).is_err();

    // (3) CoW overlay: write copies up (lower untouched), delete whiteouts.
    let mut ov = Overlay::new();
    let reads_lower = ov.resolve_read("/etc/os-release", true) == Layer::Lower;
    ov.on_write("/etc/os-release");
    let copied_up = ov.resolve_read("/etc/os-release", true) == Layer::Upper;
    ov.on_delete("/etc/hosts");
    let whiteout = ov.resolve_read("/etc/hosts", true) == Layer::Whiteout;

    let ok = good && caps_ok && tampered_refused && within && over_refused && reads_lower && copied_up && whiteout;
    crate::serial_println!(
        "[3f1] EuroContainer runtime: signed-image-verify={good} (constrained-caps={caps_ok}), tampered-manifest-REFUSED={tampered_refused}, mem-limit-enforced(3K-ok,+2K-refused)={over_refused}, overlay-CoW(copy-up={copied_up},whiteout={whiteout}) → {}",
        if ok { "OK (signed images + ResourceLimits + EuroFS overlay, on EuroGuard caps) ✓" } else { "FAILED ✗" }
    );
}

/// Boot self-test (serial-verifiable): create a container, write into it, and
/// prove that a `..` escape stays within the root.
pub fn boot_selftest(fs: &mut dyn FileSystem) {
    let net = NetScope::Allow(alloc::vec![([10, 0, 2, 2], 443)]);
    create(fs, "demo", CAP_CONSOLE | CAP_FILE | CAP_PROC_INFO, net);
    let con = match find("demo") {
        Some(c) => c,
        None => return,
    };
    // Effective caps: a process with ALL rights loses CAP_NET in this container.
    let eff = con.effective_caps(CAP_CONSOLE | CAP_PROC_INFO | CAP_FILE | CAP_NET);
    let escaped = con.resolve("../../../etc/passwd");
    let contained = con.contains(&escaped) || escaped == con.root;
    let _ = fs.write_file(&con.resolve("data.txt"), b"sandbox\n");
    crate::serial_println!(
        "[container] 'demo' root={} eff_caps={:#06b} (CAP_NET removed={}) | escape '../../../etc/passwd' → {} (within={})",
        con.root,
        eff & 0xF,
        eff & CAP_NET == 0,
        escaped,
        contained,
    );
}
