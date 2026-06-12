//! Soevereine `no_std`-mathkern voor EuroReken — geen `libm`, geen `std`.
//!
//! Transcendente functies via klassieke numerieke methoden (argument-reductie +
//! reeksen), nauwkeurig genoeg voor een rekenmachine (≈1e-10 relatief op het
//! gebruikelijke bereik). Bewust leesbaar i.p.v. micro-geoptimaliseerd.

const PI: f64 = core::f64::consts::PI;
const LN2: f64 = core::f64::consts::LN_2;

/// |x|
pub fn fabs(x: f64) -> f64 {
    if x < 0.0 {
        -x
    } else {
        x
    }
}

/// Vierkantswortel via Newton-Raphson.
pub fn sqrt(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return 0.0;
    }
    // Startschatting via bit-exponent-halvering benadert; hier volstaat x zelf.
    let mut g = if x > 1.0 { x } else { 1.0 };
    let mut i = 0;
    while i < 60 {
        let ng = 0.5 * (g + x / g);
        if fabs(ng - g) <= 1e-15 * ng {
            return ng;
        }
        g = ng;
        i += 1;
    }
    g
}

/// e^x via argument-reductie x = k·ln2 + r en een Taylor-reeks voor e^r.
pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x > 709.0 {
        return f64::INFINITY;
    }
    if x < -745.0 {
        return 0.0;
    }
    let k = round(x / LN2);
    let r = x - k * LN2;
    // Taylor van e^r, r in [-ln2/2, ln2/2] → snelle convergentie.
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut n = 1.0;
    while fabs(term) > 1e-18 && n < 40.0 {
        term *= r / n;
        sum += term;
        n += 1.0;
    }
    sum * pow2i(k as i64)
}

/// 2^n voor geheel n, via herhaald vermenigvuldigen (exact binnen f64-bereik).
fn pow2i(mut n: i64) -> f64 {
    let mut base = 2.0;
    let mut result = 1.0;
    let neg = n < 0;
    if neg {
        n = -n;
    }
    while n > 0 {
        if n & 1 == 1 {
            result *= base;
        }
        base *= base;
        n >>= 1;
    }
    if neg {
        1.0 / result
    } else {
        result
    }
}

/// natuurlijke logaritme via ln(x)=ln(m)+k·ln2 met m∈[1,2) en de atanh-reeks.
pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    // Normaliseer naar m∈[1,2): tel machten van 2.
    let mut m = x;
    let mut k = 0i64;
    while m >= 2.0 {
        m /= 2.0;
        k += 1;
    }
    while m < 1.0 {
        m *= 2.0;
        k -= 1;
    }
    // ln(m) = 2·atanh((m-1)/(m+1)) = 2·(t + t^3/3 + t^5/5 + ...)
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    let mut term = t;
    let mut sum = 0.0;
    let mut n = 1.0;
    while fabs(term) > 1e-18 && n < 200.0 {
        sum += term / n;
        term *= t2;
        n += 2.0;
    }
    2.0 * sum + (k as f64) * LN2
}

/// logaritme met grondtal 10.
pub fn log10(x: f64) -> f64 {
    ln(x) / core::f64::consts::LN_10
}

/// x^y. Geheeltallige exponenten exact; anders e^(y·ln x) (x>0).
pub fn pow(x: f64, y: f64) -> f64 {
    if y == 0.0 {
        return 1.0;
    }
    if x == 0.0 {
        return if y > 0.0 { 0.0 } else { f64::INFINITY };
    }
    // Geheeltallige y: snelle, tekenbestendige machtsverheffing.
    if y == round(y) && fabs(y) < 1024.0 {
        let mut n = y as i64;
        let neg = n < 0;
        if neg {
            n = -n;
        }
        let mut base = x;
        let mut result = 1.0;
        while n > 0 {
            if n & 1 == 1 {
                result *= base;
            }
            base *= base;
            n >>= 1;
        }
        return if neg { 1.0 / result } else { result };
    }
    if x < 0.0 {
        return f64::NAN; // niet-gehele macht van negatief getal
    }
    exp(y * ln(x))
}

/// Afronden naar dichtstbijzijnde geheel (half weg van nul).
pub fn round(x: f64) -> f64 {
    if x >= 0.0 {
        (x + 0.5) as i64 as f64
    } else {
        -((-x + 0.5) as i64 as f64)
    }
}

/// Naar beneden afronden.
pub fn floor(x: f64) -> f64 {
    let t = x as i64 as f64;
    if x < 0.0 && t != x {
        t - 1.0
    } else {
        t
    }
}

fn reduce_two_pi(x: f64) -> f64 {
    // Breng x naar [-pi, pi].
    let two_pi = 2.0 * PI;
    let k = round(x / two_pi);
    x - k * two_pi
}

/// sin(x) via Taylor na reductie naar [-pi, pi].
pub fn sin(x: f64) -> f64 {
    let r = reduce_two_pi(x);
    let r2 = r * r;
    let mut term = r;
    let mut sum = r;
    let mut n = 1.0;
    while fabs(term) > 1e-18 && n < 60.0 {
        term *= -r2 / ((2.0 * n) * (2.0 * n + 1.0));
        sum += term;
        n += 1.0;
    }
    sum
}

/// cos(x) via Taylor na reductie naar [-pi, pi].
pub fn cos(x: f64) -> f64 {
    let r = reduce_two_pi(x);
    let r2 = r * r;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut n = 1.0;
    while fabs(term) > 1e-18 && n < 60.0 {
        term *= -r2 / ((2.0 * n - 1.0) * (2.0 * n));
        sum += term;
        n += 1.0;
    }
    sum
}

/// tan(x) = sin/cos.
pub fn tan(x: f64) -> f64 {
    sin(x) / cos(x)
}

/// n! als f64 (exact tot 170!).
pub fn factorial(n: u64) -> f64 {
    let mut r = 1.0;
    let mut i = 2u64;
    while i <= n {
        r *= i as f64;
        i += 1;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        fabs(a - b) <= 1e-9 * (1.0 + fabs(b))
    }

    #[test]
    fn sqrt_values() {
        assert!(close(sqrt(2.0), 1.4142135623730951));
        assert!(close(sqrt(144.0), 12.0));
        assert!(close(sqrt(0.25), 0.5));
    }

    #[test]
    fn exp_ln_roundtrip() {
        assert!(close(exp(0.0), 1.0));
        assert!(close(exp(1.0), core::f64::consts::E));
        assert!(close(ln(core::f64::consts::E), 1.0));
        assert!(close(ln(exp(3.5)), 3.5));
        assert!(close(exp(ln(42.0)), 42.0));
    }

    #[test]
    fn pow_values() {
        assert!(close(pow(2.0, 10.0), 1024.0));
        assert!(close(pow(2.0, -2.0), 0.25));
        assert!(close(pow(9.0, 0.5), 3.0));
        assert!(close(pow(27.0, 1.0 / 3.0), 3.0));
    }

    #[test]
    fn trig_values() {
        assert!(close(sin(0.0), 0.0));
        assert!(close(sin(PI / 2.0), 1.0));
        assert!(close(cos(PI), -1.0));
        assert!(close(sin(PI), 0.0));
        // grote argumenten reduceren correct
        assert!(close(sin(10.0 * PI + PI / 6.0), 0.5));
    }

    #[test]
    fn log_and_factorial() {
        assert!(close(log10(1000.0), 3.0));
        assert!(close(factorial(5), 120.0));
        assert!(close(factorial(0), 1.0));
    }
}
