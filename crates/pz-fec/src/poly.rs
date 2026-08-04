//! Dense polynomials over GF(2^8), stored most-significant coefficient first.
//!
//! `[1, 2, 3]` represents `x^2 + 2x + 3`. This is the same convention used by
//! the QR code literature, which keeps the Reed-Solomon routines in
//! [`crate::rs`] readable next to their published pseudocode.

use crate::gf;
use alloc::vec;
use alloc::vec::Vec;

/// Add two polynomials (coefficient-wise XOR), aligning them at the
/// least-significant end.
#[must_use]
pub fn add(p: &[u8], q: &[u8]) -> Vec<u8> {
    let len = p.len().max(q.len());
    let mut r = vec![0u8; len];
    for (i, &c) in p.iter().enumerate() {
        r[i + len - p.len()] = c;
    }
    for (i, &c) in q.iter().enumerate() {
        r[i + len - q.len()] ^= c;
    }
    r
}

/// Multiply every coefficient by a scalar.
#[must_use]
pub fn scale(p: &[u8], x: u8) -> Vec<u8> {
    p.iter().map(|&c| gf::mul(c, x)).collect()
}

/// Multiply two polynomials.
#[must_use]
pub fn mul(p: &[u8], q: &[u8]) -> Vec<u8> {
    if p.is_empty() || q.is_empty() {
        return Vec::new();
    }
    let mut r = vec![0u8; p.len() + q.len() - 1];
    for (j, &qj) in q.iter().enumerate() {
        if qj == 0 {
            continue;
        }
        for (i, &pi) in p.iter().enumerate() {
            r[i + j] ^= gf::mul(pi, qj);
        }
    }
    r
}

/// Evaluate `p` at `x` using Horner's method.
#[must_use]
pub fn eval(p: &[u8], x: u8) -> u8 {
    let mut y = 0u8;
    for &c in p {
        y = gf::mul(y, x) ^ c;
    }
    y
}

/// Drop leading (most-significant) zero coefficients in place.
pub fn trim_leading_zeros(p: &mut Vec<u8>) {
    let lead = p.iter().take_while(|&&c| c == 0).count();
    p.drain(..lead);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_aligns_at_the_low_end() {
        // (x + 1) + (x^2 + 1) = x^2 + x
        assert_eq!(add(&[1, 1], &[1, 0, 1]), vec![1, 1, 0]);
    }

    #[test]
    fn mul_matches_manual_expansion() {
        // (x + 2)(x + 3) = x^2 + (2^3)x + (2*3) = x^2 + 1x + 6
        let r = mul(&[1, 2], &[1, 3]);
        assert_eq!(r, vec![1, 2 ^ 3, gf::mul(2, 3)]);
    }

    #[test]
    fn mul_by_empty_is_empty() {
        assert!(mul(&[1, 2], &[]).is_empty());
    }

    #[test]
    fn eval_matches_direct_substitution() {
        // p(x) = x^2 + 3x + 5
        let p = [1u8, 3, 5];
        for x in 0u16..256 {
            let x = x as u8;
            let expected = gf::mul(x, x) ^ gf::mul(3, x) ^ 5;
            assert_eq!(eval(&p, x), expected);
        }
    }

    #[test]
    fn scale_then_eval_is_linear() {
        let p = [1u8, 7, 9, 200];
        for x in [0u8, 1, 2, 99, 255] {
            for s in [0u8, 1, 3, 128] {
                assert_eq!(eval(&scale(&p, s), x), gf::mul(s, eval(&p, x)));
            }
        }
    }

    #[test]
    fn trim_removes_only_leading_zeros() {
        let mut p = vec![0, 0, 1, 0, 2];
        trim_leading_zeros(&mut p);
        assert_eq!(p, vec![1, 0, 2]);
        let mut all_zero = vec![0, 0];
        trim_leading_zeros(&mut all_zero);
        assert!(all_zero.is_empty());
    }
}
