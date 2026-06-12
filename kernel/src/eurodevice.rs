//! Kernel-side integratie van het **EuroDevice**-framework (Sprint R): bouw de
//! device-tree uit de ECHTE PCI-enumeratie, registreer de bestaande drivers met hun
//! match-predicaten, en bind ze. Vervangt de losse ad-hoc driver-discovery door één
//! samenhangend device-model — de basis waar toekomstige drivers (WiFi/GPU/USB-hubs)
//! op aansluiten. De `eurodevice`-shellcommando + het `[r]`-boot-zelftest tonen de boom.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use eurodevice::{DeviceKind, DeviceNode, DeviceResources, DeviceState, DeviceTree, DriverRegistry};
use spin::Mutex;

use crate::pci;

static TREE: Mutex<Option<DeviceTree>> = Mutex::new(None);
static REGISTRY: Mutex<Option<DriverRegistry>> = Mutex::new(None);

// ── Match-predicaten van de bestaande kernel-drivers ────────────────────────
fn m_virtio_blk(n: &DeviceNode) -> bool {
    n.vendor == 0x1AF4 && (n.device == 0x1001 || n.device == 0x1042)
}
fn m_virtio_net(n: &DeviceNode) -> bool {
    n.vendor == 0x1AF4 && (n.device == 0x1000 || n.device == 0x1041)
}
fn m_nvme(n: &DeviceNode) -> bool {
    n.class == 0x01 && n.subclass == 0x08
}
fn m_xhci(n: &DeviceNode) -> bool {
    n.class == 0x0C && n.subclass == 0x03 && n.prog_if == 0x30
}
fn m_hda(n: &DeviceNode) -> bool {
    n.class == 0x04 && n.subclass == 0x03
}
fn m_virtio_gpu(n: &DeviceNode) -> bool {
    n.vendor == 0x1AF4 && n.device == 0x1050
}
fn m_bridge(n: &DeviceNode) -> bool {
    n.class == 0x06 // host/PCI-bridges — geclaimd door de platform-laag
}

/// Bouw de device-tree uit de PCI-enumeratie + registreer + bind de drivers.
pub fn init() {
    let mut tree = DeviceTree::new();
    for d in pci::enumerate() {
        let nm = pci::device_name(d.vendor, d.device);
        let name = if nm.is_empty() { pci::class_name(d.class, d.subclass) } else { nm };
        let kind = match (d.class, d.subclass) {
            (0x01, 0x08) => DeviceKind::Nvme,
            (0x0C, 0x03) => DeviceKind::Usb,
            _ if d.vendor == 0x1AF4 => DeviceKind::VirtIo,
            _ => DeviceKind::Pci,
        };
        let res = DeviceResources {
            irq: Some(d.irq_line()),
            mmio: Vec::new(),
            io_port: None,
            bus_addr: ((d.bus as u32) << 16) | ((d.dev as u32) << 8) | (d.func as u32),
        };
        tree.add(
            DeviceNode::new(kind, name)
                .with_pci(d.vendor, d.device, d.class, d.subclass, d.prog_if)
                .with_resources(res),
        );
    }

    let mut reg = DriverRegistry::new();
    reg.register("virtio-blk", m_virtio_blk);
    reg.register("virtio-net", m_virtio_net);
    reg.register("euronvme", m_nvme);
    reg.register("xhci-usb", m_xhci);
    reg.register("euro-hda", m_hda);
    reg.register("virtio-gpu", m_virtio_gpu);
    reg.register("pci-bridge", m_bridge);
    let bound = reg.bind_all(&mut tree);

    crate::serial_println!(
        "[r] EuroDevice: {} apparaten in de device-tree, {} gebonden via {} geregistreerde drivers",
        tree.len() - 1, // minus de root
        bound,
        reg.len()
    );
    *TREE.lock() = Some(tree);
    *REGISTRY.lock() = Some(reg);
}

/// Produceer de `eurodevice probe`-uitvoer: één regel per apparaat met z'n gebonden
/// driver + toestand (voor de shell + het boot-zelftest).
pub fn probe_lines() -> Vec<String> {
    let mut out = Vec::new();
    let tg = TREE.lock();
    let rg = REGISTRY.lock();
    let (tree, reg) = match (tg.as_ref(), rg.as_ref()) {
        (Some(t), Some(r)) => (t, r),
        _ => {
            out.push(String::from("eurodevice: niet geïnitialiseerd"));
            return out;
        }
    };
    out.push(format!("EuroDevice — device-tree ({} apparaten):", tree.len() - 1));
    for n in tree.iter() {
        if n.kind == DeviceKind::Root {
            continue;
        }
        let drv = match n.driver {
            Some(id) => reg.driver_name(id).unwrap_or("?"),
            None => "—",
        };
        let state = match n.state {
            DeviceState::Bound => "gebonden",
            DeviceState::Unbound => "ongebonden",
            DeviceState::Failed => "gefaald",
            DeviceState::Suspended => "geschorst",
        };
        let b = n.resources.bus_addr;
        out.push(format!(
            "  {:02x}:{:02x}.{}  {:04x}:{:04x}  {:<22} → {:<12} [{}]",
            (b >> 16) & 0xFF,
            (b >> 8) & 0xFF,
            b & 0xFF,
            n.vendor,
            n.device,
            n.name,
            drv,
            state
        ));
    }
    out
}

/// Boot-zelftest: log de hele device-tree met bindingen (Sprint R-bewijs).
pub fn selftest() {
    for line in probe_lines() {
        crate::serial_println!("[r] {line}");
    }
}
