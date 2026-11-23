//! SHA-1, HMAC-SHA1 and PBKDF2-HMAC-SHA1.
//!
//! SHA-1 is broken for collision resistance and is not used for anything else in BedrockOS. It is here only
//! because WPA2-PSK (IEEE 802.11i) is defined in terms of it: the passphrase-to-key step is PBKDF2 with
//! 4096 rounds of HMAC-SHA1, and the EAPOL handshake MIC is HMAC-SHA1. Pure and host-testable.

pub const DIGEST_LEN: usize = 20;
const BLOCK_LEN: usize = 64;

#[derive(Clone)]
pub struct Sha1 {
    state: [u32; 5],
    block: [u8; BLOCK_LEN],
    filled: usize,
    total: u64,
}

impl Sha1 {
    pub const fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0],
            block: [0; BLOCK_LEN],
            filled: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.filled > 0 {
            let take = (BLOCK_LEN - self.filled).min(data.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled == BLOCK_LEN {
                let b = self.block;
                self.compress(&b);
                self.filled = 0;
            }
        }
        while data.len() >= BLOCK_LEN {
            let mut b = [0u8; BLOCK_LEN];
            b.copy_from_slice(&data[..BLOCK_LEN]);
            self.compress(&b);
            data = &data[BLOCK_LEN..];
        }
        if !data.is_empty() {
            self.block[..data.len()].copy_from_slice(data);
            self.filled = data.len();
        }
    }

    pub fn finalize(mut self) -> [u8; DIGEST_LEN] {
        let bits = self.total.wrapping_mul(8);
        self.update_padding(bits);
        let mut out = [0u8; DIGEST_LEN];
        for (i, w) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }

    fn update_padding(&mut self, bits: u64) {
        let mut pad = [0u8; BLOCK_LEN * 2];
        pad[0] = 0x80;
        // Pad so that (filled + padlen) % 64 == 56, then 8 length bytes.
        let padlen = if self.filled < 56 { 56 - self.filled } else { 120 - self.filled };
        let keep_total = self.total;
        self.update(&pad[..padlen]);
        self.update(&bits.to_be_bytes());
        self.total = keep_total;
    }

    fn compress(&mut self, block: &[u8; BLOCK_LEN]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = self.state;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

pub fn digest(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha1::new();
    h.update(data);
    h.finalize()
}

/// HMAC-SHA1 over the concatenation of `parts` (so callers need no temporary buffer).
pub fn hmac_parts(key: &[u8], parts: &[&[u8]]) -> [u8; DIGEST_LEN] {
    let mut k = [0u8; BLOCK_LEN];
    if key.len() > BLOCK_LEN {
        k[..DIGEST_LEN].copy_from_slice(&digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK_LEN];
    let mut opad = [0x5Cu8; BLOCK_LEN];
    for i in 0..BLOCK_LEN {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha1::new();
    inner.update(&ipad);
    for p in parts {
        inner.update(p);
    }
    let inner = inner.finalize();
    let mut outer = Sha1::new();
    outer.update(&opad);
    outer.update(&inner);
    outer.finalize()
}

pub fn hmac(key: &[u8], data: &[u8]) -> [u8; DIGEST_LEN] {
    hmac_parts(key, &[data])
}

/// PBKDF2-HMAC-SHA1 (RFC 8018). Fills `out` with derived key bytes.
pub fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    for (i, chunk) in out.chunks_mut(DIGEST_LEN).enumerate() {
        let block_index = i as u32 + 1;
        let mut u = hmac_parts(password, &[salt, &block_index.to_be_bytes()]);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac(password, &u);
            for (tb, ub) in t.iter_mut().zip(u.iter()) {
                *tb ^= ub;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> alloc::string::String {
        use alloc::string::String;
        use core::fmt::Write;
        let mut s = String::new();
        for x in b {
            write!(s, "{x:02x}").unwrap();
        }
        s
    }

    #[test]
    fn sha1_known_answers() {
        assert_eq!(hex(&digest(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(&digest(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(&digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn sha1_million_a_and_split_updates_agree() {
        let mut h = Sha1::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(hex(&h.finalize()), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
        let data: alloc::vec::Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        let whole = digest(&data);
        for cut in [1usize, 55, 56, 63, 64, 65, 127, 299] {
            let mut h = Sha1::new();
            h.update(&data[..cut]);
            h.update(&data[cut..]);
            assert_eq!(h.finalize(), whole, "cut {cut}");
        }
    }

    #[test]
    fn hmac_rfc2202() {
        assert_eq!(hex(&hmac(&[0x0b; 20], b"Hi There")), "b617318655057264e28bc0b6fb378c8ef146be00");
        assert_eq!(
            hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        // Key longer than the block size is hashed first.
        assert_eq!(
            hex(&hmac(&[0xaa; 80], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }

    #[test]
    fn pbkdf2_rfc6070_and_wpa_vector() {
        let mut out = [0u8; 20];
        pbkdf2(b"password", b"salt", 2, &mut out);
        assert_eq!(hex(&out), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");
        let mut pmk = [0u8; 32];
        pbkdf2(b"password", b"IEEE", 4096, &mut pmk);
        assert_eq!(hex(&pmk), "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e");
    }
}
