//! **BB-2** — native **modern-virtio (virtio-1.0)** transport + **virtio-gpu** 2D
//! driver. The existing virtio-net/blk use the *legacy* transport (port I/O
//! from BAR0); the modern transport publishes its register blocks via
//! PCI capabilities in MMIO BARs. Because the kernel **identity-maps** the lower
//! 512 GiB (`paging.rs`), every BAR/heap address is at once physical and
//! virtual — no separate MMIO mapping or IOMMU needed for DMA.
//!
//! The control virtqueue sends the `eurogpu` command stream (host-tested) to the
//! real device: `GET_DISPLAY_INFO` → `RESOURCE_CREATE_2D` → `ATTACH_BACKING` →
//! `SET_SCANOUT` → `TRANSFER_TO_HOST_2D` → `RESOURCE_FLUSH`, and thus presents the
//! framebuffer on the virtio-gpu screen.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};

use crate::pci;

// ── volatile MMIO (identity-mapped physical = virtual) ─────────────────────
unsafe fn r8(a: u64) -> u8 { core::ptr::read_volatile(a as *const u8) }
unsafe fn w8(a: u64, v: u8) { core::ptr::write_volatile(a as *mut u8, v) }
unsafe fn r16(a: u64) -> u16 { core::ptr::read_volatile(a as *const u16) }
unsafe fn w16(a: u64, v: u16) { core::ptr::write_volatile(a as *mut u16, v) }
unsafe fn r32(a: u64) -> u32 { core::ptr::read_volatile(a as *const u32) }
unsafe fn w32(a: u64, v: u32) { core::ptr::write_volatile(a as *mut u32, v) }
unsafe fn w64(a: u64, v: u64) { core::ptr::write_volatile(a as *mut u64, v) }

// virtio_pci_common_cfg field offsets (little-endian MMIO).
const C_DEV_FEAT_SEL: u64 = 0x00;
const C_DEV_FEAT: u64 = 0x04;
const C_DRV_FEAT_SEL: u64 = 0x08;
const C_DRV_FEAT: u64 = 0x0C;
const C_STATUS: u64 = 0x14;
const C_Q_SELECT: u64 = 0x16;
const C_Q_SIZE: u64 = 0x18;
const C_Q_ENABLE: u64 = 0x1C;
const C_Q_NOTIFY_OFF: u64 = 0x1E;
const C_Q_DESC: u64 = 0x20;
const C_Q_DRIVER: u64 = 0x28;
const C_Q_DEVICE: u64 = 0x30;

const S_ACK: u8 = 1;
const S_DRIVER: u8 = 2;
const S_DRIVER_OK: u8 = 4;
const S_FEAT_OK: u8 = 8;

// Split-virtqueue descriptor flags.
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

/// Allocate `size` bytes of zeroed memory with the requested alignment and return its
/// address (= physical = virtual). We deliberately "leak" it: the GPU driver lives as
/// long as the kernel, so the DMA rings must never be freed.
fn dma_zeroed(size: usize, align: usize) -> u64 {
    use alloc::alloc::{alloc_zeroed, Layout};
    let layout = Layout::from_size_align(size.max(1), align).unwrap();
    let p = unsafe { alloc_zeroed(layout) };
    p as u64
}

/// One split virtqueue (the GPU's control queue).
struct Vq {
    size: u16,
    desc: u64,
    avail: u64,
    used: u64,
    notify: u64,
    avail_idx: u16,
    last_used: u16,
}

pub struct VirtioGpu {
    common: u64,
    vq: Vq,
    pub width: u32,
    pub height: u32,
    /// RAM framebuffer (DMA backing of the scanout resource) + dimensions. We
    /// copy the desktop backbuffer here (no device VRAM: virtio-gpu
    /// DMAs out of ordinary RAM).
    fb: u64,
    sw: u32,
    sh: u32,
}

/// The LIVE scanout driver: binds the real GOP framebuffer to the virtio-gpu
/// screen so the desktop is presented via OUR driver (not OVMF-GOP).
static VGPU: spin::Mutex<Option<VirtioGpu>> = spin::Mutex::new(None);

/// Find the virtio-gpu PCI device (vendor 0x1AF4, device 0x1050 modern / 0x1010 trans).
fn find_gpu() -> Option<pci::PciDevice> {
    pci::find(|d| d.vendor == 0x1AF4 && (d.device == 0x1050 || d.device == 0x1010))
}

/// **BB-2 finishing piece** — set up a LIVE scanout: allocate a RAM framebuffer
/// (`width*height` B8G8R8A8 pixels), make a 2D resource of it, attach the RAM
/// region as backing, and bind it to scanout 0. After that `present_frame()`
/// pushes each desktop frame here. Returns false if there is no virtio-gpu device.
pub fn init_scanout(width: u32, height: u32) -> bool {
    let mut gpu = match VirtioGpu::init() {
        Some(g) => g,
        None => return false,
    };
    let fb = dma_zeroed((width * height * 4) as usize, 16);
    gpu.fb = fb;
    gpu.sw = width;
    gpu.sh = height;
    let r1 = gpu.submit(&eurogpu::resource_create_2d(1, eurogpu::FORMAT_B8G8R8A8_UNORM, width, height), 32);
    let r2 = gpu.submit(&eurogpu::resource_attach_backing(1, fb, width * height * 4), 32);
    let r3 = gpu.submit(&eurogpu::set_scanout(0, 1, width, height), 32);
    let ok = [&r1, &r2, &r3].iter().all(|r| ok_resp(r));
    if ok {
        *VGPU.lock() = Some(gpu);
    }
    ok
}

/// Copy the desktop backbuffer (0x00RRGGBB pixels from `graphics`) to the
/// virtio-gpu RAM framebuffer (B8G8R8A8, alpha forced opaque) and transfer+flush
/// it to the screen. Call this after each `FrameBuffer::present()`; no-op without
/// an active virtio-gpu scanout. `src` = backbuffer ptr, `stride` = scanline in pixels.
pub fn present_frame(src: *const u32, src_w: usize, src_h: usize, stride: usize) {
    let mut g = VGPU.lock();
    let gpu = match g.as_mut() {
        Some(g) => g,
        None => return,
    };
    let (fb, sw, sh) = (gpu.fb, gpu.sw as usize, gpu.sh as usize);
    let w = src_w.min(sw);
    let h = src_h.min(sh);
    unsafe {
        for y in 0..h {
            let srow = src.add(y * stride);
            let drow = (fb as *mut u32).add(y * sw);
            for x in 0..w {
                // 0x00RRGGBB → B8G8R8A8 (same byte order) with opaque alpha.
                core::ptr::write_volatile(drow.add(x), core::ptr::read(srow.add(x)) | 0xFF00_0000);
            }
        }
    }
    gpu.submit(&eurogpu::transfer_to_host_2d(1, gpu.sw, gpu.sh), 32);
    gpu.submit(&eurogpu::resource_flush(1, gpu.sw, gpu.sh), 32);
}

/// Is there an active virtio-gpu scanout (our native driver presents the screen)?
pub fn scanout_active() -> bool {
    VGPU.lock().is_some()
}

impl VirtioGpu {
    /// Bring the device up via the **modern** transport and set up the control queue.
    pub fn init() -> Option<VirtioGpu> {
        let dev = find_gpu()?;
        pci::claim(dev.bus, dev.dev, dev.func, "virtio-gpu"); // hwprobe (M1-3)
        // Enable bus-master (DMA) + MMIO decode.
        dev.enable(0x0006);
        let common = dev.virtio_cap(1)?.addr; // common cfg
        let notify_cap = dev.virtio_cap(2)?; // notify cfg

        unsafe {
            // 1) Reset, then ACK + DRIVER.
            w8(common + C_STATUS, 0);
            // short settle loop until the device confirms the reset (status 0).
            for _ in 0..100_000 {
                if r8(common + C_STATUS) == 0 {
                    break;
                }
            }
            w8(common + C_STATUS, S_ACK);
            w8(common + C_STATUS, S_ACK | S_DRIVER);

            // 2) Feature negotiation: require VIRTIO_F_VERSION_1 (bit 32 → sel 1, bit 0).
            w32(common + C_DEV_FEAT_SEL, 1);
            let _hi = r32(common + C_DEV_FEAT);
            w32(common + C_DRV_FEAT_SEL, 0);
            w32(common + C_DRV_FEAT, 0);
            w32(common + C_DRV_FEAT_SEL, 1);
            w32(common + C_DRV_FEAT, 1); // VERSION_1
            w8(common + C_STATUS, S_ACK | S_DRIVER | S_FEAT_OK);
            if r8(common + C_STATUS) & S_FEAT_OK == 0 {
                return None; // device refused our features
            }

            // 3) Set up the control queue (index 0).
            w16(common + C_Q_SELECT, 0);
            let qsize = r16(common + C_Q_SIZE);
            if qsize == 0 {
                return None;
            }
            let desc = dma_zeroed(qsize as usize * 16, 16);
            let avail = dma_zeroed(6 + qsize as usize * 2, 2);
            let used = dma_zeroed(6 + qsize as usize * 8, 4);
            w64(common + C_Q_DESC, desc);
            w64(common + C_Q_DRIVER, avail);
            w64(common + C_Q_DEVICE, used);
            let notify_off = r16(common + C_Q_NOTIFY_OFF);
            let notify = notify_cap.addr + notify_off as u64 * notify_cap.notify_mult as u64;
            w16(common + C_Q_ENABLE, 1);

            // 4) DRIVER_OK — the device is now operational.
            w8(common + C_STATUS, S_ACK | S_DRIVER | S_FEAT_OK | S_DRIVER_OK);

            let mut gpu = VirtioGpu {
                common,
                vq: Vq { size: qsize, desc, avail, used, notify, avail_idx: 0, last_used: 0 },
                width: 0,
                height: 0,
                fb: 0,
                sw: 0,
                sh: 0,
            };
            // 5) Query the screen (real round-trip over the control queue).
            let resp = gpu.submit(&eurogpu::get_display_info(), 256);
            if let Some((w, h)) = parse_display_info(&resp) {
                gpu.width = w;
                gpu.height = h;
            }
            Some(gpu)
        }
    }

    /// Send one control command (`cmd`) and read back up to `resp_cap` response bytes.
    /// Two descriptors: device-readable command + device-writable response.
    fn submit(&mut self, cmd: &[u8], resp_cap: usize) -> Vec<u8> {
        // Buffers for command + response (leaked; live alongside the driver).
        let cmd_buf = dma_zeroed(cmd.len().max(1), 8);
        let resp_buf = dma_zeroed(resp_cap, 8);
        unsafe {
            core::ptr::copy_nonoverlapping(cmd.as_ptr(), cmd_buf as *mut u8, cmd.len());

            let qs = self.vq.size as usize;
            // Descriptor 0 = command (read-only for the device), chains to 1.
            let d0 = self.vq.desc;
            w64(d0, cmd_buf);
            w32(d0 + 8, cmd.len() as u32);
            w16(d0 + 12, VRING_DESC_F_NEXT);
            w16(d0 + 14, 1);
            // Descriptor 1 = response (device-writable).
            let d1 = self.vq.desc + 16;
            w64(d1, resp_buf);
            w32(d1 + 8, resp_cap as u32);
            w16(d1 + 12, VRING_DESC_F_WRITE);
            w16(d1 + 14, 0);

            // Avail ring: offer head descriptor 0.
            let slot = self.vq.avail_idx as usize % qs;
            w16(self.vq.avail + 4 + slot as u64 * 2, 0);
            fence(Ordering::SeqCst);
            self.vq.avail_idx = self.vq.avail_idx.wrapping_add(1);
            w16(self.vq.avail + 2, self.vq.avail_idx);
            fence(Ordering::SeqCst);

            // Notify the control queue (queue index 0).
            w16(self.vq.notify, 0);

            // Poll the used ring until it advances (bounded → cannot hang).
            let mut spins = 0u64;
            loop {
                let used_idx = r16(self.vq.used + 2);
                if used_idx != self.vq.last_used {
                    self.vq.last_used = used_idx;
                    break;
                }
                spins += 1;
                if spins > 50_000_000 {
                    return Vec::new(); // device did not respond
                }
                core::hint::spin_loop();
            }

            // Read the response out of the response buffer.
            let mut out = Vec::with_capacity(resp_cap);
            for i in 0..resp_cap {
                out.push(r8(resp_buf + i as u64));
            }
            out
        }
    }

    /// Present a framebuffer on scanout 0: create a 2D resource, attach the
    /// backing, set the scanout, transfer + flush. `fb` = `width*height` B8G8R8A8 pixels.
    pub fn present(&mut self, fb_addr: u64, w: u32, h: u32) -> bool {
        let r1 = self.submit(&eurogpu::resource_create_2d(1, eurogpu::FORMAT_B8G8R8A8_UNORM, w, h), 32);
        let r2 = self.submit(&eurogpu::resource_attach_backing(1, fb_addr, w * h * 4), 32);
        let r3 = self.submit(&eurogpu::set_scanout(0, 1, w, h), 32);
        let r4 = self.submit(&eurogpu::transfer_to_host_2d(1, w, h), 32);
        let r5 = self.submit(&eurogpu::resource_flush(1, w, h), 32);
        [&r1, &r2, &r3, &r4, &r5].iter().all(|r| ok_resp(r))
    }
}

/// A virtio-gpu ctrl response is OK if the type is 0x1100 (OK_NODATA) or 0x1101
/// (OK_DISPLAY_INFO) (first u32 of the ctrl header, little-endian).
fn ok_resp(resp: &[u8]) -> bool {
    if resp.len() < 4 {
        return false;
    }
    let t = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]);
    t == 0x1100 || t == 0x1101
}

/// Parse an OK_DISPLAY_INFO response → (width, height) of scanout 0. Layout:
/// ctrl_hdr(24 bytes) + 16 × virtio_gpu_display_one{ rect{x,y,w,h}(16) + enabled(4) + flags(4) }.
fn parse_display_info(resp: &[u8]) -> Option<(u32, u32)> {
    if resp.len() < 24 + 24 {
        return None;
    }
    let t = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]);
    if t != 0x1101 {
        return None;
    }
    let base = 24; // after the ctrl header
    let rd = |o: usize| u32::from_le_bytes([resp[o], resp[o + 1], resp[o + 2], resp[o + 3]]);
    let w = rd(base + 8); // rect.width
    let h = rd(base + 12); // rect.height
    if w == 0 || h == 0 {
        None
    } else {
        Some((w, h))
    }
}

/// **BB-2 boot self-test** — prove the native modern-virtio transport against a
/// real `virtio-gpu-pci` device: init handshake (reset→features→queue→DRIVER_OK),
/// then a real `GET_DISPLAY_INFO` round-trip over the control virtqueue.
pub fn selftest() {
    match find_gpu() {
        None => {
            crate::serial_println!(
                "[bb2] virtio-gpu: no device present (boot with QEMU -device virtio-gpu-pci to prove the modern-virtio driver)"
            );
        }
        Some(dev) => {
            let has_modern = dev.virtio_cap(1).is_some();
            match VirtioGpu::init() {
                Some(mut gpu) if gpu.width > 0 => {
                    // Present a sovereign test fill (EU blue) on the scanout.
                    let (w, h) = (gpu.width.min(1280), gpu.height.min(1024));
                    let fb = dma_zeroed((w * h * 4) as usize, 16);
                    unsafe {
                        for i in 0..(w * h) as u64 {
                            // B8G8R8A8: EU blue #2D6BE0 → B=0xE0 G=0x6B R=0x2D A=0xFF
                            w32(fb + i * 4, 0xFF2D6BE0);
                        }
                    }
                    let presented = gpu.present(fb, w, h);
                    crate::serial_println!(
                        "[bb2] virtio-gpu NATIVE modern-virtio driver ✓: PCI caps found, init handshake (reset→VERSION_1→control-vq→DRIVER_OK), GET_DISPLAY_INFO over control virtqueue → screen {}x{}, scanout-present(create2d→backing→scanout→transfer→flush)={} (sovereign, no OVMF-GOP)",
                        gpu.width, gpu.height, presented
                    );
                }
                _ => {
                    crate::serial_println!(
                        "[bb2] virtio-gpu device present (modern-caps={}) but init/GET_DISPLAY_INFO did not return a valid screen",
                        has_modern
                    );
                }
            }
        }
    }
}

/// `gpu` shell command: show the virtio-gpu status.
pub fn shell() -> Vec<String> {
    match find_gpu() {
        None => alloc::vec![String::from("virtio-gpu: no device (boot with -device virtio-gpu-pci)")],
        Some(dev) => alloc::vec![
            alloc::format!("virtio-gpu  : {:04x}:{:04x} on {:02x}:{:02x}.{}", dev.vendor, dev.device, dev.bus, dev.dev, dev.func),
            alloc::format!("transport   : modern virtio-1.0 (common-cfg cap present: {})", dev.virtio_cap(1).is_some()),
            String::from("commands    : GET_DISPLAY_INFO → CREATE_2D → ATTACH_BACKING → SET_SCANOUT → TRANSFER → FLUSH"),
        ],
    }
}
