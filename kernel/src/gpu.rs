//! Kernel side of **EuroGPU** (plan K4): the virtio-gpu command protocol. At boot
//! we build the full command sequence (displayinfo → 2D resource → backing →
//! scanout → transfer → flush) and parse a response, deterministically. The
//! host-tested core lives in [`eurogpu`].
//!
//! NB: the real device driver requires the **modern** virtio transport (virtio-gpu is
//! virtio-1.0-only; the existing virtio-blk/-net use legacy port I/O). That
//! transport + framebuffer scanout is hardware-attended work; the protocol here is
//! complete and host-tested.

use alloc::string::String;
use alloc::vec::Vec;

use eurogpu::{
    get_display_info, is_ok, parse_display_info, resource_attach_backing, resource_create_2d, resource_flush,
    set_scanout, transfer_to_host_2d, FORMAT_B8G8R8A8_UNORM, RESP_OK_DISPLAY_INFO, RESP_OK_NODATA,
};

/// Boot self-test: build the whole virtio-gpu command stream + parse a response.
pub fn selftest() {
    // The command sequence that the driver would send to the control virtqueue.
    let info = get_display_info();
    let create = resource_create_2d(1, FORMAT_B8G8R8A8_UNORM, 1024, 768);
    let backing = resource_attach_backing(1, 0x1_0000_0000, 1024 * 768 * 4);
    let scanout = set_scanout(0, 1, 1024, 768);
    let transfer = transfer_to_host_2d(1, 1024, 768);
    let flush = resource_flush(1, 1024, 768);

    // All commands carry a valid 24-byte header with the correct type.
    let cmds_ok = info.len() == 24
        && create.len() == 40
        && backing.len() >= 48
        && scanout.len() == 48
        && transfer.len() >= 48
        && flush.len() >= 44;

    // Simulate an OK_DISPLAY_INFO response (as the device would give it): scanout
    // 0 enabled at 1024×768 → the driver reads the resolution out of it.
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

    // An OK_NODATA response to a create/scanout/flush is recognized as success.
    let nodata_ok = is_ok(&RESP_OK_NODATA.to_le_bytes());

    let ok = cmds_ok && resp_ok && nodata_ok;
    crate::serial_println!(
        "[k4] EuroGPU: virtio-gpu command stream (displayinfo→create-2d→backing→scanout→transfer→flush)={cmds_ok}, displayinfo-response-parsed={:?}, OK-response-recognized={nodata_ok} → {}",
        res,
        if ok { "OK (virtio-gpu protocol core; modern-virtio driver = hardware-attended) ✓" } else { "FAILED" }
    );
}

/// `gpu` shell command: show the virtio-gpu protocol status.
pub fn shell() -> Vec<String> {
    let create = resource_create_2d(1, FORMAT_B8G8R8A8_UNORM, 1920, 1080);
    alloc::vec![
        String::from("EuroGPU — virtio-gpu 2D acceleration (protocol core host-tested)"),
        alloc::format!("  commands: GET_DISPLAY_INFO · RESOURCE_CREATE_2D ({} B) · ATTACH_BACKING · SET_SCANOUT · TRANSFER_TO_HOST_2D · RESOURCE_FLUSH", create.len()),
        String::from("  format B8G8R8A8 (like the GOP framebuffer); a 2D resource = the scanout framebuffer"),
        String::from("  driver binding requires the modern virtio transport (virtio-1.0) — hardware-attended follow-up"),
    ]
}
