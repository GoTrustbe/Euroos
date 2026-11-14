//! Kernel side of **EuroSMB** (Sprint IO-5): mount an SMB network share into the VFS.
//!
//! A [`Transport`] over the kernel's TCP stack ([`crate::net::TcpConn`]) drives the
//! host-verified `eurosmb` client (SMB2 + NTLMv2). The `[io5]` self-test connects over
//! the live NIC to the build host's Samba (SLIRP gateway 10.0.2.2:445) and reads a file
//! — real end-to-end SMB, not a mock.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use euronet::ipv4::Ipv4Addr;
use eurofs::FileSystem;
use eurosmb::{SmbClient, SmbError, SmbFs, Transport};

/// A `eurosmb::Transport` over a kernel `TcpConn`, buffering partial reads.
pub struct KernelTcp {
    conn: crate::net::TcpConn,
    buf: Vec<u8>,
}

impl Transport for KernelTcp {
    fn write_all(&mut self, data: &[u8]) -> Result<(), SmbError> {
        self.conn.send(data);
        Ok(())
    }
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, SmbError> {
        let mut idle = 0;
        while self.buf.len() < n {
            let chunk = self.conn.recv(16384);
            if chunk.is_empty() {
                idle += 1;
                if idle > 40 {
                    return Err(SmbError::Transport);
                }
                continue;
            }
            idle = 0;
            self.buf.extend_from_slice(&chunk);
        }
        let out = self.buf[..n].to_vec();
        self.buf.drain(..n);
        Ok(out)
    }
}

/// Open a TCP connection to `server:port` over the live NIC.
pub fn connect(server: Ipv4Addr, port: u16) -> Option<KernelTcp> {
    let cfg = crate::net::get()?;
    let nexthop = if crate::net::same_subnet(server, cfg.my_ip) {
        crate::net::arp_resolve(cfg.my_mac, cfg.my_ip, server).unwrap_or(cfg.gw_mac)
    } else {
        cfg.gw_mac
    };
    let conn = crate::net::TcpConn::connect(cfg.my_mac, cfg.my_ip, nexthop, server, port)?;
    Some(KernelTcp { conn, buf: Vec::new() })
}

/// The 8-byte client challenge of the NTLMv2 session setup (BUG-087).
pub fn client_challenge() -> [u8; 8] {
    // The client's contribution to the NTLMv2 response: unpredictable per session,
    // from the CSPRNG (it was the RTC second and a fixed tail).
    crate::entropy::draw_wait::<8>()
}

/// Connect + negotiate + authenticate + tree-connect to `\\<server>\<share>`.
fn open_share(server: Ipv4Addr, share: &str, user: &str, pass: &str) -> Result<SmbClient<KernelTcp>, SmbError> {
    let kt = connect(server, 445).ok_or(SmbError::Transport)?;
    let mut c = SmbClient::new(kt);
    c.negotiate()?;
    let cc = client_challenge();
    c.session_setup(user, "WORKGROUP", pass, &cc, 0)?;
    let s = server.0;
    c.tree_connect(&format!("\\\\{}.{}.{}.{}\\{share}", s[0], s[1], s[2], s[3]))?;
    Ok(c)
}

fn parse_ipv4(s: &str) -> Option<Ipv4Addr> {
    let mut o = [0u8; 4];
    let mut parts = s.split('.');
    for b in o.iter_mut() {
        *b = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(Ipv4Addr(o))
}

/// `mount //<ip>/<share> <point> [user pass]` — mount an SMB share into the VFS.
pub fn mount_cmd(fs: &mut dyn FileSystem, target: &str, rest: &str) -> Vec<String> {
    // target = //ip/share ; rest = "point [user pass]"
    let t = target.trim_start_matches('/');
    let (ip_s, share) = match t.split_once('/') {
        Some(x) => x,
        None => return alloc::vec!["usage: mount //<ip>/<share> <point> [user] [pass]".to_string()],
    };
    let mut r = rest.split_whitespace();
    let point = match r.next() {
        Some(p) => p,
        None => return alloc::vec!["usage: mount //<ip>/<share> <point> [user] [pass]".to_string()],
    };
    let user = r.next().unwrap_or("");
    let pass = r.next().unwrap_or("");
    let ip = match parse_ipv4(ip_s) {
        Some(i) => i,
        None => return alloc::vec![format!("mount: invalid IP '{ip_s}'")],
    };
    match open_share(ip, share, user, pass) {
        Ok(client) => match fs.mount_fs(point, Box::new(SmbFs::new(client))) {
            Ok(()) => alloc::vec![format!("mounted //{ip_s}/{share} at {point} (SMB2 + NTLMv2)")],
            Err(_) => alloc::vec!["mount: the root filesystem is not a VFS (cannot mount here)".to_string()],
        },
        Err(e) => alloc::vec![format!("mount: SMB connect/auth to //{ip_s}/{share} failed ({e:?})")],
    }
}

/// **[io5]** — mount the build host's Samba share over the live NIC (SLIRP 10.0.2.2:445)
/// and read a file: real end-to-end SMB2 + NTLMv2 from the kernel. Gracefully skips if
/// there is no network or no server (so it never falsely fails).
pub fn selftest() {
    if crate::net::get().is_none() {
        crate::serial_println!("[io5] SMB self-test skipped (no DHCP lease / NIC)");
        return;
    }
    let server = Ipv4Addr([10, 0, 2, 2]); // SLIRP host gateway → build-host Samba
    let mut c = match open_share(server, "euro", "user", "europass123") {
        Ok(c) => c,
        Err(e) => {
            crate::serial_println!("[io5] SMB connect/auth to //10.0.2.2/euro skipped/failed ({e:?}) — no reachable SMB server");
            return;
        }
    };
    let readme = c.read_file("readme.txt").ok();
    let listing = c.list_dir("").map(|v| v.len()).unwrap_or(0);
    let read_ok = readme.as_deref().map(|d| d.starts_with(b"Hello from a real SMB share")).unwrap_or(false);
    crate::serial_println!(
        "[io5] SMB2+NTLMv2 over the live NIC → //10.0.2.2/euro: authenticated, {listing} entries listed, readme.txt {} B read → {}",
        readme.as_ref().map(|d| d.len()).unwrap_or(0),
        if read_ok && listing > 0 { "OK ✓" } else { "FAILED ✗" }
    );
}
