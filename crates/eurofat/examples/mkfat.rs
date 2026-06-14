//! Write a freshly-formatted FAT32 volume to a file (to validate `format_fat32` with
//! `fsck.fat`/`mtools`). Usage: `cargo run -p eurofat --example mkfat -- /tmp/fat.img [MiB]`
use std::io::Write;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/fat.img".into());
    let mib: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(64);
    let sectors = mib * 1024 * 1024 / 512;
    let mut img = vec![0u8; sectors as usize * 512];
    eurofat::format_fat32(sectors, 0x1234_5678, "EURODATA", |lba, bytes| {
        let off = lba as usize * 512;
        img[off..off + bytes.len()].copy_from_slice(bytes);
    });
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(&img).unwrap();
    println!("wrote {path} ({mib} MiB FAT32)");
}
