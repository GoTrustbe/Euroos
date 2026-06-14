//! SMB2 client: NEGOTIATE → SESSION_SETUP (NTLMv2) → TREE_CONNECT → CREATE /
//! QUERY_DIRECTORY / READ / WRITE / CLOSE, over a [`Transport`] (TCP). Dialect 2.0.2,
//! no signing/encryption (fine for a non-DC Samba/Windows share over a trusted link).

use crate::ntlm;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmbError {
    Transport,
    Protocol(u32),
    Auth,
    NotFound,
    Unsupported,
}

/// Byte transport (a TCP connection). The client adds the SMB2 "Direct TCP" 4-byte
/// length framing on top.
pub trait Transport {
    fn write_all(&mut self, data: &[u8]) -> Result<(), SmbError>;
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, SmbError>;
}

// SMB2 command codes.
const NEGOTIATE: u16 = 0x0000;
const SESSION_SETUP: u16 = 0x0001;
const TREE_CONNECT: u16 = 0x0003;
const CREATE: u16 = 0x0005;
const CLOSE: u16 = 0x0006;
const READ: u16 = 0x0008;
const WRITE: u16 = 0x0009;
const QUERY_DIRECTORY: u16 = 0x000E;

const STATUS_SUCCESS: u32 = 0x0000_0000;
const STATUS_MORE_PROCESSING: u32 = 0xC000_0016;
const STATUS_NO_MORE_FILES: u32 = 0x8000_0006;

pub struct SmbClient<T: Transport> {
    t: T,
    mid: u64,
    session_id: u64,
    tree_id: u32,
}

/// One directory entry returned by a listing.
pub struct DirItem {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}
fn utf16le(s: &str) -> Vec<u8> {
    let mut v = Vec::new();
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

impl<T: Transport> SmbClient<T> {
    pub fn new(t: T) -> Self {
        SmbClient { t, mid: 0, session_id: 0, tree_id: 0 }
    }

    fn header(&mut self, cmd: u16) -> [u8; 64] {
        let mut h = [0u8; 64];
        h[0..4].copy_from_slice(&[0xFE, b'S', b'M', b'B']);
        h[4..6].copy_from_slice(&64u16.to_le_bytes()); // StructureSize
        h[6..8].copy_from_slice(&1u16.to_le_bytes()); // CreditCharge
        h[12..14].copy_from_slice(&cmd.to_le_bytes());
        h[14..16].copy_from_slice(&256u16.to_le_bytes()); // CreditRequest
        h[24..32].copy_from_slice(&self.mid.to_le_bytes());
        h[36..40].copy_from_slice(&self.tree_id.to_le_bytes());
        h[40..48].copy_from_slice(&self.session_id.to_le_bytes());
        self.mid += 1;
        h
    }

    /// Send a message (header+body) with the 4-byte Direct-TCP length prefix and read
    /// the response. Returns (status, full_response_message).
    fn exchange(&mut self, cmd: u16, body: &[u8]) -> Result<(u32, Vec<u8>), SmbError> {
        let h = self.header(cmd);
        let mut msg = Vec::with_capacity(64 + body.len());
        msg.extend_from_slice(&h);
        msg.extend_from_slice(body);
        let mut framed = Vec::with_capacity(4 + msg.len());
        let len = msg.len() as u32;
        framed.push(0);
        framed.extend_from_slice(&len.to_be_bytes()[1..]); // 24-bit big-endian length
        framed.extend_from_slice(&msg);
        self.t.write_all(&framed)?;

        let prefix = self.t.read_exact(4)?;
        let rlen = ((prefix[1] as usize) << 16) | ((prefix[2] as usize) << 8) | prefix[3] as usize;
        let resp = self.t.read_exact(rlen)?;
        if resp.len() < 64 {
            return Err(SmbError::Transport);
        }
        let status = u32le(&resp, 8);
        Ok((status, resp))
    }

    pub fn negotiate(&mut self) -> Result<(), SmbError> {
        // NEGOTIATE request. Offer 2.0.2 … 3.0.2 (NOT 3.1.1 — that needs mandatory
        // negotiate contexts/pre-auth integrity we don't implement). Server picks one.
        let dialects: [u16; 4] = [0x0202, 0x0210, 0x0300, 0x0302];
        let mut b = Vec::new();
        b.extend_from_slice(&36u16.to_le_bytes()); // StructureSize
        b.extend_from_slice(&(dialects.len() as u16).to_le_bytes()); // DialectCount
        b.extend_from_slice(&1u16.to_le_bytes()); // SecurityMode = SIGNING_ENABLED
        b.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        b.extend_from_slice(&0u32.to_le_bytes()); // Capabilities
        b.extend_from_slice(&[0u8; 16]); // ClientGuid
        b.extend_from_slice(&0u64.to_le_bytes()); // ClientStartTime
        for d in dialects {
            b.extend_from_slice(&d.to_le_bytes());
        }
        let (status, _r) = self.exchange(NEGOTIATE, &b)?;
        if status != STATUS_SUCCESS {
            return Err(SmbError::Protocol(status));
        }
        Ok(())
    }

    fn session_setup_msg(&mut self, sec: &[u8]) -> Result<(u32, Vec<u8>), SmbError> {
        let mut b = Vec::new();
        b.extend_from_slice(&25u16.to_le_bytes()); // StructureSize
        b.push(0); // Flags
        b.push(1); // SecurityMode = SIGNING_ENABLED
        b.extend_from_slice(&0u32.to_le_bytes()); // Capabilities
        b.extend_from_slice(&0u32.to_le_bytes()); // Channel
        b.extend_from_slice(&(64u16 + 24).to_le_bytes()); // SecurityBufferOffset = 88
        b.extend_from_slice(&(sec.len() as u16).to_le_bytes()); // SecurityBufferLength
        b.extend_from_slice(&0u64.to_le_bytes()); // PreviousSessionId
        b.extend_from_slice(sec);
        self.exchange(SESSION_SETUP, &b)
    }

    /// Authenticate. Empty `user` → anonymous/guest.
    pub fn session_setup(&mut self, user: &str, domain: &str, password: &str, client_chal: &[u8; 8], now_filetime: u64) -> Result<(), SmbError> {
        // Round 1: NTLMSSP NEGOTIATE.
        let (status, resp) = self.session_setup_msg(&ntlm::negotiate())?;
        // The session id comes back in the response header even on MORE_PROCESSING.
        self.session_id = u64le(&resp, 40);
        if status != STATUS_MORE_PROCESSING && status != STATUS_SUCCESS {
            return Err(SmbError::Protocol(status));
        }
        // Extract the NTLMSSP CHALLENGE from the response security buffer.
        let off = u16le(&resp, 64 + 4) as usize; // SecurityBufferOffset (body offset 4)
        let len = u16le(&resp, 64 + 6) as usize;
        if off + len > resp.len() {
            return Err(SmbError::Protocol(status));
        }
        let chal = ntlm::parse_challenge(&resp[off..off + len]).ok_or(SmbError::Auth)?;
        // Round 2: NTLMSSP AUTHENTICATE.
        let auth = ntlm::authenticate(user, domain, password, &chal, client_chal, now_filetime);
        let (status2, _r2) = self.session_setup_msg(&auth)?;
        if status2 != STATUS_SUCCESS {
            return Err(SmbError::Auth);
        }
        Ok(())
    }

    pub fn tree_connect(&mut self, unc: &str) -> Result<(), SmbError> {
        let path = utf16le(unc);
        let mut b = Vec::new();
        b.extend_from_slice(&9u16.to_le_bytes()); // StructureSize
        b.extend_from_slice(&0u16.to_le_bytes()); // Reserved/Flags
        b.extend_from_slice(&(64u16 + 8).to_le_bytes()); // PathOffset = 72
        b.extend_from_slice(&(path.len() as u16).to_le_bytes()); // PathLength
        b.extend_from_slice(&path);
        let (status, resp) = self.exchange(TREE_CONNECT, &b)?;
        if status != STATUS_SUCCESS {
            return Err(SmbError::Protocol(status));
        }
        self.tree_id = u32le(&resp, 36); // TreeId from the response header
        Ok(())
    }

    /// Open a file or directory relative to the share. Returns (FileId[16], end_of_file).
    fn create(&mut self, name: &str, dir: bool, write: bool) -> Result<([u8; 16], u64), SmbError> {
        let nm = utf16le(name);
        let desired: u32 = if write { 0x0012_0196 } else { 0x0012_0089 }; // read/attrs (+write if write)
        let disposition: u32 = if write { 5 } else { 1 }; // OVERWRITE_IF : OPEN
        let options: u32 = if dir { 0x0000_0001 } else { 0x0000_0040 }; // DIRECTORY_FILE : NON_DIRECTORY_FILE
        let mut b = Vec::new();
        b.extend_from_slice(&57u16.to_le_bytes()); // StructureSize
        b.push(0); // SecurityFlags
        b.push(0); // RequestedOplockLevel
        b.extend_from_slice(&2u32.to_le_bytes()); // ImpersonationLevel
        b.extend_from_slice(&[0u8; 8]); // SmbCreateFlags
        b.extend_from_slice(&[0u8; 8]); // Reserved
        b.extend_from_slice(&desired.to_le_bytes()); // DesiredAccess
        b.extend_from_slice(&0u32.to_le_bytes()); // FileAttributes
        b.extend_from_slice(&7u32.to_le_bytes()); // ShareAccess READ|WRITE|DELETE
        b.extend_from_slice(&disposition.to_le_bytes()); // CreateDisposition
        b.extend_from_slice(&options.to_le_bytes()); // CreateOptions
        b.extend_from_slice(&(64u16 + 56).to_le_bytes()); // NameOffset = 120
        b.extend_from_slice(&(nm.len() as u16).to_le_bytes()); // NameLength
        b.extend_from_slice(&0u32.to_le_bytes()); // CreateContextsOffset
        b.extend_from_slice(&0u32.to_le_bytes()); // CreateContextsLength
        if nm.is_empty() {
            b.push(0); // StructureSize convention requires ≥1 buffer byte
        } else {
            b.extend_from_slice(&nm);
        }
        let (status, resp) = self.exchange(CREATE, &b)?;
        if status == 0xC000_0034 || status == 0xC000_003A {
            return Err(SmbError::NotFound); // OBJECT_NAME_NOT_FOUND / PATH_NOT_FOUND
        }
        if status != STATUS_SUCCESS {
            return Err(SmbError::Protocol(status));
        }
        let mut fid = [0u8; 16];
        fid.copy_from_slice(&resp[64 + 64..64 + 80]); // FileId at body offset 64
        let eof = u64le(&resp, 64 + 48); // EndOfFile at body offset 48
        Ok((fid, eof))
    }

    fn close(&mut self, fid: &[u8; 16]) -> Result<(), SmbError> {
        let mut b = Vec::new();
        b.extend_from_slice(&24u16.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // Flags
        b.extend_from_slice(&0u32.to_le_bytes()); // Reserved
        b.extend_from_slice(fid);
        let _ = self.exchange(CLOSE, &b)?;
        Ok(())
    }

    fn read_chunk(&mut self, fid: &[u8; 16], offset: u64, len: u32) -> Result<Vec<u8>, SmbError> {
        let mut b = Vec::new();
        b.extend_from_slice(&49u16.to_le_bytes()); // StructureSize
        b.push(0); // Padding
        b.push(0); // Flags
        b.extend_from_slice(&len.to_le_bytes()); // Length
        b.extend_from_slice(&offset.to_le_bytes()); // Offset
        b.extend_from_slice(fid);
        b.extend_from_slice(&0u32.to_le_bytes()); // MinimumCount
        b.extend_from_slice(&0u32.to_le_bytes()); // Channel
        b.extend_from_slice(&0u32.to_le_bytes()); // RemainingBytes
        b.extend_from_slice(&0u16.to_le_bytes()); // ReadChannelInfoOffset
        b.extend_from_slice(&0u16.to_le_bytes()); // ReadChannelInfoLength
        b.push(0); // Buffer (1 byte)
        let (status, resp) = self.exchange(READ, &b)?;
        if status == 0xC000_0011 {
            return Ok(Vec::new()); // END_OF_FILE
        }
        if status != STATUS_SUCCESS {
            return Err(SmbError::Protocol(status));
        }
        let data_off = resp[64 + 2] as usize; // DataOffset (from header start)
        let data_len = u32le(&resp, 64 + 4) as usize; // DataLength
        if data_off + data_len > resp.len() {
            return Err(SmbError::Transport);
        }
        Ok(resp[data_off..data_off + data_len].to_vec())
    }

    fn write_chunk(&mut self, fid: &[u8; 16], offset: u64, data: &[u8]) -> Result<u32, SmbError> {
        let mut b = Vec::new();
        b.extend_from_slice(&49u16.to_le_bytes()); // StructureSize
        b.extend_from_slice(&(64u16 + 48).to_le_bytes()); // DataOffset = 112
        b.extend_from_slice(&(data.len() as u32).to_le_bytes()); // Length
        b.extend_from_slice(&offset.to_le_bytes()); // Offset
        b.extend_from_slice(fid);
        b.extend_from_slice(&0u32.to_le_bytes()); // Channel
        b.extend_from_slice(&0u32.to_le_bytes()); // RemainingBytes
        b.extend_from_slice(&0u16.to_le_bytes()); // WriteChannelInfoOffset
        b.extend_from_slice(&0u16.to_le_bytes()); // WriteChannelInfoLength
        b.extend_from_slice(&0u32.to_le_bytes()); // Flags
        b.extend_from_slice(data);
        let (status, resp) = self.exchange(WRITE, &b)?;
        if status != STATUS_SUCCESS {
            return Err(SmbError::Protocol(status));
        }
        Ok(u32le(&resp, 64 + 4)) // Count
    }

    // ── High-level operations relative to the connected tree ──

    /// Read a whole file by path (relative to the share, e.g. "docs/note.txt").
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>, SmbError> {
        let (fid, eof) = self.create(path, false, false)?;
        let mut out = Vec::with_capacity(eof as usize);
        let mut off = 0u64;
        while off < eof {
            let want = ((eof - off).min(65536)) as u32;
            let chunk = self.read_chunk(&fid, off, want)?;
            if chunk.is_empty() {
                break;
            }
            off += chunk.len() as u64;
            out.extend_from_slice(&chunk);
        }
        let _ = self.close(&fid);
        Ok(out)
    }

    /// Write a whole file by path (overwrite/create).
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), SmbError> {
        let (fid, _eof) = self.create(path, false, true)?;
        let mut off = 0u64;
        for chunk in data.chunks(65536) {
            self.write_chunk(&fid, off, chunk)?;
            off += chunk.len() as u64;
        }
        let _ = self.close(&fid);
        Ok(())
    }

    /// List a directory (relative to the share; "" = the share root).
    pub fn list_dir(&mut self, path: &str) -> Result<Vec<DirItem>, SmbError> {
        let (fid, _eof) = self.create(path, true, false)?;
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let pattern = utf16le("*");
            let mut b = Vec::new();
            b.extend_from_slice(&33u16.to_le_bytes()); // StructureSize
            b.push(1); // FileInformationClass = FileDirectoryInformation
            b.push(if first { 1 } else { 0 }); // Flags: RESTART_SCANS on first
            b.extend_from_slice(&0u32.to_le_bytes()); // FileIndex
            b.extend_from_slice(&fid);
            b.extend_from_slice(&(64u16 + 32).to_le_bytes()); // FileNameOffset = 96
            b.extend_from_slice(&(pattern.len() as u16).to_le_bytes()); // FileNameLength
            b.extend_from_slice(&65536u32.to_le_bytes()); // OutputBufferLength
            b.extend_from_slice(&pattern);
            first = false;
            let (status, resp) = self.exchange(QUERY_DIRECTORY, &b)?;
            if status == STATUS_NO_MORE_FILES {
                break;
            }
            if status != STATUS_SUCCESS {
                let _ = self.close(&fid);
                return Err(SmbError::Protocol(status));
            }
            let out_off = u16le(&resp, 64 + 2) as usize;
            let out_len = u32le(&resp, 64 + 4) as usize;
            if out_off + out_len > resp.len() {
                break;
            }
            parse_dir_info(&resp[out_off..out_off + out_len], &mut items);
            if out_len == 0 {
                break;
            }
        }
        let _ = self.close(&fid);
        Ok(items)
    }

    /// Does a path exist (file or directory)?
    pub fn exists(&mut self, path: &str) -> bool {
        if let Ok((fid, _)) = self.create(path, false, false) {
            let _ = self.close(&fid);
            return true;
        }
        if let Ok((fid, _)) = self.create(path, true, false) {
            let _ = self.close(&fid);
            return true;
        }
        false
    }
}

/// Parse a buffer of FileDirectoryInformation entries (class 1) into items.
fn parse_dir_info(buf: &[u8], out: &mut Vec<DirItem>) {
    let mut pos = 0usize;
    loop {
        if pos + 64 > buf.len() {
            break;
        }
        let next = u32le(buf, pos) as usize;
        let eof = u64le(buf, pos + 40);
        let attrs = u32le(buf, pos + 56);
        let name_len = u32le(buf, pos + 60) as usize;
        let name_start = pos + 64;
        if name_start + name_len <= buf.len() {
            let u16s: Vec<u16> = buf[name_start..name_start + name_len]
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                .collect();
            let name = String::from_utf16_lossy(&u16s);
            if name != "." && name != ".." {
                out.push(DirItem { name, is_dir: attrs & 0x10 != 0, size: eof });
            }
        }
        if next == 0 {
            break;
        }
        pos += next;
    }
}
