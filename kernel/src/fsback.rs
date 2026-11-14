//! BUG-111: the file table of the Linux ABI and EuroFS on disk.
//!
//! What the Terminal did after a foreground program until now, as a function,
//! so the `[rename-disk]` self-test runs the same write-back: every path that
//! was opened for writing and still exists under that name is written to
//! EuroFS. (The fix follows in the next commit.)

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::FileSystem;

/// Run `f` (a foreground program) with `fs` as the file system behind the table.
pub fn with_backing_fs<R>(fs: &mut dyn FileSystem, f: impl FnOnce() -> R) -> R {
    let _ = fs;
    f()
}

/// After a program: write back what it wrote. One line per change, for the Terminal.
pub fn write_back(fs: &mut dyn FileSystem) -> Vec<String> {
    let mut out = Vec::new();
    for (p, bytes) in super::take_dirty() {
        let n = bytes.len();
        if fs.write_file(&p, &bytes).is_ok() {
            out.push(alloc::format!("[fs] {p} ({n} B) -> EuroFS synced"));
        }
    }
    out
}

/// For the `[rename-disk]` self-test: forget every table entry under
/// `prefix`, as the table of the next boot would not have them.
pub fn forget_for_test(prefix: &str) {
    let mut dead = Vec::new();
    x86_64::instructions::interrupts::without_interrupts(|| {
        for (i, (q, _)) in super::FILES.lock().iter_mut().enumerate() {
            if q.starts_with(prefix) {
                q.clear();
                dead.push(i);
            }
        }
        super::MKDIRS.lock().retain(|d| !d.starts_with(prefix));
    });
    super::reclaim_tombstones(&dead);
}
