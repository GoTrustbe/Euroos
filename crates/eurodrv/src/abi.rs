//! The driver syscall ABI: numbers, error codes and the info block.
//!
//! Drivers run under the kernel's `driver` syscall profile, which allows these
//! calls plus log, clock, sleep and exit from the Euro API, and nothing else.
//! Every call is checked against the caller's device grant; a process without
//! a grant gets [`EPERM`].

/// ABI generation reported in [`DriverInfo::abi`].
pub const ABI_VERSION: u64 = 1;

/// `drv_info(out, len)`: write the [`DriverInfo`] words (at most `len` bytes).
pub const SYS_DRV_INFO: u64 = 0x6200;
/// `ipc_recv(buf, max)`: one EuroIPC message for this driver's port. A pending
/// interrupt notification comes first, as an `Irq` message. Returns the
/// length, or [`EAGAIN`] after sleeping one scheduler tick when nothing is
/// pending.
pub const SYS_IPC_RECV: u64 = 0x6201;
/// `ipc_send(port, buf, len)`: send one message. Only the ports named in the
/// grant (the netstack port) accept it.
pub const SYS_IPC_SEND: u64 = 0x6202;

/// First and last driver syscall number (the `driver` profile allows the range).
pub const SYS_FIRST: u64 = SYS_DRV_INFO;
pub const SYS_LAST: u64 = SYS_IPC_SEND;

/// Error codes (negative on the wire).
pub const EPERM: i64 = -1;
pub const ESRCH: i64 = -3;
pub const EAGAIN: i64 = -11;
pub const EFAULT: i64 = -14;
pub const EINVAL: i64 = -22;
pub const ENOSPC: i64 = -28;

/// [`DriverInfo::flags`]: DMA is confined by an IOMMU domain.
pub const FLAG_IOMMU: u64 = 1 << 0;
/// [`DriverInfo::flags`]: the device interrupts through MSI-X (otherwise the
/// driver is serviced on its receive timeout: polling).
pub const FLAG_MSIX: u64 = 1 << 1;

/// Number of little-endian u64 words in the info block.
pub const INFO_WORDS: usize = 12;

/// What the kernel tells a driver about its grant. Addresses are in the
/// driver's own address space; the DMA window's addresses are also the bus
/// addresses the device must be given (IOVA == virtual == physical).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DriverInfo {
    pub abi: u64,
    pub vendor: u64,
    pub device: u64,
    /// bus << 8 | dev << 3 | func (the PCI requester id).
    pub bdf: u64,
    pub mmio_base: u64,
    pub mmio_len: u64,
    pub dma_base: u64,
    pub dma_len: u64,
    /// The EuroIPC port this driver receives on.
    pub own_port: u64,
    /// The EuroIPC port of the netstack (where `Hello`/`Rx` go).
    pub stack_port: u64,
    /// The interrupt vector, 0 when none.
    pub irq_vector: u64,
    pub flags: u64,
}

impl DriverInfo {
    pub fn to_words(&self) -> [u64; INFO_WORDS] {
        [
            self.abi, self.vendor, self.device, self.bdf, self.mmio_base, self.mmio_len,
            self.dma_base, self.dma_len, self.own_port, self.stack_port, self.irq_vector, self.flags,
        ]
    }

    pub fn from_words(w: &[u64; INFO_WORDS]) -> DriverInfo {
        DriverInfo {
            abi: w[0],
            vendor: w[1],
            device: w[2],
            bdf: w[3],
            mmio_base: w[4],
            mmio_len: w[5],
            dma_base: w[6],
            dma_len: w[7],
            own_port: w[8],
            stack_port: w[9],
            irq_vector: w[10],
            flags: w[11],
        }
    }

    /// Serialize into `out` (little-endian words); returns the bytes written.
    pub fn write_bytes(&self, out: &mut [u8]) -> usize {
        let mut n = 0;
        for w in self.to_words() {
            let b = w.to_le_bytes();
            let take = b.len().min(out.len() - n);
            out[n..n + take].copy_from_slice(&b[..take]);
            n += take;
            if n == out.len() {
                break;
            }
        }
        n
    }

    /// Parse from bytes (a short buffer leaves the remaining words zero).
    pub fn read_bytes(b: &[u8]) -> DriverInfo {
        let mut w = [0u64; INFO_WORDS];
        for (i, slot) in w.iter_mut().enumerate() {
            let s = i * 8;
            if s + 8 <= b.len() {
                let mut a = [0u8; 8];
                a.copy_from_slice(&b[s..s + 8]);
                *slot = u64::from_le_bytes(a);
            }
        }
        DriverInfo::from_words(&w)
    }
}

/// Is `num` a driver syscall?
pub fn is_driver_call(num: u64) -> bool {
    (SYS_FIRST..=SYS_LAST).contains(&num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_roundtrip() {
        let i = DriverInfo {
            abi: ABI_VERSION, vendor: 0x8086, device: 0x10d3, bdf: 0x0010, mmio_base: 0xfeb8_0000,
            mmio_len: 0x2_0000, dma_base: 0x0123_4000, dma_len: 32 * 4096, own_port: 0x4e00,
            stack_port: 0x4e10, irq_vector: 0x4c, flags: FLAG_IOMMU | FLAG_MSIX,
        };
        let mut b = [0u8; INFO_WORDS * 8];
        assert_eq!(i.write_bytes(&mut b), INFO_WORDS * 8);
        assert_eq!(DriverInfo::read_bytes(&b), i);
    }

    #[test]
    fn short_buffers_are_safe() {
        let i = DriverInfo { abi: 1, vendor: 0x8086, ..Default::default() };
        let mut b = [0u8; 11];
        assert_eq!(i.write_bytes(&mut b), 11);
        let back = DriverInfo::read_bytes(&b);
        assert_eq!(back.abi, 1);
        assert_eq!(back.vendor, 0); // word 1 only partly present -> not trusted
        assert_eq!(DriverInfo::read_bytes(&[]), DriverInfo::default());
    }

    #[test]
    fn driver_call_range() {
        assert!(is_driver_call(SYS_DRV_INFO));
        assert!(is_driver_call(SYS_IPC_SEND));
        assert!(!is_driver_call(SYS_IPC_SEND + 1));
        assert!(!is_driver_call(0x610C));
    }
}
