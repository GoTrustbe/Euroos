//! System-context guard: kernel services (journal, audit, identity store) write
//! system files regardless of who is logged in. They run their FS access under
//! uid 0 explicitly and restore the session uid afterwards — an auditable,
//! narrow bypass for kernel services, not a route user actions can take.

use eurofs::fs::FileSystem;

/// Run `f` with the FS in system context (uid 0), restoring the session uid.
pub fn as_system<R>(fs: &mut dyn FileSystem, f: impl FnOnce(&mut dyn FileSystem) -> R) -> R {
    let prev = fs.uid_context();
    fs.set_uid_context(0);
    let r = f(fs);
    fs.set_uid_context(prev);
    r
}
