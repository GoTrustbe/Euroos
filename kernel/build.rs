//! Assembleert de SMP AP-trampoline (`asm/trampoline.S`) tot een flat binary op
//! org 0x8000, die de kernel via `include_bytes!` insluit en naar physiek 0x8000
//! kopieert vóór de INIT-SIPI-SIPI.

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
        .expect("kon `as` niet uitvoeren");
    assert!(status.success(), "as faalde op {src}");

    // -Ttext=0x8000 + flat binary: absolute labels resolven naar 0x8000+offset.
    let status = Command::new("ld")
        .args(["-melf_x86_64", "-Ttext=0x8000", "--oformat", "binary", "-o"])
        .arg(&bin)
        .arg(&obj)
        .status()
        .expect("kon `ld` niet uitvoeren");
    assert!(status.success(), "ld faalde op de trampoline");
}
