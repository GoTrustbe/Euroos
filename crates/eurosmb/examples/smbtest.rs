//! Exercise the SMB2 client against a real server. Usage:
//!   cargo run -p eurosmb --example smbtest -- 127.0.0.1 euro user europass123
use eurosmb::{SmbClient, SmbError, Transport};
use std::io::{Read, Write};
use std::net::TcpStream;

struct TcpTransport(TcpStream);
impl Transport for TcpTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<(), SmbError> {
        self.0.write_all(data).map_err(|_| SmbError::Transport)
    }
    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, SmbError> {
        let mut v = vec![0u8; n];
        self.0.read_exact(&mut v).map_err(|_| SmbError::Transport)?;
        Ok(v)
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let host = a.get(1).cloned().unwrap_or_else(|| "127.0.0.1".into());
    let share = a.get(2).cloned().unwrap_or_else(|| "euro".into());
    let user = a.get(3).cloned().unwrap_or_else(|| "user".into());
    let pass = a.get(4).cloned().unwrap_or_else(|| "europass123".into());

    let stream = TcpStream::connect((host.as_str(), 445)).expect("connect 445");
    let mut c = SmbClient::new(TcpTransport(stream));
    c.negotiate().expect("negotiate");
    c.session_setup(&user, "WORKGROUP", &pass, &[0x11; 8], 0).expect("session_setup (auth)");
    println!("OK: authenticated as {user}");
    c.tree_connect(&format!("\\\\{host}\\{share}")).expect("tree_connect");
    println!("OK: tree-connected to \\\\{host}\\{share}");

    println!("--- list of share root ---");
    for it in c.list_dir("").expect("list_dir") {
        println!("  {} {}{}", if it.is_dir { "DIR " } else { "FILE" }, it.name, if it.is_dir { String::new() } else { format!(" ({} B)", it.size) });
    }

    let readme = c.read_file("readme.txt").expect("read readme.txt");
    println!("--- readme.txt ({} B) ---\n{}", readme.len(), String::from_utf8_lossy(&readme));

    let note = c.read_file("docs/note.txt").expect("read docs/note.txt");
    println!("--- docs/note.txt: {:?}", String::from_utf8_lossy(&note));

    // Write a file and read it back (proves the write path).
    let payload = b"written by the EuroOS SMB client";
    c.write_file("from-euroos.txt", payload).expect("write_file");
    let back = c.read_file("from-euroos.txt").expect("read back");
    assert_eq!(back, payload, "write/read-back mismatch");
    println!("OK: wrote + read back from-euroos.txt ({} B) — write path works", back.len());
    println!("ALL OK");
}
