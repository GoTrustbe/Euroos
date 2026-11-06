//! MFI command frames: 64 bytes, little-endian, a common 24-byte header.
//!
//! Layouts (hardware facts, QEMU `hw/scsi/mfi.h`):
//!
//! | off | header field | | off | init | io (LD read/write) | pass-through | dcmd |
//! |---|---|---|---|---|---|---|---|
//! | 0 | cmd | | 24 | qinfo lo | sense lo | sense lo | opcode |
//! | 1 | sense_len | | 28 | qinfo hi | sense hi | sense hi | mbox[12] |
//! | 2 | cmd_status | | 32 | | lba lo | cdb[16] | |
//! | 3 | scsi_status | | 36 | | lba hi | | |
//! | 4 | target_id | | 40 | | SGL | | SGL |
//! | 5 | lun | | 48 | | | SGL | |
//! | 6 | cdb_len | | | | | | |
//! | 7 | sge_count | | | | | | |
//! | 8 | context (u64) | | | | | | |
//! | 16 | flags (u16) | | | | | | |
//! | 18 | timeout (u16) | | | | | | |
//! | 20 | data_len (u32) | | | | | | |
//!
//! One 64-bit SGL entry is 12 bytes (`addr u64, len u32`), so every frame
//! built here carries its single SGL entry inside its own 64 bytes.

pub const FRAME_SIZE: usize = 64;
pub const SENSE_LEN: usize = 128;
pub const QINFO_LEN: usize = 32;
/// The value `cmd_status` holds until the firmware completes the frame.
pub const STATUS_PENDING: u8 = 0xFF;

/// Frame commands.
pub const CMD_INIT: u8 = 0x00;
pub const CMD_LD_READ: u8 = 0x01;
pub const CMD_LD_WRITE: u8 = 0x02;
pub const CMD_LD_SCSI_IO: u8 = 0x03;
pub const CMD_DCMD: u8 = 0x05;

/// Frame flags.
pub const FLAG_DONT_POST_IN_REPLY_QUEUE: u16 = 0x0001;
pub const FLAG_SGL64: u16 = 0x0002;
pub const FLAG_SENSE64: u16 = 0x0004;
pub const FLAG_DIR_WRITE: u16 = 0x0008;
pub const FLAG_DIR_READ: u16 = 0x0010;

/// DCMD opcodes used by the driver.
pub const DCMD_CTRL_GET_INFO: u32 = 0x0101_0000;
pub const DCMD_CTRL_CACHE_FLUSH: u32 = 0x0110_1000;
pub const DCMD_LD_GET_LIST: u32 = 0x0301_0000;
/// mbox[0] for the cache flush: controller cache and disk caches.
pub const FLUSH_CTRL_AND_DISK: u8 = 0x03;

/// Completion status codes (a subset, for messages).
pub const STAT_OK: u8 = 0x00;
pub const STAT_INVALID_CMD: u8 = 0x01;
pub const STAT_INVALID_DCMD: u8 = 0x02;
pub const STAT_INVALID_PARAMETER: u8 = 0x03;
pub const STAT_DEVICE_NOT_FOUND: u8 = 0x0C;
pub const STAT_LD_LBA_OUT_OF_RANGE: u8 = 0x19;
pub const STAT_MEMORY_NOT_AVAILABLE: u8 = 0x20;
pub const STAT_SCSI_DONE_WITH_ERROR: u8 = 0x2D;
pub const STAT_SCSI_IO_FAILED: u8 = 0x2E;
pub const STAT_LD_OFFLINE: u8 = 0x33;

pub fn status_name(s: u8) -> &'static str {
    match s {
        STAT_OK => "ok",
        STAT_INVALID_CMD => "invalid command",
        STAT_INVALID_DCMD => "invalid DCMD",
        STAT_INVALID_PARAMETER => "invalid parameter",
        STAT_DEVICE_NOT_FOUND => "device not found",
        STAT_LD_LBA_OUT_OF_RANGE => "LBA out of range",
        STAT_MEMORY_NOT_AVAILABLE => "memory not available",
        STAT_SCSI_DONE_WITH_ERROR => "SCSI done with error",
        STAT_SCSI_IO_FAILED => "SCSI I/O failed",
        STAT_LD_OFFLINE => "LD offline",
        STATUS_PENDING => "pending",
        _ => "other",
    }
}

/// Data direction of a frame's buffer, seen from the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// The device writes host memory (a read from the disk / an info DCMD).
    FromDevice,
    /// The device reads host memory (a write to the disk).
    ToDevice,
}

/// One 64-bit scatter-gather entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sg {
    pub addr: u64,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// A CDB longer than 16 bytes or empty.
    CdbLength,
    /// An SGL entry of zero length or at address 0.
    EmptySg,
}

fn put16(f: &mut [u8], at: usize, v: u16) {
    f[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(f: &mut [u8], at: usize, v: u32) {
    f[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(f: &mut [u8], at: usize, v: u64) {
    f[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
pub(crate) fn get32(f: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([f[at], f[at + 1], f[at + 2], f[at + 3]])
}
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn get64(f: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&f[at..at + 8]);
    u64::from_le_bytes(b)
}

fn header(cmd: u8, context: u32, flags: u16, data_len: u32) -> [u8; FRAME_SIZE] {
    let mut f = [0u8; FRAME_SIZE];
    f[0] = cmd;
    f[2] = STATUS_PENDING;
    put64(&mut f, 8, context as u64);
    put16(&mut f, 16, flags);
    put32(&mut f, 20, data_len);
    f
}

fn put_sg(f: &mut [u8], at: usize, sg: Sg) -> Result<(), FrameError> {
    if sg.len == 0 || sg.addr == 0 {
        return Err(FrameError::EmptySg);
    }
    put64(f, at, sg.addr);
    put32(f, at + 8, sg.len);
    f[7] = 1; // sge_count
    Ok(())
}

fn dir_flag(d: Dir) -> u16 {
    match d {
        Dir::FromDevice => FLAG_DIR_READ,
        Dir::ToDevice => FLAG_DIR_WRITE,
    }
}

/// The init frame: points the firmware at the queue-info block. Polled (the
/// reply queue does not exist yet until this frame completes).
pub fn init(context: u32, qinfo_addr: u64) -> [u8; FRAME_SIZE] {
    let mut f = header(CMD_INIT, context, FLAG_DONT_POST_IN_REPLY_QUEUE, QINFO_LEN as u32);
    put32(&mut f, 24, qinfo_addr as u32);
    put32(&mut f, 28, (qinfo_addr >> 32) as u32);
    f
}

/// The queue-info block of the init frame: reply ring of `entries` u32
/// contexts at `ring`, producer index word at `pi`, consumer index word at `ci`.
/// Flags 0: 32-bit contexts.
pub fn qinfo(entries: u32, ring: u64, pi: u64, ci: u64) -> [u8; QINFO_LEN] {
    let mut q = [0u8; QINFO_LEN];
    put32(&mut q, 0, 0);
    put32(&mut q, 4, entries);
    put64(&mut q, 8, ring);
    put64(&mut q, 16, pi);
    put64(&mut q, 24, ci);
    q
}

/// A DCMD frame with an optional data buffer.
pub fn dcmd(context: u32, opcode: u32, mbox: [u8; 12], data: Option<(Sg, Dir)>) -> Result<[u8; FRAME_SIZE], FrameError> {
    let (flags, len) = match data {
        Some((sg, d)) => (FLAG_SGL64 | dir_flag(d), sg.len),
        None => (0, 0),
    };
    let mut f = header(CMD_DCMD, context, flags, len);
    put32(&mut f, 24, opcode);
    f[28..40].copy_from_slice(&mbox);
    if let Some((sg, _)) = data {
        put_sg(&mut f, 40, sg)?;
    }
    Ok(f)
}

/// A logical-drive read or write of `sectors` 512-byte sectors at `lba`.
/// `data_len` of an LD frame counts sectors, not bytes.
pub fn ld_io(context: u32, write: bool, target: u8, lba: u64, sectors: u32, sense: u64, buf: u64) -> Result<[u8; FRAME_SIZE], FrameError> {
    let dir = if write { Dir::ToDevice } else { Dir::FromDevice };
    let cmd = if write { CMD_LD_WRITE } else { CMD_LD_READ };
    let bytes = sectors.checked_mul(512).ok_or(FrameError::EmptySg)?;
    let mut f = header(cmd, context, FLAG_SGL64 | dir_flag(dir), sectors);
    f[1] = SENSE_LEN as u8;
    f[4] = target;
    put32(&mut f, 24, sense as u32);
    put32(&mut f, 28, (sense >> 32) as u32);
    put32(&mut f, 32, lba as u32);
    put32(&mut f, 36, (lba >> 32) as u32);
    put_sg(&mut f, 40, Sg { addr: buf, len: bytes })?;
    Ok(f)
}

/// A SCSI pass-through frame to logical drive `target` (CDB up to 16 bytes).
pub fn pass_ld(context: u32, target: u8, cdb: &[u8], sense: u64, data: Option<(Sg, Dir)>) -> Result<[u8; FRAME_SIZE], FrameError> {
    if cdb.is_empty() || cdb.len() > 16 {
        return Err(FrameError::CdbLength);
    }
    let (flags, len) = match data {
        Some((sg, d)) => (FLAG_SGL64 | dir_flag(d), sg.len),
        None => (0, 0),
    };
    let mut f = header(CMD_LD_SCSI_IO, context, flags, len);
    f[1] = SENSE_LEN as u8;
    f[4] = target;
    f[6] = cdb.len() as u8;
    put32(&mut f, 24, sense as u32);
    put32(&mut f, 28, (sense >> 32) as u32);
    f[32..32 + cdb.len()].copy_from_slice(cdb);
    if let Some((sg, _)) = data {
        put_sg(&mut f, 48, sg)?;
    }
    Ok(f)
}

/// READ CAPACITY(16) CDB (SERVICE ACTION IN 0x9E / 0x10), allocation length 32.
pub fn cdb_read_capacity16() -> [u8; 16] {
    let mut c = [0u8; 16];
    c[0] = 0x9E;
    c[1] = 0x10;
    c[13] = 32;
    c
}

/// (cmd_status, scsi_status) of a frame read back from memory.
pub fn status(frame: &[u8]) -> (u8, u8) {
    (frame[2], frame[3])
}

/// The context stored in a frame header (low 32 bits).
pub fn context_of(frame: &[u8]) -> u32 {
    get32(frame, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_frame_layout() {
        let f = init(0x0001_0001, 0x1_2345_6800);
        assert_eq!(f[0], CMD_INIT);
        assert_eq!(f[2], STATUS_PENDING);
        assert_eq!(context_of(&f), 0x0001_0001);
        assert_eq!(get32(&f, 20), 32); // data_len = sizeof(qinfo)
        assert_eq!(get32(&f, 24), 0x2345_6800);
        assert_eq!(get32(&f, 28), 1);
        assert_eq!(u16::from_le_bytes([f[16], f[17]]), FLAG_DONT_POST_IN_REPLY_QUEUE);
        let q = qinfo(1008, 0x1000, 0x840, 0x844);
        assert_eq!(get32(&q, 4), 1008);
        assert_eq!(get64(&q, 8), 0x1000);
        assert_eq!(get64(&q, 16), 0x840);
        assert_eq!(get64(&q, 24), 0x844);
    }

    #[test]
    fn dcmd_layout() {
        let f = dcmd(7, DCMD_CTRL_GET_INFO, [0; 12], Some((Sg { addr: 0x4000, len: 0x800 }, Dir::FromDevice))).unwrap();
        assert_eq!(f[0], CMD_DCMD);
        assert_eq!(f[7], 1);
        assert_eq!(get32(&f, 20), 0x800);
        assert_eq!(get32(&f, 24), DCMD_CTRL_GET_INFO);
        assert_eq!(get64(&f, 40), 0x4000);
        assert_eq!(get32(&f, 48), 0x800);
        assert_eq!(u16::from_le_bytes([f[16], f[17]]), FLAG_SGL64 | FLAG_DIR_READ);
        let mut mbox = [0u8; 12];
        mbox[0] = FLUSH_CTRL_AND_DISK;
        let f = dcmd(8, DCMD_CTRL_CACHE_FLUSH, mbox, None).unwrap();
        assert_eq!(f[7], 0);
        assert_eq!(f[28], 3);
        assert_eq!(get32(&f, 20), 0);
    }

    #[test]
    fn ld_io_layout() {
        let f = ld_io(9, true, 1, 0x1_0000_0002, 16, 0x3000, 0x8000).unwrap();
        assert_eq!(f[0], CMD_LD_WRITE);
        assert_eq!(f[4], 1);
        assert_eq!(get32(&f, 20), 16); // sectors, not bytes
        assert_eq!(get32(&f, 24), 0x3000);
        assert_eq!(get32(&f, 32), 2);
        assert_eq!(get32(&f, 36), 1);
        assert_eq!(get64(&f, 40), 0x8000);
        assert_eq!(get32(&f, 48), 16 * 512);
        assert_eq!(u16::from_le_bytes([f[16], f[17]]), FLAG_SGL64 | FLAG_DIR_WRITE);
        let r = ld_io(9, false, 0, 0, 1, 0x3000, 0x8000).unwrap();
        assert_eq!(r[0], CMD_LD_READ);
        assert_eq!(u16::from_le_bytes([r[16], r[17]]), FLAG_SGL64 | FLAG_DIR_READ);
    }

    #[test]
    fn pass_through_layout() {
        let cdb = cdb_read_capacity16();
        let f = pass_ld(3, 0, &cdb, 0x3080, Some((Sg { addr: 0x4800, len: 32 }, Dir::FromDevice))).unwrap();
        assert_eq!(f[0], CMD_LD_SCSI_IO);
        assert_eq!(f[6], 16);
        assert_eq!(&f[32..48], &cdb);
        assert_eq!(get64(&f, 48), 0x4800);
        assert_eq!(get32(&f, 56), 32);
        assert_eq!(pass_ld(3, 0, &[], 0, None), Err(FrameError::CdbLength));
        assert_eq!(pass_ld(3, 0, &[0u8; 17], 0, None), Err(FrameError::CdbLength));
    }

    #[test]
    fn status_names() {
        assert_eq!(status_name(STAT_OK), "ok");
        assert_eq!(status_name(STAT_SCSI_DONE_WITH_ERROR), "SCSI done with error");
        assert_eq!(status_name(STATUS_PENDING), "pending");
        assert_eq!(status_name(0x77), "other");
    }

    #[test]
    fn empty_sg_is_refused() {
        assert_eq!(ld_io(1, false, 0, 0, 0, 0x3000, 0x8000), Err(FrameError::EmptySg));
        assert_eq!(ld_io(1, false, 0, 0, 1, 0x3000, 0), Err(FrameError::EmptySg));
        assert!(dcmd(1, DCMD_LD_GET_LIST, [0; 12], Some((Sg { addr: 0x10, len: 0 }, Dir::FromDevice))).is_err());
        assert!(ld_io(1, false, 0, 0, u32::MAX, 0x3000, 0x8000).is_err()); // byte count overflows
    }
}
