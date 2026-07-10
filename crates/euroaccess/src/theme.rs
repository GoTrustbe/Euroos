//! 3F-3 — themes + **WCAG 2.x contrast** for the European Accessibility Act.
//!
//! EN 301 549 / WCAG require a minimum contrast ratio between text and its
//! background (AA: 4.5:1 normal text, 3:1 large text; AAA: 7:1). This module
//! provides a runtime [`Theme`] palette (Light / Dark / **HighContrast**) and the
//! exact WCAG contrast-ratio math, so a high-contrast mode can be *proven* to
//! meet the requirement rather than merely asserted.

use crate::Rgb;

/// sRGB 8-bit → linear-light, precomputed (WCAG 2.x relative luminance).
/// Avoids a runtime `powf` (no libm needed): the gamma is baked into this table.
pub const SRGB_LINEAR: [f64; 256] = [
    0.00000000, 0.00030353, 0.00060705, 0.00091058, 0.00121411, 0.00151763,
    0.00182116, 0.00212469, 0.00242822, 0.00273174, 0.00303527, 0.00334654,
    0.00367651, 0.00402472, 0.00439144, 0.00477695, 0.00518152, 0.00560539,
    0.00604883, 0.00651209, 0.00699541, 0.00749903, 0.00802319, 0.00856813,
    0.00913406, 0.00972122, 0.01032982, 0.01096009, 0.01161225, 0.01228649,
    0.01298303, 0.01370208, 0.01444384, 0.01520851, 0.01599629, 0.01680738,
    0.01764195, 0.01850022, 0.01938236, 0.02028856, 0.02121901, 0.02217388,
    0.02315337, 0.02415763, 0.02518686, 0.02624122, 0.02732089, 0.02842604,
    0.02955683, 0.03071344, 0.03189603, 0.03310477, 0.03433981, 0.03560131,
    0.03688945, 0.03820437, 0.03954624, 0.04091520, 0.04231141, 0.04373503,
    0.04518620, 0.04666509, 0.04817182, 0.04970657, 0.05126946, 0.05286065,
    0.05448028, 0.05612849, 0.05780543, 0.05951124, 0.06124605, 0.06301002,
    0.06480327, 0.06662594, 0.06847817, 0.07036010, 0.07227185, 0.07421357,
    0.07618538, 0.07818742, 0.08021982, 0.08228271, 0.08437621, 0.08650046,
    0.08865559, 0.09084171, 0.09305896, 0.09530747, 0.09758735, 0.09989873,
    0.10224173, 0.10461648, 0.10702310, 0.10946171, 0.11193243, 0.11443537,
    0.11697067, 0.11953843, 0.12213877, 0.12477182, 0.12743768, 0.13013648,
    0.13286832, 0.13563333, 0.13843162, 0.14126329, 0.14412847, 0.14702727,
    0.14995979, 0.15292615, 0.15592646, 0.15896084, 0.16202938, 0.16513219,
    0.16826940, 0.17144110, 0.17464740, 0.17788842, 0.18116424, 0.18447499,
    0.18782077, 0.19120168, 0.19461783, 0.19806932, 0.20155625, 0.20507874,
    0.20863687, 0.21223076, 0.21586050, 0.21952620, 0.22322796, 0.22696587,
    0.23074005, 0.23455058, 0.23839757, 0.24228112, 0.24620133, 0.25015828,
    0.25415209, 0.25818285, 0.26225066, 0.26635560, 0.27049779, 0.27467731,
    0.27889426, 0.28314874, 0.28744084, 0.29177065, 0.29613827, 0.30054379,
    0.30498731, 0.30946892, 0.31398871, 0.31854678, 0.32314321, 0.32777810,
    0.33245154, 0.33716362, 0.34191442, 0.34670406, 0.35153260, 0.35640014,
    0.36130678, 0.36625260, 0.37123768, 0.37626212, 0.38132601, 0.38642943,
    0.39157248, 0.39675523, 0.40197778, 0.40724021, 0.41254261, 0.41788507,
    0.42326767, 0.42869050, 0.43415364, 0.43965717, 0.44520119, 0.45078578,
    0.45641102, 0.46207700, 0.46778380, 0.47353150, 0.47932018, 0.48514994,
    0.49102085, 0.49693300, 0.50288646, 0.50888132, 0.51491767, 0.52099557,
    0.52711513, 0.53327640, 0.53947949, 0.54572446, 0.55201140, 0.55834039,
    0.56471151, 0.57112483, 0.57758044, 0.58407842, 0.59061884, 0.59720179,
    0.60382734, 0.61049557, 0.61720656, 0.62396039, 0.63075714, 0.63759687,
    0.64447968, 0.65140564, 0.65837482, 0.66538730, 0.67244316, 0.67954247,
    0.68668531, 0.69387176, 0.70110189, 0.70837578, 0.71569350, 0.72305513,
    0.73046074, 0.73791041, 0.74540421, 0.75294222, 0.76052450, 0.76815115,
    0.77582222, 0.78353779, 0.79129794, 0.79910274, 0.80695226, 0.81484657,
    0.82278575, 0.83076988, 0.83879901, 0.84687323, 0.85499261, 0.86315721,
    0.87136712, 0.87962240, 0.88792312, 0.89626935, 0.90466117, 0.91309865,
    0.92158186, 0.93011086, 0.93868573, 0.94730654, 0.95597335, 0.96468625,
    0.97344529, 0.98225055, 0.99110210, 1.00000000,
];

/// WCAG relative luminance of an sRGB colour (0.0–1.0).
pub fn relative_luminance(c: Rgb) -> f64 {
    0.2126 * SRGB_LINEAR[c.0 as usize] + 0.7152 * SRGB_LINEAR[c.1 as usize] + 0.0722 * SRGB_LINEAR[c.2 as usize]
}

/// WCAG contrast ratio between two colours (1.0–21.0).
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Does `fg` on `bg` meet WCAG AA? (`large` text = ≥ 3:1, normal text = ≥ 4.5:1.)
pub fn meets_aa(fg: Rgb, bg: Rgb, large: bool) -> bool {
    contrast_ratio(fg, bg) >= if large { 3.0 } else { 4.5 }
}
/// Does `fg` on `bg` meet the stricter WCAG AAA (7:1)?
pub fn meets_aaa(fg: Rgb, bg: Rgb) -> bool {
    contrast_ratio(fg, bg) >= 7.0
}

/// A UI palette: the colours the compositor resolves through a runtime theme
/// (instead of compile-time constants) so a high-contrast mode can replace them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: Rgb,
    pub surface: Rgb,
    pub ink: Rgb,
    pub ink_secondary: Rgb,
    pub accent: Rgb,
    pub border: Rgb,
    /// The focus-ring colour (keyboard-focus visible indicator).
    pub focus: Rgb,
    pub danger: Rgb,
    pub success: Rgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
    HighContrast,
}

impl Theme {
    pub fn palette(self) -> Palette {
        match self {
            // The current EuroOS light palette (graphics.rs).
            Theme::Light => Palette {
                bg: Rgb(0xF4, 0xF1, 0xEB),
                surface: Rgb(0xFF, 0xFF, 0xFF),
                ink: Rgb(0x24, 0x30, 0x3B),
                ink_secondary: Rgb(0x5A, 0x67, 0x72),
                accent: Rgb(0x2D, 0x6B, 0xE0),
                border: Rgb(0xD8, 0xD2, 0xC6),
                focus: Rgb(0x2D, 0x6B, 0xE0),
                danger: Rgb(0xC0, 0x2A, 0x2A),
                success: Rgb(0x1E, 0x7A, 0x3C),
            },
            Theme::Dark => Palette {
                bg: Rgb(0x14, 0x18, 0x1D),
                surface: Rgb(0x1E, 0x24, 0x2B),
                ink: Rgb(0xEC, 0xEF, 0xF2),
                ink_secondary: Rgb(0xAF, 0xB8, 0xC0),
                accent: Rgb(0x6C, 0xA8, 0xFF),
                border: Rgb(0x33, 0x3B, 0x44),
                focus: Rgb(0x8A, 0xC0, 0xFF),
                danger: Rgb(0xFF, 0x8A, 0x8A),
                success: Rgb(0x74, 0xD9, 0x9A),
            },
            // Maximum-contrast: pure black background, pure white/ yellow text —
            // designed so every text/bg pair clears WCAG AAA (7:1).
            Theme::HighContrast => Palette {
                bg: Rgb(0x00, 0x00, 0x00),
                surface: Rgb(0x00, 0x00, 0x00),
                ink: Rgb(0xFF, 0xFF, 0xFF),
                ink_secondary: Rgb(0xFF, 0xFF, 0xFF),
                accent: Rgb(0xFF, 0xFF, 0x00),
                border: Rgb(0xFF, 0xFF, 0xFF),
                focus: Rgb(0xFF, 0xFF, 0x00),
                danger: Rgb(0xFF, 0x60, 0x60),
                success: Rgb(0x60, 0xFF, 0x60),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wcag_reference_ratios() {
        // Black on white is the maximum ratio, 21:1.
        assert!((contrast_ratio(Rgb(0, 0, 0), Rgb(255, 255, 255)) - 21.0).abs() < 0.01);
        // Identical colours = 1:1.
        assert!((contrast_ratio(Rgb(80, 80, 80), Rgb(80, 80, 80)) - 1.0).abs() < 0.001);
    }

    #[test]
    fn high_contrast_theme_meets_aaa() {
        let p = Theme::HighContrast.palette();
        // Every text-on-background pairing must clear AAA (7:1).
        assert!(meets_aaa(p.ink, p.bg));
        assert!(meets_aaa(p.ink_secondary, p.surface));
        assert!(meets_aaa(p.accent, p.bg));
        assert!(meets_aaa(p.focus, p.bg));
    }

    #[test]
    fn light_theme_body_text_meets_aa() {
        let p = Theme::Light.palette();
        assert!(meets_aa(p.ink, p.bg, false)); // primary text ≥ 4.5:1
    }
}
