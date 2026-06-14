//! Host validation: assemble a COMPLETE bootable disk from the real
//! build binaries (loader.efi + eurokernel.efi) and write it to /tmp/disk2.img,
//! so that sgdisk/fsck/mtools — and QEMU itself — can verify it.

fn main() {
    let dir = "target/x86_64-unknown-uefi/release";
    let loader = std::fs::read(format!("{dir}/loader.efi")).expect("loader.efi (cargo lbuild-release)");
    let kernel = std::fs::read(format!("{dir}/eurokernel.efi")).expect("eurokernel.efi (cargo kbuild-release)");
    let total = 512 * 1024 * 1024 / 512; // 512 MiB
    let (img, layout) = eurofat::build_boot_disk(total, 0x1234_5678, &loader, &kernel, &kernel);
    std::fs::write("/tmp/disk2.img", &img).unwrap();
    println!(
        "wrote /tmp/disk2.img ({} MiB): loader {} B, kernel {} B; ESP @ LBA {} ({} MiB), EuroFS @ LBA {}",
        img.len() / 1024 / 1024,
        loader.len(),
        kernel.len(),
        layout.esp_first,
        layout.esp_sectors * 512 / 1024 / 1024,
        layout.eurofs_first
    );
    // Write the ESP out separately so fsck/mtools can check it directly.
    let off = layout.esp_first as usize * 512;
    let esp = &img[off..off + layout.esp_sectors as usize * 512];
    std::fs::write("/tmp/disk2-esp.img", esp).unwrap();
}
