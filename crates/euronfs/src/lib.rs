//! **EuroNFS** — a sovereign NFSv3 client: mount a Unix NFS export into the EuroOS VFS.
//! ONC RPC (RFC 1057) + XDR over TCP, AUTH_UNIX (no crypto). Flow: portmap GETPORT →
//! MOUNT MNT → NFS LOOKUP / READ / READDIR / GETATTR / WRITE.
//!
//! Transport-agnostic via [`Connector`] (one TCP connection at a time, reconnected per
//! service port) so the same core runs on the host (std TCP, against a real `nfsd`) and
//! in the kernel (euronet TCP). `no_std`, no `unsafe`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::{DirEntry, EntryKind, FileSystem, FsError, FsResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfsError {
    Transport,
    Rpc,
    Mount(u32),
    Nfs(u32),
    NotFound,
    Unsupported,
}

/// Opens TCP connections to the server on a requested port (one at a time).
pub trait Connector {
    fn connect(&mut self, port: u16) -> Result<(), NfsError>;
    fn write_all(&mut self, data: &[u8]) -> Result<(), NfsError>;
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, NfsError>;
}

// ── XDR helpers (big-endian, 4-byte aligned) ──────────────────────────────────

fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_be_bytes());
}
fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_be_bytes());
}
fn put_opaque(b: &mut Vec<u8>, data: &[u8]) {
    put_u32(b, data.len() as u32);
    b.extend_from_slice(data);
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
}

struct Xdr<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Xdr<'a> {
    fn new(b: &'a [u8]) -> Self {
        Xdr { b, p: 0 }
    }
    fn u32(&mut self) -> Option<u32> {
        if self.p + 4 > self.b.len() {
            return None;
        }
        let v = u32::from_be_bytes([self.b[self.p], self.b[self.p + 1], self.b[self.p + 2], self.b[self.p + 3]]);
        self.p += 4;
        Some(v)
    }
    fn u64(&mut self) -> Option<u64> {
        let hi = self.u32()? as u64;
        let lo = self.u32()? as u64;
        Some((hi << 32) | lo)
    }
    fn opaque(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        if self.p + n > self.b.len() {
            return None;
        }
        let v = self.b[self.p..self.p + n].to_vec();
        self.p += n;
        while !self.p.is_multiple_of(4) && self.p < self.b.len() {
            self.p += 1;
        }
        Some(v)
    }
    fn skip(&mut self, n: usize) {
        self.p += n;
    }
    /// post_op_attr: bool(value_follows) + fattr3(84 bytes) if present.
    fn post_op_attr(&mut self) -> Option<()> {
        if self.u32()? != 0 {
            self.skip(84); // fattr3 is fixed 84 bytes
        }
        Some(())
    }
}

// Program numbers.
const PMAP_PROG: u32 = 100000;
const MOUNT_PROG: u32 = 100005;
const NFS_PROG: u32 = 100003;

pub struct NfsClient<C: Connector> {
    c: C,
    xid: u32,
    machine: String,
}

/// A directory entry.
pub struct NfsItem {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

impl<C: Connector> NfsClient<C> {
    pub fn new(c: C) -> Self {
        NfsClient { c, xid: 1, machine: String::from("euroos") }
    }

    /// One RPC call over the current connection (record-marked). Returns the result
    /// bytes (after the RPC reply header), or an error.
    fn call(&mut self, prog: u32, vers: u32, proc_: u32, args: &[u8]) -> Result<Vec<u8>, NfsError> {
        self.xid = self.xid.wrapping_add(1);
        let mut msg = Vec::new();
        put_u32(&mut msg, self.xid);
        put_u32(&mut msg, 0); // msg_type = CALL
        put_u32(&mut msg, 2); // rpcvers
        put_u32(&mut msg, prog);
        put_u32(&mut msg, vers);
        put_u32(&mut msg, proc_);
        // AUTH_UNIX credential.
        let mut cred = Vec::new();
        put_u32(&mut cred, 0); // stamp
        put_opaque(&mut cred, self.machine.as_bytes());
        put_u32(&mut cred, 0); // uid (root)
        put_u32(&mut cred, 0); // gid
        put_u32(&mut cred, 0); // gids count
        put_u32(&mut msg, 1); // AUTH_UNIX
        put_opaque(&mut msg, &cred);
        put_u32(&mut msg, 0); // verifier AUTH_NULL
        put_u32(&mut msg, 0); // verifier length 0
        msg.extend_from_slice(args);

        // Record marking: last fragment + length.
        let mut framed = Vec::with_capacity(4 + msg.len());
        put_u32(&mut framed, 0x8000_0000 | msg.len() as u32);
        framed.extend_from_slice(&msg);
        self.c.write_all(&framed)?;

        // Read the reply (handle the record marker, possibly multiple fragments).
        let mut reply = Vec::new();
        loop {
            let m = self.c.read_exact(4)?;
            let marker = u32::from_be_bytes([m[0], m[1], m[2], m[3]]);
            let last = marker & 0x8000_0000 != 0;
            let len = (marker & 0x7FFF_FFFF) as usize;
            reply.extend_from_slice(&self.c.read_exact(len)?);
            if last {
                break;
            }
        }
        // Parse the RPC reply header.
        let mut x = Xdr::new(&reply);
        let _xid = x.u32().ok_or(NfsError::Rpc)?;
        if x.u32().ok_or(NfsError::Rpc)? != 1 {
            return Err(NfsError::Rpc); // not a REPLY
        }
        if x.u32().ok_or(NfsError::Rpc)? != 0 {
            return Err(NfsError::Rpc); // reply_stat != MSG_ACCEPTED
        }
        // verifier (flavor + opaque)
        let _vf = x.u32().ok_or(NfsError::Rpc)?;
        let _ = x.opaque().ok_or(NfsError::Rpc)?;
        if x.u32().ok_or(NfsError::Rpc)? != 0 {
            return Err(NfsError::Rpc); // accept_stat != SUCCESS
        }
        Ok(reply[x.p..].to_vec())
    }

    /// portmap GETPORT (prog 100000 v2 proc 3) for a TCP service.
    fn getport(&mut self, prog: u32, vers: u32) -> Result<u16, NfsError> {
        self.c.connect(111)?;
        let mut a = Vec::new();
        put_u32(&mut a, prog);
        put_u32(&mut a, vers);
        put_u32(&mut a, 6); // IPPROTO_TCP
        put_u32(&mut a, 0);
        let r = self.call(PMAP_PROG, 2, 3, &a)?;
        let mut x = Xdr::new(&r);
        Ok(x.u32().ok_or(NfsError::Rpc)? as u16)
    }

    /// MOUNT MNT (prog 100005 v3 proc 1) → the export's root file handle.
    fn mnt(&mut self, mountd_port: u16, dirpath: &str) -> Result<Vec<u8>, NfsError> {
        self.c.connect(mountd_port)?;
        let mut a = Vec::new();
        put_opaque(&mut a, dirpath.as_bytes());
        let r = self.call(MOUNT_PROG, 3, 1, &a)?;
        let mut x = Xdr::new(&r);
        let status = x.u32().ok_or(NfsError::Rpc)?;
        if status != 0 {
            return Err(NfsError::Mount(status));
        }
        x.opaque().ok_or(NfsError::Rpc) // fhandle3
    }

    /// Connect + portmap + mount; leaves the connection on nfsd (2049). Returns root fh.
    pub fn mount(&mut self, export: &str) -> Result<Vec<u8>, NfsError> {
        let mountd_port = self.getport(MOUNT_PROG, 3)?;
        let root = self.mnt(mountd_port, export)?;
        // NFS is on the standard port 2049.
        self.c.connect(2049)?;
        Ok(root)
    }

    /// NFS LOOKUP (proc 3): resolve `name` in directory `dir_fh` → (fh, is_dir, size).
    fn lookup(&mut self, dir_fh: &[u8], name: &str) -> Result<(Vec<u8>, bool, u64), NfsError> {
        let mut a = Vec::new();
        put_opaque(&mut a, dir_fh);
        put_opaque(&mut a, name.as_bytes());
        let r = self.call(NFS_PROG, 3, 3, &a)?;
        let mut x = Xdr::new(&r);
        let status = x.u32().ok_or(NfsError::Rpc)?;
        if status == 2 {
            return Err(NfsError::NotFound); // NFS3ERR_NOENT
        }
        if status != 0 {
            return Err(NfsError::Nfs(status));
        }
        let fh = x.opaque().ok_or(NfsError::Rpc)?;
        // obj_attributes (post_op_attr): bool + fattr3. Parse type+size if present.
        let (mut is_dir, mut size) = (false, 0u64);
        if x.u32().ok_or(NfsError::Rpc)? != 0 {
            let ftype = x.u32().ok_or(NfsError::Rpc)?; // NF3DIR = 2
            is_dir = ftype == 2;
            x.skip(4 * 4); // mode, nlink, uid, gid
            size = x.u64().ok_or(NfsError::Rpc)?;
            x.skip(84 - (4 + 16 + 8)); // skip the rest of fattr3
        }
        Ok((fh, is_dir, size))
    }

    /// Resolve a slash path (relative to the export root) → (fh, is_dir, size).
    fn resolve(&mut self, root: &[u8], path: &str) -> Result<(Vec<u8>, bool, u64), NfsError> {
        let mut fh = root.to_vec();
        let mut is_dir = true;
        let mut size = 0;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let (f, d, s) = self.lookup(&fh, part)?;
            fh = f;
            is_dir = d;
            size = s;
        }
        Ok((fh, is_dir, size))
    }

    fn read_chunk(&mut self, fh: &[u8], offset: u64, count: u32) -> Result<(Vec<u8>, bool), NfsError> {
        let mut a = Vec::new();
        put_opaque(&mut a, fh);
        put_u64(&mut a, offset);
        put_u32(&mut a, count);
        let r = self.call(NFS_PROG, 3, 6, &a)?;
        let mut x = Xdr::new(&r);
        let status = x.u32().ok_or(NfsError::Rpc)?;
        if status != 0 {
            return Err(NfsError::Nfs(status));
        }
        x.post_op_attr().ok_or(NfsError::Rpc)?; // file_attributes
        let _count = x.u32().ok_or(NfsError::Rpc)?;
        let eof = x.u32().ok_or(NfsError::Rpc)? != 0;
        let data = x.opaque().ok_or(NfsError::Rpc)?;
        Ok((data, eof))
    }

    /// Read a whole file (path relative to the export root).
    pub fn read_file(&mut self, root: &[u8], path: &str) -> Result<Vec<u8>, NfsError> {
        let (fh, is_dir, size) = self.resolve(root, path)?;
        if is_dir {
            return Err(NfsError::Nfs(21)); // ISDIR
        }
        let mut out = Vec::with_capacity(size as usize);
        let mut off = 0u64;
        loop {
            let (chunk, eof) = self.read_chunk(&fh, off, 32768)?;
            if chunk.is_empty() {
                break;
            }
            off += chunk.len() as u64;
            out.extend_from_slice(&chunk);
            if eof || off >= size {
                break;
            }
        }
        Ok(out)
    }

    /// NFS WRITE (proc 7): write `data` to a file handle at `offset` (FILE_SYNC).
    fn write_chunk(&mut self, fh: &[u8], offset: u64, data: &[u8]) -> Result<(), NfsError> {
        let mut a = Vec::new();
        put_opaque(&mut a, fh);
        put_u64(&mut a, offset);
        put_u32(&mut a, data.len() as u32);
        put_u32(&mut a, 2); // stable = FILE_SYNC
        put_opaque(&mut a, data);
        let r = self.call(NFS_PROG, 3, 7, &a)?;
        let mut x = Xdr::new(&r);
        if x.u32().ok_or(NfsError::Rpc)? != 0 {
            return Err(NfsError::Nfs(0));
        }
        Ok(())
    }

    /// NFS CREATE (proc 8, UNCHECKED) then write the data — overwrite/create a file.
    pub fn write_file(&mut self, root: &[u8], path: &str, data: &[u8]) -> Result<(), NfsError> {
        let (parent, name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        let (dir_fh, _d, _s) = self.resolve(root, parent)?;
        let mut a = Vec::new();
        put_opaque(&mut a, &dir_fh);
        put_opaque(&mut a, name.as_bytes());
        put_u32(&mut a, 0); // mode = UNCHECKED
        // sattr3: set_mode(no) set_uid(no) set_gid(no) set_size(no) set_atime(0) set_mtime(0)
        put_u32(&mut a, 1); // set_mode = yes
        put_u32(&mut a, 0o644);
        put_u32(&mut a, 0);
        put_u32(&mut a, 0);
        put_u32(&mut a, 0);
        put_u32(&mut a, 0);
        put_u32(&mut a, 0);
        let r = self.call(NFS_PROG, 3, 8, &a)?;
        let mut x = Xdr::new(&r);
        if x.u32().ok_or(NfsError::Rpc)? != 0 {
            return Err(NfsError::Nfs(0));
        }
        // post_op_fh3: bool + fh.
        let fh = if x.u32().ok_or(NfsError::Rpc)? != 0 {
            x.opaque().ok_or(NfsError::Rpc)?
        } else {
            // Fall back to a LOOKUP if the server didn't return the new fh.
            self.resolve(root, path)?.0
        };
        let mut off = 0u64;
        for chunk in data.chunks(32768) {
            self.write_chunk(&fh, off, chunk)?;
            off += chunk.len() as u64;
        }
        Ok(())
    }

    /// NFS READDIR (proc 16): list a directory by path.
    pub fn list_dir(&mut self, root: &[u8], path: &str) -> Result<Vec<NfsItem>, NfsError> {
        let (dir_fh, _d, _s) = self.resolve(root, path)?;
        let mut out = Vec::new();
        let mut cookie = 0u64;
        let mut cookieverf = [0u8; 8];
        loop {
            let mut a = Vec::new();
            put_opaque(&mut a, &dir_fh);
            put_u64(&mut a, cookie);
            a.extend_from_slice(&cookieverf);
            put_u32(&mut a, 8192); // count
            let r = self.call(NFS_PROG, 3, 16, &a)?;
            let mut x = Xdr::new(&r);
            if x.u32().ok_or(NfsError::Rpc)? != 0 {
                return Err(NfsError::Nfs(0));
            }
            x.post_op_attr().ok_or(NfsError::Rpc)?; // dir_attributes
            let cv = x.opaque_fixed8().ok_or(NfsError::Rpc)?;
            cookieverf.copy_from_slice(&cv);
            // entries.
            let mut last_cookie = cookie;
            while x.u32().ok_or(NfsError::Rpc)? != 0 {
                let _fileid = x.u64().ok_or(NfsError::Rpc)?;
                let name = x.opaque().ok_or(NfsError::Rpc)?;
                let c = x.u64().ok_or(NfsError::Rpc)?;
                last_cookie = c;
                let nm = String::from_utf8_lossy(&name).into_owned();
                if nm != "." && nm != ".." {
                    out.push(NfsItem { name: nm, is_dir: false, size: 0 });
                }
            }
            let eof = x.u32().ok_or(NfsError::Rpc)? != 0;
            if eof || last_cookie == cookie {
                break;
            }
            cookie = last_cookie;
        }
        // Fill in is_dir/size via LOOKUP for each (READDIR has no attrs).
        for it in out.iter_mut() {
            if let Ok((_fh, d, s)) = self.lookup(&dir_fh, &it.name) {
                it.is_dir = d;
                it.size = s;
            }
        }
        Ok(out)
    }
}

impl<'a> Xdr<'a> {
    fn opaque_fixed8(&mut self) -> Option<[u8; 8]> {
        if self.p + 8 > self.b.len() {
            return None;
        }
        let mut o = [0u8; 8];
        o.copy_from_slice(&self.b[self.p..self.p + 8]);
        self.p += 8;
        Some(o)
    }
}

/// A mounted NFS export as a [`FileSystem`].
pub struct NfsFs<C: Connector> {
    inner: spin::Mutex<(NfsClient<C>, Vec<u8>)>, // (client, root file handle)
}

impl<C: Connector> NfsFs<C> {
    pub fn new(client: NfsClient<C>, root_fh: Vec<u8>) -> Self {
        NfsFs { inner: spin::Mutex::new((client, root_fh)) }
    }
}

fn map_err(e: NfsError) -> FsError {
    match e {
        NfsError::NotFound => FsError::NotFound,
        NfsError::Unsupported => FsError::Unsupported,
        _ => FsError::IoError,
    }
}

impl<C: Connector> FileSystem for NfsFs<C> {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let mut g = self.inner.lock();
        let root = g.1.clone();
        g.0.read_file(&root, path).map_err(map_err)
    }
    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let mut g = self.inner.lock();
        let root = g.1.clone();
        let items = g.0.list_dir(&root, path).map_err(map_err)?;
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
        let mut g = self.inner.lock();
        let root = g.1.clone();
        g.0.resolve(&root, path).is_ok()
    }
    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        let mut g = self.inner.lock();
        let root = g.1.clone();
        let (_fh, is_dir, size) = g.0.resolve(&root, path).map_err(map_err)?;
        let name = path.rsplit('/').find(|p| !p.is_empty()).unwrap_or("/").into();
        Ok(DirEntry {
            name,
            kind: if is_dir { EntryKind::Directory } else { EntryKind::File },
            size,
            mode: if is_dir { 0o755 } else { 0o644 },
            mtime: 0,
        })
    }
    fn space_info(&self) -> (u64, u64) {
        (0, 0)
    }
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        let mut g = self.inner.lock();
        let root = g.1.clone();
        g.0.write_file(&root, path, data).map_err(map_err)
    }
    fn remove_file(&mut self, _path: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
    }
    fn create_dir(&mut self, _path: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
    }
}
