//! EuroAudio — de software-mixer + sample-conversie-kern (plan I2).
//!
//! Het hardware-deel (Intel HDA/AC'97-driver) levert één PCM-uitvoerstroom; deze
//! module is de architectuur-onafhankelijke kern erboven: meerdere applicatie-
//! streams (elk met eigen volume) worden naar één uitvoerbuffer **gemixt** met
//! clipping-bescherming, plus de gangbare sample-formaat-conversies (u8/i16/f32).
//! Pure `no_std`-logica → de mixer-rekenkunde is volledig op de host getest, los
//! van enige geluidskaart.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

/// Volume als Q8-vaste-komma (256 = 1.0 = unity gain). Mixen schaalt elke stream
/// hiermee vóór sommeren.
pub type Volume = u16;
pub const UNITY: Volume = 256;

/// Schaal een i16-sample met een Q8-volume, met clamping naar i16.
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

/// Mix een aantal i16-PCM-streams (elk met eigen volume) in `out`. Alle streams en
/// `out` hebben dezelfde lengte (geïnterleaved frames). Sommeren gebeurt in i32 en
/// wordt naar i16 geclamped — zo veroorzaakt overlappend geluid vervorming i.p.v.
/// wrap-around-gekraak.
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

// ── Sample-formaat-conversies (naar/van i16, het mixer-interne formaat) ──

/// Unsigned 8-bit (0..255, midden 128) → i16.
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

/// f32 (genormaliseerd −1.0..1.0) → i16.
pub fn f32_to_i16(src: &[f32], out: &mut [i16]) {
    for (o, &s) in out.iter_mut().zip(src.iter()) {
        let v = (s * 32767.0) as i32;
        *o = clamp_i32(v);
    }
}

/// Eenvoudige nearest-neighbour-resampling van mono i16 van `src_hz` naar `dst_hz`.
/// Genoeg voor een eerste mixer; lineaire/polyphase-interpolatie = latere verfijning.
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
        assert_eq!(out[0], i16::MAX); // 60000 → clamp, geen wrap-around
    }

    #[test]
    fn format_conversions_roundtrip_u8() {
        let src = [0u8, 128, 255];
        let mut mid = [0i16; 3];
        u8_to_i16(&src, &mut mid);
        assert_eq!(mid[1], 0); // 128 = stilte
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
