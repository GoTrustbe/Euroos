//! Size-checked whole-file reads for the in-kernel apps and shell commands.
//!
//! `read_file` returns the file in one `Vec`. BUG-025 made that allocation
//! fallible, but a caller that then copies the buffer (UTF-8 conversion, a line
//! table, a decoded image, an undo snapshot) still aborts the kernel when the
//! copies no longer fit: the allocation-failure handler of a `no_std` kernel
//! panics. An editor has no business opening a 300 MiB file into a 512 MiB
//! heap, so the apps ask for the size first and refuse with a message.

use crate::fs::{EntryKind, FileSystem, FsError};
use alloc::vec::Vec;
use core::fmt;

/// Why a bounded read did not return the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundedReadError {
    /// The file has `size` bytes, the caller accepts at most `limit`.
    TooLarge { size: u64, limit: u64 },
    /// The filesystem refused (not found, permission, I/O, ...).
    Fs(FsError),
}

impl From<FsError> for BoundedReadError {
    fn from(e: FsError) -> Self {
        BoundedReadError::Fs(e)
    }
}

fn human(n: u64, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if n >= 10 * 1024 * 1024 {
        write!(f, "{} MiB", n / (1024 * 1024))
    } else if n >= 10 * 1024 {
        write!(f, "{} KiB", n / 1024)
    } else {
        write!(f, "{n} B")
    }
}

impl fmt::Display for BoundedReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BoundedReadError::TooLarge { size, limit } => {
                f.write_str("file too large to open (")?;
                human(*size, f)?;
                f.write_str("; at most ")?;
                human(*limit, f)?;
                f.write_str(" fits in memory here)")
            }
            BoundedReadError::Fs(e) => write!(f, "{e:?}"),
        }
    }
}

/// The largest file a caller should read in one piece: its own cap `app_cap`,
/// and never more than a `1/(copies+1)` share of the free heap, where `copies`
/// is how many further buffers of about the file's size the caller builds from
/// it (0 = it only passes the bytes on).
pub fn read_budget(free_heap: u64, app_cap: u64, copies: u64) -> u64 {
    app_cap.min(free_heap / (copies.saturating_add(1)))
}

/// Read `path` in one piece when it has at most `limit` bytes. The size is
/// taken from the metadata BEFORE anything is read; a filesystem that reports
/// a smaller size than it then delivers is caught after the read.
pub fn read_file_bounded(fs: &dyn FileSystem, path: &str, limit: u64) -> Result<Vec<u8>, BoundedReadError> {
    let meta = fs.metadata(path)?;
    if meta.kind == EntryKind::Directory {
        return Err(BoundedReadError::Fs(FsError::NotAFile));
    }
    if meta.size > limit {
        return Err(BoundedReadError::TooLarge { size: meta.size, limit });
    }
    let data = fs.read_file(path)?;
    if data.len() as u64 > limit {
        return Err(BoundedReadError::TooLarge { size: data.len() as u64, limit });
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ramdisk::RamDisk;
    use alloc::format;
    use alloc::vec;

    #[test]
    fn small_file_is_read() {
        let mut fs = RamDisk::new(1 << 20);
        fs.write_file("/a.txt", b"hello").unwrap();
        assert_eq!(read_file_bounded(&fs, "/a.txt", 5).unwrap(), b"hello");
    }

    #[test]
    fn file_over_the_limit_is_refused_before_reading() {
        let mut fs = RamDisk::new(1 << 20);
        fs.write_file("/big.bin", &vec![7u8; 4096]).unwrap();
        let e = read_file_bounded(&fs, "/big.bin", 4095).unwrap_err();
        assert_eq!(e, BoundedReadError::TooLarge { size: 4096, limit: 4095 });
        assert_eq!(format!("{e}"), "file too large to open (4096 B; at most 4095 B fits in memory here)");
    }

    #[test]
    fn missing_file_and_directory_keep_their_fs_error() {
        let mut fs = RamDisk::new(1 << 20);
        fs.create_dir("/d").unwrap();
        assert_eq!(read_file_bounded(&fs, "/nope", 10), Err(BoundedReadError::Fs(FsError::NotFound)));
        assert_eq!(read_file_bounded(&fs, "/d", 10), Err(BoundedReadError::Fs(FsError::NotAFile)));
    }

    #[test]
    fn budget_is_a_share_of_the_free_heap_capped_by_the_app() {
        // 512 MiB heap free, an editor that makes two copies, 16 MiB app cap.
        assert_eq!(read_budget(512 << 20, 16 << 20, 2), 16 << 20);
        // 30 MiB free: a third of it, below the app cap.
        assert_eq!(read_budget(30 << 20, 16 << 20, 2), 10 << 20);
        assert_eq!(read_budget(0, 16 << 20, 2), 0);
        assert_eq!(read_budget(100, u64::MAX, u64::MAX), 0);
    }

    #[test]
    fn message_uses_readable_units() {
        let e = BoundedReadError::TooLarge { size: 300 << 20, limit: 16 << 20 };
        assert_eq!(format!("{e}"), "file too large to open (300 MiB; at most 16 MiB fits in memory here)");
    }
}
