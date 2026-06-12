//! virtio-blk-pci legacy driver (Run 7 / doc §10) — een ECHTE blok-schijf.
//!
//! Zelfde legacy-virtio-aanpak als `virtio_net` (PIO via BAR0, split-ring
//! virtqueue), maar met één request-queue. Een blok-request is een keten van
//! drie descriptors: [16-byte header | 512-byte data | 1-byte status].
//! Hiermee kan EuroFS straks op een echte schijf staan → bestanden overleven
//! een herstart.

use core::sync::atomic::{compiler_fence, Ordering};

use euromm::FrameAllocator;
use x86_64::instructions::port::Port;

const VIRTIO_DEVICE_FEATURES: u16 = 0x00;
const VIRTIO_DRIVER_FEATURES: u16 = 0x04;
const VIRTIO_QUEUE_PFN: u16 = 0x08;
const VIRTIO_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_STATUS: u16 = 0x12;
const VIRTIO_BLK_CAPACITY: u16 = 0x14; // u64 capaciteit in sectoren

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

pub const SECTOR: usize = 512;
const VIRTIO_BLK_T_IN: u32 = 0; // lezen (device → ons)
const VIRTIO_BLK_T_OUT: u32 = 1; // schrijven (ons → device)
const VIRTIO_BLK_T_FLUSH: u32 = 4; // forceer device-cache → persistent medium
const VIRTIO_BLK_F_FLUSH: u32 = 1 << 9; // device ondersteunt het FLUSH-commando

#[repr(C)]
struct VqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

struct VirtQueue {
    size: u16,
    desc: u64,
    avail: u64,
    used: u64,
    last_used: u16,
}

impl VirtQueue {
    fn desc(&self, i: u16) -> *mut VqDesc {
        (self.desc + (i as u64) * 16) as *mut VqDesc
    }
    fn avail_idx_ptr(&self) -> *mut u16 {
        (self.avail + 2) as *mut u16
    }
    fn avail_ring(&self, i: u16) -> *mut u16 {
        (self.avail + 4 + (i as u64) * 2) as *mut u16
    }
    fn used_idx(&self) -> u16 {
        unsafe { ((self.used + 2) as *const u16).read_volatile() }
    }
}

pub struct VirtioBlk {
    io: u16,
    vq: VirtQueue,
    pub capacity_sectors: u64,
    hdr: u64,
    data: u64,
    status: u64,
    flush_ok: bool, // VIRTIO_BLK_F_FLUSH onderhandeld → het FLUSH-commando is geldig
}

/// Tot 4 virtio-blk-schijven (root + extra mounts). Index 0 = de eerste/root.
pub const MAX_BLK: usize = 4;
static mut BLKS: [Option<VirtioBlk>; MAX_BLK] = [None, None, None, None];

/// Geef een mutabele referentie naar schijf `dev` (None als afwezig).
unsafe fn dev_mut(dev: usize) -> Option<&'static mut VirtioBlk> {
    if dev >= MAX_BLK {
        return None;
    }
    (*core::ptr::addr_of_mut!(BLKS))[dev].as_mut()
}

fn setup_queue(io: u16, sel: u16, falloc: &mut FrameAllocator) -> Option<VirtQueue> {
    unsafe {
        Port::new(io + VIRTIO_QUEUE_SELECT).write(sel);
        let qsz: u16 = Port::new(io + VIRTIO_QUEUE_SIZE).read();
        // De driver gebruikt een vaste 3-descriptor-keten (hdr/data/status); een
        // device dat een kleinere queue adverteert zou OOB-descriptors laten
        // schrijven (audit C2). Weiger qsz < 3.
        if qsz < 3 {
            return None;
        }
        // Legacy split-ring: ÉÉN contiguë regio. De device berekent de used-ring
        // op `align(desc+avail, 4096)` vanaf de QUEUE_PFN-basis — dus dezelfde
        // layout aanhouden en genoeg aaneengesloten frames reserveren.
        let q = qsz as u64;
        let desc_sz = 16 * q;
        let avail_sz = 6 + 2 * q;
        let used_off = (desc_sz + avail_sz + 4095) & !4095;
        let used_sz = 6 + 8 * q;
        let total = used_off + used_sz;
        let frames = ((total + 4095) / 4096) as usize;
        let base = falloc.allocate().ok()?;
        for _ in 1..frames {
            falloc.allocate().ok()?; // reserveer de aaneengesloten vervolgframes
        }
        core::ptr::write_bytes(base as *mut u8, 0, frames * 4096);
        let desc = base;
        let avail = base + desc_sz;
        let used = base + used_off;
        Port::new(io + VIRTIO_QUEUE_PFN).write((base >> 12) as u32);
        Some(VirtQueue { size: qsz, desc, avail, used, last_used: 0 })
    }
}

/// Initialiseer ALLE virtio-blk-schijven (tot `MAX_BLK`). Geeft false als er geen
/// enkel apparaat is. Schijf 0 is de root; verdere schijven zijn extra mounts (B3).
pub fn init(falloc: &mut FrameAllocator) -> bool {
    let devs: alloc::vec::Vec<_> = crate::pci::enumerate()
        .into_iter()
        .filter(|d| d.vendor == 0x1AF4 && (d.device == 0x1001 || d.device == 0x1042))
        .collect();
    if devs.is_empty() {
        crate::serial_println!("[blk] geen virtio-blk apparaat gevonden");
        return false;
    }
    let mut count = 0;
    for d in devs.iter() {
        if count >= MAX_BLK {
            break;
        }
        if let Some(blk) = setup_device(d, falloc) {
            unsafe {
                (*core::ptr::addr_of_mut!(BLKS))[count] = Some(blk);
            }
            count += 1;
        }
    }
    crate::serial_println!("[blk] {count} virtio-blk schijf/schijven geïnitialiseerd");
    count > 0
}

/// Zet één virtio-blk-apparaat op (eigen virtqueue + bufferframes). None bij fout.
fn setup_device(dev: &crate::pci::PciDevice, falloc: &mut FrameAllocator) -> Option<VirtioBlk> {
    let io = (dev.bar(0) & 0xFFFC) as u16;
    dev.enable(0x5); // I/O + bus-master
    unsafe {
        let mut status: Port<u8> = Port::new(io + VIRTIO_STATUS);
        status.write(0);
        status.write(STATUS_ACK);
        status.write(STATUS_ACK | STATUS_DRIVER);
        // Onderhandel ALLEEN VIRTIO_BLK_F_FLUSH (als het device het biedt): dat geeft
        // ons een echt FLUSH-commando zodat EuroFS-checkpoints écht op het medium
        // belanden i.p.v. in de write-back-cache van de schijf. Verder geen features.
        let dev_features = Port::<u32>::new(io + VIRTIO_DEVICE_FEATURES).read();
        let flush_ok = dev_features & VIRTIO_BLK_F_FLUSH != 0;
        Port::<u32>::new(io + VIRTIO_DRIVER_FEATURES).write(if flush_ok { VIRTIO_BLK_F_FLUSH } else { 0 });

        let capacity_sectors: u64 = {
            let lo = Port::<u32>::new(io + VIRTIO_BLK_CAPACITY).read() as u64;
            let hi = Port::<u32>::new(io + VIRTIO_BLK_CAPACITY + 4).read() as u64;
            (hi << 32) | lo
        };

        let vq = match setup_queue(io, 0, falloc) {
            Some(q) => q,
            None => return None,
        };
        // Vaste bufferframes: header/status in frame 0, een volledig 4 KiB
        // data-frame zodat we tot 8 sectoren (één EuroFS-blok) per request doen.
        let frame = falloc.allocate().expect("blk-buf");
        let data_frame = falloc.allocate().expect("blk-data");
        let hdr = frame; // 16 B
        let status_buf = frame + 16; // 1 B
        let data = data_frame; // 4096 B

        // J2: MSI-X op de storage-controller. ADDITIEF — de used-ring-poll blijft de
        // completion-bevestiging; de IRQ bewijst interrupt-gedreven storage-completion.
        // Capaciteit is HIERBOVEN al gelezen (device-config @0x14, MSI-X nog uit); zodra
        // MSI-X aan staat schuift device-config naar 0x18 en zijn 0x14/0x16 de vector-
        // registers — we lezen daarna géén device-config meer, dus geen regressie.
        let msix_n = crate::msix::enable(dev, 0, crate::interrupts::VIRTIO_BLK_MSIX_VECTOR, crate::apic::lapic_id() as u8);
        if msix_n > 0 {
            Port::<u16>::new(io + 0x0E).write(0); // queue select 0
            Port::<u16>::new(io + 0x16).write(0); // queue_msix_vector = MSI-X-entry 0
            let rb: u16 = Port::<u16>::new(io + 0x16).read(); // 0xFFFF = NO_VECTOR (faalde)
            Port::<u16>::new(io + 0x14).write(0xFFFF); // config_msix_vector = NO_VECTOR
            crate::serial_println!(
                "[j2-blk] virtio-blk MSI-X aan ({} entries) → vector {:#x}, queue_msix_vector readback={:#06x}",
                msix_n, crate::interrupts::VIRTIO_BLK_MSIX_VECTOR, rb
            );
        }

        status.write(STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);
        crate::serial_println!(
            "[blk] virtio-blk OK — {} sectoren ({} MiB) @ BAR0 I/O={:#06x} · FLUSH {}",
            capacity_sectors,
            capacity_sectors * 512 / (1024 * 1024),
            io,
            if flush_ok { "aan (echte duurzaamheid)" } else { "n/b" }
        );
        Some(VirtioBlk { io, vq, capacity_sectors, hdr, data, status: status_buf, flush_ok })
    }
}

const DATA_MAX: usize = 4096; // max bytes per request = 8 sectoren = 1 EuroFS-blok

/// Eén blok-request van `nbytes` (veelvoud van 512, ≤ 4096) vanaf `sector`.
/// Bij schrijven kopieert de aanroeper eerst naar `blk.data`; bij lezen leest
/// de aanroeper er daarna uit.
unsafe fn submit(blk: &mut VirtioBlk, write: bool, sector: u64, nbytes: usize) -> bool {
    let dlen = ((nbytes + 511) / 512 * 512) as u32;
    (blk.hdr as *mut u32).write_volatile(if write { VIRTIO_BLK_T_OUT } else { VIRTIO_BLK_T_IN });
    ((blk.hdr + 4) as *mut u32).write_volatile(0);
    ((blk.hdr + 8) as *mut u64).write_volatile(sector);
    (blk.status as *mut u8).write_volatile(0xFF);

    let d0 = blk.vq.desc(0);
    (*d0).addr = blk.hdr;
    (*d0).len = 16;
    (*d0).flags = DESC_NEXT;
    (*d0).next = 1;
    let d1 = blk.vq.desc(1);
    (*d1).addr = blk.data;
    (*d1).len = dlen;
    (*d1).flags = if write { DESC_NEXT } else { DESC_NEXT | DESC_WRITE };
    (*d1).next = 2;
    let d2 = blk.vq.desc(2);
    (*d2).addr = blk.status;
    (*d2).len = 1;
    (*d2).flags = DESC_WRITE;
    (*d2).next = 0;

    kick_and_wait(blk)
}

/// Plaats descriptor 0 in de avail-ring, notify het device en wacht (busy) tot het
/// request in de used-ring verschijnt; geeft true bij status 0 (OK).
unsafe fn kick_and_wait(blk: &mut VirtioBlk) -> bool {
    let idx = blk.vq.avail_idx_ptr().read();
    blk.vq.avail_ring(idx % blk.vq.size).write(0);
    compiler_fence(Ordering::SeqCst);
    blk.vq.avail_idx_ptr().write(idx.wrapping_add(1));
    compiler_fence(Ordering::SeqCst);
    Port::<u16>::new(blk.io + VIRTIO_QUEUE_NOTIFY).write(0);

    for _ in 0..40_000_000 {
        if blk.vq.used_idx() != blk.vq.last_used {
            blk.vq.last_used = blk.vq.used_idx();
            return (blk.status as *const u8).read_volatile() == 0;
        }
        core::hint::spin_loop();
    }
    false
}

/// Stuur een VIRTIO_BLK_T_FLUSH: het device persisteert z'n write-back-cache naar
/// het medium. Een FLUSH-request heeft GEEN data-descriptor (enkel hdr + status).
unsafe fn submit_flush(blk: &mut VirtioBlk) -> bool {
    (blk.hdr as *mut u32).write_volatile(VIRTIO_BLK_T_FLUSH);
    ((blk.hdr + 4) as *mut u32).write_volatile(0);
    ((blk.hdr + 8) as *mut u64).write_volatile(0); // sector genegeerd bij FLUSH
    (blk.status as *mut u8).write_volatile(0xFF);

    let d0 = blk.vq.desc(0);
    (*d0).addr = blk.hdr;
    (*d0).len = 16;
    (*d0).flags = DESC_NEXT;
    (*d0).next = 1;
    let d1 = blk.vq.desc(1);
    (*d1).addr = blk.status;
    (*d1).len = 1;
    (*d1).flags = DESC_WRITE;
    (*d1).next = 0;

    kick_and_wait(blk)
}

/// Forceer dat alle eerder geschreven blokken op het PERSISTENTE medium staan (niet
/// enkel in de write-back-cache van de schijf). Geeft true bij succes; als het device
/// geen FLUSH-feature heeft (geen vluchtige cache) is dit een geslaagde no-op.
pub fn flush() -> bool {
    flush_dev(0)
}

/// FLUSH op een specifieke schijf.
pub fn flush_dev(dev: usize) -> bool {
    unsafe {
        let blk = match dev_mut(dev) {
            Some(b) => b,
            None => return false,
        };
        if !blk.flush_ok {
            return true; // geen onderhandelde FLUSH-feature → niets te doen
        }
        submit_flush(blk)
    }
}

/// Lees `buf.len()` bytes (≤ 4096, veelvoud van 512) vanaf `sector` van schijf `dev`.
pub fn read_io_dev(dev: usize, sector: u64, buf: &mut [u8]) -> bool {
    unsafe {
        let blk = match dev_mut(dev) {
            Some(b) => b,
            None => return false,
        };
        // Geen stille truncatie of out-of-range LBA (audit C3): weiger i.p.v. een
        // gedeeltelijke overdracht als succes te melden.
        let n = buf.len();
        if n > DATA_MAX || sector + (n as u64).div_ceil(512) > blk.capacity_sectors {
            return false;
        }
        if !submit(blk, false, sector, n) {
            return false;
        }
        core::ptr::copy_nonoverlapping(blk.data as *const u8, buf.as_mut_ptr(), n);
        true
    }
}

/// Schrijf `buf.len()` bytes (≤ 4096) vanaf `sector` naar schijf `dev`.
pub fn write_io_dev(dev: usize, sector: u64, buf: &[u8]) -> bool {
    unsafe {
        let blk = match dev_mut(dev) {
            Some(b) => b,
            None => return false,
        };
        // Geen stille truncatie of out-of-range LBA (audit C3).
        let n = buf.len();
        if n > DATA_MAX || sector + (n as u64).div_ceil(512) > blk.capacity_sectors {
            return false;
        }
        core::ptr::copy_nonoverlapping(buf.as_ptr(), blk.data as *mut u8, n);
        // De rest van de laatste sector nul-vullen.
        let dlen = (n + 511) / 512 * 512;
        if dlen > n {
            core::ptr::write_bytes((blk.data + n as u64) as *mut u8, 0, dlen - n);
        }
        submit(blk, true, sector, n)
    }
}

/// Lees/schrijf op schijf 0 (root) — backward-compat.
pub fn read_io(sector: u64, buf: &mut [u8]) -> bool {
    read_io_dev(0, sector, buf)
}
pub fn write_io(sector: u64, buf: &[u8]) -> bool {
    write_io_dev(0, sector, buf)
}

/// Lees/schrijf één 512-byte sector (voor de zelftest).
pub fn read_sector(sector: u64, buf: &mut [u8]) -> bool {
    read_io(sector, buf)
}
pub fn write_sector(sector: u64, buf: &[u8]) -> bool {
    write_io(sector, buf)
}

pub fn present() -> bool {
    present_dev(0)
}

/// Is schijf `dev` aanwezig?
pub fn present_dev(dev: usize) -> bool {
    unsafe { dev < MAX_BLK && (*core::ptr::addr_of!(BLKS))[dev].is_some() }
}

/// Aantal geïnitialiseerde virtio-blk-schijven.
pub fn device_count() -> usize {
    unsafe { (*core::ptr::addr_of!(BLKS)).iter().filter(|b| b.is_some()).count() }
}

/// Capaciteit van schijf 0 in 512-byte sectoren (0 als geen schijf).
pub fn capacity_sectors() -> u64 {
    capacity_sectors_dev(0)
}

/// Capaciteit van schijf `dev` in 512-byte sectoren.
pub fn capacity_sectors_dev(dev: usize) -> u64 {
    unsafe {
        if dev >= MAX_BLK {
            return 0;
        }
        (*core::ptr::addr_of!(BLKS))[dev].as_ref().map(|b| b.capacity_sectors).unwrap_or(0)
    }
}

/// Zelftest: schrijf een patroon naar een sector, lees het terug, verifieer.
pub fn self_test() {
    if !present() {
        return;
    }
    let sector = 2048u64; // ruim voorbij eventuele metadata
    let mut wbuf = [0u8; SECTOR];
    for (i, b) in wbuf.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    let w = write_sector(sector, &wbuf);
    let mut rbuf = [0u8; SECTOR];
    let r = read_sector(sector, &mut rbuf);
    let ok = w && r && rbuf == wbuf;
    crate::serial_println!(
        "[blk] zelftest sector {}: schrijf={} lees={} data-match={} -> {}",
        sector, w, r, rbuf == wbuf, if ok { "OK (echte schijf werkt!)" } else { "MISLUKT" }
    );
}
