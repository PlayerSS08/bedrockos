//! Boot random seed: several weak or untrusted sources are hashed together, so the seed is only as
//! predictable as the best source. Pure and host-testable; the hardware reads live in `main.rs`.

use crate::sha256::{Sha256, DIGEST_LEN};

pub const SEED_LEN: usize = DIGEST_LEN;

/// Accumulates entropy inputs. Each input is length-prefixed so `("ab","c")` and `("a","bc")`
/// differ.
pub struct Pool {
    h: Sha256,
    inputs: u32,
}

impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}

impl Pool {
    pub fn new() -> Self {
        let mut h = Sha256::new();
        h.update(b"aeon-boot-seed-v1");
        Self { h, inputs: 0 }
    }

    pub fn add(&mut self, data: &[u8]) {
        self.h.update(&(data.len() as u32).to_le_bytes());
        self.h.update(data);
        self.inputs += 1;
    }

    pub fn add_u64(&mut self, v: u64) {
        self.add(&v.to_le_bytes());
    }

    pub fn inputs(&self) -> u32 {
        self.inputs
    }

    pub fn finish(self) -> [u8; SEED_LEN] {
        self.h.finalize()
    }
}

/// A keyed hash generator for values that must not be guessable by someone on the network: TCP sequence numbers,
/// source ports, DNS ids. Each output is SHA-256 over a secret key and a counter, so knowing earlier outputs tells
/// an attacker nothing about later ones as long as the key stays secret. The key must come from real entropy
/// (see `Pool`); it is never reseeded, which is fine for the few thousand values one session draws.
pub struct Drbg {
    key: [u8; SEED_LEN],
    counter: u64,
}

impl Drbg {
    pub fn new(key: [u8; SEED_LEN]) -> Self {
        Self { key, counter: 0 }
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(DIGEST_LEN) {
            let mut h = Sha256::new();
            h.update(b"aeon-drbg-v1");
            h.update(&self.key);
            h.update(&self.counter.to_le_bytes());
            self.counter = self.counter.wrapping_add(1);
            let d = h.finalize();
            chunk.copy_from_slice(&d[..chunk.len()]);
        }
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill(&mut b);
        u32::from_le_bytes(b)
    }
}

/// A non-zero 64-bit canary value derived from the seed.
pub fn canary_from(seed: &[u8; SEED_LEN]) -> u64 {
    let v = u64::from_le_bytes(seed[..8].try_into().unwrap());
    if v == 0 {
        0xAE0A_5AFE_C0DE_D00D
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drbg_is_deterministic_per_key_and_never_repeats_nearby() {
        let mut a = Drbg::new([1; SEED_LEN]);
        let mut b = Drbg::new([1; SEED_LEN]);
        let mut c = Drbg::new([2; SEED_LEN]);
        let xs: alloc::vec::Vec<u32> = (0..64).map(|_| a.next_u32()).collect();
        assert_eq!(xs, (0..64).map(|_| b.next_u32()).collect::<alloc::vec::Vec<_>>());
        assert_ne!(xs, (0..64).map(|_| c.next_u32()).collect::<alloc::vec::Vec<_>>());
        let mut sorted = xs.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 64, "no repeats in 64 draws");
        // Roughly half the bits are set.
        let ones: u32 = xs.iter().map(|x| x.count_ones()).sum();
        assert!((800..1250).contains(&ones), "{ones} of 2048 bits set");
    }

    #[test]
    fn drbg_fill_spans_blocks() {
        let mut g = Drbg::new([9; SEED_LEN]);
        let mut big = [0u8; 100];
        g.fill(&mut big);
        assert_ne!(big[..32], big[32..64]);
        assert!(big.iter().any(|&b| b != 0));
    }

    #[test]
    fn same_inputs_same_seed_different_inputs_differ() {
        let mk = |x: &[u8]| {
            let mut p = Pool::new();
            p.add(x);
            p.finish()
        };
        assert_eq!(mk(b"a"), mk(b"a"));
        assert_ne!(mk(b"a"), mk(b"b"));
    }

    #[test]
    fn input_boundaries_matter() {
        let mut p = Pool::new();
        p.add(b"ab");
        p.add(b"c");
        let mut q = Pool::new();
        q.add(b"a");
        q.add(b"bc");
        assert_ne!(p.finish(), q.finish());
    }

    #[test]
    fn canary_never_zero() {
        assert_ne!(canary_from(&[0; SEED_LEN]), 0);
        assert_eq!(canary_from(&[1; SEED_LEN]), 0x0101_0101_0101_0101);
    }
}
