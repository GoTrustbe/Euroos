//! **3B-7 (VM-verifiable)** — native **virtio-sound** driver on the shared
//! modern virtio transport (`virtio_pci.rs`, the same one virtio-blk/net/gpu/
//! input use). Where the Intel HDA path finds no controller under QEMU's
//! default machine, `virtio-sound-pci` IS emulated, so this driver is provable
//! in a VM: it brings the device up, reads the device config (how many PCM
//! streams), and round-trips a `VIRTIO_SND_R_PCM_INFO` control request that the
//! device answers `VIRTIO_SND_S_OK`.
//!
//! Scope: this proves the transport + control queue live in a VM. Actual PCM
//! playback (SET_PARAMS → PREPARE → START + PCM frames on the tx queue, fed by
//! the [`euroaudio::Router`] mix) is the remaining, larger step and is honestly
//! not claimed here.

use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};

use crate::pci;

const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

// virtio-sound control request codes + status.
const VIRTIO_SND_R_PCM_INFO: u32 = 0x0100;
/// virtio-sound status codes are 0x8000-based (NOT the 0x8000_0000 used by some
/// other virtio device classes): OK=0x8000, BAD_MSG=0x8001, NOT_SUPP=0x8002.
const VIRTIO_SND_S_OK: u32 = 0x8000;

#[inline]
unsafe fn r8(a: u64) -> u8 {
    core::ptr::read_volatile(a as *const u8)
}
#[inline]
unsafe fn r16(a: u64) -> u16 {
    core::ptr::read_volatile(a as *const u16)
}
#[inline]
unsafe fn w16(a: u64, v: u16) {
    core::ptr::write_volatile(a as *mut u16, v)
}
#[inline]
unsafe fn w32(a: u64, v: u32) {
    core::ptr::write_volatile(a as *mut u32, v)
}
#[inline]
unsafe fn w64(a: u64, v: u64) {
    core::ptr::write_volatile(a as *mut u64, v)
}

fn dma_zeroed(size: usize, align: usize) -> u64 {
    use alloc::alloc::{alloc_zeroed, Layout};
    let layout = Layout::from_size_align(size.max(1), align).unwrap();
    let pa = unsafe { alloc_zeroed(layout) as u64 };
    crate::iommu::dma_add("virtio-snd", pa, size.max(1)); // heap-backed DMA buffer: into the device's domain
    pa
}

struct Vq {
    size: u16,
    desc: u64,
    avail: u64,
    used: u64,
    avail_idx: u16,
    last_used: u16,
}

pub struct VirtioSnd {
    tp: crate::virtio_pci::Transport,
    q: crate::virtio_pci::Queue,
    ctrl: Vq,
    /// Device config: number of jacks / PCM streams / channel maps.
    pub jacks: u32,
    pub streams: u32,
    pub chmaps: u32,
}

/// Find the virtio-sound PCI device (vendor 0x1AF4; device 0x1059 modern /
/// 0x1019 transitional = virtio device type 25).
fn find_snd() -> Option<pci::PciDevice> {
    pci::find(|d| d.vendor == 0x1AF4 && (d.device == 0x1059 || d.device == 0x1019))
}

pub fn present() -> bool {
    find_snd().is_some()
}

impl VirtioSnd {
    /// Bring the device up via the modern transport (no device features
    /// accepted), set up the control queue, and read the device config.
    pub fn init() -> Option<VirtioSnd> {
        let dev = find_snd()?;
        pci::claim(dev.bus, dev.dev, dev.func, "virtio-snd"); // hwprobe (M1-3)
        let mut tp = crate::virtio_pci::Transport::probe(&dev)?;
        if !tp.is_modern() {
            return None;
        }
        tp.begin();
        tp.negotiate(0)?;
        let q = tp.setup_queue(0, 64, &mut |pages| Some(dma_zeroed(pages * 4096, 4096)))?;
        tp.driver_ok();
        // virtio_snd_config: jacks, streams, chmaps.
        let (jacks, streams, chmaps) = (tp.cfg32(0), tp.cfg32(4), tp.cfg32(8));
        let ctrl = Vq { size: q.size, desc: q.desc, avail: q.avail, used: q.used, avail_idx: 0, last_used: 0 };
        Some(VirtioSnd { tp, q, ctrl, jacks, streams, chmaps })
    }

    /// Send a control request and read back `resp_cap` response bytes.
    fn control(&mut self, req: &[u8], resp_cap: usize) -> Vec<u8> {
        let cmd_buf = dma_zeroed(req.len().max(1), 8);
        let resp_buf = dma_zeroed(resp_cap, 8);
        unsafe {
            core::ptr::copy_nonoverlapping(req.as_ptr(), cmd_buf as *mut u8, req.len());
            // Descriptor 0 = request (device-readable), chains to 1.
            let d0 = self.ctrl.desc;
            w64(d0, cmd_buf);
            w32(d0 + 8, req.len() as u32);
            w16(d0 + 12, VRING_DESC_F_NEXT);
            w16(d0 + 14, 1);
            // Descriptor 1 = response (device-writable).
            let d1 = self.ctrl.desc + 16;
            w64(d1, resp_buf);
            w32(d1 + 8, resp_cap as u32);
            w16(d1 + 12, VRING_DESC_F_WRITE);
            w16(d1 + 14, 0);

            let qs = self.ctrl.size as usize;
            let slot = self.ctrl.avail_idx as usize % qs;
            w16(self.ctrl.avail + 4 + slot as u64 * 2, 0);
            fence(Ordering::SeqCst);
            self.ctrl.avail_idx = self.ctrl.avail_idx.wrapping_add(1);
            w16(self.ctrl.avail + 2, self.ctrl.avail_idx);
            fence(Ordering::SeqCst);
            self.tp.notify(&self.q);

            let mut spins = 0u64;
            loop {
                let used_idx = r16(self.ctrl.used + 2);
                if used_idx != self.ctrl.last_used {
                    self.ctrl.last_used = used_idx;
                    break;
                }
                spins += 1;
                if spins > 50_000_000 {
                    return Vec::new();
                }
                core::hint::spin_loop();
            }
            let mut out = Vec::with_capacity(resp_cap);
            for i in 0..resp_cap {
                out.push(r8(resp_buf + i as u64));
            }
            out
        }
    }

    /// Query PCM stream info (VIRTIO_SND_R_PCM_INFO for `count` streams from 0).
    /// Returns the device's status word (VIRTIO_SND_S_OK on success).
    pub fn pcm_info_status(&mut self, count: u32) -> u32 {
        // struct virtio_snd_query_info { hdr{code:u32}, start_id:u32, count:u32, size:u32 }
        let pcm_info_size = 32u32; // sizeof(virtio_snd_pcm_info) header portion
        let mut req = Vec::with_capacity(16);
        req.extend_from_slice(&VIRTIO_SND_R_PCM_INFO.to_le_bytes());
        req.extend_from_slice(&0u32.to_le_bytes()); // start_id
        req.extend_from_slice(&count.to_le_bytes());
        req.extend_from_slice(&pcm_info_size.to_le_bytes());
        let resp = self.control(&req, 4 + (count as usize).max(1) * pcm_info_size as usize);
        if resp.len() >= 4 {
            u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]])
        } else {
            0
        }
    }
}

/// `[3b7]` boot self-test — prove the modern-virtio transport + control queue
/// against a real emulated virtio-sound device. Boot QEMU with
/// `-device virtio-sound-pci` (see scripts/run-snd.sh).
pub fn selftest() {
    match VirtioSnd::init() {
        None => {
            crate::serial_println!(
                "[3b7] virtio-snd: no device present (boot with QEMU -device virtio-sound-pci to prove the modern-virtio audio driver)"
            );
        }
        Some(mut snd) => {
            let (jacks, streams, chmaps) = (snd.jacks, snd.streams, snd.chmaps);
            let status = if streams > 0 { snd.pcm_info_status(streams.min(8)) } else { 0 };
            let ok = streams > 0 && status == VIRTIO_SND_S_OK;
            crate::serial_println!(
                "[3b7] virtio-snd NATIVE modern-virtio driver: init handshake (reset→VERSION_1→control-vq→DRIVER_OK), config: jacks={jacks} streams={streams} chmaps={chmaps}, PCM_INFO control round-trip status={status:#010x} → {}",
                if ok { "OK (device answered VIRTIO_SND_S_OK over the control queue; sovereign audio transport) ✓" } else { "device present but control query did not return OK" }
            );
        }
    }
}
