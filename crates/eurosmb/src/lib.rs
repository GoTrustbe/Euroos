//! **EuroSMB** — a sovereign SMB2/3 client: mount a network share (NAS / Windows /
//! Samba) into the EuroOS VFS. SMB2 over TCP (port 445) with NTLMv2 authentication.
//! Transport-agnostic (a [`Transport`] feeds bytes) so the same core runs on the host
//! (std TCP, against a real Samba) and in the kernel (euronet TCP).
//!
//! `no_std`, no `unsafe`. The crypto is verified against RFC vectors; the protocol is
//! verified against a live Samba server (see `examples/smbtest.rs`).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod crypto;
pub mod ntlm;
pub mod smb2;

pub use smb2::{SmbClient, SmbError, Transport};

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::{DirEntry, EntryKind, FileSystem, FsError, FsResult};

/// A mounted SMB share as a [`FileSystem`] for the VFS. Wraps an authenticated,
/// tree-connected [`SmbClient`] behind a lock so the read-only `&self` trait methods
/// can drive the (stateful) protocol.
pub struct SmbFs<T: Transport> {
    inner: spin::Mutex<SmbClient<T>>,
}

impl<T: Transport> SmbFs<T> {
    /// Wrap an already negotiated + authenticated + tree-connected client.
    pub fn new(client: SmbClient<T>) -> Self {
        SmbFs { inner: spin::Mutex::new(client) }
    }
}

/// VFS path ("/a/b") → SMB share-relative path ("a\\b"); root → "".
fn smb_path(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    trimmed.replace('/', "\\")
}

fn map_err(e: SmbError) -> FsError {
    match e {
        SmbError::NotFound => FsError::NotFound,
        SmbError::Auth => FsError::PermissionDenied,
        SmbError::Unsupported => FsError::Unsupported,
        _ => FsError::IoError,
    }
}

impl<T: Transport> FileSystem for SmbFs<T> {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        self.inner.lock().read_file(&smb_path(path)).map_err(map_err)
    }
    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let items = self.inner.lock().list_dir(&smb_path(path)).map_err(map_err)?;
        Ok(items
            .into_iter()
            .map(|i| DirEntry {
                name: i.name,
                kind: if i.is_dir { EntryKind::Directory } else { EntryKind::File },
                size: i.size,
                mode: if i.is_dir { 0o755 } else { 0o644 },
                mtime: 0,
            })
            .collect())
    }
    fn exists(&self, path: &str) -> bool {
        self.inner.lock().exists(&smb_path(path))
    }
    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        // Derive from the parent listing (SMB has no cheap single-path stat here).
        let p = path.trim_end_matches('/');
        let (parent, name) = match p.rfind('/') {
            Some(i) => (&p[..i], &p[i + 1..]),
            None => ("", p),
        };
        for e in self.list_dir(if parent.is_empty() { "/" } else { parent })? {
            if e.name.eq_ignore_ascii_case(name) {
                return Ok(e);
            }
        }
        Err(FsError::NotFound)
    }
    fn space_info(&self) -> (u64, u64) {
        (0, 0) // SMB doesn't report this cheaply here
    }
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        self.inner.lock().write_file(&smb_path(path), data).map_err(map_err)
    }
    fn remove_file(&mut self, _path: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
    }
    fn create_dir(&mut self, _path: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
    }
}
