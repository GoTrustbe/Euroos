//! EuroGPU — the virtio-gpu command protocol (plan K4).
//!
//! A sovereign GPU driver talks to the virtio-gpu device via the control
//! virtqueue: query the display info, create a 2D resource (the framebuffer), attach
//! backing memory to it, bind it to a scanout, and transfer+flush to display.
//! This crate is the host-tested serialization/parse core; the kernel driver
//! ([`kernel::virtio_gpu`]) executes the bytes over a real virtqueue in QEMU.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

// ── Command/response types (virtio-gpu spec) ────────────────────────────────
pub const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const CMD_SET_SCANOUT: u32 = 0x0103;
pub const CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;

pub const RESP_OK_NODATA: u32 = 0x1100;
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

/// The pixel format (B8G8R8A8 = what the GOP framebuffer also uses).
pub const FORMAT_B8G8R8A8_UNORM: u32 = 1;

/// The 24-byte control header that precedes every command/response.
fn ctrl_hdr(cmd_type: u32) -> [u8; 24] {
    let mut h = [0u8; 24];
    h[0..4].copy_from_slice(&cmd_type.to_le_bytes());
    // flags=0, fence_id=0, ctx_id=0, padding=0
    h
}

fn push_rect(out: &mut Vec<u8>, x: u32, y: u32, w: u32, h: u32) {
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
}

/// `GET_DISPLAY_INFO` — query the screen resolutions (header only).
pub fn get_display_info() -> Vec<u8> {
    ctrl_hdr(CMD_GET_DISPLAY_INFO).to_vec()
}

/// `RESOURCE_CREATE_2D` — create a 2D host resource with a format + dimensions.
pub fn resource_create_2d(resource_id: u32, format: u32, width: u32, height: u32) -> Vec<u8> {
    let mut v = ctrl_hdr(CMD_RESOURCE_CREATE_2D).to_vec();
    v.extend_from_slice(&resource_id.to_le_bytes());
    v.extend_from_slice(&format.to_le_bytes());
    v.extend_from_slice(&width.to_le_bytes());
    v.extend_from_slice(&height.to_le_bytes());
    v
}

/// `RESOURCE_ATTACH_BACKING` — attach a single guest memory region (addr+len) as the
/// pixel storage of the resource.
pub fn resource_attach_backing(resource_id: u32, addr: u64, length: u32) -> Vec<u8> {
    let mut v = ctrl_hdr(CMD_RESOURCE_ATTACH_BACKING).to_vec();
    v.extend_from_slice(&resource_id.to_le_bytes());
    v.extend_from_slice(&1u32.to_le_bytes()); // nr_entries
    // mem_entry: addr(u64) + length(u32) + padding(u32)
    v.extend_from_slice(&addr.to_le_bytes());
    v.extend_from_slice(&length.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}

/// `SET_SCANOUT` — bind a resource to a scanout (screen).
pub fn set_scanout(scanout_id: u32, resource_id: u32, width: u32, height: u32) -> Vec<u8> {
    let mut v = ctrl_hdr(CMD_SET_SCANOUT).to_vec();
    push_rect(&mut v, 0, 0, width, height);
    v.extend_from_slice(&scanout_id.to_le_bytes());
    v.extend_from_slice(&resource_id.to_le_bytes());
    v
}

/// `TRANSFER_TO_HOST_2D` — copy the backing pixels to the host resource.
pub fn transfer_to_host_2d(resource_id: u32, width: u32, height: u32) -> Vec<u8> {
    let mut v = ctrl_hdr(CMD_TRANSFER_TO_HOST_2D).to_vec();
    push_rect(&mut v, 0, 0, width, height);
    v.extend_from_slice(&0u64.to_le_bytes()); // offset
    v.extend_from_slice(&resource_id.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // padding
    v
}

/// `RESOURCE_FLUSH` — display the updated region on the screen.
pub fn resource_flush(resource_id: u32, width: u32, height: u32) -> Vec<u8> {
    let mut v = ctrl_hdr(CMD_RESOURCE_FLUSH).to_vec();
    push_rect(&mut v, 0, 0, width, height);
    v.extend_from_slice(&resource_id.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // padding
    v
}

/// Read the response type from the first 4 bytes of a response buffer.
pub fn response_type(resp: &[u8]) -> Option<u32> {
    if resp.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]))
}

/// Is the response a success (OK_NODATA or OK_DISPLAY_INFO)?
pub fn is_ok(resp: &[u8]) -> bool {
    matches!(response_type(resp), Some(RESP_OK_NODATA | RESP_OK_DISPLAY_INFO))
}

/// Parse the first scanout from an `OK_DISPLAY_INFO` response → (width, height).
/// Layout: 24-byte header, then 16 scanouts of each {rect(16) enabled(4) flags(4)}.
pub fn parse_display_info(resp: &[u8]) -> Option<(u32, u32)> {
    if response_type(resp) != Some(RESP_OK_DISPLAY_INFO) || resp.len() < 24 + 24 {
        return None;
    }
    let r = &resp[24..]; // first scanout
    let width = u32::from_le_bytes([r[8], r[9], r[10], r[11]]);
    let height = u32::from_le_bytes([r[12], r[13], r[14], r[15]]);
    let enabled = u32::from_le_bytes([r[16], r[17], r[18], r[19]]);
    if enabled != 0 {
        Some((width, height))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_encodes_cmd_type() {
        let cmd = get_display_info();
        assert_eq!(cmd.len(), 24);
        assert_eq!(response_type(&cmd), Some(CMD_GET_DISPLAY_INFO));
    }

    #[test]
    fn create_2d_layout() {
        let cmd = resource_create_2d(1, FORMAT_B8G8R8A8_UNORM, 1024, 768);
        assert_eq!(response_type(&cmd), Some(CMD_RESOURCE_CREATE_2D));
        // resource_id at offset 24, width at 32, height at 36.
        assert_eq!(u32::from_le_bytes([cmd[24], cmd[25], cmd[26], cmd[27]]), 1);
        assert_eq!(u32::from_le_bytes([cmd[32], cmd[33], cmd[34], cmd[35]]), 1024);
        assert_eq!(u32::from_le_bytes([cmd[36], cmd[37], cmd[38], cmd[39]]), 768);
    }

    #[test]
    fn attach_backing_carries_addr() {
        let cmd = resource_attach_backing(1, 0xDEAD_BEEF_0000, 4096 * 768);
        assert_eq!(response_type(&cmd), Some(CMD_RESOURCE_ATTACH_BACKING));
        // header(24) + resource_id(4) + nr_entries(4) → addr at offset 32.
        let addr = u64::from_le_bytes([cmd[32], cmd[33], cmd[34], cmd[35], cmd[36], cmd[37], cmd[38], cmd[39]]);
        assert_eq!(addr, 0xDEAD_BEEF_0000);
    }

    #[test]
    fn scanout_and_flush_rects() {
        let s = set_scanout(0, 1, 800, 600);
        assert_eq!(response_type(&s), Some(CMD_SET_SCANOUT));
        // rect.width at offset 24+8, rect.height at 24+12.
        assert_eq!(u32::from_le_bytes([s[32], s[33], s[34], s[35]]), 800);
        let f = resource_flush(1, 800, 600);
        assert_eq!(response_type(&f), Some(CMD_RESOURCE_FLUSH));
    }

    #[test]
    fn ok_and_display_info_parse() {
        // Build an OK_DISPLAY_INFO response with a first scanout 1280×720.
        let mut resp = RESP_OK_DISPLAY_INFO.to_le_bytes().to_vec();
        resp.extend_from_slice(&[0u8; 20]); // rest of the header
        // scanout 0: rect (x,y,w,h) + enabled + flags
        resp.extend_from_slice(&0u32.to_le_bytes()); // x
        resp.extend_from_slice(&0u32.to_le_bytes()); // y
        resp.extend_from_slice(&1280u32.to_le_bytes()); // w
        resp.extend_from_slice(&720u32.to_le_bytes()); // h
        resp.extend_from_slice(&1u32.to_le_bytes()); // enabled
        resp.extend_from_slice(&0u32.to_le_bytes()); // flags
        assert!(is_ok(&resp));
        assert_eq!(parse_display_info(&resp), Some((1280, 720)));
    }

    #[test]
    fn error_response_not_ok() {
        let err = 0x1200u32.to_le_bytes(); // an ERR_* response
        assert!(!is_ok(&err));
    }
}
