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
