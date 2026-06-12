//! EuroDevice — een **unified device model + driver-framework** (Sprint R).
//!
//! Tot nu toe zijn PCI, NVMe, VirtIO, xHCI, HDA en netwerk-drivers los van elkaar
//! gebouwd: elke driver hervindt het wiel voor discovery, binding, lifecycle en
//! foutafhandeling. Zonder een gemeenschappelijk device-model wordt de codebase
//! onhoudbaar zodra WiFi, GPU en USB-hubs erbij komen.
//!
//! Deze crate is de architecturale basis: een **`DeviceTree`** (parent/child-boom
//! van [`DeviceNode`]s met stabiele [`DeviceId`]-handles), een **`DriverRegistry`**
//! die drivers op apparaten matcht en bindt, een **`trait Driver`**-lifecycle
//! (start/stop/suspend/resume), en een **hotplug-event-queue**. De bus-laag (PCI/
//! VirtIO/platform) vult de boom; de kernel levert de echte `probe`-implementaties.
//!
//! Pure `no_std`-logica (boom + matching + binding + hotplug) → volledig host-getest,
//! los van enige hardware.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

/// Stabiele handle naar een apparaat in de [`DeviceTree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(pub u64);

/// Stabiele handle naar een geregistreerde driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverId(pub u64);

/// Het soort bus/apparaat (bepaalt hoe het ontdekt + geadresseerd wordt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Root,
    Pci,
    VirtIo,
    Nvme,
    Usb,
    Platform,
    Other,
}

/// De bindings-toestand van een apparaat in zijn lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Unbound,
    Bound,
    Failed,
    Suspended,
}

/// De fysieke resources die een apparaat aan een driver aanbiedt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceResources {
    pub irq: Option<u8>,
    pub mmio: Vec<(u64, u64)>, // (basis, lengte)
    pub io_port: Option<u16>,
    /// Bus-specifiek adres (PCI: (bus<<16)|(dev<<8)|func; platform: 0).
    pub bus_addr: u32,
}

/// Een node in de device-tree: één ontdekt apparaat.
#[derive(Debug, Clone)]
pub struct DeviceNode {
    pub id: DeviceId,
    pub kind: DeviceKind,
    pub name: String,
    // PCI-identificatie (0 voor niet-PCI) — gebruikt door driver-matchers.
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub parent: Option<DeviceId>,
    pub children: Vec<DeviceId>,
    pub state: DeviceState,
    pub driver: Option<DriverId>,
    pub resources: DeviceResources,
}

impl DeviceNode {
    /// Bouw een kale node (id wordt door de tree toegekend in [`DeviceTree::add`]).
    pub fn new(kind: DeviceKind, name: &str) -> DeviceNode {
        DeviceNode {
            id: DeviceId(0),
            kind,
            name: String::from(name),
            vendor: 0,
            device: 0,
            class: 0,
            subclass: 0,
            prog_if: 0,
            parent: None,
            children: Vec::new(),
            state: DeviceState::Unbound,
            driver: None,
            resources: DeviceResources::default(),
        }
    }

    /// Vul de PCI-identificatie (voor matching).
    pub fn with_pci(mut self, vendor: u16, device: u16, class: u8, subclass: u8, prog_if: u8) -> Self {
        self.vendor = vendor;
        self.device = device;
        self.class = class;
        self.subclass = subclass;
        self.prog_if = prog_if;
        self
    }

    pub fn with_resources(mut self, res: DeviceResources) -> Self {
        self.resources = res;
        self
    }
}

/// De device-tree: alle ontdekte apparaten + hun parent/child-relaties.
pub struct DeviceTree {
    nodes: BTreeMap<u64, DeviceNode>,
    next_id: u64,
    root: DeviceId,
}

impl Default for DeviceTree {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceTree {
    /// Maak een verse tree met één root-node.
    pub fn new() -> DeviceTree {
        let mut nodes = BTreeMap::new();
        let root = DeviceId(1);
        let mut rn = DeviceNode::new(DeviceKind::Root, "euro-root");
        rn.id = root;
        rn.state = DeviceState::Bound;
        nodes.insert(1, rn);
        DeviceTree { nodes, next_id: 2, root }
    }

    pub fn root(&self) -> DeviceId {
        self.root
    }

    /// Voeg een node toe (als kind van root tenzij `parent` gezet is). Geeft de
    /// toegekende [`DeviceId`].
    pub fn add(&mut self, mut node: DeviceNode) -> DeviceId {
        let id = DeviceId(self.next_id);
        self.next_id += 1;
        node.id = id;
        let parent = node.parent.unwrap_or(self.root);
        node.parent = Some(parent);
        self.nodes.insert(id.0, node);
        if let Some(p) = self.nodes.get_mut(&parent.0) {
            p.children.push(id);
        }
        id
    }

    pub fn get(&self, id: DeviceId) -> Option<&DeviceNode> {
        self.nodes.get(&id.0)
    }
    pub fn get_mut(&mut self, id: DeviceId) -> Option<&mut DeviceNode> {
        self.nodes.get_mut(&id.0)
    }

    /// Herhang `child` onder `parent` (corrigeert beide child-lijsten).
    pub fn set_parent(&mut self, child: DeviceId, parent: DeviceId) {
        let old_parent = self.nodes.get(&child.0).and_then(|n| n.parent);
        if let Some(op) = old_parent {
            if let Some(opn) = self.nodes.get_mut(&op.0) {
                opn.children.retain(|c| *c != child);
            }
        }
        if let Some(cn) = self.nodes.get_mut(&child.0) {
            cn.parent = Some(parent);
        }
        if let Some(pn) = self.nodes.get_mut(&parent.0) {
            if !pn.children.contains(&child) {
                pn.children.push(child);
            }
        }
    }

    /// Aantal apparaten (inclusief root).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Itereer over alle nodes (volgorde = oplopende id).
    pub fn iter(&self) -> impl Iterator<Item = &DeviceNode> {
        self.nodes.values()
    }

    /// Alle nog-niet-gebonden node-id's (voor `bind_all`).
    pub fn unbound(&self) -> Vec<DeviceId> {
        self.nodes
            .values()
            .filter(|n| n.state == DeviceState::Unbound)
            .map(|n| n.id)
            .collect()
    }
}

/// Een geregistreerde driver: een naam + een **match-predicaat** dat bepaalt welke
/// apparaten hij claimt. (De echte `probe`/lifecycle leeft kernel-side achter
/// [`trait Driver`]; de registry doet de matching.)
#[derive(Clone)]
pub struct DriverDescriptor {
    pub id: DriverId,
    pub name: &'static str,
    pub matches: fn(&DeviceNode) -> bool,
}

/// De lifecycle die een gebonden driver implementeert (kernel-side).
pub trait Driver: Send {
    fn start(&mut self) -> Result<(), DeviceError>;
    fn stop(&mut self) -> Result<(), DeviceError>;
    fn suspend(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
    fn resume(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

/// Foutsoorten in de device-/driver-laag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    NoMatch,
    ProbeFailed,
    AlreadyBound,
    NotFound,
}

/// De driver-registry: houdt alle bekende drivers + bindt ze aan apparaten.
#[derive(Default)]
pub struct DriverRegistry {
    drivers: Vec<DriverDescriptor>,
    next_id: u64,
}

impl DriverRegistry {
    pub fn new() -> DriverRegistry {
        DriverRegistry { drivers: Vec::new(), next_id: 1 }
    }

    /// Registreer een driver met z'n match-predicaat. Geeft de [`DriverId`].
    pub fn register(&mut self, name: &'static str, matches: fn(&DeviceNode) -> bool) -> DriverId {
        let id = DriverId(self.next_id);
        self.next_id += 1;
        self.drivers.push(DriverDescriptor { id, name, matches });
        id
    }

    pub fn driver_name(&self, id: DriverId) -> Option<&'static str> {
        self.drivers.iter().find(|d| d.id == id).map(|d| d.name)
    }

    pub fn len(&self) -> usize {
        self.drivers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.drivers.is_empty()
    }

    /// Bind de eerste passende driver aan `node`. Markeert de node `Bound` + zet
    /// `driver`. Geeft de gekozen [`DriverId`] (of None als niets matcht).
    pub fn bind(&mut self, tree: &mut DeviceTree, node_id: DeviceId) -> Option<DriverId> {
        let node = tree.get(node_id)?;
        if node.state == DeviceState::Bound {
            return node.driver;
        }
        let chosen = self.drivers.iter().find(|d| (d.matches)(node)).map(|d| d.id);
        if let Some(drv) = chosen {
            if let Some(n) = tree.get_mut(node_id) {
                n.driver = Some(drv);
                n.state = DeviceState::Bound;
            }
        }
        chosen
    }

    /// Bind elke nog-ongebonden node; geeft het aantal nieuw-gebonden apparaten.
    pub fn bind_all(&mut self, tree: &mut DeviceTree) -> usize {
        let mut bound = 0;
        for id in tree.unbound() {
            if self.bind(tree, id).is_some() {
                bound += 1;
            }
        }
        bound
    }

    /// Ontbind een apparaat (driver weg, terug naar `Unbound`).
    pub fn unbind(&mut self, tree: &mut DeviceTree, node_id: DeviceId) {
        if let Some(n) = tree.get_mut(node_id) {
            n.driver = None;
            n.state = DeviceState::Unbound;
        }
    }
}

/// Een hotplug-gebeurtenis op de bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotplugEvent {
    Attached(DeviceId),
    Detached(DeviceId),
    Failed(DeviceId),
}

/// Een eenvoudige FIFO-event-queue (kernel-side wordt dit een lock-vrije ring,
/// geconsumeerd door de scheduler of een device-fd).
#[derive(Default)]
pub struct HotplugQueue {
    events: VecDeque<HotplugEvent>,
}

impl HotplugQueue {
    pub fn new() -> HotplugQueue {
        HotplugQueue { events: VecDeque::new() }
    }
    pub fn push(&mut self, ev: HotplugEvent) {
        self.events.push_back(ev);
    }
    pub fn pop(&mut self) -> Option<HotplugEvent> {
        self.events.pop_front()
    }
    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Voorbeeld-matchers (zoals de echte kernel-drivers ze leveren).
    fn match_virtio_blk(n: &DeviceNode) -> bool {
        n.vendor == 0x1AF4 && (n.device == 0x1001 || n.device == 0x1042)
    }
    fn match_nvme(n: &DeviceNode) -> bool {
        n.class == 0x01 && n.subclass == 0x08
    }
    fn match_xhci(n: &DeviceNode) -> bool {
        n.class == 0x0C && n.subclass == 0x03 && n.prog_if == 0x30
    }

    fn sample_tree() -> DeviceTree {
        let mut t = DeviceTree::new();
        t.add(DeviceNode::new(DeviceKind::VirtIo, "virtio-blk").with_pci(0x1AF4, 0x1001, 0x01, 0x00, 0x00));
        t.add(DeviceNode::new(DeviceKind::Nvme, "nvme").with_pci(0x8086, 0x5845, 0x01, 0x08, 0x02));
        t.add(DeviceNode::new(DeviceKind::Usb, "xhci").with_pci(0x1B36, 0x000D, 0x0C, 0x03, 0x30));
        t
    }

    #[test]
    fn tree_add_and_parent() {
        let mut t = DeviceTree::new();
        let a = t.add(DeviceNode::new(DeviceKind::Pci, "bridge"));
        let mut child = DeviceNode::new(DeviceKind::Usb, "usb-kbd");
        child.parent = Some(a);
        let c = t.add(child);
        assert_eq!(t.get(c).unwrap().parent, Some(a));
        assert!(t.get(a).unwrap().children.contains(&c));
        assert_eq!(t.len(), 3); // root + 2
    }

    #[test]
    fn set_parent_reparents() {
        let mut t = DeviceTree::new();
        let a = t.add(DeviceNode::new(DeviceKind::Pci, "a"));
        let b = t.add(DeviceNode::new(DeviceKind::Pci, "b"));
        let c = t.add(DeviceNode::new(DeviceKind::Usb, "c")); // onder root
        t.set_parent(c, a);
        assert!(t.get(a).unwrap().children.contains(&c));
        t.set_parent(c, b);
        assert!(t.get(b).unwrap().children.contains(&c));
        assert!(!t.get(a).unwrap().children.contains(&c)); // niet meer dubbel
        let root_kids = t.get(t.root()).unwrap().children.len();
        assert_eq!(root_kids, 2); // a en b (c hangt nu onder b)
    }

    #[test]
    fn registry_binds_matching_drivers() {
        let mut t = sample_tree();
        let mut reg = DriverRegistry::new();
        reg.register("virtio-blk", match_virtio_blk);
        reg.register("nvme", match_nvme);
        reg.register("xhci", match_xhci);
        let bound = reg.bind_all(&mut t);
        assert_eq!(bound, 3);
        // Elk apparaat heeft de juiste driver.
        for n in t.iter() {
            match n.name.as_str() {
                "virtio-blk" => assert_eq!(reg.driver_name(n.driver.unwrap()), Some("virtio-blk")),
                "nvme" => assert_eq!(reg.driver_name(n.driver.unwrap()), Some("nvme")),
                "xhci" => assert_eq!(reg.driver_name(n.driver.unwrap()), Some("xhci")),
                _ => {} // root: ongebonden / geen match
            }
        }
    }

    #[test]
    fn unmatched_device_stays_unbound() {
        let mut t = DeviceTree::new();
        let id = t.add(DeviceNode::new(DeviceKind::Pci, "mystery").with_pci(0xDEAD, 0xBEEF, 0xFF, 0xFF, 0xFF));
        let mut reg = DriverRegistry::new();
        reg.register("nvme", match_nvme);
        assert_eq!(reg.bind(&mut t, id), None);
        assert_eq!(t.get(id).unwrap().state, DeviceState::Unbound);
    }

    #[test]
    fn unbind_returns_to_unbound() {
        let mut t = sample_tree();
        let mut reg = DriverRegistry::new();
        reg.register("nvme", match_nvme);
        let nvme_id = t.iter().find(|n| n.name == "nvme").unwrap().id;
        assert!(reg.bind(&mut t, nvme_id).is_some());
        assert_eq!(t.get(nvme_id).unwrap().state, DeviceState::Bound);
        reg.unbind(&mut t, nvme_id);
        assert_eq!(t.get(nvme_id).unwrap().state, DeviceState::Unbound);
        assert!(t.get(nvme_id).unwrap().driver.is_none());
    }

    #[test]
    fn hotplug_queue_fifo() {
        let mut q = HotplugQueue::new();
        q.push(HotplugEvent::Attached(DeviceId(5)));
        q.push(HotplugEvent::Detached(DeviceId(5)));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(HotplugEvent::Attached(DeviceId(5))));
        assert_eq!(q.pop(), Some(HotplugEvent::Detached(DeviceId(5))));
        assert!(q.is_empty());
    }
}
