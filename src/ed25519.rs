//! Ed25519 signature verification (RFC 8032), pure and host-testable. Verification only: the kernel checks
//! that a package index was signed by the Aeon repository key; signing happens on the maintainer's PC
//! (`tools/aeon_repo.py`).
//!
//! Field elements are four 64-bit limbs modulo 2^255 - 19, kept "weakly reduced" (below 2^256) and made
//! canonical only when compared or encoded. The code is written for clarity, not speed or constant time:
//! it only ever handles public data (a public key, a signature and a public index).

use crate::sha512;

type Limbs = [u64; 4];

const P: Limbs = [0xffff_ffff_ffff_ffed, u64::MAX, u64::MAX, 0x7fff_ffff_ffff_ffff];
/// The group order L = 2^252 + 27742317777372353535851937790883648493.
const L: Limbs = [0x5812_631a_5cf5_d3ed, 0x14de_f9de_a2f7_9cd6, 0, 0x1000_0000_0000_0000];
const P_MINUS_2: Limbs = [0xffff_ffff_ffff_ffeb, u64::MAX, u64::MAX, 0x7fff_ffff_ffff_ffff];
/// (p - 5) / 8
const P58: Limbs = [0xffff_ffff_ffff_fffd, u64::MAX, u64::MAX, 0x0fff_ffff_ffff_ffff];
/// (p - 1) / 4
const P14: Limbs = [0xffff_ffff_ffff_fffb, u64::MAX, u64::MAX, 0x1fff_ffff_ffff_ffff];

fn geq(a: &Limbs, b: &Limbs) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

/// `a - b` modulo 2^256, with the final borrow.
fn sub_raw(a: &Limbs, b: &Limbs) -> (Limbs, bool) {
    let mut out = [0u64; 4];
    let mut borrow = false;
    for i in 0..4 {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
        out[i] = d2;
        borrow = b1 || b2;
    }
    (out, borrow)
}

#[derive(Clone, Copy, Debug)]
struct Fe(Limbs);

impl Fe {
    const ZERO: Fe = Fe([0; 4]);
    const ONE: Fe = Fe([1, 0, 0, 0]);

    fn small(v: u64) -> Fe {
        Fe([v, 0, 0, 0])
    }

    /// Adds a small value, folding any carry out of bit 256 back in as 38 (2^256 = 38 mod p).
    fn add_small(mut r: Limbs, mut v: u64) -> Limbs {
        for limb in r.iter_mut() {
            let (s, c) = limb.overflowing_add(v);
            *limb = s;
            v = u64::from(c);
            if v == 0 {
                return r;
            }
        }
        // Carried out of the top: 2^256 = 38. The value is tiny now, so this cannot carry again.
        Fe::add_small(r, 38 * v)
    }

    fn add(&self, o: &Fe) -> Fe {
        let mut r = [0u64; 4];
        let mut carry = 0u64;
        for (i, limb) in r.iter_mut().enumerate() {
            let s = u128::from(self.0[i]) + u128::from(o.0[i]) + u128::from(carry);
            *limb = s as u64;
            carry = (s >> 64) as u64;
        }
        Fe(Fe::add_small(r, 38 * carry))
    }

    fn sub(&self, o: &Fe) -> Fe {
        let (mut r, mut borrow) = sub_raw(&self.0, &o.0);
        while borrow {
            // Wrapped around 2^256 (= 38 mod p): take 38 back off.
            let (r2, b2) = sub_raw(&r, &[38, 0, 0, 0]);
            r = r2;
            borrow = b2;
        }
        Fe(r)
    }

    fn neg(&self) -> Fe {
        Fe::ZERO.sub(self)
    }

    fn mul(&self, o: &Fe) -> Fe {
        let mut t = [0u64; 8];
        for i in 0..4 {
            let mut carry = 0u128;
            for j in 0..4 {
                let cur = u128::from(t[i + j]) + u128::from(self.0[i]) * u128::from(o.0[j]) + carry;
                t[i + j] = cur as u64;
                carry = cur >> 64;
            }
            t[i + 4] = carry as u64;
        }
        // Fold the high half: 2^256 = 38 (mod p).
        let mut r = [0u64; 4];
        let mut carry = 0u128;
        for k in 0..4 {
            let cur = u128::from(t[k]) + 38 * u128::from(t[k + 4]) + carry;
            r[k] = cur as u64;
            carry = cur >> 64;
        }
        Fe(Fe::add_small(r, 38 * (carry as u64)))
    }

    fn square(&self) -> Fe {
        self.mul(self)
    }

    /// `self^e` for a 256-bit exponent (square and multiply).
    fn pow(&self, e: &Limbs) -> Fe {
        let mut acc = Fe::ONE;
        for i in (0..256).rev() {
            acc = acc.square();
            if (e[i / 64] >> (i % 64)) & 1 == 1 {
                acc = acc.mul(self);
            }
        }
        acc
    }

    fn invert(&self) -> Fe {
        self.pow(&P_MINUS_2)
    }

    /// The unique representative in 0..p.
    fn canonical(&self) -> Limbs {
        let mut r = self.0;
        for _ in 0..2 {
            if geq(&r, &P) {
                r = sub_raw(&r, &P).0;
            }
        }
        r
    }

    fn eq(&self, o: &Fe) -> bool {
        self.canonical() == o.canonical()
    }

    fn is_odd(&self) -> bool {
        self.canonical()[0] & 1 == 1
    }

    fn from_bytes_masked(b: &[u8; 32]) -> Option<Fe> {
        let mut l = [0u64; 4];
        for (i, limb) in l.iter_mut().enumerate() {
            *limb = u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        }
        l[3] &= 0x7fff_ffff_ffff_ffff;
        if geq(&l, &P) {
            return None; // not a canonical encoding
        }
        Some(Fe(l))
    }
}

#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

fn curve_d() -> Fe {
    // d = -121665 / 121666
    Fe::small(121665).neg().mul(&Fe::small(121666).invert())
}

impl Point {
    fn identity() -> Point {
        Point { x: Fe::ZERO, y: Fe::ONE, z: Fe::ONE, t: Fe::ZERO }
    }

    /// Unified addition for a = -1 twisted Edwards curves (RFC 8032 section 5.1.4); also doubles.
    fn add(&self, o: &Point, d2: &Fe) -> Point {
        let a = self.y.sub(&self.x).mul(&o.y.sub(&o.x));
        let b = self.y.add(&self.x).mul(&o.y.add(&o.x));
        let c = self.t.mul(d2).mul(&o.t);
        let dd = self.z.add(&self.z).mul(&o.z);
        let (e, f, g, h) = (b.sub(&a), dd.sub(&c), dd.add(&c), b.add(&a));
        Point { x: e.mul(&f), y: g.mul(&h), t: e.mul(&h), z: f.mul(&g) }
    }

    fn scalar_mul(&self, k: &Limbs, d2: &Fe) -> Point {
        let mut acc = Point::identity();
        for i in (0..256).rev() {
            acc = acc.add(&acc, d2);
            if (k[i / 64] >> (i % 64)) & 1 == 1 {
                acc = acc.add(self, d2);
            }
        }
        acc
    }

    fn decode(b: &[u8; 32]) -> Option<Point> {
        let sign = b[31] >> 7 == 1;
        let y = Fe::from_bytes_masked(b)?;
        let d = curve_d();
        let y2 = y.square();
        let u = y2.sub(&Fe::ONE);
        let v = d.mul(&y2).add(&Fe::ONE);
        // x = u v^3 (u v^7)^((p-5)/8)
        let v3 = v.square().mul(&v);
        let v7 = v3.square().mul(&v);
        let mut x = u.mul(&v3).mul(&u.mul(&v7).pow(&P58));
        let vx2 = v.mul(&x.square());
        if !vx2.eq(&u) {
            if !vx2.eq(&u.neg()) {
                return None; // not a point on the curve
            }
            x = x.mul(&Fe::small(2).pow(&P14)); // times sqrt(-1)
        }
        if x.eq(&Fe::ZERO) && sign {
            return None;
        }
        if x.is_odd() != sign {
            x = x.neg();
        }
        Some(Point { x, y, z: Fe::ONE, t: x.mul(&y) })
    }

    fn equals(&self, o: &Point) -> bool {
        self.x.mul(&o.z).eq(&o.x.mul(&self.z)) && self.y.mul(&o.z).eq(&o.y.mul(&self.z))
    }
}

/// A 512-bit little-endian value reduced modulo L, bit by bit.
fn reduce_mod_l(bytes: &[u8; 64]) -> Limbs {
    let mut r = [0u64; 4];
    for i in (0..512).rev() {
        // r = 2r + bit; r < L < 2^253 before the shift, so nothing is lost.
        let mut carry = (bytes[i / 8] >> (i % 8)) & 1;
        for limb in r.iter_mut() {
            let next = (*limb >> 63) as u8;
            *limb = (*limb << 1) | u64::from(carry);
            carry = next;
        }
        if geq(&r, &L) {
            r = sub_raw(&r, &L).0;
        }
    }
    r
}

fn le_limbs(b: &[u8]) -> Limbs {
    let mut l = [0u64; 4];
    for (i, limb) in l.iter_mut().enumerate() {
        *limb = u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    }
    l
}

/// Checks `sig` (64 bytes) over `msg` against the 32-byte public key `pk`.
pub fn verify(pk: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let Some(a) = Point::decode(pk) else { return false };
    let r_bytes: [u8; 32] = sig[..32].try_into().unwrap();
    let Some(r) = Point::decode(&r_bytes) else { return false };
    let s = le_limbs(&sig[32..]);
    if geq(&s, &L) {
        return false; // a malleable signature: S must be reduced
    }
    let mut h = sha512::Sha512::new();
    h.update(&r_bytes);
    h.update(pk);
    h.update(msg);
    let k = reduce_mod_l(&h.finalize());

    let mut base_bytes = [0x66u8; 32];
    base_bytes[0] = 0x58;
    let base = Point::decode(&base_bytes).expect("the base point is valid");
    let d2 = curve_d().add(&curve_d());
    let lhs = base.scalar_mul(&s, &d2);
    let rhs = r.add(&a.scalar_mul(&k, &d2), &d2);
    lhs.equals(&rhs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex<const N: usize>(s: &str) -> [u8; N] {
        let v: alloc::vec::Vec<u8> = (0..s.len() / 2).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap()).collect();
        v.try_into().unwrap()
    }

    #[test]
    fn field_arithmetic_identities() {
        let a = Fe([0x1234_5678_9abc_def0, 0xfedc_ba98_7654_3210, 0x0f0f_0f0f_0f0f_0f0f, 0x4444_4444_4444_4444]);
        let b = Fe(P); // p itself, which is 0
        assert!(b.eq(&Fe::ZERO));
        assert!(a.add(&b).eq(&a));
        assert!(a.sub(&a).eq(&Fe::ZERO));
        assert!(a.mul(&a.invert()).eq(&Fe::ONE));
        assert!(a.add(&a.neg()).eq(&Fe::ZERO));
        let c = a.mul(&a).add(&Fe::small(7));
        assert!(c.sub(&Fe::small(7)).eq(&a.square()));
        // (a + b)(a - b) = a^2 - b^2 with a wrapped-around operand
        let big = Fe([u64::MAX; 4]);
        assert!(big.add(&a).mul(&big.sub(&a)).eq(&big.square().sub(&a.square())));
        // sqrt(-1)^2 = -1
        assert!(Fe::small(2).pow(&P14).square().eq(&Fe::ONE.neg()));
    }

    #[test]
    fn the_base_point_has_order_l() {
        let mut bb = [0x66u8; 32];
        bb[0] = 0x58;
        let base = Point::decode(&bb).unwrap();
        let d2 = curve_d().add(&curve_d());
        assert!(base.scalar_mul(&L, &d2).equals(&Point::identity()));
        assert!(!base.scalar_mul(&[5, 0, 0, 0], &d2).equals(&Point::identity()));
    }

    #[test]
    fn mod_l_reduction() {
        let mut b = [0u8; 64];
        assert_eq!(reduce_mod_l(&b), [0; 4]);
        b[..32].copy_from_slice(&unhex::<32>("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010"));
        assert_eq!(reduce_mod_l(&b), [0; 4], "L itself reduces to 0");
        b[0] += 1;
        assert_eq!(reduce_mod_l(&b), [1, 0, 0, 0]);
        let ff = [0xFFu8; 64];
        let r = reduce_mod_l(&ff);
        assert!(!geq(&r, &L));
    }

    const VECTORS: [(&str, &str, &str); 3] = [
        (
            "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29",
            "",
            "8f895b3cafe2c9506039d0e2a66382568004674fe8d237785092e40d6aaf483e4fc60168705f31f101596138ce21aa357c0d32a064f423dc3ee4aa3abf53f803",
        ),
        (
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
        (
            "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8",
            "",
            "04292ed381456dbaf4f047645f37bcc9b2c93898f4345db1a1da344c6095a1f01c6bbbd2ac860d99bfbee50a0afaf91e5ef60d56e92b4f9d2dca94a90b64520f",
        ),
    ];

    fn message(i: usize) -> alloc::vec::Vec<u8> {
        match i {
            0 => alloc::vec![],
            1 => alloc::vec![0x72],
            _ => b"Aeon package index test".repeat(20),
        }
    }

    #[test]
    fn rfc8032_and_python_generated_signatures_verify() {
        for (i, (pk, _, sig)) in VECTORS.iter().enumerate() {
            assert!(verify(&unhex(pk), &message(i), &unhex(sig)), "vector {i}");
        }
        // RFC 8032 section 7.1, test 1.
        assert!(verify(
            &unhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
            b"",
            &unhex("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
        ));
    }

    #[test]
    fn tampering_is_detected() {
        let (pk, _, sig) = VECTORS[1];
        let (pk, sig): ([u8; 32], [u8; 64]) = (unhex(pk), unhex(sig));
        assert!(verify(&pk, &[0x72], &sig));
        assert!(!verify(&pk, &[0x73], &sig), "wrong message");
        assert!(!verify(&pk, &[], &sig), "empty message");
        for bit in [0usize, 7, 100, 255, 256, 300, 511] {
            let mut bad = sig;
            bad[bit / 8] ^= 1 << (bit % 8);
            assert!(!verify(&pk, &[0x72], &bad), "flipped signature bit {bit}");
        }
        let mut other = pk;
        other[5] ^= 1;
        assert!(!verify(&other, &[0x72], &sig), "wrong key");
        // The other vector's key with this signature.
        assert!(!verify(&unhex(VECTORS[0].0), &[0x72], &sig));
    }

    #[test]
    fn malleable_and_garbage_inputs_are_refused() {
        let (pk, sig): ([u8; 32], [u8; 64]) = (unhex(VECTORS[1].0), unhex(VECTORS[1].2));
        // S + L is the same point equation but a non-canonical signature.
        let mut s = le_limbs(&sig[32..]);
        let mut carry = 0u128;
        for i in 0..4 {
            let v = u128::from(s[i]) + u128::from(L[i]) + carry;
            s[i] = v as u64;
            carry = v >> 64;
        }
        let mut mall = sig;
        for i in 0..4 {
            mall[32 + i * 8..40 + i * 8].copy_from_slice(&s[i].to_le_bytes());
        }
        assert!(!verify(&pk, &[0x72], &mall));
        assert!(!verify(&[0xFF; 32], &[0x72], &sig), "key that is not a valid encoding");
        let mut seed = 77u32;
        for _ in 0..20 {
            let mut junk = [0u8; 64];
            for b in junk.iter_mut() {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                *b = (seed >> 16) as u8;
            }
            assert!(!verify(&pk, b"anything", &junk));
        }
    }
}
