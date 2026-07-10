//! EuroAudio — the software mixer + sample-conversion core (plan I2).
//!
//! The hardware part (Intel HDA/AC'97 driver) provides a single PCM output stream; this
//! module is the architecture-independent core on top of it: multiple application
//! streams (each with its own volume) are **mixed** into one output buffer with
//! clipping protection, plus the common sample-format conversions (u8/i16/f32).
//! Pure `no_std` logic → the mixer arithmetic is fully tested on the host, independent
//! of any sound card.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod router;
pub use router::{Device, Router, Stream};

use alloc::vec;
use alloc::vec::Vec;

/// Volume as Q8 fixed-point (256 = 1.0 = unity gain). Mixing scales each stream
/// by this before summing.
pub type Volume = u16;
pub const UNITY: Volume = 256;

/// Scale an i16 sample by a Q8 volume, with clamping to i16.
#[inline]
pub fn scale(sample: i16, vol: Volume) -> i16 {
    clamp_i32((sample as i32 * vol as i32) >> 8)
}

#[inline]
fn clamp_i32(v: i32) -> i16 {
    if v > i16::MAX as i32 {
        i16::MAX
    } else if v < i16::MIN as i32 {
        i16::MIN
    } else {
        v as i16
    }
}

/// Mix a number of i16 PCM streams (each with its own volume) into `out`. All streams and
/// `out` have the same length (interleaved frames). Summing is done in i32 and
/// clamped to i16 — so overlapping sound causes distortion instead of
/// wrap-around crackle.
pub fn mix(streams: &[(&[i16], Volume)], out: &mut [i16]) {
    for s in out.iter_mut() {
        *s = 0;
    }
    let mut acc: Vec<i32> = vec![0i32; out.len()];
    for (buf, vol) in streams {
        let n = buf.len().min(out.len());
        for i in 0..n {
            acc[i] += (buf[i] as i32 * *vol as i32) >> 8;
        }
    }
    for (o, a) in out.iter_mut().zip(acc.iter()) {
        *o = clamp_i32(*a);
    }
}

// ── Sample-format conversions (to/from i16, the mixer-internal format) ──

/// Unsigned 8-bit (0..255, midpoint 128) → i16.
pub fn u8_to_i16(src: &[u8], out: &mut [i16]) {
    for (o, &s) in out.iter_mut().zip(src.iter()) {
        *o = ((s as i32 - 128) * 256) as i16;
    }
}

/// i16 → unsigned 8-bit.
pub fn i16_to_u8(src: &[i16], out: &mut [u8]) {
    for (o, &s) in out.iter_mut().zip(src.iter()) {
        *o = ((s as i32 / 256) + 128).clamp(0, 255) as u8;
    }
}

/// f32 (normalized −1.0..1.0) → i16.
pub fn f32_to_i16(src: &[f32], out: &mut [i16]) {
    for (o, &s) in out.iter_mut().zip(src.iter()) {
        let v = (s * 32767.0) as i32;
        *o = clamp_i32(v);
    }
}

/// Simple nearest-neighbour resampling of mono i16 from `src_hz` to `dst_hz`.
/// Enough for a first mixer; linear/polyphase interpolation = later refinement.
pub fn resample_nn(src: &[i16], src_hz: u32, dst_hz: u32) -> Vec<i16> {
    if src_hz == 0 || dst_hz == 0 || src.is_empty() {
        return Vec::new();
    }
    let out_len = (src.len() as u64 * dst_hz as u64 / src_hz as u64) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let si = (i as u64 * src_hz as u64 / dst_hz as u64) as usize;
        out.push(src[si.min(src.len() - 1)]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_unity_is_identity() {
        assert_eq!(scale(1000, UNITY), 1000);
        assert_eq!(scale(-1000, UNITY), -1000);
    }

    #[test]
    fn scale_half_volume() {
        assert_eq!(scale(1000, UNITY / 2), 500);
    }

    #[test]
    fn scale_clamps_on_overflow() {
        assert_eq!(scale(20000, UNITY * 2), i16::MAX); // 40000 → clamp
        assert_eq!(scale(-20000, UNITY * 2), i16::MIN);
    }

    #[test]
    fn mix_sums_streams() {
        let a = [1000i16, -2000, 3000];
        let b = [500i16, 500, 500];
        let mut out = [0i16; 3];
        mix(&[(&a, UNITY), (&b, UNITY)], &mut out);
        assert_eq!(out, [1500, -1500, 3500]);
    }

    #[test]
    fn mix_respects_volume() {
        let a = [1000i16, 1000];
        let b = [1000i16, 1000];
        let mut out = [0i16; 2];
        mix(&[(&a, UNITY / 2), (&b, UNITY / 4)], &mut out);
        assert_eq!(out, [750, 750]); // 500 + 250
    }

    #[test]
    fn mix_clamps_loud_sum() {
        let a = [30000i16];
        let b = [30000i16];
        let mut out = [0i16; 1];
        mix(&[(&a, UNITY), (&b, UNITY)], &mut out);
        assert_eq!(out[0], i16::MAX); // 60000 → clamp, no wrap-around
    }

    #[test]
    fn format_conversions_roundtrip_u8() {
        let src = [0u8, 128, 255];
        let mut mid = [0i16; 3];
        u8_to_i16(&src, &mut mid);
        assert_eq!(mid[1], 0); // 128 = silence
        let mut back = [0u8; 3];
        i16_to_u8(&mid, &mut back);
        assert_eq!(back[1], 128);
        assert!(back[0] <= 1 && back[2] >= 254);
    }

    #[test]
    fn f32_conversion() {
        let src = [0.0f32, 1.0, -1.0, 0.5];
        let mut out = [0i16; 4];
        f32_to_i16(&src, &mut out);
        assert_eq!(out[0], 0);
        assert_eq!(out[1], 32767);
        assert_eq!(out[2], -32767);
        assert_eq!(out[3], 16383);
    }

    #[test]
    fn resample_up_and_down() {
        let src = [10i16, 20, 30, 40];
        let up = resample_nn(&src, 8000, 16000); // 2× → 8 samples
        assert_eq!(up.len(), 8);
        assert_eq!(up[0], 10);
        let down = resample_nn(&src, 16000, 8000); // ½× → 2 samples
        assert_eq!(down.len(), 2);
    }
}
