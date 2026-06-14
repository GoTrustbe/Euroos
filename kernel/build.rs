//! Assembles the SMP AP trampoline (`asm/trampoline.S`) into a flat binary at
//! org 0x8000, which the kernel embeds via `include_bytes!` and copies to physical
//! 0x8000 before the INIT-SIPI-SIPI.

use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    let src = "asm/trampoline.S";
    println!("cargo:rerun-if-changed={src}");

    let out = env::var("OUT_DIR").unwrap();
    let obj = Path::new(&out).join("trampoline.o");
    let bin = Path::new(&out).join("trampoline.bin");

    let status = Command::new("as")
        .args(["--64", src, "-o"])
        .arg(&obj)
        .status()
        .expect("could not run `as`");
    assert!(status.success(), "as failed on {src}");

    // -Ttext=0x8000 + flat binary: absolute labels resolve to 0x8000+offset.
    let status = Command::new("ld")
        .args(["-melf_x86_64", "-Ttext=0x8000", "--oformat", "binary", "-o"])
        .arg(&bin)
        .arg(&obj)
        .status()
        .expect("could not run `ld`");
    assert!(status.success(), "ld failed on the trampoline");
}
