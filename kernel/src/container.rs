//! EuroContainers (plan F2): lichte sandboxes bovenop het EuroGuard-capability-
//! model — geen Linux-namespaces. Een container ge-chroot't een uitvoering naar
//! `/containers/<naam>` en perkt capabilities + netwerk in. De veiligheidskritische
//! padresolutie (geen `..`-ontsnapping) zit in de host-geteste `eurosandbox`-crate.

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

/// Maak een container: registreer hem en leg z'n bestandswortel aan.
pub fn create(fs: &mut dyn FileSystem, name: &str, caps: u64, net: NetScope) -> Vec<String> {
    if find(name).is_some() {
        return alloc::vec![alloc::format!("container '{name}' bestaat al")];
    }
    let con = Container::new(name, caps, net);
    let _ = fs.create_dir("/containers");
    let _ = fs.create_dir(&con.root);
    CONTAINERS.lock().push(con.clone());
    alloc::vec![alloc::format!("container '{name}' aangemaakt — wortel {}, caps {:#06b}", con.root, caps & 0xF)]
}

/// Lijst de geregistreerde containers.
pub fn list() -> Vec<String> {
    let cs = CONTAINERS.lock();
    if cs.is_empty() {
        return alloc::vec!["(geen containers)".into()];
    }
    let mut out = alloc::vec![String::from("CONTAINER         WORTEL                 CAPS   NET")];
    for c in cs.iter() {
        let net = match &c.net {
            NetScope::None => String::from("geen"),
            NetScope::Any => String::from("vrij"),
            NetScope::Allow(l) => alloc::format!("{} regel(s)", l.len()),
        };
        out.push(alloc::format!("  {:<15} {:<22} {:#06b} {}", c.name, c.root, c.caps & 0xF, net));
    }
    out
}

/// `container run <naam> <pad>` — demonstreer de sandbox: schrijf een bestand via
/// een container-pad, en toon dat een ontsnappingspoging (`..`) binnen de wortel
/// blijft. Bewijst de chroot-semantiek met het echte filesysteem.
pub fn run(fs: &mut dyn FileSystem, name: &str, path: &str) -> Vec<String> {
    let con = match find(name) {
        Some(c) => c,
        None => return alloc::vec![alloc::format!("container '{name}' bestaat niet")],
    };
    let mut out = Vec::new();
    let resolved = con.resolve(path);
    out.push(alloc::format!("container '{name}': pad '{path}' → '{resolved}'"));
    if con.contains(&resolved) || resolved == con.root {
        out.push("  ✓ binnen de container-wortel (geen ontsnapping)".into());
    } else {
        out.push("  ✗ ONTSNAPT — dit zou een bug zijn".into());
    }
    // Schrijf+lees via het opgeloste pad (echt FS-bewijs).
    if fs.write_file(&resolved, b"hallo vanuit de container\n").is_ok() {
        if let Ok(d) = fs.read_file(&resolved) {
            out.push(alloc::format!("  {} bytes geschreven+gelezen op het gesandboxte pad ✓", d.len()));
        }
    }
    out
}

/// Boot-zelftest (serial-verifieerbaar): maak een container, schrijf erin, en
/// bewijs dat een `..`-ontsnapping binnen de wortel blijft.
pub fn boot_selftest(fs: &mut dyn FileSystem) {
    let net = NetScope::Allow(alloc::vec![([10, 0, 2, 2], 443)]);
    create(fs, "demo", CAP_CONSOLE | CAP_FILE | CAP_PROC_INFO, net);
    let con = match find("demo") {
        Some(c) => c,
        None => return,
    };
    // Effectieve caps: een proces met ALLE rechten verliest CAP_NET in deze container.
    let eff = con.effective_caps(CAP_CONSOLE | CAP_PROC_INFO | CAP_FILE | CAP_NET);
    let escaped = con.resolve("../../../etc/passwd");
    let contained = con.contains(&escaped) || escaped == con.root;
    let _ = fs.write_file(&con.resolve("data.txt"), b"sandbox\n");
    crate::serial_println!(
        "[container] 'demo' wortel={} eff_caps={:#06b} (CAP_NET ontnomen={}) | escape '../../../etc/passwd' → {} (binnen={})",
        con.root,
        eff & 0xF,
        eff & CAP_NET == 0,
        escaped,
        contained,
    );
}
