//! Systematic Reed-Solomon coding over GF(2^8) with combined
//! errors-and-erasures decoding.
//!
//! An `RS(n, k)` code appends `n - k` parity symbols to `k` data symbols. The
//! decoder repairs any combination of `e` symbol errors (unknown position,
//! unknown value) and `f` erasures (known position, unknown value) satisfying
//!
//! ```text
//! 2e + f <= n - k
//! ```
//!
//! Erasures are half the price of errors, which is exactly why the PZ
//! demodulator reports per-cell confidence: a cell whose sampled colour sits
//! near a decision boundary is far more useful to the decoder when it is
//! flagged as "unknown" than when it is guessed at.
//!
//! The decoder follows the classical pipeline: syndrome computation, Forney
//! syndromes to fold in the known erasures, Berlekamp-Massey for the unknown
//! error locations, Chien search to find the roots, and Forney's algorithm for
//! the error magnitudes.

use crate::gf;
use crate::poly;
use alloc::vec;
use alloc::vec::Vec;

/// Errors produced by the Reed-Solomon codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FecError {
    /// `n` or `k` was outside the range the field can represent.
    InvalidParams,
    /// The supplied buffer did not have the length the code requires.
    WrongLength,
    /// An erasure position pointed outside the codeword.
    ErasureOutOfRange,
    /// The corruption exceeded the correction capability of the code.
    TooManyErrors,
}

impl core::fmt::Display for FecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::InvalidParams => "invalid Reed-Solomon parameters",
            Self::WrongLength => "buffer length does not match the code",
            Self::ErasureOutOfRange => "erasure position outside the codeword",
            Self::TooManyErrors => "too many errors to correct",
        };
        f.write_str(s)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for FecError {}

/// A systematic Reed-Solomon code with a cached generator polynomial.
#[derive(Debug, Clone)]
pub struct ReedSolomon {
    n: usize,
    k: usize,
    generator: Vec<u8>,
}

impl ReedSolomon {
    /// Build an `RS(n, k)` code.
    ///
    /// Requires `0 < k < n <= 255`.
    ///
    /// # Errors
    /// Returns [`FecError::InvalidParams`] if the parameters are out of range.
    pub fn new(n: usize, k: usize) -> Result<Self, FecError> {
        if k == 0 || n <= k || n > gf::ORDER {
            return Err(FecError::InvalidParams);
        }
        let nsym = n - k;
        // g(x) = product over i in [0, nsym) of (x - alpha^i)
        let mut generator = vec![1u8];
        for i in 0..nsym {
            generator = poly::mul(&generator, &[1, gf::pow(gf::GENERATOR, i as i32)]);
        }
        Ok(Self { n, k, generator })
    }

    /// Total codeword length in symbols.
    #[must_use]
    pub const fn n(&self) -> usize {
        self.n
    }

    /// Number of data symbols per codeword.
    #[must_use]
    pub const fn k(&self) -> usize {
        self.k
    }

    /// Number of parity symbols per codeword.
    #[must_use]
    pub const fn parity(&self) -> usize {
        self.n - self.k
    }

    /// Maximum number of symbol errors correctable when there are no erasures.
    #[must_use]
    pub const fn max_errors(&self) -> usize {
        (self.n - self.k) / 2
    }

    /// Maximum number of erasures correctable when there are no errors.
    #[must_use]
    pub const fn max_erasures(&self) -> usize {
        self.n - self.k
    }

    /// Encode `k` data symbols into an `n` symbol codeword.
    ///
    /// The result is systematic: the first `k` symbols are `data` verbatim and
    /// the remaining `n - k` are parity.
    ///
    /// # Errors
    /// Returns [`FecError::WrongLength`] unless `data.len() == k`.
    pub fn encode(&self, data: &[u8]) -> Result<Vec<u8>, FecError> {
        if data.len() != self.k {
            return Err(FecError::WrongLength);
        }
        let mut out = vec![0u8; self.n];
        out[..self.k].copy_from_slice(data);

        // Synthetic division of data * x^parity by the generator polynomial.
        // The data region is used as scratch space and restored afterwards.
        for i in 0..self.k {
            let coef = out[i];
            if coef != 0 {
                for j in 1..self.generator.len() {
                    out[i + j] ^= gf::mul(self.generator[j], coef);
                }
            }
        }
        out[..self.k].copy_from_slice(data);
        Ok(out)
    }

    /// Return the syndromes of a codeword. All-zero means "no detected error".
    #[must_use]
    fn syndromes(&self, code: &[u8]) -> Vec<u8> {
        // Index 0 is left as a zero pad so the Forney routines below can use
        // the same indexing as the reference formulation.
        let mut synd = vec![0u8; self.parity() + 1];
        for i in 0..self.parity() {
            synd[i + 1] = poly::eval(code, gf::pow(gf::GENERATOR, i as i32));
        }
        synd
    }

    /// Returns `true` when the codeword carries no detectable corruption.
    ///
    /// # Errors
    /// Returns [`FecError::WrongLength`] unless `code.len() == n`.
    pub fn is_clean(&self, code: &[u8]) -> Result<bool, FecError> {
        if code.len() != self.n {
            return Err(FecError::WrongLength);
        }
        Ok(self.syndromes(code).iter().all(|&s| s == 0))
    }

    /// Repair a codeword in place.
    ///
    /// `erasures` lists indices whose values are known to be unreliable. They
    /// may be empty, need not be sorted, and duplicates are ignored. Returns
    /// the number of symbols that were actually altered.
    ///
    /// # Errors
    /// Returns [`FecError::TooManyErrors`] when the corruption exceeds
    /// `2e + f <= n - k`, in which case `code` is left unmodified.
    pub fn decode(&self, code: &mut [u8], erasures: &[usize]) -> Result<usize, FecError> {
        if code.len() != self.n {
            return Err(FecError::WrongLength);
        }
        if erasures.iter().any(|&p| p >= self.n) {
            return Err(FecError::ErasureOutOfRange);
        }

        // Deduplicate erasure positions without requiring the caller to sort.
        let mut seen = vec![false; self.n];
        let mut erase_pos: Vec<usize> = Vec::with_capacity(erasures.len());
        for &p in erasures {
            if !seen[p] {
                seen[p] = true;
                erase_pos.push(p);
            }
        }
        if erase_pos.len() > self.parity() {
            return Err(FecError::TooManyErrors);
        }

        let original = code.to_vec();
        match self.decode_inner(code, &erase_pos) {
            Ok(()) => Ok(original
                .iter()
                .zip(code.iter())
                .filter(|(a, b)| a != b)
                .count()),
            Err(e) => {
                code.copy_from_slice(&original);
                Err(e)
            }
        }
    }

    fn decode_inner(&self, code: &mut [u8], erase_pos: &[usize]) -> Result<(), FecError> {
        let nsym = self.parity();

        // Erased symbols carry no information; zero them so they cannot bias
        // the syndromes.
        for &p in erase_pos {
            code[p] = 0;
        }

        let synd = self.syndromes(code);
        if synd.iter().all(|&s| s == 0) {
            return Ok(());
        }

        // Fold the known erasure positions into the syndromes so
        // Berlekamp-Massey only has to solve for the unknown error locations.
        let fsynd = forney_syndromes(&synd, erase_pos, self.n);
        let mut err_loc = find_error_locator(&fsynd, nsym, erase_pos.len())?;
        err_loc.reverse();
        let err_pos = find_errors(&err_loc, self.n)?;

        let mut all_pos = Vec::with_capacity(erase_pos.len() + err_pos.len());
        all_pos.extend_from_slice(erase_pos);
        for p in err_pos {
            if !all_pos.contains(&p) {
                all_pos.push(p);
            }
        }
        if all_pos.len() > nsym {
            return Err(FecError::TooManyErrors);
        }

        correct_errata(code, &synd, &all_pos)?;

        // A successful correction must drive every syndrome to zero. Without
        // this check a codeword corrupted beyond the correction radius can
        // decode to a valid but wrong codeword and be reported as success.
        if self.syndromes(code).iter().any(|&s| s != 0) {
            return Err(FecError::TooManyErrors);
        }
        Ok(())
    }

    /// Decode and return just the `k` data symbols.
    ///
    /// # Errors
    /// See [`ReedSolomon::decode`].
    pub fn decode_data(&self, code: &mut [u8], erasures: &[usize]) -> Result<Vec<u8>, FecError> {
        self.decode(code, erasures)?;
        Ok(code[..self.k].to_vec())
    }
}

/// Fold erasure positions into the syndrome sequence.
fn forney_syndromes(synd: &[u8], erase_pos: &[usize], nmess: usize) -> Vec<u8> {
    let mut fsynd = synd[1..].to_vec();
    for &p in erase_pos {
        let x = gf::pow(gf::GENERATOR, (nmess - 1 - p) as i32);
        for j in 0..fsynd.len().saturating_sub(1) {
            fsynd[j] = gf::mul(fsynd[j], x) ^ fsynd[j + 1];
        }
    }
    fsynd
}

/// Berlekamp-Massey, solving for the locations of the unknown errors only.
fn find_error_locator(synd: &[u8], nsym: usize, erase_count: usize) -> Result<Vec<u8>, FecError> {
    let mut err_loc: Vec<u8> = vec![1];
    let mut old_loc: Vec<u8> = vec![1];

    let synd_shift = synd.len().saturating_sub(nsym);
    let iterations = nsym.saturating_sub(erase_count);

    for i in 0..iterations {
        let kk = i + synd_shift;
        let mut delta = *synd.get(kk).ok_or(FecError::TooManyErrors)?;
        for j in 1..err_loc.len() {
            let idx = kk.checked_sub(j).ok_or(FecError::TooManyErrors)?;
            delta ^= gf::mul(err_loc[err_loc.len() - 1 - j], synd[idx]);
        }

        old_loc.push(0);

        if delta != 0 {
            if old_loc.len() > err_loc.len() {
                let new_loc = poly::scale(&old_loc, delta);
                old_loc = poly::scale(&err_loc, gf::inv(delta));
                err_loc = new_loc;
            }
            err_loc = poly::add(&err_loc, &poly::scale(&old_loc, delta));
        }
    }

    poly::trim_leading_zeros(&mut err_loc);
    let errs = err_loc.len().saturating_sub(1);
    if errs * 2 + erase_count > nsym {
        return Err(FecError::TooManyErrors);
    }
    Ok(err_loc)
}

/// Chien search: the roots of the locator polynomial give the error positions.
fn find_errors(err_loc_rev: &[u8], nmess: usize) -> Result<Vec<usize>, FecError> {
    let errs = err_loc_rev.len().saturating_sub(1);
    let mut positions = Vec::with_capacity(errs);
    for i in 0..nmess {
        if poly::eval(err_loc_rev, gf::pow(gf::GENERATOR, i as i32)) == 0 {
            positions.push(nmess - 1 - i);
        }
    }
    // A locator of degree d that does not have exactly d roots inside the
    // codeword means the received word is outside the decoding radius.
    if positions.len() != errs {
        return Err(FecError::TooManyErrors);
    }
    Ok(positions)
}

/// Forney's algorithm: given the positions, solve for the error magnitudes.
fn correct_errata(code: &mut [u8], synd: &[u8], positions: &[usize]) -> Result<(), FecError> {
    let nmess = code.len();

    // Convert codeword indices into polynomial coefficient positions.
    let coef_pos: Vec<usize> = positions.iter().map(|&p| nmess - 1 - p).collect();

    // Errata locator: product of (1 - x * alpha^coef_pos).
    let mut err_loc: Vec<u8> = vec![1];
    for &cp in &coef_pos {
        err_loc = poly::mul(
            &err_loc,
            &poly::add(&[1], &[gf::pow(gf::GENERATOR, cp as i32), 0]),
        );
    }

    // Errata evaluator: (synd_reversed * err_loc) mod x^(deg+1).
    let mut synd_rev = synd.to_vec();
    synd_rev.reverse();
    let product = poly::mul(&synd_rev, &err_loc);
    let keep = err_loc.len();
    let err_eval = product[product.len().saturating_sub(keep)..].to_vec();

    let x: Vec<u8> = coef_pos
        .iter()
        .map(|&cp| gf::pow(gf::GENERATOR, cp as i32))
        .collect();

    let mut magnitudes = vec![0u8; nmess];
    for (i, &xi) in x.iter().enumerate() {
        let xi_inv = gf::inv(xi);

        // Formal derivative of the errata locator, evaluated at xi_inv.
        let mut denom = 1u8;
        for (j, &xj) in x.iter().enumerate() {
            if j != i {
                denom = gf::mul(denom, gf::sub(1, gf::mul(xi_inv, xj)));
            }
        }
        if denom == 0 {
            return Err(FecError::TooManyErrors);
        }

        let y = gf::mul(xi, poly::eval(&err_eval, xi_inv));
        magnitudes[positions[i]] = gf::div(y, denom);
    }

    for (c, m) in code.iter_mut().zip(magnitudes.iter()) {
        *c ^= *m;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small deterministic PRNG so the randomised tests are reproducible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn byte(&mut self) -> u8 {
            (self.next() & 0xFF) as u8
        }
    }

    #[test]
    fn rejects_invalid_parameters() {
        assert_eq!(
            ReedSolomon::new(10, 10).unwrap_err(),
            FecError::InvalidParams
        );
        assert_eq!(
            ReedSolomon::new(10, 0).unwrap_err(),
            FecError::InvalidParams
        );
        assert_eq!(
            ReedSolomon::new(256, 200).unwrap_err(),
            FecError::InvalidParams
        );
    }

    #[test]
    fn encode_is_systematic_and_clean() {
        let rs = ReedSolomon::new(32, 16).unwrap();
        let data: Vec<u8> = (0..16).collect();
        let code = rs.encode(&data).unwrap();
        assert_eq!(code.len(), 32);
        assert_eq!(&code[..16], &data[..]);
        assert!(rs.is_clean(&code).unwrap());
    }

    #[test]
    fn decodes_untouched_codeword_without_changes() {
        let rs = ReedSolomon::new(32, 16).unwrap();
        let data: Vec<u8> = (0..16).map(|i| i * 7 + 1).collect();
        let mut code = rs.encode(&data).unwrap();
        assert_eq!(rs.decode(&mut code, &[]).unwrap(), 0);
        assert_eq!(&code[..16], &data[..]);
    }

    #[test]
    fn corrects_up_to_t_errors() {
        let rs = ReedSolomon::new(255, 191).unwrap(); // t = 32
        let data: Vec<u8> = (0..191).map(|i| (i * 13 + 5) as u8).collect();
        let clean = rs.encode(&data).unwrap();

        for errors in [1usize, 5, 16, 31, 32] {
            let mut code = clean.clone();
            for i in 0..errors {
                // Spread the errors out and make sure each one really changes
                // the symbol.
                let pos = i * 7 % 255;
                code[pos] ^= 0xA5;
            }
            let corrected = rs.decode(&mut code, &[]).unwrap();
            assert!(
                corrected <= errors,
                "reported {corrected} > injected {errors}"
            );
            assert_eq!(&code[..191], &data[..], "failed at {errors} errors");
        }
    }

    #[test]
    fn corrects_up_to_2t_erasures() {
        let rs = ReedSolomon::new(255, 191).unwrap(); // 64 erasures
        let data: Vec<u8> = (0..191).map(|i| (i * 3 + 200) as u8).collect();
        let clean = rs.encode(&data).unwrap();

        for count in [1usize, 10, 63, 64] {
            let mut code = clean.clone();
            let positions: Vec<usize> = (0..count).map(|i| i * 3 % 255).collect();
            for &p in &positions {
                code[p] = 0xFF; // wrong value, but we know where it is
            }
            rs.decode(&mut code, &positions).unwrap();
            assert_eq!(&code[..191], &data[..], "failed at {count} erasures");
        }
    }

    #[test]
    fn corrects_mixed_errors_and_erasures() {
        let rs = ReedSolomon::new(255, 191).unwrap(); // 2e + f <= 64
        let data: Vec<u8> = (0..191).map(|i| (i ^ 0x5A) as u8).collect();
        let clean = rs.encode(&data).unwrap();

        for (e, f) in [
            (0usize, 0usize),
            (1, 1),
            (10, 20),
            (20, 24),
            (31, 2),
            (32, 0),
        ] {
            let mut code = clean.clone();
            let mut erasures = Vec::new();
            for i in 0..f {
                let p = i * 5 % 255;
                code[p] ^= 0x3C;
                erasures.push(p);
            }
            for i in 0..e {
                let p = 254 - i * 3;
                if erasures.contains(&p) {
                    continue;
                }
                code[p] ^= 0x77;
            }
            rs.decode(&mut code, &erasures)
                .unwrap_or_else(|err| panic!("e={e} f={f}: {err}"));
            assert_eq!(&code[..191], &data[..], "e={e} f={f}");
        }
    }

    #[test]
    fn reports_failure_beyond_the_correction_radius() {
        let rs = ReedSolomon::new(64, 32).unwrap(); // t = 16
        let data: Vec<u8> = (0..32).map(|i| i as u8).collect();
        let clean = rs.encode(&data).unwrap();

        // Far beyond the radius: overwrite most of the codeword.
        let mut code = clean.clone();
        for (i, c) in code.iter_mut().enumerate() {
            if i % 2 == 0 {
                *c ^= 0xEE;
            }
        }
        let err = rs.decode(&mut code, &[]).unwrap_err();
        assert_eq!(err, FecError::TooManyErrors);
        // On failure the buffer must be left exactly as it was handed in.
        let mut expected = clean.clone();
        for (i, c) in expected.iter_mut().enumerate() {
            if i % 2 == 0 {
                *c ^= 0xEE;
            }
        }
        assert_eq!(code, expected, "buffer must be untouched after a failure");
    }

    #[test]
    fn rejects_more_erasures_than_parity() {
        let rs = ReedSolomon::new(32, 16).unwrap();
        let data: Vec<u8> = (0..16).collect();
        let mut code = rs.encode(&data).unwrap();
        let too_many: Vec<usize> = (0..17).collect();
        assert_eq!(
            rs.decode(&mut code, &too_many).unwrap_err(),
            FecError::TooManyErrors
        );
    }

    #[test]
    fn rejects_out_of_range_erasure() {
        let rs = ReedSolomon::new(32, 16).unwrap();
        let mut code = rs.encode(&(0..16).collect::<Vec<u8>>()).unwrap();
        assert_eq!(
            rs.decode(&mut code, &[99]).unwrap_err(),
            FecError::ErasureOutOfRange
        );
    }

    #[test]
    fn duplicate_erasures_are_tolerated() {
        let rs = ReedSolomon::new(32, 16).unwrap();
        let data: Vec<u8> = (0..16).map(|i| i * 3).collect();
        let mut code = rs.encode(&data).unwrap();
        code[4] ^= 0x11;
        rs.decode(&mut code, &[4, 4, 4, 4]).unwrap();
        assert_eq!(&code[..16], &data[..]);
    }

    #[test]
    fn randomised_stress_within_radius() {
        let mut rng = Rng(0x50D0_1234_ABCD_9876);
        for trial in 0..400 {
            let k = 8 + rng.below(180);
            let parity = 4 + rng.below(60);
            let n = k + parity;
            if n > 255 {
                continue;
            }
            let rs = ReedSolomon::new(n, k).unwrap();
            let data: Vec<u8> = (0..k).map(|_| rng.byte()).collect();
            let clean = rs.encode(&data).unwrap();
            assert!(rs.is_clean(&clean).unwrap());

            // Choose f erasures and e errors with 2e + f <= parity.
            let f = rng.below(parity + 1);
            let e = rng.below((parity - f) / 2 + 1);

            let mut code = clean.clone();
            let mut used = vec![false; n];
            let mut erasures = Vec::new();
            for _ in 0..f {
                let mut p = rng.below(n);
                while used[p] {
                    p = (p + 1) % n;
                }
                used[p] = true;
                code[p] = rng.byte();
                erasures.push(p);
            }
            for _ in 0..e {
                let mut p = rng.below(n);
                while used[p] {
                    p = (p + 1) % n;
                }
                used[p] = true;
                code[p] ^= 1 + rng.byte() % 255; // guarantee a real change
            }

            rs.decode(&mut code, &erasures)
                .unwrap_or_else(|err| panic!("trial {trial}: n={n} k={k} e={e} f={f}: {err}"));
            assert_eq!(
                &code[..k],
                &data[..],
                "trial {trial}: n={n} k={k} e={e} f={f}"
            );
        }
    }
}
