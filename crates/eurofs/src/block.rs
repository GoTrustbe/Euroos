//! `BlockDevice` abstraction over physical storage (NVMe/SATA) + an in-memory
//! implementation for unit tests and the Phase-1 ramdisk emulation.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    OutOfBounds,
    IoError,
    /// Buffer size is not a multiple of the block size.
    NotAligned,
}

pub type BlockResult<T> = Result<T, BlockError>;

/// A block-oriented storage device. All EuroFS I/O runs through this.
pub trait BlockDevice {
    fn block_size(&self) -> u32;
    fn block_count(&self) -> u64;

    /// Read `count` blocks starting at `start_block` into `buffer`.
    /// `buffer.len()` must be exactly `count * block_size`.
    fn read_blocks(&self, start_block: u64, count: u32, buffer: &mut [u8]) -> BlockResult<()>;

    /// Write `count` blocks starting at `start_block` from `buffer`.
    fn write_blocks(&mut self, start_block: u64, count: u32, buffer: &[u8]) -> BlockResult<()>;

    /// Force pending writes to permanent storage. Required before a
    /// CoW checkpoint commit, otherwise crash consistency is not guaranteed.
    fn flush(&mut self) -> BlockResult<()>;
}

/// Blanket impl: a `&mut D` is also a `BlockDevice`. Makes remount tests
/// possible (`EuroFs::format(&mut dev, ..)` followed by `EuroFs::mount(&mut dev, ..)`)
/// without giving up ownership of the device.
impl<D: BlockDevice + ?Sized> BlockDevice for &mut D {
    fn block_size(&self) -> u32 {
        (**self).block_size()
    }
    fn block_count(&self) -> u64 {
        (**self).block_count()
    }
    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        (**self).read_blocks(start, count, buf)
    }
    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        (**self).write_blocks(start, count, buf)
    }
    fn flush(&mut self) -> BlockResult<()> {
        (**self).flush()
    }
}

/// In-memory block device. For tests and as the backing store under the ramdisk.
pub struct MemoryBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    /// Counts flushes — useful in tests to prove that the checkpoint commit
    /// actually forces a flush.
    pub flush_count: u64,
}

impl MemoryBlockDevice {
    pub fn new(block_count: u64, block_size: u32) -> Self {
        assert!(block_size >= 512 && block_size.is_power_of_two());
        Self {
            data: vec![0u8; (block_count * block_size as u64) as usize],
            block_size,
            flush_count: 0,
        }
    }

    fn span(&self, start: u64, count: u32) -> BlockResult<(usize, usize)> {
        let bs = self.block_size as u64;
        let offset = start
            .checked_mul(bs)
            .ok_or(BlockError::OutOfBounds)? as usize;
        let len = (count as u64 * bs) as usize;
        if offset + len > self.data.len() {
            return Err(BlockError::OutOfBounds);
        }
        Ok((offset, len))
    }
}

impl BlockDevice for MemoryBlockDevice {
    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.data.len() as u64 / self.block_size as u64
    }

    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        let (offset, len) = self.span(start, count)?;
        if buf.len() != len {
            return Err(BlockError::NotAligned);
        }
        buf.copy_from_slice(&self.data[offset..offset + len]);
        Ok(())
    }

    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        let (offset, len) = self.span(start, count)?;
        if buf.len() != len {
            return Err(BlockError::NotAligned);
        }
        self.data[offset..offset + len].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        self.flush_count += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_blok() {
        let mut dev = MemoryBlockDevice::new(64, 4096);
        let mut out = vec![0u8; 4096];
        let mut in_buf = vec![0u8; 4096];
        in_buf[..5].copy_from_slice(b"hallo");
        dev.write_blocks(3, 1, &in_buf).unwrap();
        dev.read_blocks(3, 1, &mut out).unwrap();
        assert_eq!(&out[..5], b"hallo");
    }

    #[test]
    fn out_of_bounds_geweigerd() {
        let dev = MemoryBlockDevice::new(8, 512);
        let mut buf = vec![0u8; 512];
        assert_eq!(dev.read_blocks(8, 1, &mut buf), Err(BlockError::OutOfBounds));
    }

    #[test]
    fn verkeerde_buffergrootte_geweigerd() {
        let mut dev = MemoryBlockDevice::new(8, 512);
        let buf = vec![0u8; 500]; // not 512
        assert_eq!(dev.write_blocks(0, 1, &buf), Err(BlockError::NotAligned));
    }

    #[test]
    fn flush_wordt_geteld() {
        let mut dev = MemoryBlockDevice::new(4, 512);
        assert_eq!(dev.flush_count, 0);
        dev.flush().unwrap();
        assert_eq!(dev.flush_count, 1);
    }
}
