//! Host validation: build an ESP image and write it to /tmp/esp.img so that
//! `mtools` (an independent FAT implementation) can verify it.

fn main() {
    let sectors = 48 * 1024 * 1024 / 512;
    let mut fs = eurofat::FatFs::new(sectors, 0x1234_5678, "EUROKERNEL");
    let loader: Vec<u8> = (0..24_000u32).map(|i| (i % 256) as u8).collect();
    let kernel_a: Vec<u8> = (0..200_003u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let kernel_b: Vec<u8> = (0..199_997u32).map(|i| (i.wrapping_mul(40503) >> 7) as u8).collect();
    fs.add_file("/EFI/BOOT/BOOTX64.EFI", &loader);
    fs.add_file("/EFI/BOOT/eurokernel-A.efi", &kernel_a);
    fs.add_file("/EFI/BOOT/eurokernel-B.efi", &kernel_b);
    let img = fs.build();
    std::fs::write("/tmp/esp.img", &img).unwrap();
    std::fs::write("/tmp/ref-loader.bin", &loader).unwrap();
    std::fs::write("/tmp/ref-kernel-a.bin", &kernel_a).unwrap();
    std::fs::write("/tmp/ref-kernel-b.bin", &kernel_b).unwrap();
    println!("wrote /tmp/esp.img ({} bytes)", img.len());
}
