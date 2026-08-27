//! Trash (recycle bin) with undo: deleting a file moves it to `/var/trash`
//! instead of destroying it, and remembers where it came from so it can be put
//! back. This is what turns an irreversible delete into a recoverable one, one
//! of the everyday conveniences a desktop is expected to have.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use eurofs::FileSystem;
use spin::Mutex;

const TRASH_DIR: &str = "/var/trash";
/// (path inside the trash, original path) for each trashed item, oldest first.
static ITEMS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static CTR: AtomicU64 = AtomicU64::new(1);

fn ensure_dir(fs: &mut dyn FileSystem) {
    let _ = fs.create_dir("/var");
    let _ = fs.create_dir(TRASH_DIR);
}

/// Move a file to the trash. Returns false if it could not be moved (e.g. the
/// path is a directory, which this first version does not handle).
pub fn to_trash(fs: &mut dyn FileSystem, path: &str) -> bool {
    ensure_dir(fs);
    let name = path.rsplit('/').next().unwrap_or("file");
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    let tp = alloc::format!("{TRASH_DIR}/{n}-{name}");
    if fs.rename(path, &tp).is_ok() {
        ITEMS.lock().push((tp, path.to_string()));
        true
    } else {
        false
    }
}

/// Put the most recently trashed item back where it came from (undo delete).
/// Returns the restored original path.
pub fn restore_last(fs: &mut dyn FileSystem) -> Option<String> {
    let item = ITEMS.lock().pop()?;
    let (tp, orig) = item;
    if fs.rename(&tp, &orig).is_ok() {
        Some(orig)
    } else {
        ITEMS.lock().push((tp, orig));
        None
    }
}

/// How many items are in the trash (so a menu can offer Restore only when useful).
pub fn count() -> usize {
    ITEMS.lock().len()
}

/// Original paths of trashed items, most recent first (for the `trash` command).
pub fn list_lines() -> Vec<String> {
    ITEMS.lock().iter().rev().map(|(_, orig)| orig.clone()).collect()
}

/// Permanently delete everything in the trash. Returns how many were removed.
pub fn empty(fs: &mut dyn FileSystem) -> usize {
    let items: Vec<(String, String)> = ITEMS.lock().drain(..).collect();
    let mut n = 0;
    for (tp, _) in items {
        if fs.remove_file(&tp).is_ok() {
            n += 1;
        }
    }
    n
}

/// `[trash]` boot self-test: a real delete-to-trash and undo round-trip on the
/// live filesystem (leaves nothing behind).
pub fn selftest(fs: &mut dyn FileSystem) {
    let orig = "/var/trash-selftest.txt";
    let _ = fs.write_file(orig, b"bye");
    let moved = to_trash(fs, orig);
    let gone = fs.read_file(orig).is_err();
    let listed = count() >= 1;
    let restored = restore_last(fs).as_deref() == Some(orig);
    let back = fs.read_file(orig).is_ok();
    let _ = fs.remove_file(orig); // clean up the scratch file
    let ok = moved && gone && listed && restored && back;
    crate::serial_println!(
        "[trash] Trash + undo: delete-to-trash={moved}, original-gone={gone}, restore-brings-it-back={restored} → {}",
        if ok { "OK (deletes are recoverable) ✓" } else { "FAILED ✗" }
    );
}
