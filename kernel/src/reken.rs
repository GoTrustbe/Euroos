//! Kernel-zijde van **EuroReken** (Sprint AC-1): de soevereine rekenmachine.
//! Bij boot bewijzen we de drie modi — standaard/wetenschappelijk (op de eigen
//! `no_std`-mathkern) en programmeur (bases + bitwise) — plus eenhedenconversie.
//! Host-geteste kern: [`euroreken`].

use crate::serial_println;

/// Boot-zelftest: rekenkundige precedentie, functies, bases/bitwise, conversie.
pub fn selftest() {
    let arith = euroreken::eval("1 + 2 * 3").unwrap_or(0.0); // 7
    let pow = euroreken::eval("2 ^ 10").unwrap_or(0.0); // 1024
    let sci = euroreken::eval("sqrt(2) + sin(pi/2)").unwrap_or(0.0); // ~2.4142
    let prog = euroreken::eval_programmer("0xF0 | 0x0F").unwrap_or(0); // 255
    let shifted = euroreken::eval_programmer("1 << 8").unwrap_or(0); // 256
    let hex = euroreken::format_base(255, 16); // 0xFF
    let km = euroreken::convert(1.0, "mi", "km").unwrap_or(0.0); // 1.609344
    let temp = euroreken::convert(100.0, "c", "f").unwrap_or(0.0); // 212

    let approx = |a: f64, b: f64| euroreken::math::fabs(a - b) < 1e-6;

    let ok = approx(arith, 7.0)
        && approx(pow, 1024.0)
        && approx(sci, 1.414_213_562_373 + 1.0)
        && prog == 255
        && shifted == 256
        && hex == "0xFF"
        && approx(km, 1.609344)
        && approx(temp, 212.0);

    serial_println!(
        "[ar] EuroReken: 1+2*3={} 2^10={} sqrt2+sin(pi/2)={} | 0xF0|0x0F={}={} 1<<8={} | 1mi={:.4}km 100C={}F {}",
        arith as i64,
        pow as i64,
        sci,
        prog,
        hex,
        shifted,
        km,
        temp as i64,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
