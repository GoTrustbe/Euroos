//! Exercise the NFSv3 client against a real server. Usage:
//!   cargo run -p euronfs --example nfstest -- 127.0.0.1 /srv/nfsshare
use euronfs::{Connector, NfsClient, NfsError};
use std::io::{Read, Write};
use std::net::TcpStream;

struct TcpConnector {
    host: String,
    s: Option<TcpStream>,
}
impl Connector for TcpConnector {
    fn connect(&mut self, port: u16) -> Result<(), NfsError> {
        self.s = Some(TcpStream::connect((self.host.as_str(), port)).map_err(|_| NfsError::Transport)?);
        Ok(())
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), NfsError> {
        self.s.as_mut().ok_or(NfsError::Transport)?.write_all(data).map_err(|_| NfsError::Transport)
    }
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, NfsError> {
        let mut v = vec![0u8; n];
        self.s.as_mut().ok_or(NfsError::Transport)?.read_exact(&mut v).map_err(|_| NfsError::Transport)?;
        Ok(v)
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let host = a.get(1).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let export = a.get(2).cloned().unwrap_or_else(|| "/srv/nfsshare".into());

    let mut c = NfsClient::new(TcpConnector { host: host.clone(), s: None });
    let root = c.mount(&export).expect("mount export");
    println!("OK: mounted {host}:{export} (root fh {} bytes)", root.len());

    println!("--- list of export root ---");
    for it in c.list_dir(&root, "").expect("readdir") {
        println!("  {} {}{}", if it.is_dir { "DIR " } else { "FILE" }, it.name, if it.is_dir { String::new() } else { format!(" ({} B)", it.size) });
    }

    let readme = c.read_file(&root, "readme.txt").expect("read readme.txt");
    println!("--- readme.txt ({} B) ---\n{}", readme.len(), String::from_utf8_lossy(&readme));
    let note = c.read_file(&root, "docs/note.txt").expect("read docs/note.txt");
    println!("--- docs/note.txt: {:?}", String::from_utf8_lossy(&note));

    let payload = b"written by the EuroOS NFS client";
    c.write_file(&root, "from-euroos-nfs.txt", payload).expect("write");
    let back = c.read_file(&root, "from-euroos-nfs.txt").expect("read back");
    assert_eq!(back, payload, "write/read-back mismatch");
    println!("OK: wrote + read back from-euroos-nfs.txt ({} B) — write path works", back.len());
    println!("ALL OK");
}
