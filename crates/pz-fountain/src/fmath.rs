//! Deterministic `sqrt` and `ln` built from IEEE-754 primitives.
//!
//! The degree distribution in [`crate::soliton`] depends on `ln` and `sqrt`.
//! If two implementations of PZ disagree about those values by even one bit
//! they can compute different degree tables, sample different source blocks
//! for the same frame seed, and fail to interoperate.
//!
//! `libm` and the platform math library are *not* bit-reproducible across
//! targets. Addition, subtraction, multiplication and division of `f64`
//! **are** exactly specified by IEEE-754, so these routines are built only
//! from those four operations plus bit manipulation. Every conforming PZ
//! implementation that follows this algorithm gets identical results on every
//! platform.
//!
//! Accuracy is around one part in 10^15, far tighter than the distribution
//! needs; exactness is what matters here, not the last ulp.

/// Square root by Newton-Raphson from a bit-pattern initial guess.
///
/// Returns NaN for negative inputs and preserves zero and infinity.
#[must_use]
pub fn sqrt(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 || x.is_infinite() {
        return x;
    }

    // Halving the biased exponent gives a guess accurate to a few percent.
    let bits = x.to_bits();
    let guess = f64::from_bits((bits >> 1) + (0x1FF8_0000_0000_0000));

    // Each iteration doubles the number of correct digits; six is far more
    // than enough to reach full double precision from this starting point.
    let mut y = guess;
    for _ in 0..6 {
        y = 0.5 * (y + x / y);
    }
    y
}

/// Natural logarithm.
///
/// Decomposes `x = m * 2^e` with `m` in `[1, 2)`, then uses the rapidly
/// converging `atanh` series
/// `ln(m) = 2 * (s + s^3/3 + s^5/5 + ...)` where `s = (m - 1) / (m + 1)`.
/// For `m` in `[1, 2)`, `s <= 1/3`, so the series converges geometrically.
#[must_use]
pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x.is_infinite() {
        return f64::INFINITY;
    }

    // The correctly rounded double nearest to ln(2). Taken from `core` rather
    // than written out so there is no chance of a transcription slip in a
    // value the whole degree distribution depends on.
    const LN2: f64 = core::f64::consts::LN_2;

    let bits = x.to_bits();
    let raw_exp = ((bits >> 52) & 0x7FF) as i64;

    let (mantissa, exponent) = if raw_exp == 0 {
        // Subnormal: scale into the normal range first, then compensate.
        let scaled = x * 9_007_199_254_740_992.0; // 2^53
        let sbits = scaled.to_bits();
        let sexp = ((sbits >> 52) & 0x7FF) as i64 - 1023 - 53;
        let m = f64::from_bits((sbits & 0x000F_FFFF_FFFF_FFFF) | 0x3FF0_0000_0000_0000);
        (m, sexp)
    } else {
        let e = raw_exp - 1023;
        let m = f64::from_bits((bits & 0x000F_FFFF_FFFF_FFFF) | 0x3FF0_0000_0000_0000);
        (m, e)
    };

    let s = (mantissa - 1.0) / (mantissa + 1.0);
    let s2 = s * s;

    // Sum odd terms until they fall below the precision floor. A fixed term
    // count keeps this loop identical everywhere.
    let mut term = s;
    let mut sum = s;
    for i in 1..24 {
        term *= s2;
        sum += term / ((2 * i + 1) as f64);
    }

    2.0 * sum + (exponent as f64) * LN2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        let d = if a > b { a - b } else { b - a };
        d <= tol * (1.0 + if b > 0.0 { b } else { -b })
    }

    #[test]
    fn sqrt_matches_std() {
        for x in [
            0.0, 1.0, 2.0, 3.0, 4.0, 1e-9, 0.5, 100.0, 255.0, 1024.0, 1e12, 1e300,
        ] {
            assert!(
                close(sqrt(x), x.sqrt(), 1e-15),
                "sqrt({x}) = {} vs {}",
                sqrt(x),
                x.sqrt()
            );
        }
        assert!(sqrt(-1.0).is_nan());
        assert!(sqrt(f64::INFINITY).is_infinite());
    }

    #[test]
    fn sqrt_squares_back() {
        let mut x = 1e-8f64;
        while x < 1e8 {
            let r = sqrt(x);
            assert!(close(r * r, x, 1e-14), "sqrt({x})^2 = {}", r * r);
            x *= 3.7;
        }
    }

    #[test]
    fn ln_matches_std() {
        for x in [
            1.0,
            2.0,
            core::f64::consts::E,
            0.5,
            10.0,
            1000.0,
            1e-12,
            1e15,
            255.0,
            1.000_001,
        ] {
            assert!(
                close(ln(x), x.ln(), 1e-14),
                "ln({x}) = {} vs {}",
                ln(x),
                x.ln()
            );
        }
        assert_eq!(ln(1.0), 0.0);
        assert!(ln(0.0).is_infinite());
        assert!(ln(-1.0).is_nan());
    }

    #[test]
    fn ln_handles_subnormals() {
        let sub = f64::from_bits(1); // smallest positive subnormal
        assert!(close(ln(sub), sub.ln(), 1e-13), "ln(subnormal)");
    }

    #[test]
    fn ln_is_additive() {
        for (a, b) in [(2.0, 3.0), (7.0, 11.0), (0.25, 400.0), (1e6, 1e-6)] {
            assert!(
                close(ln(a * b), ln(a) + ln(b), 1e-13),
                "ln({a}*{b}) additivity"
            );
        }
    }
}
