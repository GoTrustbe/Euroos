//! BUG-111: the file table of the Linux ABI and EuroFS on disk.
//!
//! A Linux-ABI program works on `ring3::FILES`, a table in RAM. What reaches
//! the disk is decided after a foreground program from the Terminal ends:
//! [`write_back`] writes every file that was opened for writing and still has
//! its name, and removes from EuroFS what the program removed from the table.
//!
//! Three gaps made a document saved by a Linux program vanish (the atomic
//! save: write `a.txt.tmp`, rename it over `a.txt`):
//! - the dirty mark stayed on the temporary name, which no longer existed
//!   when the Terminal wrote back, so nothing was written;
//! - an `unlink` (and the old name of a rename) never reached EuroFS;
//! - a file written in an earlier boot is on disk but not in the table of
//!   the next boot, so a program did not see it at all.
//!
//! While the Terminal runs a program it lends the program its file system
//! ([`with_backing_fs`]): a path the table does not know is looked up in
//! EuroFS on first use and loaded into the table ([`fetch`]); `unlink` and
//! `rename` record the names that left the namespace ([`removed`]), and
//! `rename` carries the dirty mark to the new name. Outside such a run (boot
//! self-tests, the browser) nothing is looked up and nothing is recorded for
//! removal, as before.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use eurofs::FileSystem;
use spin::Mutex;

struct Backing(*mut (dyn FileSystem + 'static));
// SAFETY: the pointer is only dereferenced while BACKING is held, and only
// while `with_backing_fs` keeps the borrow it was made from alive.
unsafe impl Send for Backing {}

static BACKING: Mutex<Option<Backing>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Paths looked up in EuroFS during this run and not found there.
static MISSING: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Names a program removed from the namespace (unlink, the old name of a
/// rename) during a run with a backing file system.
static REMOVED: Mutex<Vec<String>> = Mutex::new(Vec::new());

const MISSING_MAX: usize = 4096;

/// Run `f` (a foreground program) with `fs` as the file system behind the
/// table. The borrow ends with the call; so does the lookup.
pub fn with_backing_fs<R>(fs: &mut dyn FileSystem, f: impl FnOnce() -> R) -> R {
    let p: *mut (dyn FileSystem + '_) = fs;
    // SAFETY: only the lifetime bound changes; the guard below clears the
    // pointer before the borrow it was made from ends.
    let p: *mut (dyn FileSystem + 'static) = unsafe { core::mem::transmute(p) };
    x86_64::instructions::interrupts::without_interrupts(|| {
        *BACKING.lock() = Some(Backing(p));
        MISSING.lock().clear();
        ACTIVE.store(true, Ordering::SeqCst);
    });
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            x86_64::instructions::interrupts::without_interrupts(|| {
                ACTIVE.store(false, Ordering::SeqCst);
                *BACKING.lock() = None;
                MISSING.lock().clear();
            });
        }
    }
    let _clear = Clear;
    f()
}

/// A program runs with a backing file system.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn never_fetched(path: &[u8]) -> bool {
    !path.starts_with(b"/")
        || path.starts_with(b"/proc/")
        || path.starts_with(b"/dev/")
        || path.starts_with(b"/sys/")
        || path == b"/etc/shadow"
        || path.len() > 1024
}

/// Load `path` from the backing file system into the table if the table does
/// not have it, the program did not remove it in this run, and EuroFS has it
/// as a file. Called before every lookup by path (open, stat, unlink, the old
/// name of a rename).
pub fn fetch(path: &[u8]) {
    if !active() || never_fetched(path) {
        return;
    }
    let Ok(p) = core::str::from_utf8(path) else { return };
    let _if_off = crate::sched::IfOffGuard::new();
    let b = BACKING.lock();
    let Some(Backing(fs)) = b.as_ref() else { return };
    if super::FILES.lock().iter().any(|(q, _)| q == p) || super::DISK_FILES.lock().iter().any(|(q, _, _, _)| q == p) {
        return;
    }
    if REMOVED.lock().iter().any(|q| q == p) || MISSING.lock().iter().any(|q| q == p) {
        return;
    }
    // SAFETY: see `Backing`; BACKING is held, nothing else uses the file system now.
    let fs: &dyn FileSystem = unsafe { &**fs };
    match fs.read_file(p) {
        Ok(bytes) => super::FILES.lock().push((String::from(p), alloc::borrow::Cow::Owned(bytes))),
        Err(_) => {
            let mut m = MISSING.lock();
            if m.len() < MISSING_MAX {
                m.push(String::from(p));
            }
        }
    }
}

/// `path` left the namespace (unlink, or the old name of a rename).
pub fn removed(path: &str) {
    if !active() || never_fetched(path.as_bytes()) {
        return;
    }
    let _if_off = crate::sched::IfOffGuard::new();
    let mut r = REMOVED.lock();
    if !r.iter().any(|q| q == path) {
        r.push(String::from(path));
    }
}

/// After a program: make EuroFS what the program left in the table. First the
/// names it removed (unless the same name is written now), then every file it
/// wrote. One line per change, for the Terminal.
pub fn write_back(fs: &mut dyn FileSystem) -> Vec<String> {
    let removed: Vec<String> = x86_64::instructions::interrupts::without_interrupts(|| core::mem::take(&mut *REMOVED.lock()));
    let written = super::take_dirty();
    let mut out = Vec::new();
    for p in removed {
        if written.iter().any(|(q, _)| *q == p) {
            continue;
        }
        let back = x86_64::instructions::interrupts::without_interrupts(|| super::FILES.lock().iter().any(|(q, _)| *q == p));
        if back {
            continue;
        }
        if fs.remove_file(&p).is_ok() {
            out.push(alloc::format!("[fs] {p} -> removed from EuroFS"));
        }
    }
    for (p, bytes) in written {
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
