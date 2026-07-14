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
const TRB_ISOCH: u32 = 5;
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
    last_arm: u64, // tick of the last endpoint (re-)arm — for idle re-arming
    // M4-2: layout parsed from the device's HID report descriptor (report-
    // protocol pointers). None = boot-protocol/fallback fixed layout.
    map: Option<eurousb::InputMap>,
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
    pci::claim(dev.bus, dev.dev, dev.func, "xhci"); // hwprobe (M1-3)
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
    // A root-port device has an empty route string and no parent hub.
    enumerate_device(falloc, port, 0, speed, None, 0)
}

/// Address + configure one USB device (M4-1: shared by root ports and hub
/// downstream ports). `route` is the xHCI route string (one 4-bit hub-port
/// nibble per tier); `parent` = (hub slot, hub port, hub speed) when the
/// device sits behind a hub — needed for the TT fields when a low/full-speed
/// device hangs off a high-speed hub.
unsafe fn enumerate_device(
    falloc: &mut FrameAllocator,
    root_port: u8,
    route: u32,
    speed: u32,
    parent: Option<(u8, u8, u32)>,
    depth: u8,
) -> bool {
    let xp = core::ptr::addr_of_mut!(XHCI);
    let x = (*xp).as_mut().unwrap();
    let max_pkt0: u16 = match speed {
        2 => 8,    // Low-speed
        4 | 5 => 512, // Super(Plus)-speed
        _ => 64,   // Full/High-speed
    };

    // Enable Slot.
    let (cc, slot) = match run_command(x, 0, 0, TRB_ENABLE_SLOT << 10) {
        Some(v) => v,
        None => {
            crate::serial_println!("[xhci] port {root_port}: Enable-Slot timeout");
            return false;
        }
    };
    if cc != CC_SUCCESS || slot == 0 {
        crate::serial_println!("[xhci] port {root_port}: Enable-Slot cc={cc}");
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

    // TT fields (slot-context DW2): only a LS/FS device behind a HS hub needs
    // the hub's slot/port for split transactions.
    let tt = match parent {
        Some((hub_slot, hub_port, hub_speed)) if hub_speed == 3 && (speed == 1 || speed == 2) => {
            (hub_slot as u32) | ((hub_port as u32) << 8)
        }
        _ => 0,
    };

    build_input_context(in_ctx, cs, 0b11, |slot_ctx, ep_ctxs| {
        // Slot context: route string, context-entries=1, speed, root-hub port.
        w32(slot_ctx, (route & 0xF_FFFF) | (1 << 27) | (speed << 20));
        w32(slot_ctx + 4, (root_port as u32) << 16);
        w32(slot_ctx + 8, tt);
        // EP0 context (Control type=4), max-packet, TR-dequeue.
        let ep0 = ep_ctxs; // DCI 1 = first endpoint context
        w32(ep0 + 4, (4 << 3) | (3 << 1) | ((max_pkt0 as u32) << 16)); // type=Control, CErr=3
        w64(ep0 + 8, ep0_ring_frame | 1); // TR-dequeue | DCS
        w32(ep0 + 16, 8); // average TRB length
    });

    // Address Device (input-context pointer, slot id).
    let (cc, _) = run_command(x, in_ctx as u32, (in_ctx >> 32) as u32, (TRB_ADDRESS_DEVICE << 10) | ((slot as u32) << 24))
        .unwrap_or((0, 0));
    if cc != CC_SUCCESS {
        crate::serial_println!("[xhci] port {root_port}/slot {slot}: Address-Device cc={cc}");
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
        "[xhci] slot {slot} port {root_port}{}: USB {:x}.{:x} device {:04x}:{:04x}",
        if route != 0 { " (behind hub)" } else { "" },
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

    // A hub (device class 9): power its ports and enumerate what hangs off
    // them (M4-1). Bounded depth so a misbehaving topology can't recurse away.
    if dd.class == 0x09 {
        if depth >= 2 {
            crate::serial_println!("[xhci] slot {slot}: hub depth limit reached — skipped");
            return false;
        }
        return setup_hub(falloc, slot, root_port, route, speed, cfg.value, &mut ep0_ring, in_ctx, cs, buf, depth);
    }

    // Mass storage (USB disk) takes priority: configure the bulk endpoints + SCSI.
    if let Some(iface) = cfg.interfaces.iter().find(|i| i.is_mass_storage_bot()) {
        return setup_mass_storage(x, falloc, slot, root_port, speed, cfg.value, iface, &mut ep0_ring, in_ctx, cs, buf);
    }

    // CDC-ECM USB ethernet (M3-3). QEMU's usb-net (and some real dongles) put
    // RNDIS in the FIRST configuration and CDC-ECM in the SECOND — check the
    // current config, then walk the others.
    {
        let mut ecm_cfg = None;
        let has_ecm = |c: &eurousb::Configuration| {
            c.interfaces.iter().any(|i| i.class == 0x02 && i.subclass == 0x06)
        };
        if has_ecm(&cfg) {
            ecm_cfg = Some((cfg.clone(), total));
        } else {
            for ci in 1..dd.num_configurations.min(4) as u16 {
                if !control_in(x, slot, &mut ep0_ring, 0x80, 6, 0x0200 | ci, 0, 9, buf) {
                    break;
                }
                let t = {
                    let b = core::slice::from_raw_parts(buf as *const u8, 9);
                    u16::from_le_bytes([b[2], b[3]])
                }
                .min(512);
                if !control_in(x, slot, &mut ep0_ring, 0x80, 6, 0x0200 | ci, 0, t, buf) {
                    break;
                }
                let c = {
                    let bytes = core::slice::from_raw_parts(buf as *const u8, t as usize);
                    eurousb::Configuration::parse(bytes)
                };
                if let Some(c) = c {
                    if has_ecm(&c) {
                        ecm_cfg = Some((c, t));
                        break;
                    }
                }
            }
        }
        if let Some((ecm, raw_len)) = ecm_cfg {
            return setup_usbnet(x, falloc, slot, root_port, route, speed, tt, &ecm, raw_len, &mut ep0_ring, in_ctx, cs, buf);
        }
    }

    // USB audio (M4-3): an AudioStreaming interface (class 1, subclass 2) with
    // an isochronous OUT endpoint in an alternate setting.
    if cfg.interfaces.iter().any(|i| i.class == 0x01 && i.subclass == 0x02) {
        return setup_usbaudio(x, falloc, slot, root_port, route, speed, tt, &cfg, total, &mut ep0_ring, in_ctx, cs, buf);
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

    // M4-2: report-protocol devices (the absolute pointer) describe their
    // report layout in a HID report descriptor — fetch + parse it so decoding
    // follows the device instead of a hardcoded format. 256 bytes is plenty
    // for pointer descriptors; the device short-terminates.
    let mut input_map: Option<eurousb::InputMap> = None;
    if is_abs {
        if control_in(x, slot, &mut ep0_ring, 0x81, 6, 0x2200, iface_num as u16, 256, buf) {
            let d = core::slice::from_raw_parts(buf as *const u8, 256);
            input_map = eurousb::parse_report_descriptor(d);
            if let Some(m) = &input_map {
                crate::serial_println!(
                    "[xhci] slot {slot}: HID report descriptor parsed — X {}@{} bits, Y @{}, {} button(s), max {}",
                    if m.x.unwrap().relative { "rel" } else { "abs" },
                    m.x.unwrap().bit_off, m.y.unwrap().bit_off, m.buttons_n,
                    m.x.unwrap().logical_max
                );
            } else {
                crate::serial_println!("[xhci] slot {slot}: report descriptor not a pointer map — fallback layout");
            }
        }
    }

    // Configure Endpoint: add the interrupt-IN endpoint to the device context.
    // DCI = (endpoint number × 2) + (IN ? 1 : 0).
    let ep_num = (ep_addr & 0x0F) as u32;
    let ep_dci = (ep_num * 2 + 1) as u8;
    let ep_ring_frame = falloc.allocate().expect("xhci ep-ring");
    let ep_ring = Ring::new(ep_ring_frame);

    build_input_context(in_ctx, cs, 1 | (1 << ep_dci), |slot_ctx, ep_ctxs| {
        // Slot context: bump context-entries up to the highest DCI.
        w32(slot_ctx, (route & 0xF_FFFF) | (((ep_dci as u32) & 0x1F) << 27) | (speed << 20));
        w32(slot_ctx + 4, (root_port as u32) << 16);
        w32(slot_ctx + 8, tt);
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
        last_arm: 0,
        map: input_map,
    };
    arm_interrupt(x, &mut hid);
    crate::serial_println!(
        "[xhci] slot {slot}: HID {} configured (ep DCI {ep_dci}, interval {ep_interval}){} → live",
        if is_kbd { "keyboard" } else if is_abs { "tablet (absolute)" } else { "mouse" },
        if route != 0 { " via hub" } else { "" }
    );
    for s in (*core::ptr::addr_of_mut!(HIDS)).iter_mut() {
        if s.is_none() {
            *s = Some(hid);
            return true;
        }
    }
    true
}

/// Bring up a hub (M4-1): mark the slot as a hub, power the downstream ports,
/// reset whatever is connected and enumerate it with the extended route string.
#[allow(clippy::too_many_arguments)]
unsafe fn setup_hub(
    falloc: &mut FrameAllocator,
    slot: u8,
    root_port: u8,
    route: u32,
    hub_speed: u32,
    cfg_value: u8,
    ep0_ring: &mut Ring,
    in_ctx: u64,
    cs: u64,
    buf: u64,
    depth: u8,
) -> bool {
    let xp = core::ptr::addr_of_mut!(XHCI);
    let x = (*xp).as_mut().unwrap();

    if !control_no_data(x, slot, ep0_ring, 0x00, 9, cfg_value as u16, 0) {
        crate::serial_println!("[xhci] hub slot {slot}: SET_CONFIGURATION failed");
        return false;
    }
    // Hub class descriptor (0x29): bNbrPorts at offset 2.
    if !control_in(x, slot, ep0_ring, 0xA0, 6, 0x2900, 0, 9, buf) {
        crate::serial_println!("[xhci] hub slot {slot}: hub descriptor failed");
        return false;
    }
    let nports = core::slice::from_raw_parts(buf as *const u8, 9)[2].min(8);

    // Tell the controller this slot is a hub (slot-context HUB bit + port
    // count) — required for downstream addressing/TT handling.
    build_input_context(in_ctx, cs, 0b1, |slot_ctx, _| {
        w32(slot_ctx, (route & 0xF_FFFF) | (1 << 27) | (hub_speed << 20) | (1 << 26));
        w32(slot_ctx + 4, ((root_port as u32) << 16) | ((nports as u32) << 24));
    });
    let _ = run_command(x, in_ctx as u32, (in_ctx >> 32) as u32, (TRB_CONFIGURE_ENDPOINT << 10) | ((slot as u32) << 24));

    crate::serial_println!("[xhci] hub slot {slot}: {nports} port(s) — powering + scanning");
    let mut found = false;
    for p in 1..=nports as u16 {
        // SET_FEATURE(PORT_POWER), settle, then read the port status.
        let _ = control_no_data(x, slot, ep0_ring, 0x23, 3, 8, p);
        for _ in 0..3_000_000u64 {
            core::hint::spin_loop();
        }
        if !control_in(x, slot, ep0_ring, 0xA3, 0, 0, p, 4, buf) {
            continue;
        }
        let st = core::slice::from_raw_parts(buf as *const u8, 4);
        let status = u16::from_le_bytes([st[0], st[1]]);
        if status & 1 == 0 {
            continue; // no device on this hub port
        }
        // Reset the port; poll until the reset-change bit reports completion.
        let _ = control_no_data(x, slot, ep0_ring, 0x23, 3, 4, p);
        let mut child_status = 0u16;
        for _ in 0..40 {
            for _ in 0..1_000_000u64 {
                core::hint::spin_loop();
            }
            if !control_in(x, slot, ep0_ring, 0xA3, 0, 0, p, 4, buf) {
                break;
            }
            let st = core::slice::from_raw_parts(buf as *const u8, 4);
            let change = u16::from_le_bytes([st[2], st[3]]);
            child_status = u16::from_le_bytes([st[0], st[1]]);
            if change & (1 << 4) != 0 {
                // C_PORT_RESET: acknowledge it (+ the connect change).
                let _ = control_no_data(x, slot, ep0_ring, 0x23, 1, 20, p);
                let _ = control_no_data(x, slot, ep0_ring, 0x23, 1, 16, p);
                break;
            }
        }
        if child_status & 2 == 0 {
            crate::serial_println!("[xhci] hub slot {slot} port {p}: reset did not enable — skipped");
            continue;
        }
        // Child speed from the hub port status (LS bit9, HS bit10, else FS).
        let child_speed = if child_status & (1 << 9) != 0 {
            2 // low
        } else if child_status & (1 << 10) != 0 {
            3 // high
        } else {
            1 // full
        };
        // Extend the route string: this tier's nibble carries the hub port.
        let nibble_shift = 4 * depth;
        let child_route = route | ((p as u32 & 0xF) << nibble_shift);
        crate::serial_println!(
            "[xhci] hub slot {slot} port {p}: device (speed {child_speed}) → enumerating (route {child_route:#x})"
        );
        if enumerate_device(falloc, root_port, child_route, child_speed, Some((slot, p as u8, hub_speed)), depth + 1) {
            found = true;
        }
    }
    found
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

/// A control transfer with an OUT data stage (e.g. the UAC SET_CUR sampling
/// frequency, M4-3). `buf` holds `len` bytes to send to the device.
#[allow(clippy::too_many_arguments)]
unsafe fn control_out(
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
    let setup_lo = (bm_request_type as u32) | ((b_request as u32) << 8) | ((w_value as u32) << 16);
    let setup_hi = (w_index as u32) | ((len as u32) << 16);
    // TRT = 2: OUT data stage follows.
    ep0.push(setup_lo, setup_hi, 8, (TRB_SETUP << 10) | (1 << 6) | (2 << 16)); // IDT=1
    ep0.push(buf as u32, (buf >> 32) as u32, len as u32, TRB_DATA << 10); // DIR=OUT
    // Status stage: IN (opposite of the data direction), IOC.
    ep0.push(0, 0, 0, (TRB_STATUS << 10) | (1 << 5) | (1 << 16));
    doorbell(x, slot, 1);
    match wait_event(x, TRB_EVT_TRANSFER, 20_000_000) {
        Some(ev) => ((ev[2] >> 24) & 0xFF) == CC_SUCCESS,
        None => false,
    }
}

/// Place a Normal-TRB on the interrupt-IN ring (8-byte report) + ring the doorbell.
unsafe fn arm_interrupt(x: &Xhci, hid: &mut HidDevice) {
    core::ptr::write_bytes(hid.buf as *mut u8, 0, 8);
    hid.ring.push(hid.buf as u32, (hid.buf >> 32) as u32, 8, (TRB_NORMAL << 10) | (1 << 5)); // IOC
    doorbell(x, hid.slot, hid.ep_dci as u32);
    hid.armed = true;
    hid.last_arm = crate::interrupts::ticks();
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
            // CDC-ECM traffic (M3-3): queue completed receives, release TX.
            if let Some(n) = (*core::ptr::addr_of_mut!(USBNET)).as_mut() {
                if slot == n.slot && ep_id == n.in_dci as u32 {
                    if cc == CC_SUCCESS || cc == 13 {
                        // Event status bits 0..23 = REMAINING transfer length.
                        let got = 2048usize.saturating_sub((r32(trb + 8) & 0xFF_FFFF) as usize);
                        let src = n.rx_bufs[n.rx_deq % USBNET_RX_BUFS];
                        if got > 0 && got <= 2048 {
                            let mut f = alloc::vec![0u8; got];
                            core::ptr::copy_nonoverlapping(src as *const u8, f.as_mut_ptr(), got);
                            x86_64::instructions::interrupts::without_interrupts(|| {
                                let mut q = USBNET_RX.lock();
                                if q.len() >= 64 {
                                    q.pop_front(); // drop oldest under overload
                                }
                                q.push_back(f);
                            });
                        }
                        usbnet_rearm_one(x, n);
                    }
                    continue;
                }
                if slot == n.slot && ep_id == n.out_dci as u32 {
                    USBNET_TX_BUSY.store(false, core::sync::atomic::Ordering::Release);
                    continue;
                }
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
                                inject_pointer(hid, report);
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

/// Translate a report-protocol pointer report into cursor position + buttons.
/// M4-2: decode via the device's parsed report descriptor when available
/// (layout + logical range straight from the device); otherwise fall back to
/// the fixed usb-tablet layout.
fn inject_pointer(hid: &HidDevice, report: &[u8]) {
    if let Some(m) = &hid.map {
        if let Some((x, y, buttons, absolute)) = m.decode(report) {
            if absolute {
                // Rescale the device's logical range to the 0..0x7FFF the
                // cursor math expects.
                let lmax = m.x.map(|f| f.logical_max).unwrap_or(0x7FFF).max(1);
                let sx = (x.clamp(0, lmax) as i64 * 0x7FFF / lmax as i64) as u16;
                let sy = (y.clamp(0, lmax) as i64 * 0x7FFF / lmax as i64) as u16;
                crate::mouse::apply_usb_abs(sx, sy, buttons);
            } else {
                crate::mouse::apply_usb(x, y, buttons);
            }
            return;
        }
    }
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

/// A CDC-ECM USB network function (M3-3): bulk pipes carrying raw ethernet
/// frames. This is the class real USB-ethernet dongles and phone tethering
/// speak (ECM here; NCM is framing on top — later if needed).
const USBNET_RX_BUFS: usize = 8; // in-flight receive buffers (burst tolerance)

/// A configured USB Audio (UAC1) playback endpoint kept alive so the audio
/// layer can stream into it (M4-3 live wiring, not just the one-shot proof).
struct UsbAudio {
    slot: u8,
    ep_dci: u8,
    ring: Ring,
    buf: u64,      // contiguous DMA staging buffer for isoch packets
    buf_bytes: usize,
}

static mut USBAUDIO: Option<UsbAudio> = None;

pub fn usbaudio_present() -> bool {
    unsafe { (*core::ptr::addr_of!(USBAUDIO)).is_some() }
}

/// Stream a stereo-interleaved 16-bit PCM buffer (48 kHz) to the USB DAC as 1 ms
/// isochronous packets; returns the number of packets the device consumed.
/// This is the live mixer→USB path (fed by `euroaudio::Router::render`).
pub fn usbaudio_play(pcm: &[i16]) -> usize {
    unsafe {
        let xp = core::ptr::addr_of_mut!(XHCI);
        let x = match (*xp).as_mut() {
            Some(x) => x,
            None => return 0,
        };
        let a = match (*core::ptr::addr_of_mut!(USBAUDIO)).as_mut() {
            Some(a) => a,
            None => return 0,
        };
        const PKT_BYTES: usize = 192; // 48 frames × 2 ch × 2 B = 1 ms @ 48 kHz
        let bytes = (pcm.len() * 2).min(a.buf_bytes);
        core::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, a.buf as *mut u8, bytes);
        let npkts = bytes / PKT_BYTES;
        for k in 0..npkts {
            let pa = a.buf + (k * PKT_BYTES) as u64;
            a.ring.push(pa as u32, (pa >> 32) as u32, PKT_BYTES as u32, (TRB_ISOCH << 10) | (1 << 5) | (1 << 31));
        }
        doorbell(x, a.slot, a.ep_dci as u32);
        let mut done = 0usize;
        for _ in 0..npkts {
            match wait_event(x, TRB_EVT_TRANSFER, 30_000_000) {
                Some(ev) => {
                    if ((ev[3] >> 24) & 0xFF) as u8 == a.slot && (ev[3] >> 16) & 0x1F == a.ep_dci as u32 {
                        done += 1;
                    }
                }
                None => break,
            }
        }
        done
    }
}

struct UsbNet {
    slot: u8,
    in_dci: u8,
    out_dci: u8,
    in_ring: Ring,
    out_ring: Ring,
    rx_bufs: [u64; USBNET_RX_BUFS], // ring of 2 KiB receive buffers
    rx_deq: usize,                  // next buffer expected to complete (FIFO)
    tx_buf: u64,
    mac: [u8; 6],
}

static mut USBNET: Option<UsbNet> = None;
/// TX-in-flight flag, cleared by the event harvest when the OUT transfer
/// completes (poll runs in IRQ context too → atomics only).
static USBNET_TX_BUSY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Received frames, queued by the event harvest (IRQ context!) and popped by
/// `usbnet_poll_recv` (task context) — so the lock follows the irqsave rule.
static USBNET_RX: spin::Mutex<alloc::collections::VecDeque<alloc::vec::Vec<u8>>> =
    spin::Mutex::new(alloc::collections::VecDeque::new());

pub fn usbnet_present() -> bool {
    unsafe { (*core::ptr::addr_of!(USBNET)).is_some() }
}

pub fn usbnet_mac() -> Option<[u8; 6]> {
    unsafe { (*core::ptr::addr_of!(USBNET)).as_ref().map(|n| n.mac) }
}

/// Transmit one ethernet frame over the bulk-OUT pipe. Waits (bounded) for a
/// previous in-flight transmit, then fires without blocking on completion.
pub fn usbnet_send(frame: &[u8]) -> bool {
    use core::sync::atomic::Ordering;
    if frame.is_empty() || frame.len() > 2048 {
        return false;
    }
    // Drain a previous in-flight TX first. The 100 Hz timer + MSI-X harvest
    // clear TX_BUSY in the background; poll() here only as a light nudge so a
    // wedged pipe cannot hang the caller (bounded).
    for i in 0..200_000u64 {
        if !USBNET_TX_BUSY.load(Ordering::Acquire) {
            break;
        }
        if i % 4096 == 0 {
            poll();
        }
        core::hint::spin_loop();
    }
    if USBNET_TX_BUSY.load(Ordering::Acquire) {
        return false;
    }
    unsafe {
        let xp = core::ptr::addr_of_mut!(XHCI);
        let x = match (*xp).as_mut() {
            Some(x) => x,
            None => return false,
        };
        let n = match (*core::ptr::addr_of_mut!(USBNET)).as_mut() {
            Some(n) => n,
            None => return false,
        };
        core::ptr::copy_nonoverlapping(frame.as_ptr(), n.tx_buf as *mut u8, frame.len());
        USBNET_TX_BUSY.store(true, Ordering::Release);
        n.out_ring.push(n.tx_buf as u32, (n.tx_buf >> 32) as u32, frame.len() as u32, (TRB_NORMAL << 10) | (1 << 5));
        doorbell(x, n.slot, n.out_dci as u32);
    }
    true
}

/// Non-blocking receive of the next queued frame. A cheap pop: the 100 Hz
/// timer tick and the xHCI MSI-X handler harvest completed receives into the
/// queue in the background, so the network layer's millions-of-spins timeouts
/// don't each pay for an event-ring drain (that made TCP over USB effectively
/// hang). A rare empty-queue nudge covers the case where neither fired yet.
pub fn usbnet_poll_recv() -> Option<alloc::vec::Vec<u8>> {
    if let Some(f) = x86_64::instructions::interrupts::without_interrupts(|| USBNET_RX.lock().pop_front()) {
        return Some(f);
    }
    static NUDGE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    if NUDGE.fetch_add(1, core::sync::atomic::Ordering::Relaxed) % 1024 == 0 {
        poll();
        return x86_64::instructions::interrupts::without_interrupts(|| USBNET_RX.lock().pop_front());
    }
    None
}

/// Arm ALL receive buffers on the bulk-IN ring (burst tolerance).
unsafe fn usbnet_arm_all_rx(x: &Xhci, n: &mut UsbNet) {
    for &b in &n.rx_bufs {
        n.in_ring.push(b as u32, (b >> 32) as u32, 2048, (TRB_NORMAL << 10) | (1 << 5));
    }
    doorbell(x, n.slot, n.in_dci as u32);
}

/// Re-arm the buffer that just completed (FIFO) and advance the dequeue index.
unsafe fn usbnet_rearm_one(x: &Xhci, n: &mut UsbNet) {
    let b = n.rx_bufs[n.rx_deq % USBNET_RX_BUFS];
    n.in_ring.push(b as u32, (b >> 32) as u32, 2048, (TRB_NORMAL << 10) | (1 << 5));
    n.rx_deq = n.rx_deq.wrapping_add(1);
    doorbell(x, n.slot, n.in_dci as u32);
}
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

/// Configure a USB Audio Class playback function (M4-3) and prove the
/// isochronous OUT path: select the endpoint-bearing alternate setting of the
/// AudioStreaming interface, set the sampling frequency (UAC1 SET_CUR), then
/// stream a real stereo tone (euroaudio::mix) as 1 ms isochronous packets and
/// count the transfer completions — DMA-consumption proof, like HDA's LPIB.
///
/// Scope, honestly: UAC1 (what QEMU's usb-audio emulates and simple DACs/
/// headsets speak). UAC2-only devices (different descriptors/clock units) are
/// deferred until real-hardware validation. Playback proof only; the ongoing
/// mixer→USB wiring is follow-up work.
#[allow(clippy::too_many_arguments)]
unsafe fn setup_usbaudio(
    x: &mut Xhci,
    falloc: &mut FrameAllocator,
    slot: u8,
    port: u8,
    route: u32,
    speed: u32,
    tt: u32,
    cfg: &eurousb::Configuration,
    raw_len: u16,
    ep0: &mut Ring,
    in_ctx: u64,
    cs: u64,
    buf: u64,
) -> bool {
    // The AudioStreaming interface entry WITH the isoch OUT endpoint (that
    // parse entry is the endpoint-bearing alternate setting).
    let stream = match cfg.interfaces.iter().find(|i| {
        i.class == 0x01
            && i.subclass == 0x02
            && i.endpoints.iter().any(|e| !e.is_in() && e.attributes & 3 == 1)
    }) {
        Some(i) => i,
        None => {
            crate::serial_println!("[xhci] slot {slot}: audio without an isoch OUT endpoint");
            return false;
        }
    };
    let ep = stream.endpoints.iter().find(|e| !e.is_in() && e.attributes & 3 == 1).unwrap();

    // Recover the alternate-setting number from the raw config blob
    // (Configuration::parse drops bAlternateSetting).
    let raw = core::slice::from_raw_parts(buf as *const u8, raw_len as usize);
    let mut alt = 1u8;
    let mut p = 0usize;
    while p + 2 <= raw.len() {
        let l = raw[p] as usize;
        if l == 0 || p + l > raw.len() {
            break;
        }
        if l >= 9 && raw[p + 1] == 4 && raw[p + 2] == stream.number && raw[p + 4] > 0 {
            alt = raw[p + 3];
        }
        p += l;
    }

    if !control_no_data(x, slot, ep0, 0x00, 9, cfg.value as u16, 0) {
        crate::serial_println!("[xhci] slot {slot}: SET_CONFIGURATION (audio) failed");
        return false;
    }
    let _ = control_no_data(x, slot, ep0, 0x01, 11, alt as u16, stream.number as u16); // SET_INTERFACE

    // Configure the isochronous OUT endpoint. FS isoch bInterval is log2-coded
    // (2^(b-1) frames) — unlike FS interrupt (frames) — so encode directly.
    let ep_num = (ep.address & 0x0F) as u32;
    let ep_dci = (ep_num * 2) as u8; // OUT
    let ring_frame = falloc.allocate().expect("xhci audio-ring");
    let mut iso_ring = Ring::new(ring_frame);
    let interval = match speed {
        1 | 2 => (ep.interval.max(1) as u32 - 1 + 3).min(15), // FS/LS isoch: log2 + 3
        _ => encode_interval(speed, ep.interval),
    };
    build_input_context(in_ctx, cs, 1 | (1 << ep_dci), |slot_ctx, ep_ctxs| {
        w32(slot_ctx, (route & 0xF_FFFF) | (((ep_dci as u32) & 0x1F) << 27) | (speed << 20));
        w32(slot_ctx + 4, (port as u32) << 16);
        w32(slot_ctx + 8, tt);
        let epc = ep_ctxs + (ep_dci as u64 - 1) * cs;
        w32(epc, interval << 16);
        // type 1 = Isoch OUT; CErr must be 0 for isochronous endpoints.
        w32(epc + 4, (1 << 3) | ((ep.max_packet as u32) << 16));
        w64(epc + 8, ring_frame | 1);
        w32(epc + 16, ep.max_packet as u32);
    });
    let (cc, _) = run_command(x, in_ctx as u32, (in_ctx >> 32) as u32, (TRB_CONFIGURE_ENDPOINT << 10) | ((slot as u32) << 24))
        .unwrap_or((0, 0));
    if cc != CC_SUCCESS {
        crate::serial_println!("[xhci] slot {slot}: Configure-Endpoint (isoch audio) cc={cc}");
        return false;
    }

    // UAC1 SET_CUR sampling frequency on the endpoint: 48 kHz little-endian.
    let rate_buf = falloc.allocate().expect("xhci audio-rate");
    let rate = 48_000u32;
    core::ptr::copy_nonoverlapping(rate.to_le_bytes().as_ptr(), rate_buf as *mut u8, 3);
    let rate_ok = control_out(x, slot, ep0, 0x22, 0x01, 0x0100, ep.address as u16, 3, rate_buf);

    // Stream a real tone: 96 packets of 1 ms @ 48 kHz stereo 16-bit = 192 B each.
    const PKT_BYTES: usize = 192; // 48 samples × 2 ch × 2 B
    const NPKTS: usize = 96;
    let audio_buf = falloc
        .allocate_aligned((NPKTS * PKT_BYTES).div_ceil(4096), 1)
        .expect("xhci audio-buf");
    let tone = crate::hda::tone_for_usb(NPKTS * PKT_BYTES / 2); // i16 samples; usbaudio_play copies it in

    // Keep the endpoint alive so the audio layer can stream into it later.
    USBAUDIO = Some(UsbAudio {
        slot,
        ep_dci,
        ring: iso_ring,
        buf: audio_buf,
        buf_bytes: NPKTS * PKT_BYTES,
    });

    // Initial DMA-consumption proof: stream the tone once and count completions.
    let done = usbaudio_play(&tone);
    let ok = done >= NPKTS / 2;
    crate::serial_println!(
        "[m43] USB audio (UAC1) {}: isoch OUT ep DCI {ep_dci} @48 kHz stereo (rate-set={rate_ok}), {done}/{NPKTS} packets consumed by the device {}",
        if ok { "LIVE" } else { "NOT streaming" },
        if ok { "✓" } else { "✗" }
    );
    ok
}

/// Configure a CDC-ECM interface pair (M3-3): select the ECM configuration,
/// switch the data interface to its endpoint-bearing alternate setting, read
/// the MAC from the Ethernet functional descriptor's string, and open the
/// bulk pipes. `buf` still holds the RAW config blob of the chosen config.
#[allow(clippy::too_many_arguments)]
unsafe fn setup_usbnet(
    x: &mut Xhci,
    falloc: &mut FrameAllocator,
    slot: u8,
    port: u8,
    route: u32,
    speed: u32,
    tt: u32,
    cfg: &eurousb::Configuration,
    raw_len: u16,
    ep0: &mut Ring,
    in_ctx: u64,
    cs: u64,
    buf: u64,
) -> bool {
    // The data interface: class 0x0A with both bulk endpoints (that parse
    // entry is the endpoint-bearing alternate setting).
    let data = match cfg.interfaces.iter().find(|i| {
        i.class == 0x0A
            && i.endpoints.iter().any(|e| e.is_in() && e.attributes & 3 == 2)
            && i.endpoints.iter().any(|e| !e.is_in() && e.attributes & 3 == 2)
    }) {
        Some(d) => d,
        None => {
            crate::serial_println!("[xhci] slot {slot}: ECM without a bulk data interface");
            return false;
        }
    };
    let raw = core::slice::from_raw_parts(buf as *const u8, raw_len as usize);
    // Ethernet functional descriptor (0x24/0x0F): iMACAddress at offset 3.
    // Also recover the data interface's alternate-setting number from the raw
    // blob (Configuration::parse drops bAlternateSetting).
    let mut imac = 0u8;
    let mut alt = 1u8;
    let mut p = 0usize;
    while p + 2 <= raw.len() {
        let l = raw[p] as usize;
        if l == 0 || p + l > raw.len() {
            break;
        }
        if l >= 4 && raw[p + 1] == 0x24 && raw[p + 2] == 0x0F {
            imac = raw[p + 3];
        }
        if l >= 9 && raw[p + 1] == 4 && raw[p + 2] == data.number && raw[p + 4] > 0 {
            alt = raw[p + 3]; // interface descriptor with endpoints → its alt id
        }
        p += l;
    }

    // SET_CONFIGURATION for the ECM config, then activate the data alt-setting.
    if !control_no_data(x, slot, ep0, 0x00, 9, cfg.value as u16, 0) {
        crate::serial_println!("[xhci] slot {slot}: SET_CONFIGURATION (ECM) failed");
        return false;
    }
    let _ = control_no_data(x, slot, ep0, 0x01, 11, alt as u16, data.number as u16); // SET_INTERFACE

    // MAC address: a string descriptor of 12 UTF-16LE hex digits.
    let mut mac = [0u8; 6];
    if imac != 0
        && control_in(x, slot, ep0, 0x80, 6, 0x0300 | imac as u16, 0x0409, 64, buf)
    {
        let sd = core::slice::from_raw_parts(buf as *const u8, 64);
        let n = (sd[0] as usize).min(64);
        let hexval = |c: u8| -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            }
        };
        let mut nib = [0u8; 12];
        let mut got = 0usize;
        let mut q = 2;
        while q + 1 < n && got < 12 {
            if let Some(v) = hexval(sd[q]) {
                nib[got] = v;
                got += 1;
            }
            q += 2; // UTF-16LE: every other byte
        }
        if got == 12 {
            for k in 0..6 {
                mac[k] = (nib[k * 2] << 4) | nib[k * 2 + 1];
            }
        }
    }
    if mac == [0u8; 6] {
        // No usable iMACAddress: derive a stable locally-administered one.
        mac = [0x02, 0x45, 0x55, 0x52, 0x4F, slot];
        crate::serial_println!("[xhci] slot {slot}: ECM without MAC string — using locally administered");
    }

    // Open the bulk pipes (same shape as mass storage).
    let ein = data.endpoints.iter().find(|e| e.is_in() && e.attributes & 3 == 2).unwrap();
    let eout = data.endpoints.iter().find(|e| !e.is_in() && e.attributes & 3 == 2).unwrap();
    let in_dci = ((ein.address & 0x0F) * 2 + 1) as u8;
    let out_dci = ((eout.address & 0x0F) * 2) as u8;
    let in_ring_frame = falloc.allocate().expect("xhci ecm-in-ring");
    let out_ring_frame = falloc.allocate().expect("xhci ecm-out-ring");
    let in_ring = Ring::new(in_ring_frame);
    let out_ring = Ring::new(out_ring_frame);
    let max_dci = in_dci.max(out_dci) as u32;
    build_input_context(in_ctx, cs, 1 | (1 << in_dci) | (1 << out_dci), |slot_ctx, ep_ctxs| {
        w32(slot_ctx, (route & 0xF_FFFF) | ((max_dci & 0x1F) << 27) | (speed << 20));
        w32(slot_ctx + 4, (port as u32) << 16);
        w32(slot_ctx + 8, tt);
        let oc = ep_ctxs + (out_dci as u64 - 1) * cs;
        w32(oc + 4, (2 << 3) | (3 << 1) | ((eout.max_packet as u32) << 16));
        w64(oc + 8, out_ring_frame | 1);
        w32(oc + 16, eout.max_packet as u32);
        let ic = ep_ctxs + (in_dci as u64 - 1) * cs;
        w32(ic + 4, (6 << 3) | (3 << 1) | ((ein.max_packet as u32) << 16));
        w64(ic + 8, in_ring_frame | 1);
        w32(ic + 16, ein.max_packet as u32);
    });
    let (cc, _) = run_command(x, in_ctx as u32, (in_ctx >> 32) as u32, (TRB_CONFIGURE_ENDPOINT << 10) | ((slot as u32) << 24))
        .unwrap_or((0, 0));
    if cc != CC_SUCCESS {
        crate::serial_println!("[xhci] slot {slot}: Configure-Endpoint (ECM) cc={cc}");
        return false;
    }

    let mut rx_bufs = [0u64; USBNET_RX_BUFS];
    for b in &mut rx_bufs {
        *b = falloc.allocate().expect("xhci ecm-rx");
        core::ptr::write_bytes(*b as *mut u8, 0, 4096);
    }
    let tx_buf = falloc.allocate().expect("xhci ecm-tx");
    let mut un = UsbNet { slot, in_dci, out_dci, in_ring, out_ring, rx_bufs, rx_deq: 0, tx_buf, mac };
    usbnet_arm_all_rx(x, &mut un);
    crate::serial_println!(
        "[xhci] slot {slot}: USB ethernet (CDC-ECM) LIVE — MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ✓",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    USBNET = Some(un);
    true
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
