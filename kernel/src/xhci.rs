//! xHCI (USB 3.x host controller) driver — plan I1, REAL USB-HID input.
//!
//! Modern machines no longer have a PS/2 port; without a USB stack EuroOS cannot
//! get keyboard/mouse input on real hardware. This is the full
//! hardware layer beneath the host-tested [`eurousb`] parser core: we talk to the
//! xHCI controller via its MMIO registers, set up the **command ring**, **event ring**
//! and **device-context array**, reset the controller, and run the real
//! USB enumeration of each root-port device:
//!
//!   Enable Slot → Address Device → GET_DESCRIPTOR(device) → GET_DESCRIPTOR(config)
//!   → SET_CONFIGURATION → Configure Endpoint → SET_PROTOCOL(boot) → interrupt-IN poll.
//!
//! The 8-byte HID boot reports from the interrupt endpoint are decoded by
//! [`eurousb::BootKeyboard`] / [`eurousb::parse_mouse`] and pushed into the same
//! input paths as PS/2 ([`crate::ps2::push_scancode`] + [`crate::mouse`]),
//! so that the shell and desktop transparently work over USB.
//!
//! All DMA structures come from the identity-mapped frame allocator (virtual =
//! physical < 512 GiB), so the physical addresses the controller reads are exactly
//! the same pointers we use.

use euromm::FrameAllocator;

use crate::pci;

// ── Capability register offsets (from the MMIO base) ───────────────────────
const CAP_CAPLENGTH: u64 = 0x00; // u8 (+ HCIVERSION u16 @ 0x02)
const CAP_HCSPARAMS1: u64 = 0x04;
const CAP_HCSPARAMS2: u64 = 0x08;
const CAP_HCCPARAMS1: u64 = 0x10;
const CAP_DBOFF: u64 = 0x14;
const CAP_RTSOFF: u64 = 0x18;

// ── Operational registers (from op_base = mmio + CAPLENGTH) ─────────────────
const OP_USBCMD: u64 = 0x00;
const OP_USBSTS: u64 = 0x04;
const OP_CRCR: u64 = 0x18; // 64-bit command-ring control
const OP_DCBAAP: u64 = 0x30; // 64-bit device-context-base-array-pointer
const OP_CONFIG: u64 = 0x38;
const OP_PORTSC_BASE: u64 = 0x400; // port 1 at +0x400, port n at +0x400+(n-1)*0x10

const USBCMD_RS: u32 = 1 << 0; // run/stop
const USBCMD_HCRST: u32 = 1 << 1; // host-controller reset
const USBCMD_INTE: u32 = 1 << 2; // interrupter enable
const USBSTS_HCH: u32 = 1 << 0; // HC halted
const USBSTS_CNR: u32 = 1 << 11; // controller not ready

// ── Runtime / interrupter-0 registers (from rt_base = mmio + RTSOFF) ────────
const RT_IR0: u64 = 0x20; // interrupter 0
const IR_IMAN: u64 = 0x00;
const IR_IMOD: u64 = 0x04; // interrupt-moderation interval (0 = no moderation)
const IR_ERSTSZ: u64 = 0x08;
const IR_ERSTBA: u64 = 0x10; // 64-bit
const IR_ERDP: u64 = 0x18; // 64-bit

// ── TRB types ────────────────────────────────────────────────────────────────
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_EVT_TRANSFER: u32 = 32;
const TRB_EVT_CMD_COMPLETE: u32 = 33;

const CC_SUCCESS: u32 = 1;

const RING_TRBS: u16 = 256; // one frame = 256 × 16 byte
const RING_BYTES: usize = RING_TRBS as usize * 16;

// ── Low-level MMIO helpers (identity-mapped physical) ──────────────────────
#[inline]
unsafe fn r32(addr: u64) -> u32 {
    (addr as *const u32).read_volatile()
}
#[inline]
unsafe fn w32(addr: u64, v: u32) {
    (addr as *mut u32).write_volatile(v);
}
#[inline]
unsafe fn w64(addr: u64, v: u64) {
    // xHCI registers may be 64-bit, but we split lo/hi for 32-bit-safe MMIO.
    (addr as *mut u32).write_volatile(v as u32);
    ((addr + 4) as *mut u32).write_volatile((v >> 32) as u32);
}

/// A TRB ring (command or transfer): one frame, the last TRB is a Link back
/// to the start with the Toggle-Cycle bit. We are the producer (cycle bit).
struct Ring {
    base: u64,
    enqueue: u16,
    cycle: u8,
}

impl Ring {
    /// Create an empty ring on a fresh, zeroed frame; set up the Link-TRB.
    fn new(frame: u64) -> Ring {
        unsafe {
            core::ptr::write_bytes(frame as *mut u8, 0, RING_BYTES);
            // Link-TRB at the last index: param = ring base, type = Link, TC bit.
            let link = frame + (RING_TRBS as u64 - 1) * 16;
            w64(link, frame);
            w32(link + 8, 0);
            w32(link + 12, (TRB_LINK << 10) | (1 << 1)); // type + Toggle-Cycle
        }
        Ring { base: frame, enqueue: 0, cycle: 1 }
    }

    /// Place a TRB (control without cycle bit) and return the physical address.
    fn push(&mut self, p_lo: u32, p_hi: u32, status: u32, control: u32) -> u64 {
        let trb = self.base + self.enqueue as u64 * 16;
        unsafe {
            w32(trb, p_lo);
            w32(trb + 4, p_hi);
            w32(trb + 8, status);
            w32(trb + 12, control | self.cycle as u32);
        }
        self.enqueue += 1;
        if self.enqueue == RING_TRBS - 1 {
            // We've reached the Link-TRB: set its cycle bit to the current cycle,
            // then toggle our producer cycle and wrap back to the start.
            let link = self.base + (RING_TRBS as u64 - 1) * 16;
            unsafe {
                let c = r32(link + 12) & !1;
                w32(link + 12, c | self.cycle as u32);
            }
            self.cycle ^= 1;
            self.enqueue = 0;
        }
        trb
    }
}

/// The main controller state (one global xHCI controller suffices for QEMU).
struct Xhci {
    op: u64,
    rt: u64,
    db: u64,
    max_ports: u8,
    ctx_size: u64, // 32 or 64 byte (HCCPARAMS1.CSZ)
    dcbaa: u64,
    cmd: Ring,
    ev_seg: u64,
    ev_deq: u16,
    ev_cycle: u8,
}

static mut XHCI: Option<Xhci> = None;
/// Number of HID reports logged to serial (diagnostics; capped against spam).
static mut REPORTS_LOGGED: u32 = 0;
/// Whether the MSI-X delivery confirmation has already been logged.
static mut MSIX_LOGGED: bool = false;
/// Re-entrancy guard: `poll()` may NOT run simultaneously from the desktop loop and the MSI-X
/// IRQ handler (that would corrupt the event ring + scancode Mutex). Whoever
/// sets it to true harvests; the other bails (the winner drains everything anyway).
static POLLING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Up to 2 HID devices (keyboard + mouse): we poll their interrupt-IN endpoint.
const MAX_HID: usize = 4;
static mut HIDS: [Option<HidDevice>; MAX_HID] = [None, None, None, None];

/// An enumerated HID boot device that we poll live.
struct HidDevice {
    slot: u8,
    ep_dci: u8, // doorbell target (device-context-index) of the interrupt-IN endpoint
    ring: Ring,
    buf: u64, // 8-byte report buffer (interrupt transfers land here)
    is_keyboard: bool,
    is_abs_pointer: bool, // usb-tablet / touchscreen: absolute X/Y report
    kb: eurousb::BootKeyboard,
    prev_mods: u8,
    armed: bool,
}

#[inline]
unsafe fn op_r(x: &Xhci, off: u64) -> u32 {
    r32(x.op + off)
}
#[inline]
unsafe fn op_w(x: &Xhci, off: u64, v: u32) {
    w32(x.op + off, v);
}

/// Ring the doorbell of `slot` with `target` (0 = command ring; DCI for endpoints).
#[inline]
unsafe fn doorbell(x: &Xhci, slot: u8, target: u32) {
    w32(x.db + slot as u64 * 4, target);
}

/// Poll the event ring for the next valid event; busy-wait until `tries` runs out.
/// Returns the 4 TRB dwords (param-lo, param-hi, status, control).
unsafe fn wait_event(x: &mut Xhci, want_type: u32, tries: u32) -> Option<[u32; 4]> {
    for _ in 0..tries {
        let trb = x.ev_seg + x.ev_deq as u64 * 16;
        let ctrl = r32(trb + 12);
        if (ctrl & 1) == x.ev_cycle as u32 {
            let out = [r32(trb), r32(trb + 4), r32(trb + 8), ctrl];
            // Dequeue forward (one segment of RING_TRBS).
            x.ev_deq += 1;
            if x.ev_deq == RING_TRBS {
                x.ev_deq = 0;
                x.ev_cycle ^= 1;
            }
            // Update ERDP (+ clear EHB bit by writing bit3=1).
            let erdp = x.ev_seg + x.ev_deq as u64 * 16;
            w64(x.rt + RT_IR0 + IR_ERDP, erdp | (1 << 3));
            let ttype = (ctrl >> 10) & 0x3F;
            if ttype == want_type {
                return Some(out);
            }
            // Other event type (e.g. Port Status Change) — skip and keep reading.
            continue;
        }
        core::hint::spin_loop();
    }
    None
}

/// Place a command TRB, ring the command doorbell and wait for the completion event.
/// Returns (completion code, slot id).
unsafe fn run_command(x: &mut Xhci, p_lo: u32, p_hi: u32, control: u32) -> Option<(u32, u8)> {
    x.cmd.push(p_lo, p_hi, 0, control);
    doorbell(x, 0, 0);
    let ev = wait_event(x, TRB_EVT_CMD_COMPLETE, 20_000_000)?;
    let cc = (ev[2] >> 24) & 0xFF;
    let slot = (ev[3] >> 24) & 0xFF;
    Some((cc, slot as u8))
}

/// Detect + initialize the xHCI controller and enumerate all HID devices.
pub fn init(falloc: &mut FrameAllocator) -> bool {
    let dev = match pci::find(|d| d.class == 0x0C && d.subclass == 0x03 && d.prog_if == 0x30) {
        Some(d) => d,
        None => {
            crate::serial_println!("[xhci] no xHCI controller found (PCI 0C:03:30)");
            return false;
        }
    };
    // 64-bit MMIO-BAR0 (BAR0 lo + BAR1 hi); mask type bits.
    let bar0 = dev.bar(0);
    let mmio = if bar0 & 0x6 == 0x4 {
        ((dev.bar(1) as u64) << 32) | (bar0 as u64 & 0xFFFF_FFF0)
    } else {
        bar0 as u64 & 0xFFFF_FFF0
    };
    if mmio == 0 {
        crate::serial_println!("[xhci] BAR0 not assigned");
        return false;
    }
    dev.enable(0x6); // memory-space + bus-master

    unsafe {
        let caplen = (r32(mmio + CAP_CAPLENGTH) & 0xFF) as u64;
        let hcsp1 = r32(mmio + CAP_HCSPARAMS1);
        let hcsp2 = r32(mmio + CAP_HCSPARAMS2);
        let hccp1 = r32(mmio + CAP_HCCPARAMS1);
        let max_slots = (hcsp1 & 0xFF) as u8;
        let max_ports = ((hcsp1 >> 24) & 0xFF) as u8;
        let ctx_size: u64 = if hccp1 & (1 << 2) != 0 { 64 } else { 32 };
        let dboff = (r32(mmio + CAP_DBOFF) & !0x3) as u64;
        let rtsoff = (r32(mmio + CAP_RTSOFF) & !0x1F) as u64;
        let op = mmio + caplen;
        let rt = mmio + rtsoff;
        let db = mmio + dboff;
        crate::serial_println!(
            "[xhci] controller @ {:#x} — {} slots, {} ports, ctx={}B, op+{:#x} rt+{:#x} db+{:#x}",
            mmio, max_slots, max_ports, ctx_size, caplen, rtsoff, dboff
        );

        // 1. Reset: stop, wait for HCH, set HCRST, wait until HCRST + CNR clear.
        let mut cmd = r32(op + OP_USBCMD);
        cmd &= !USBCMD_RS;
        w32(op + OP_USBCMD, cmd);
        for _ in 0..10_000_000 {
            if r32(op + OP_USBSTS) & USBSTS_HCH != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        w32(op + OP_USBCMD, USBCMD_HCRST);
        for _ in 0..10_000_000 {
            if r32(op + OP_USBCMD) & USBCMD_HCRST == 0 && r32(op + OP_USBSTS) & USBSTS_CNR == 0 {
                break;
            }
            core::hint::spin_loop();
        }

        // 2. Device-Context-Base-Address-Array (one frame, 256 × u64 pointers).
        let dcbaa = falloc.allocate().expect("xhci dcbaa");
        core::ptr::write_bytes(dcbaa as *mut u8, 0, 4096);

        // 3. Scratchpad buffers (if the controller requires them): DCBAA[0] = array.
        let max_scratch = (((hcsp2 >> 21) & 0x1F) << 5) | ((hcsp2 >> 27) & 0x1F);
        if max_scratch > 0 {
            let arr = falloc.allocate().expect("xhci scratch-array");
            core::ptr::write_bytes(arr as *mut u8, 0, 4096);
            for i in 0..max_scratch as u64 {
                let pg = falloc.allocate().expect("xhci scratch-page");
                core::ptr::write_bytes(pg as *mut u8, 0, 4096);
                w64(arr + i * 8, pg);
            }
            w64(dcbaa, arr); // slot 0 = scratchpad array
        }
        w64(op + OP_DCBAAP, dcbaa);

        // 4. Command ring.
        let cmd_frame = falloc.allocate().expect("xhci cmd-ring");
        let cmd_ring = Ring::new(cmd_frame);
        w64(op + OP_CRCR, cmd_frame | 1); // RCS = 1

        // 5. Event ring: one segment + an ERST with one entry.
        let ev_seg = falloc.allocate().expect("xhci ev-seg");
        core::ptr::write_bytes(ev_seg as *mut u8, 0, RING_BYTES);
        let erst = falloc.allocate().expect("xhci erst");
        core::ptr::write_bytes(erst as *mut u8, 0, 4096);
        w64(erst, ev_seg); // ring-segment base address
        w32(erst + 8, RING_TRBS as u32); // segment size (number of TRBs)
        w32(rt + RT_IR0 + IR_ERSTSZ, 1); // one segment
        w64(rt + RT_IR0 + IR_ERDP, ev_seg | (1 << 3)); // dequeue pointer
        w64(rt + RT_IR0 + IR_ERSTBA, erst); // ERST base (activates the event ring)
        w32(rt + RT_IR0 + IR_IMOD, 0); // no interrupt moderation (deliver immediately)
        w32(rt + RT_IR0 + IR_IMAN, 0x2); // interrupt-pending clear + IE

        // 6. Enable max-slots + let the controller run.
        w32(op + OP_CONFIG, max_slots as u32);
        let mut c = r32(op + OP_USBCMD);
        c |= USBCMD_RS | USBCMD_INTE;
        w32(op + OP_USBCMD, c);

        XHCI = Some(Xhci {
            op,
            rt,
            db,
            max_ports,
            ctx_size,
            dcbaa,
            cmd: cmd_ring,
            ev_seg,
            ev_deq: 0,
            ev_cycle: 1,
        });

        // J2: program MSI-X so that the event-ring interrupter sends its interrupt as
        // a message to a LAPIC vector (instead of shared INTx). The event
        // harvest is still done by the non-re-entrant desktop-loop `poll()`; the IRQ
        // proves the MSI-X delivery (J2 foundation for virtio-blk/NVMe completion).
        let nvec = crate::msix::enable(&dev, 0, crate::interrupts::XHCI_MSIX_VECTOR, crate::apic::lapic_id() as u8);
        if nvec > 0 {
            crate::serial_println!(
                "[xhci] MSI-X on: {nvec} table entries, interrupter 0 → vector {:#x}",
                crate::interrupts::XHCI_MSIX_VECTOR
            );
        }
    }

    // 7. Enumerate each connected root-port device.
    let mut found = 0;
    unsafe {
        let ports = (*core::ptr::addr_of!(XHCI)).as_ref().unwrap().max_ports;
        for port in 1..=ports {
            if enumerate_port(falloc, port) {
                found += 1;
            }
        }
    }
    // Reset the interrupter clean (clear IP + EINT) so that the FIRST event after
    // enabling interrupts gives a fresh MSI-X edge.
    unsafe {
        if let Some(x) = (*core::ptr::addr_of_mut!(XHCI)).as_ref() {
            w32(x.op + OP_USBSTS, 1 << 3);
            w32(x.rt + RT_IR0 + IR_IMAN, 0x3);
        }
    }
    crate::serial_println!("[xhci] enumeration done — {found} HID device(s) polled live");
    found > 0
}

/// PORTSC address of port `port` (1-based).
#[inline]
unsafe fn portsc(x: &Xhci, port: u8) -> u64 {
    x.op + OP_PORTSC_BASE + (port as u64 - 1) * 0x10
}

/// Write PORTSC preserving Port-Power (bit9); `set_bits` are OR'd in.
unsafe fn portsc_write(x: &Xhci, port: u8, set_bits: u32) {
    let addr = portsc(x, port);
    let cur = r32(addr);
    w32(addr, (cur & (1 << 9)) | set_bits);
}

/// Enumerate one port: reset (if needed), Enable Slot, Address Device, read the
/// descriptors, configure the HID interrupt endpoint and arm the first poll.
unsafe fn enumerate_port(falloc: &mut FrameAllocator, port: u8) -> bool {
    let xp = core::ptr::addr_of_mut!(XHCI);
    let x = (*xp).as_mut().unwrap();
    let psc = r32(portsc(x, port));
    if psc & 1 == 0 {
        return false; // nothing connected (CCS=0)
    }
    // USB2 devices must go through a port reset; USB3 enables itself. If PED is still
    // 0: trigger reset (bit4) and wait until PED (bit1) goes high.
    if psc & 2 == 0 {
        portsc_write(x, port, 1 << 4); // PR
        let mut ok = false;
        for _ in 0..10_000_000 {
            let v = r32(portsc(x, port));
            if v & 2 != 0 {
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }
        // Acknowledge reset-change (PRC bit21) + connect-change (CSC bit17).
        portsc_write(x, port, (1 << 21) | (1 << 17));
        if !ok {
            crate::serial_println!("[xhci] port {port}: reset timeout");
            return false;
        }
    }
    let psc = r32(portsc(x, port));
    let speed = (psc >> 10) & 0xF;
    let max_pkt0: u16 = match speed {
        2 => 8,    // Low-speed
        4 | 5 => 512, // Super(Plus)-speed
        _ => 64,   // Full/High-speed
    };

    // Enable Slot.
    let (cc, slot) = match run_command(x, 0, 0, TRB_ENABLE_SLOT << 10) {
        Some(v) => v,
        None => {
            crate::serial_println!("[xhci] port {port}: Enable-Slot timeout");
            return false;
        }
    };
    if cc != CC_SUCCESS || slot == 0 {
        crate::serial_println!("[xhci] port {port}: Enable-Slot cc={cc}");
        return false;
    }

    // Device context (output) + input context. Both one frame, zeroed.
    let dev_ctx = falloc.allocate().expect("xhci dev-ctx");
    core::ptr::write_bytes(dev_ctx as *mut u8, 0, 4096);
    w64(x.dcbaa + slot as u64 * 8, dev_ctx);

    let in_ctx = falloc.allocate().expect("xhci in-ctx");
    let cs = x.ctx_size;
    let ep0_ring_frame = falloc.allocate().expect("xhci ep0-ring");
    let mut ep0_ring = Ring::new(ep0_ring_frame);

    build_input_context(in_ctx, cs, 0b11, |slot_ctx, ep_ctxs| {
        // Slot context: context-entries=1, speed, root-hub port = `port`.
        w32(slot_ctx, (1 << 27) | (speed << 20));
        w32(slot_ctx + 4, (port as u32) << 16);
        // EP0 context (interrupt? no, Control type=4), max-packet, TR-dequeue.
        let ep0 = ep_ctxs; // DCI 1 = first endpoint context
        w32(ep0 + 4, (4 << 3) | (3 << 1) | ((max_pkt0 as u32) << 16)); // type=Control, CErr=3
        w64(ep0 + 8, ep0_ring_frame | 1); // TR-dequeue | DCS
        w32(ep0 + 16, 8); // average TRB length
    });

    // Address Device (input-context pointer, slot id).
    let (cc, _) = run_command(x, in_ctx as u32, (in_ctx >> 32) as u32, (TRB_ADDRESS_DEVICE << 10) | ((slot as u32) << 24))
        .unwrap_or((0, 0));
    if cc != CC_SUCCESS {
        crate::serial_println!("[xhci] port {port}/slot {slot}: Address-Device cc={cc}");
        return false;
    }

    // GET_DESCRIPTOR(device, 18) via EP0.
    let buf = falloc.allocate().expect("xhci ctrl-buf");
    core::ptr::write_bytes(buf as *mut u8, 0, 4096);
    if !control_in(x, slot, &mut ep0_ring, 0x80, 6, 0x0100, 0, 18, buf) {
        crate::serial_println!("[xhci] slot {slot}: GET_DESCRIPTOR(device) failed");
        return false;
    }
    let dd = {
        let bytes = core::slice::from_raw_parts(buf as *const u8, 18);
        eurousb::DeviceDescriptor::parse(bytes)
    };
    let dd = match dd {
        Some(d) => d,
        None => {
            crate::serial_println!("[xhci] slot {slot}: device descriptor unreadable");
            return false;
        }
    };
    crate::serial_println!(
        "[xhci] slot {slot} port {port}: USB {:x}.{:x} device {:04x}:{:04x}",
        dd.usb_version >> 8, (dd.usb_version >> 4) & 0xF, dd.vendor, dd.product
    );

    // GET_DESCRIPTOR(config, 9) → wTotalLength → fetch the full config.
    if !control_in(x, slot, &mut ep0_ring, 0x80, 6, 0x0200, 0, 9, buf) {
        return false;
    }
    let total = {
        let b = core::slice::from_raw_parts(buf as *const u8, 9);
        u16::from_le_bytes([b[2], b[3]])
    }
    .min(512);
    if !control_in(x, slot, &mut ep0_ring, 0x80, 6, 0x0200, 0, total, buf) {
        return false;
    }
    let cfg = {
        let bytes = core::slice::from_raw_parts(buf as *const u8, total as usize);
        eurousb::Configuration::parse(bytes)
    };
    let cfg = match cfg {
        Some(c) => c,
        None => return false,
    };

    // Mass storage (USB disk) takes priority: configure the bulk endpoints + SCSI.
    if let Some(iface) = cfg.interfaces.iter().find(|i| i.is_mass_storage_bot()) {
        return setup_mass_storage(x, falloc, slot, port, speed, cfg.value, iface, &mut ep0_ring, in_ctx, cs, buf);
    }

    // Look for a HID boot interface (keyboard or mouse) + its interrupt-IN endpoint.
    let mut chosen: Option<(bool, bool, u8, u8, u16, u8)> = None; // (is_kbd, is_abs, iface, ep_addr, max_pkt, interval)
    for iface in &cfg.interfaces {
        let kbd = iface.is_boot_keyboard();
        let mouse = iface.is_boot_mouse();
        let abs = iface.is_hid_absolute_pointer(); // usb-tablet / touchscreen
        if !(kbd || mouse || abs) {
            continue;
        }
        if let Some(ep) = iface.endpoints.iter().find(|e| e.is_in() && (e.attributes & 0x03) == 0x03) {
            chosen = Some((kbd, abs, iface.number, ep.address, ep.max_packet, ep.interval));
            break;
        }
    }
    let (is_kbd, is_abs, iface_num, ep_addr, ep_pkt, ep_interval) = match chosen {
        Some(c) => c,
        None => {
            crate::serial_println!("[xhci] slot {slot}: no HID input interface");
            return false;
        }
    };

    // SET_CONFIGURATION(cfg.value).
    if !control_no_data(x, slot, &mut ep0_ring, 0x00, 9, cfg.value as u16, 0) {
        crate::serial_println!("[xhci] slot {slot}: SET_CONFIGURATION failed");
        return false;
    }

    // Configure Endpoint: add the interrupt-IN endpoint to the device context.
    // DCI = (endpoint number × 2) + (IN ? 1 : 0).
    let ep_num = (ep_addr & 0x0F) as u32;
    let ep_dci = (ep_num * 2 + 1) as u8;
    let ep_ring_frame = falloc.allocate().expect("xhci ep-ring");
    let ep_ring = Ring::new(ep_ring_frame);

    build_input_context(in_ctx, cs, 1 | (1 << ep_dci), |slot_ctx, ep_ctxs| {
        // Slot context: bump context-entries up to the highest DCI.
        w32(slot_ctx, (((ep_dci as u32) & 0x1F) << 27) | (speed << 20));
        w32(slot_ctx + 4, (port as u32) << 16);
        // The interrupt-IN endpoint context at DCI `ep_dci`.
        let epc = ep_ctxs + (ep_dci as u64 - 1) * cs;
        let interval = encode_interval(speed, ep_interval);
        w32(epc, interval << 16); // EP-state 0, interval
        w32(epc + 4, (7 << 3) | (3 << 1) | ((ep_pkt as u32) << 16)); // type=Interrupt-IN, CErr=3
        w64(epc + 8, ep_ring_frame | 1); // TR-dequeue | DCS
        w32(epc + 16, ep_pkt as u32); // average TRB length ≈ max-packet
    });
    let (cc, _) = run_command(x, in_ctx as u32, (in_ctx >> 32) as u32, (TRB_CONFIGURE_ENDPOINT << 10) | ((slot as u32) << 24))
        .unwrap_or((0, 0));
    if cc != CC_SUCCESS {
        crate::serial_println!("[xhci] slot {slot}: Configure-Endpoint cc={cc}");
        return false;
    }

    // SET_PROTOCOL(boot=0) on boot keyboards/mice only. The usb-tablet has no
    // boot protocol; it stays in report protocol (its native absolute format).
    if !is_abs {
        let _ = control_no_data(x, slot, &mut ep0_ring, 0x21, 0x0B, 0, iface_num as u16);
    }

    // Register the device + arm the first interrupt-IN transfer.
    let report_buf = falloc.allocate().expect("xhci report-buf");
    core::ptr::write_bytes(report_buf as *mut u8, 0, 4096);
    let mut hid = HidDevice {
        slot,
        ep_dci,
        ring: ep_ring,
        buf: report_buf,
        is_keyboard: is_kbd,
        is_abs_pointer: is_abs,
        kb: eurousb::BootKeyboard::new(),
        prev_mods: 0,
        armed: false,
    };
    arm_interrupt(x, &mut hid);
    crate::serial_println!(
        "[xhci] slot {slot}: HID {} configured (ep DCI {ep_dci}, interval {ep_interval}) → live",
        if is_kbd { "keyboard" } else if is_abs { "tablet (absolute)" } else { "mouse" }
    );
    for s in (*core::ptr::addr_of_mut!(HIDS)).iter_mut() {
        if s.is_none() {
            *s = Some(hid);
            return true;
        }
    }
    true
}

/// Encode the xHCI endpoint interval (logarithmic) from the USB bInterval.
fn encode_interval(speed: u32, b_interval: u8) -> u32 {
    match speed {
        4 | 5 => (b_interval.saturating_sub(1)).min(15) as u32, // SS: already logarithmic (1..16)
        3 => (b_interval.saturating_sub(1)).min(15) as u32,     // HS: same
        _ => {
            // FS/LS: bInterval is in frames (ms). Find log2 → xHCI interval (+3 for µframes).
            let ms = b_interval.max(1) as u32;
            let mut log = 0u32;
            while (1u32 << log) < ms && log < 10 {
                log += 1;
            }
            (log + 3).min(15)
        }
    }
}

/// Fill the input context: input-control-context (add-flags) + slot/EP contexts.
/// `add_flags` sets the A-bits (bit0=slot, bit n=EP-DCI n). The closure fills the
/// slot context and the endpoint-context array (both after the control context).
unsafe fn build_input_context(in_ctx: u64, cs: u64, add_flags: u32, fill: impl FnOnce(u64, u64)) {
    core::ptr::write_bytes(in_ctx as *mut u8, 0, 4096);
    // Input-Control-Context: drop-flags (0) at +0, add-flags at +4.
    w32(in_ctx + 4, add_flags);
    let slot_ctx = in_ctx + cs; // slot context after the control context
    let ep_ctxs = slot_ctx + cs; // EP context array (DCI 1 = first)
    fill(slot_ctx, ep_ctxs);
}

/// A GET-style control-IN transfer over EP0: Setup → Data(IN) → Status(OUT).
/// Reads `len` bytes into `buf`. Returns true on success.
unsafe fn control_in(
    x: &mut Xhci,
    slot: u8,
    ep0: &mut Ring,
    bm_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    len: u16,
    buf: u64,
) -> bool {
    // Setup-stage TRB (immediate data: the 8-byte setup packet in param-lo/hi).
    let setup_lo = (bm_request_type as u32)
        | ((b_request as u32) << 8)
        | ((w_value as u32) << 16);
    let setup_hi = (w_index as u32) | ((len as u32) << 16);
    // TRT (transfer type) = 3 (IN-data) if there is data, otherwise 0.
    let trt = if len > 0 { 3u32 } else { 0 };
    ep0.push(setup_lo, setup_hi, 8, (TRB_SETUP << 10) | (1 << 6) | (trt << 16)); // IDT=1
    if len > 0 {
        // Data stage (IN): buffer pointer, length, DIR=IN (bit16).
        ep0.push(buf as u32, (buf >> 32) as u32, len as u32, (TRB_DATA << 10) | (1 << 16));
    }
    // Status stage: direction opposite to data; IOC (bit5) so that we get an event.
    let status_dir = if len > 0 { 0 } else { 1 << 16 };
    ep0.push(0, 0, 0, (TRB_STATUS << 10) | (1 << 5) | status_dir);
    doorbell(x, slot, 1); // EP0 = DCI 1
    match wait_event(x, TRB_EVT_TRANSFER, 20_000_000) {
        Some(ev) => ((ev[2] >> 24) & 0xFF) == CC_SUCCESS,
        None => false,
    }
}

/// A control transfer without a data stage (e.g. SET_CONFIGURATION / SET_PROTOCOL).
unsafe fn control_no_data(
    x: &mut Xhci,
    slot: u8,
    ep0: &mut Ring,
    bm_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
) -> bool {
    control_in(x, slot, ep0, bm_request_type, b_request, w_value, w_index, 0, 0)
}

/// Place a Normal-TRB on the interrupt-IN ring (8-byte report) + ring the doorbell.
unsafe fn arm_interrupt(x: &Xhci, hid: &mut HidDevice) {
    core::ptr::write_bytes(hid.buf as *mut u8, 0, 8);
    hid.ring.push(hid.buf as u32, (hid.buf >> 32) as u32, 8, (TRB_NORMAL << 10) | (1 << 5)); // IOC
    doorbell(x, hid.slot, hid.ep_dci as u32);
    hid.armed = true;
}

/// Called by the desktop loop: harvest incoming interrupt transfers and
/// inject them into the PS/2 scancode buffer (keyboard) or mouse atomics, and
/// re-arm the endpoint. Non-blocking.
/// Called by the MSI-X handler: clear the interrupter-pending status (USBSTS.EINT
/// + IMAN.IP, both write-1-clear) so that the next event interrupt can come.
pub fn ack_interrupt() {
    unsafe {
        if let Some(x) = (*core::ptr::addr_of_mut!(XHCI)).as_mut() {
            w32(x.op + OP_USBSTS, 1 << 3); // EINT clear
            w32(x.rt + RT_IR0 + IR_IMAN, 0x3); // IP clear + IE
        }
    }
}

/// Harvest incoming USB events. Re-entrancy safe: callable from BOTH the
/// desktop loop (interrupts on) AND the MSI-X IRQ handler (interrupts off). The
/// `POLLING` flag prevents both from touching the event ring/scancode Mutex at once.
pub fn poll() {
    use core::sync::atomic::Ordering;
    if POLLING.swap(true, Ordering::Acquire) {
        return; // the other context is already harvesting — bail (it drains everything)
    }
    poll_inner();
    POLLING.store(false, Ordering::Release);
}

fn poll_inner() {
    unsafe {
        let xp = core::ptr::addr_of_mut!(XHCI);
        let x = match (*xp).as_mut() {
            Some(x) => x,
            None => return,
        };
        // Once: confirm that MSI-X interrupts actually arrive (J2).
        if !MSIX_LOGGED {
            let c = crate::interrupts::XHCI_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed);
            if c > 0 {
                MSIX_LOGGED = true;
                crate::serial_println!("[xhci] MSI-X delivery confirmed: {c} interrupt(s) received ✓");
            }
        }
        // Drain ALL pending events this round (bounded) so that a burst of HID
        // reports doesn't fall behind one-per-frame.
        for _ in 0..32 {
            let trb = x.ev_seg + x.ev_deq as u64 * 16;
            let ctrl = r32(trb + 12);
            if (ctrl & 1) != x.ev_cycle as u32 {
                return; // no more new events
            }
            let ttype = (ctrl >> 10) & 0x3F;
            let ep_id = (ctrl >> 16) & 0x1F;
            let slot = ((ctrl >> 24) & 0xFF) as u8;
            let cc = (r32(trb + 8) >> 24) & 0xFF;
            // Dequeue + update ERDP.
            x.ev_deq += 1;
            if x.ev_deq == RING_TRBS {
                x.ev_deq = 0;
                x.ev_cycle ^= 1;
            }
            let erdp = x.ev_seg + x.ev_deq as u64 * 16;
            w64(x.rt + RT_IR0 + IR_ERDP, erdp | (1 << 3));

            if ttype != TRB_EVT_TRANSFER {
                continue;
            }
            // Find the HID device that belongs to (slot, ep_id).
            let hids = core::ptr::addr_of_mut!(HIDS);
            for s in (*hids).iter_mut() {
                if let Some(hid) = s {
                    if hid.slot == slot && hid.ep_dci as u32 == ep_id {
                        if cc == CC_SUCCESS || cc == 13 {
                            // 13 = Short-Packet (also valid). Read the 8-byte report.
                            let report = core::slice::from_raw_parts(hid.buf as *const u8, 8);
                            // Diagnostics: log the first few reports so that the
                            // interrupt-IN path (and QMP-sendkey injection) is verifiable.
                            if REPORTS_LOGGED < 12 {
                                REPORTS_LOGGED += 1;
                                crate::serial_println!(
                                    "[xhci-rpt] slot {} {}: {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
                                    slot, if hid.is_keyboard { "kbd" } else if hid.is_abs_pointer { "tablet" } else { "mouse" },
                                    report[0], report[1], report[2], report[3],
                                    report[4], report[5], report[6], report[7]
                                );
                            }
                            if hid.is_keyboard {
                                inject_keyboard(hid, report);
                            } else if hid.is_abs_pointer {
                                inject_tablet(report);
                            } else {
                                inject_mouse(report);
                            }
                        }
                        arm_interrupt(x, hid);
                        break; // this event is handled; move on to the next
                    }
                }
            }
        }
        // Re-arm the interrupter (IMAN.IP + USBSTS.EINT are write-1-clear) so that the
        // NEXT event sends a fresh MSI-X message again — otherwise the
        // interrupter stays "pending" and no new one comes after the first IRQ.
        w32(x.op + OP_USBSTS, 1 << 3);
        w32(x.rt + RT_IR0 + IR_IMAN, 0x3);
    }
}

/// Translate a HID keyboard report into PS/2 set-1 scancodes (so that the
/// existing [`crate::ps2`] decoder + shell process it transparently).
fn inject_keyboard(hid: &mut HidDevice, report: &[u8]) {
    let mods = report[0];
    // Shift transitions as 0x2A/0xAA (make/break) so that the ps2 SHIFT latch is correct.
    let shift_now = mods & 0x22 != 0; // LShift (bit1) or RShift (bit5)
    let shift_prev = hid.prev_mods & 0x22 != 0;
    if shift_now && !shift_prev {
        crate::ps2::push_scancode(0x2A);
    } else if !shift_now && shift_prev {
        crate::ps2::push_scancode(0xAA);
    }
    hid.prev_mods = mods;
    for ev in hid.kb.feed(report) {
        if let Some(sc) = hid_to_set1(ev.keycode) {
            crate::ps2::push_scancode(if ev.pressed { sc } else { sc | 0x80 });
        }
    }
}

/// Translate a HID mouse report into relative cursor movement + buttons.
fn inject_mouse(report: &[u8]) {
    if let Some(m) = eurousb::parse_mouse(report) {
        crate::mouse::apply_usb(m.dx as i32, m.dy as i32, m.buttons);
    }
}

/// Translate a usb-tablet report into an ABSOLUTE cursor position + buttons.
fn inject_tablet(report: &[u8]) {
    if let Some(a) = eurousb::parse_tablet(report) {
        crate::mouse::apply_usb_abs(a.x, a.y, a.buttons);
    }
}

/// USB-HID usage id (Keyboard/Keypad page) → PS/2 scancode-set-1 make code.
fn hid_to_set1(usage: u8) -> Option<u8> {
    Some(match usage {
        0x04 => 0x1E, 0x05 => 0x30, 0x06 => 0x2E, 0x07 => 0x20, 0x08 => 0x12, // a b c d e
        0x09 => 0x21, 0x0A => 0x22, 0x0B => 0x23, 0x0C => 0x17, 0x0D => 0x24, // f g h i j
        0x0E => 0x25, 0x0F => 0x26, 0x10 => 0x32, 0x11 => 0x31, 0x12 => 0x18, // k l m n o
        0x13 => 0x19, 0x14 => 0x10, 0x15 => 0x13, 0x16 => 0x1F, 0x17 => 0x14, // p q r s t
        0x18 => 0x16, 0x19 => 0x2F, 0x1A => 0x11, 0x1B => 0x2D, 0x1C => 0x15, // u v w x y
        0x1D => 0x2C, // z
        0x1E => 0x02, 0x1F => 0x03, 0x20 => 0x04, 0x21 => 0x05, 0x22 => 0x06, // 1..5
        0x23 => 0x07, 0x24 => 0x08, 0x25 => 0x09, 0x26 => 0x0A, 0x27 => 0x0B, // 6..0
        0x28 => 0x1C, // Enter
        0x29 => 0x01, // Esc
        0x2A => 0x0E, // Backspace
        0x2B => 0x0F, // Tab
        0x2C => 0x39, // Space
        0x2D => 0x0C, 0x2E => 0x0D, // - =
        0x2F => 0x1A, 0x30 => 0x1B, // [ ]
        0x31 => 0x2B, // backslash
        0x33 => 0x27, 0x34 => 0x28, 0x35 => 0x29, // ; ' `
        0x36 => 0x33, 0x37 => 0x34, 0x38 => 0x35, // , . /
        _ => return None,
    })
}

// ── USB mass storage (Bulk-Only-Transport + SCSI) — plan I1, USB disk ───────
struct MassStorage {
    slot: u8,
    in_dci: u8,
    out_dci: u8,
    in_ring: Ring,
    out_ring: Ring,
    io: u64, // 4 KiB DMA buffer: CBW@+0, CSW@+64, data@+512
    block_size: u32,
    last_lba: u32,
}
static mut MASS: Option<MassStorage> = None;
static mut BOT_TAG: u32 = 0x1000;

/// One bulk transfer (Normal-TRB + doorbell + wait for the transfer event).
unsafe fn bulk_xfer(x: &mut Xhci, slot: u8, dci: u8, ring: &mut Ring, buf: u64, len: u32) -> bool {
    ring.push(buf as u32, (buf >> 32) as u32, len, (TRB_NORMAL << 10) | (1 << 5)); // IOC
    doorbell(x, slot, dci as u32);
    match wait_event(x, TRB_EVT_TRANSFER, 20_000_000) {
        Some(ev) => {
            let cc = (ev[2] >> 24) & 0xFF;
            cc == CC_SUCCESS || cc == 13 // success or short-packet
        }
        None => false,
    }
}

/// One SCSI command via BOT: CBW (out) → optional data phase → CSW (in).
/// Returns the CSW status (0 = succeeded) or None on a transport error.
unsafe fn scsi(x: &mut Xhci, ms: &mut MassStorage, cdb: &[u8], data_len: u32, in_dir: bool) -> Option<u8> {
    BOT_TAG = BOT_TAG.wrapping_add(1);
    let tag = BOT_TAG;
    let cbw = eurousb::bot::cbw(tag, data_len, in_dir, 0, cdb);
    core::ptr::copy_nonoverlapping(cbw.as_ptr(), ms.io as *mut u8, 31);
    // 1. CBW (31 byte) via bulk-OUT.
    if !bulk_xfer(x, ms.slot, ms.out_dci, &mut ms.out_ring, ms.io, 31) {
        return None;
    }
    // 2. Data phase (if present) in the correct direction; data buffer at io+512.
    if data_len > 0 {
        let dbuf = ms.io + 512;
        let ok = if in_dir {
            bulk_xfer(x, ms.slot, ms.in_dci, &mut ms.in_ring, dbuf, data_len)
        } else {
            bulk_xfer(x, ms.slot, ms.out_dci, &mut ms.out_ring, dbuf, data_len)
        };
        if !ok {
            return None;
        }
    }
    // 3. CSW (13 byte) via bulk-IN, at io+64.
    let csw = ms.io + 64;
    if !bulk_xfer(x, ms.slot, ms.in_dci, &mut ms.in_ring, csw, 13) {
        return None;
    }
    let bytes = core::slice::from_raw_parts(csw as *const u8, 13);
    eurousb::bot::parse_csw(bytes).map(|(_, _, status)| status)
}

/// Configure a USB mass-storage interface (bulk-IN + bulk-OUT) and run a
/// SCSI self-test: INQUIRY → READ CAPACITY → READ(10) sector 0.
unsafe fn setup_mass_storage(
    x: &mut Xhci,
    falloc: &mut FrameAllocator,
    slot: u8,
    port: u8,
    speed: u32,
    cfg_value: u8,
    iface: &eurousb::Interface,
    ep0: &mut Ring,
    in_ctx: u64,
    cs: u64,
    _buf: u64,
) -> bool {
    let bulk_in = iface.endpoints.iter().find(|e| e.is_in() && (e.attributes & 0x03) == 0x02);
    let bulk_out = iface.endpoints.iter().find(|e| !e.is_in() && (e.attributes & 0x03) == 0x02);
    let (ein, eout) = match (bulk_in, bulk_out) {
        (Some(i), Some(o)) => (i, o),
        _ => {
            crate::serial_println!("[xhci] slot {slot}: mass storage without bulk-IN+OUT");
            return false;
        }
    };
    let in_dci = ((ein.address & 0x0F) * 2 + 1) as u8;
    let out_dci = ((eout.address & 0x0F) * 2) as u8;
    let in_pkt = ein.max_packet;
    let out_pkt = eout.max_packet;
    let in_ring_frame = falloc.allocate().expect("xhci bulk-in-ring");
    let out_ring_frame = falloc.allocate().expect("xhci bulk-out-ring");
    let in_ring = Ring::new(in_ring_frame);
    let out_ring = Ring::new(out_ring_frame);

    // SET_CONFIGURATION.
    if !control_no_data(x, slot, ep0, 0x00, 9, cfg_value as u16, 0) {
        crate::serial_println!("[xhci] slot {slot}: SET_CONFIGURATION (mass storage) failed");
        return false;
    }

    // Configure Endpoint: add bulk-IN + bulk-OUT.
    let max_dci = in_dci.max(out_dci) as u32;
    build_input_context(in_ctx, cs, 1 | (1 << in_dci) | (1 << out_dci), |slot_ctx, ep_ctxs| {
        w32(slot_ctx, ((max_dci & 0x1F) << 27) | (speed << 20));
        w32(slot_ctx + 4, (port as u32) << 16);
        // bulk-OUT context (type 2).
        let oc = ep_ctxs + (out_dci as u64 - 1) * cs;
        w32(oc + 4, (2 << 3) | (3 << 1) | ((out_pkt as u32) << 16));
        w64(oc + 8, out_ring_frame | 1);
        w32(oc + 16, out_pkt as u32);
        // bulk-IN context (type 6).
        let ic = ep_ctxs + (in_dci as u64 - 1) * cs;
        w32(ic + 4, (6 << 3) | (3 << 1) | ((in_pkt as u32) << 16));
        w64(ic + 8, in_ring_frame | 1);
        w32(ic + 16, in_pkt as u32);
    });
    let (cc, _) = run_command(x, in_ctx as u32, (in_ctx >> 32) as u32, (TRB_CONFIGURE_ENDPOINT << 10) | ((slot as u32) << 24))
        .unwrap_or((0, 0));
    if cc != CC_SUCCESS {
        crate::serial_println!("[xhci] slot {slot}: Configure-Endpoint (bulk) cc={cc}");
        return false;
    }

    let io = falloc.allocate().expect("xhci ms-iobuf");
    core::ptr::write_bytes(io as *mut u8, 0, 4096);
    let mut ms = MassStorage { slot, in_dci, out_dci, in_ring, out_ring, io, block_size: 512, last_lba: 0 };

    // SCSI self-test. TEST UNIT READY a few times (the medium may briefly be "not ready").
    for _ in 0..5 {
        if scsi(x, &mut ms, &eurousb::bot::test_unit_ready(), 0, false) == Some(0) {
            break;
        }
    }
    // INQUIRY (36 byte): vendor (8) + product (16).
    let inq = scsi(x, &mut ms, &eurousb::bot::inquiry(), 36, true);
    if inq == Some(0) {
        let d = core::slice::from_raw_parts((io + 512) as *const u8, 36);
        let vendor = core::str::from_utf8(&d[8..16]).unwrap_or("?").trim();
        let product = core::str::from_utf8(&d[16..32]).unwrap_or("?").trim();
        crate::serial_println!("[xhci] slot {slot} port {port}: USB disk — \"{vendor} {product}\"");
    } else {
        crate::serial_println!("[xhci] slot {slot}: INQUIRY failed ({inq:?})");
        return false;
    }
    // READ CAPACITY(10): last-LBA + block size.
    if scsi(x, &mut ms, &eurousb::bot::read_capacity10(), 8, true) == Some(0) {
        let d = core::slice::from_raw_parts((io + 512) as *const u8, 8);
        if let Some((last, bs)) = eurousb::bot::parse_capacity(d) {
            ms.last_lba = last;
            ms.block_size = bs;
            let mib = ((last as u64 + 1) * bs as u64) / (1024 * 1024);
            crate::serial_println!("[xhci] slot {slot}: capacity {} blocks × {} B = {} MiB", last + 1, bs, mib);
        }
    }
    // READ(10) sector 0 → log the first bytes (proves real data read).
    let read_ok = scsi(x, &mut ms, &eurousb::bot::read10(0, 1), 512, true) == Some(0);
    if read_ok {
        let d = core::slice::from_raw_parts((io + 512) as *const u8, 16);
        crate::serial_println!(
            "[xhci] slot {slot}: READ(10) sector 0 OK — first bytes {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
            d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        );
    }
    crate::serial_println!("[xhci] slot {slot}: USB mass storage LIVE (BOT/SCSI) ✓");
    MASS = Some(ms);
    read_ok
}

/// Read one block (`block_size` bytes) from the USB disk into `out`. Returns false if
/// there is no USB disk or the SCSI READ fails.
pub fn usb_read_block(lba: u32, out: &mut [u8]) -> bool {
    // Mask interrupts for this single BOT transfer so the xHCI IRQ handler can't consume
    // the completion event our busy-poll is waiting on (bounded, one transfer).
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let x = match (*core::ptr::addr_of_mut!(XHCI)).as_mut() {
            Some(x) => x,
            None => return false,
        };
        let ms = match (*core::ptr::addr_of_mut!(MASS)).as_mut() {
            Some(m) => m,
            None => return false,
        };
        let n = out.len().min(ms.block_size as usize);
        if scsi(x, ms, &eurousb::bot::read10(lba, 1), ms.block_size, true) != Some(0) {
            return false;
        }
        core::ptr::copy_nonoverlapping((ms.io + 512) as *const u8, out.as_mut_ptr(), n);
        true
    })
}

/// Write one block (`block_size` bytes) to the USB disk from `data` via SCSI WRITE(10)
/// over the bulk-OUT endpoint. Returns false if there is no USB disk or the write fails.
/// (IO-2: makes a mounted FAT USB stick writable.)
pub fn usb_write_block(lba: u32, data: &[u8]) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let x = match (*core::ptr::addr_of_mut!(XHCI)).as_mut() {
            Some(x) => x,
            None => return false,
        };
        let ms = match (*core::ptr::addr_of_mut!(MASS)).as_mut() {
            Some(m) => m,
            None => return false,
        };
        let n = data.len().min(ms.block_size as usize);
        // Stage the block into the data buffer (io+512), zero-padded to one sector.
        core::ptr::write_bytes((ms.io + 512) as *mut u8, 0, ms.block_size as usize);
        core::ptr::copy_nonoverlapping(data.as_ptr(), (ms.io + 512) as *mut u8, n);
        scsi(x, ms, &eurousb::bot::write10(lba, 1), ms.block_size, false) == Some(0)
    })
}

/// Is there a USB mass-storage device (USB disk) present?
pub fn usb_disk_present() -> bool {
    unsafe { (*core::ptr::addr_of!(MASS)).is_some() }
}

/// Number of 512-byte-equivalent blocks on the USB disk (`last_lba + 1`), 0 if none.
pub fn usb_block_count() -> u64 {
    unsafe {
        (*core::ptr::addr_of!(MASS)).as_ref().map(|m| m.last_lba as u64 + 1).unwrap_or(0)
    }
}

/// Is an xHCI controller initialized?
pub fn present() -> bool {
    unsafe { (*core::ptr::addr_of!(XHCI)).is_some() }
}

/// Number of live polled HID devices (diagnostics/self-test).
pub fn hid_count() -> usize {
    unsafe { (*core::ptr::addr_of!(HIDS)).iter().filter(|h| h.is_some()).count() }
}
