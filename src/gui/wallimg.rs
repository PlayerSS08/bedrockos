//! Background pictures: the boot screen's (`assets/mc-bg.png`) and the desktop's (`assets/mc-bg-hs.png`),
//! embedded as QOI files made by `tools/genbg.py`. Decoding and scaling need no allocator (the boot menu runs
//! before the heap exists): the caller passes the buffers. Pure, host-tested.

use super::canvas::{mix256, Color};

/// The boot screen and boot menu background.
pub static BOOT: &[u8] = include_bytes!("../../assets/mc-bg.qoi");
/// The desktop background.
pub static DESK: &[u8] = include_bytes!("../../assets/mc-bg-hs.qoi");

/// Largest picture (in pixels) the decode buffer must hold; `tools/genbg.py` keeps pictures within it.
pub const SRC_PIXELS: usize = 1 << 20;

/// Decodes a QOI picture (qoiformat.org) into `out` as 0x00RRGGBB pixels, row by row. Returns its width and
/// height, or `None` when the data is not QOI, is cut short or does not fit `out`.
pub fn qoi_decode(data: &[u8], out: &mut [u32]) -> Option<(usize, usize)> {
    if data.len() < 14 || &data[..4] != b"qoif" {
        return None;
    }
    let w = u32::from_be_bytes(data[4..8].try_into().ok()?) as usize;
    let h = u32::from_be_bytes(data[8..12].try_into().ok()?) as usize;
    let n = w.checked_mul(h)?;
    if n == 0 || n > out.len() {
        return None;
    }
    let mut index = [[0u8; 4]; 64];
    let (mut r, mut g, mut b, mut a) = (0u8, 0u8, 0u8, 255u8);
    let mut p = 14;
    let mut i = 0;
    while i < n {
        let t = *data.get(p)?;
        p += 1;
        let mut run = 1;
        match t {
            0xFE => {
                let c = data.get(p..p + 3)?;
                (r, g, b) = (c[0], c[1], c[2]);
                p += 3;
            }
            0xFF => {
                let c = data.get(p..p + 4)?;
                (r, g, b, a) = (c[0], c[1], c[2], c[3]);
                p += 4;
            }
            _ => match t >> 6 {
                0 => [r, g, b, a] = index[usize::from(t)],
                1 => {
                    r = r.wrapping_add((t >> 4) & 3).wrapping_sub(2);
                    g = g.wrapping_add((t >> 2) & 3).wrapping_sub(2);
                    b = b.wrapping_add(t & 3).wrapping_sub(2);
                }
                2 => {
                    let t2 = *data.get(p)?;
                    p += 1;
                    let dg = (t & 0x3F).wrapping_sub(32);
                    r = r.wrapping_add(dg).wrapping_add(t2 >> 4).wrapping_sub(8);
                    g = g.wrapping_add(dg);
                    b = b.wrapping_add(dg).wrapping_add(t2 & 0x0F).wrapping_sub(8);
                }
                _ => run = usize::from(t & 0x3F) + 1,
            },
        }
        let slot = (usize::from(r) * 3 + usize::from(g) * 5 + usize::from(b) * 7 + usize::from(a) * 11) % 64;
        index[slot] = [r, g, b, a];
        let px = u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
        let end = (i + run).min(n);
        out[i..end].fill(px);
        i = end;
    }
    Some((w, h))
}

/// Scales `src` (`sw` x `sh`) to cover all of `dst` (`dw` x `dh`, rows packed): the picture keeps its
/// proportions, is centred, and what sticks out is cut off. Bilinear filtering in integer maths.
/// `bright` (0..=256) darkens the result (256 = unchanged), so text over it stays readable.
pub fn cover(src: &[u32], sw: usize, sh: usize, dst: &mut [u32], dw: usize, dh: usize, bright: u32) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || src.len() < sw * sh || dst.len() < dw * dh {
        return;
    }
    // Source pixels per screen pixel (16.16 fixed point): the smaller ratio, so the picture covers the screen.
    let step = core::cmp::min(((sw as u64) << 16) / dw as u64, ((sh as u64) << 16) / dh as u64).max(1);
    let ox = (((sw as u64) << 16).saturating_sub(step * dw as u64)) / 2;
    let oy = (((sh as u64) << 16).saturating_sub(step * dh as u64)) / 2;
    let (maxx, maxy) = (((sw - 1) as u64) << 16, ((sh - 1) as u64) << 16);
    // Sample at pixel centres.
    let at = |o: u64, k: usize, max: u64| (o + step * k as u64 + step / 2).saturating_sub(1 << 15).min(max);
    for y in 0..dh {
        let fy = at(oy, y, maxy);
        let (y0, wy) = ((fy >> 16) as usize, ((fy >> 8) & 0xFF) as u32);
        let y1 = (y0 + 1).min(sh - 1);
        let (r0, r1) = (&src[y0 * sw..y0 * sw + sw], &src[y1 * sw..y1 * sw + sw]);
        let row = &mut dst[y * dw..y * dw + dw];
        for (x, d) in row.iter_mut().enumerate() {
            let fx = at(ox, x, maxx);
            let (x0, wx) = ((fx >> 16) as usize, ((fx >> 8) & 0xFF) as u32);
            let x1 = (x0 + 1).min(sw - 1);
            let top: Color = mix256(r0[x0], r0[x1], wx);
            let bottom: Color = mix256(r1[x0], r1[x1], wx);
            let c = mix256(top, bottom, wy);
            *d = if bright >= 256 { c } else { mix256(0, c, bright) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn decodes_each_qoi_op() {
        // 2x3: RGB (10,20,30), DIFF (+1,-1,0), LUMA (dg +4, dr-dg +1, db-dg -1: +5, +4, +3), INDEX of the first, RUN of 2.
        let first_slot = (10 * 3 + 20 * 5 + 30 * 7 + 255 * 11) % 64;
        let mut d = vec![b'q', b'o', b'i', b'f', 0, 0, 0, 2, 0, 0, 0, 3, 3, 0];
        d.extend([0xFE, 10, 20, 30]);
        d.push(0x40 | 3 << 4 | 1 << 2 | 2);
        d.extend([0x80 | (4 + 32), (1 + 8) << 4 | (8 - 1)]);
        d.push(first_slot as u8);
        d.push(0xC0 | 1);
        d.extend([0, 0, 0, 0, 0, 0, 0, 1]);
        let mut out = [0u32; 6];
        assert_eq!(qoi_decode(&d, &mut out), Some((2, 3)));
        let p = |r: u32, g: u32, b: u32| r << 16 | g << 8 | b;
        assert_eq!(out, [p(10, 20, 30), p(11, 19, 30), p(16, 23, 33), p(10, 20, 30), p(10, 20, 30), p(10, 20, 30)]);
        // Too small an output buffer, cut-short data, not QOI.
        assert_eq!(qoi_decode(&d, &mut [0u32; 5]), None);
        assert_eq!(qoi_decode(&d[..16], &mut out), None);
        assert_eq!(qoi_decode(b"nope", &mut out), None);
    }

    #[test]
    fn embedded_pictures_decode() {
        let mut buf = vec![0u32; SRC_PIXELS];
        assert_eq!(qoi_decode(BOOT, &mut buf), Some((1344, 768)));
        assert_ne!(buf[1344 * 400 + 600], 0);
        assert_eq!(qoi_decode(DESK, &mut buf), Some((1024, 1024)));
    }

    #[test]
    fn cover_keeps_proportions_and_fills() {
        // A 4x2 picture, left half red, right half blue, onto a 2x2 screen: the middle is kept.
        let (red, blue) = (0xFF0000, 0x0000FF);
        let src = [red, red, blue, blue, red, red, blue, blue];
        let mut dst = [0u32; 4];
        cover(&src, 4, 2, &mut dst, 2, 2, 256);
        assert_eq!(dst[0], red);
        assert_eq!(dst[1], blue);
        // Upscaling a flat colour stays that colour; `bright` darkens it.
        let mut big = [0u32; 30];
        cover(&[0x808080; 4], 2, 2, &mut big, 6, 5, 256);
        assert!(big.iter().all(|&c| c == 0x808080));
        cover(&[0x808080; 4], 2, 2, &mut big, 6, 5, 128);
        assert!(big.iter().all(|&c| c == 0x404040));
    }
}
