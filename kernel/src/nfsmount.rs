//! Kernel side of **EuroNFS** (Sprint IO-6): mount an NFSv3 export into the VFS.
//!
//! A [`Connector`] over the kernel TCP stack drives the host-verified `euronfs` client.
//! The `[io6]` self-test mounts the build host's export over the live NIC (SLIRP) and
//! reads a file — real ONC-RPC/NFSv3, not a mock.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use euronet::ipv4::Ipv4Addr;
use eurofs::FileSystem;
use euronfs::{Connector, NfsClient, NfsError, NfsFs};

/// A `euronfs::Connector` over the kernel's TCP stack — reconnects per service port.
pub struct KernelConn {
    server: Ipv4Addr,
    conn: Option<crate::net::TcpConn>,
    buf: Vec<u8>,
}

impl KernelConn {
    pub fn new(server: Ipv4Addr) -> Self {
        KernelConn { server, conn: None, buf: Vec::new() }
    }
}

impl Connector for KernelConn {
    fn connect(&mut self, port: u16) -> Result<(), NfsError> {
        if let Some(c) = self.conn.as_mut() {
            c.close();
        }
        self.buf.clear();
        let cfg = crate::net::get().ok_or(NfsError::Transport)?;
        let nexthop = if crate::net::same_subnet(self.server, cfg.my_ip) {
            crate::net::arp_resolve(cfg.my_mac, cfg.my_ip, self.server).unwrap_or(cfg.gw_mac)
        } else {
            cfg.gw_mac
        };
        self.conn = Some(
            crate::net::TcpConn::connect(cfg.my_mac, cfg.my_ip, nexthop, self.server, port).ok_or(NfsError::Transport)?,
        );
        Ok(())
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), NfsError> {
        self.conn.as_mut().ok_or(NfsError::Transport)?.send(data);
        Ok(())
    }
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, NfsError> {
        let mut idle = 0;
        while self.buf.len() < n {
            let chunk = self.conn.as_mut().ok_or(NfsError::Transport)?.recv(16384);
            if chunk.is_empty() {
                idle += 1;
                if idle > 40 {
                    return Err(NfsError::Transport);
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

/// `mount nfs://<ip>/<export> <point>` — mount an NFSv3 export into the VFS.
pub fn mount_cmd(fs: &mut dyn FileSystem, target: &str, rest: &str) -> Vec<String> {
    let t = match target.strip_prefix("nfs://") {
        Some(t) => t,
        None => return alloc::vec!["usage: mount nfs://<ip>/<export> <point>".to_string()],
    };
    let (ip_s, export) = match t.split_once('/') {
        Some((a, b)) => (a, alloc::format!("/{b}")),
        None => return alloc::vec!["usage: mount nfs://<ip>/<export> <point>".to_string()],
    };
    let point = match rest.split_whitespace().next() {
        Some(p) => p,
        None => return alloc::vec!["usage: mount nfs://<ip>/<export> <point>".to_string()],
    };
    let ip = match parse_ipv4(ip_s) {
        Some(i) => i,
        None => return alloc::vec![format!("mount: invalid IP '{ip_s}'")],
    };
    let mut client = NfsClient::new(KernelConn::new(ip));
    match client.mount(&export) {
        Ok(root) => match fs.mount_fs(point, Box::new(NfsFs::new(client, root))) {
            Ok(()) => alloc::vec![format!("mounted nfs://{ip_s}{export} at {point} (NFSv3)")],
            Err(_) => alloc::vec!["mount: the root filesystem is not a VFS (cannot mount here)".to_string()],
        },
        Err(e) => alloc::vec![format!("mount: NFS mount of nfs://{ip_s}{export} failed ({e:?})")],
    }
}

/// **[io6]** — mount the build host's NFS export over the live NIC (SLIRP 10.0.2.2) and
/// read a file: real NFSv3 from the kernel. Gracefully skips if there is no network/server.
pub fn selftest() {
    if crate::net::get().is_none() {
        crate::serial_println!("[io6] NFS self-test skipped (no DHCP lease / NIC)");
        return;
    }
    let server = Ipv4Addr([10, 0, 2, 2]);
    let mut client = NfsClient::new(KernelConn::new(server));
    let root = match client.mount("/srv/nfsshare") {
        Ok(r) => r,
        Err(e) => {
            crate::serial_println!("[io6] NFS mount of //10.0.2.2/srv/nfsshare skipped/failed ({e:?}) — no reachable NFS server");
            return;
        }
    };
    let listing = client.list_dir(&root, "").map(|v| v.len()).unwrap_or(0);
    let readme = client.read_file(&root, "readme.txt").ok();
    let read_ok = readme.as_deref().map(|d| d.starts_with(b"hello from a real NFS export")).unwrap_or(false);
    crate::serial_println!(
        "[io6] NFSv3 over the live NIC → nfs://10.0.2.2/srv/nfsshare: mounted (root fh {} B), {listing} entries, readme.txt {} B read → {}",
        root.len(),
        readme.as_ref().map(|d| d.len()).unwrap_or(0),
        if read_ok && listing > 0 { "OK ✓" } else { "FAILED ✗" }
    );
}
