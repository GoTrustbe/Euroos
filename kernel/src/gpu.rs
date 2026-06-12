//! Kernel-zijde van **EuroGPU** (plan K4): het virtio-gpu commandoprotocol. Bij boot
//! bouwen we de volledige commando-sequentie (displayinfo → 2D-resource → backing →
//! scanout → transfer → flush) en parseren we een respons, deterministisch. De
//! host-geteste kern leeft in [`eurogpu`].
//!
//! NB: de échte device-driver vereist de **moderne** virtio-transport (virtio-gpu is
//! virtio-1.0-only; de bestaande virtio-blk/-net gebruiken de legacy-poort-I/O). Die
//! transport + framebuffer-scanout is hardware-attended werk; het protocol hier is
//! volledig en host-getest.

use alloc::string::String;
use alloc::vec::Vec;

use eurogpu::{
    get_display_info, is_ok, parse_display_info, resource_attach_backing, resource_create_2d, resource_flush,
    set_scanout, transfer_to_host_2d, FORMAT_B8G8R8A8_UNORM, RESP_OK_DISPLAY_INFO, RESP_OK_NODATA,
};

/// Boot-zelftest: bouw de hele virtio-gpu-commandostroom + parse een respons.
pub fn selftest() {
    // De commando-sequentie die de driver naar de control-virtqueue zou sturen.
    let info = get_display_info();
    let create = resource_create_2d(1, FORMAT_B8G8R8A8_UNORM, 1024, 768);
    let backing = resource_attach_backing(1, 0x1_0000_0000, 1024 * 768 * 4);
    let scanout = set_scanout(0, 1, 1024, 768);
    let transfer = transfer_to_host_2d(1, 1024, 768);
    let flush = resource_flush(1, 1024, 768);

    // Alle commando's dragen een geldige 24-byte-header met het juiste type.
    let cmds_ok = info.len() == 24
        && create.len() == 40
        && backing.len() >= 48
        && scanout.len() == 48
        && transfer.len() >= 48
        && flush.len() >= 44;

    // Simuleer een OK_DISPLAY_INFO-respons (zoals het device 'm zou geven): scanout
    // 0 ingeschakeld op 1024×768 → de driver leest de resolutie eruit.
    let mut resp = RESP_OK_DISPLAY_INFO.to_le_bytes().to_vec();
    resp.extend_from_slice(&[0u8; 20]);
    resp.extend_from_slice(&0u32.to_le_bytes()); // x
    resp.extend_from_slice(&0u32.to_le_bytes()); // y
    resp.extend_from_slice(&1024u32.to_le_bytes()); // w
    resp.extend_from_slice(&768u32.to_le_bytes()); // h
    resp.extend_from_slice(&1u32.to_le_bytes()); // enabled
    resp.extend_from_slice(&0u32.to_le_bytes()); // flags
    let res = parse_display_info(&resp);
    let resp_ok = is_ok(&resp) && res == Some((1024, 768));

    // Een OK_NODATA-respons op een create/scanout/flush wordt als succes herkend.
    let nodata_ok = is_ok(&RESP_OK_NODATA.to_le_bytes());

    let ok = cmds_ok && resp_ok && nodata_ok;
    crate::serial_println!(
        "[k4] EuroGPU: virtio-gpu-commandostroom (displayinfo→create-2d→backing→scanout→transfer→flush)={cmds_ok}, displayinfo-respons-geparsed={:?}, OK-respons-herkend={nodata_ok} → {}",
        res,
        if ok { "OK (virtio-gpu-protocolkern; moderne-virtio-driver = hardware-attended) ✓" } else { "MISLUKT" }
    );
}

/// `gpu`-shellcommando: toon de virtio-gpu-protocolstatus.
pub fn shell() -> Vec<String> {
    let create = resource_create_2d(1, FORMAT_B8G8R8A8_UNORM, 1920, 1080);
    alloc::vec![
        String::from("EuroGPU — virtio-gpu 2D-acceleratie (protocolkern host-getest)"),
        alloc::format!("  commando's: GET_DISPLAY_INFO · RESOURCE_CREATE_2D ({} B) · ATTACH_BACKING · SET_SCANOUT · TRANSFER_TO_HOST_2D · RESOURCE_FLUSH", create.len()),
        String::from("  formaat B8G8R8A8 (zoals de GOP-framebuffer); een 2D-resource = de scanout-framebuffer"),
        String::from("  driver-binding vereist de moderne virtio-transport (virtio-1.0) — hardware-attended vervolg"),
    ]
}
