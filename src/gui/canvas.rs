//! A software canvas: anti-aliased drawing into a `0x00RRGGBB` pixel buffer. Pure and host-tested; the
//! hardware side only copies finished pixels to the framebuffer (converting the channel order there).
//!
//! Shapes are drawn with signed distance functions: each pixel's coverage is how far its centre lies
//! inside the shape's edge, clamped to one pixel, which gives smooth edges at any size without
//! supersampling.
//!
//! **Per-pixel work is integer only.** The UEFI target has no SSE, so floating point is emulated in software
//! (about 1 µs per operation). The public API takes `f32` positions for convenience; they are converted once
//! per shape to 24.8 fixed point (1/256 pixel), and opacity to 0..=256.

// Drawing calls take position, size, radius, colour and opacity as plain numbers on purpose.
#![allow(clippy::too_many_arguments)]

use super::math;
use crate::gfont::{self, Face};

/// `0x00RRGGBB`.
pub type Color = u32;

/// Fixed point: 1.0 pixel = 256.
const ONE: i32 = 256;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

pub fn channels(c: Color) -> (u8, u8, u8) {
    ((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// `a` to `b` with weight `t` in 0..=256 (integer; the hot path).
#[inline]
pub fn mix256(a: Color, b: Color, t: u32) -> Color {
    let t = t.min(256);
    let u = 256 - t;
    // Red and blue together, then green: two multiplies per pixel.
    let rb = ((a & 0xFF00FF) * u + (b & 0xFF00FF) * t + 0x800080) >> 8 & 0xFF00FF;
    let g = ((a & 0x00FF00) * u + (b & 0x00FF00) * t + 0x008000) >> 8 & 0x00FF00;
    rb | g
}

/// `a` to `b` at `t` in 0..=1.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    mix256(a, b, unit256(t))
}

/// Multiplies a colour's brightness by `k` (0..=1).
pub fn scale(c: Color, k: f32) -> Color {
    mix256(0, c, unit256(k))
}

/// Adds `b * k/256` to `a` with saturation (for glows), `k` in 0..=256.
#[inline]
pub fn add256(a: Color, b: Color, k: u32) -> Color {
    let ch = |s: u32| {
        let v = ((a >> s) & 0xFF) + ((((b >> s) & 0xFF) * k) >> 8);
        v.min(255) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Adds `b * k` to `a` with saturation, `k` in 0..=1.
pub fn add(a: Color, b: Color, k: f32) -> Color {
    add256(a, b, unit256(k))
}

/// 0..=1 as 0..=256.
pub fn unit256(t: f32) -> u32 {
    (math::clamp01(t) * 256.0 + 0.5) as u32
}

fn fx(v: f32) -> i32 {
    (v * ONE as f32) as i32
}

/// Integer square root (floor).
pub fn isqrt(v: u64) -> u64 {
    if v < 2 {
        return v;
    }
    // Start above the root (from the bit length), then Newton steps downward.
    let mut x = 1u64 << (64 - v.leading_zeros()).div_ceil(2);
    loop {
        let y = (x + v / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

/// Coverage 0..=256 of a pixel whose centre is `d` (1/256 px, negative inside) from an edge.
#[inline]
fn cov256(d: i32) -> u32 {
    (128 - d).clamp(0, 256) as u32
}

/// Integer smoothstep: 0..=256 as `x` goes from `e0` to `e1`.
#[inline]
fn smooth256(e0: i32, e1: i32, x: i32) -> u32 {
    if e1 <= e0 {
        return if x >= e1 { 256 } else { 0 };
    }
    let t = (((x - e0) as i64 * 256) / (e1 - e0) as i64).clamp(0, 256) as u32;
    (t * t * (768 - 2 * t)) >> 16
}

/// Signed distance (1/256 px) from `p` to a rounded box at centre `c`, half-size `h`, radius `r`, all fixed point.
#[inline]
fn sd_box_fx(px: i32, py: i32, cx: i32, cy: i32, hw: i32, hh: i32, r: i32) -> i32 {
    let r = r.min(hw).min(hh).max(0);
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    if qx > 0 && qy > 0 {
        isqrt(qx as u64 * qx as u64 + qy as u64 * qy as u64) as i32 - r
    } else {
        qx.max(qy) - r
    }
}

/// Signed distance from `(px, py)` to a box centred on `(cx, cy)` with half-size `(hw, hh)` and corner
/// radius `r`: negative inside. (Float version, for tests and one-off use.)
pub fn sd_round_box(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    sd_box_fx(fx(px), fx(py), fx(cx), fx(cy), fx(hw), fx(hh), fx(r)) as f32 / ONE as f32
}

/// Coverage of a pixel whose centre is `d` from an edge (negative = inside), with a one-pixel ramp.
pub fn coverage(d: f32) -> f32 {
    math::clamp01(0.5 - d)
}

/// Integer rectangle (may be empty or partly off-screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0), (b - y).max(0))
    }

    /// Smallest rectangle holding both (an empty one is ignored).
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// Grown by `d` on every side (shrunk for negative `d`).
    pub fn inflate(&self, d: i32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2 * d, self.h + 2 * d)
    }
}

pub struct Canvas<'a> {
    px: &'a mut [u32],
    w: usize,
    h: usize,
    stride: usize,
    clip: Rect,
}

/// Bounding box of a float rectangle grown by `pad` pixels.
fn bbox(x: f32, y: f32, w: f32, h: f32, pad: i32) -> Rect {
    let x0 = math::floor(x) as i32 - pad;
    let y0 = math::floor(y) as i32 - pad;
    let x1 = math::floor(x + w) as i32 + 1 + pad;
    let y1 = math::floor(y + h) as i32 + 1 + pad;
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

impl<'a> Canvas<'a> {
    /// A canvas over `px`, `w` x `h` pixels with `stride` pixels per row. Returns `None` if `px` is too small.
    pub fn new(px: &'a mut [u32], w: usize, h: usize, stride: usize) -> Option<Canvas<'a>> {
        if stride < w || px.len() < stride.checked_mul(h)? {
            return None;
        }
        Some(Canvas { px, w, h, stride, clip: Rect::new(0, 0, w as i32, h as i32) })
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn height(&self) -> usize {
        self.h
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w as i32, self.h as i32)
    }

    /// Restricts drawing to `r` (inside the canvas).
    pub fn set_clip(&mut self, r: Rect) {
        self.clip = r.intersect(&self.bounds());
    }

    pub fn reset_clip(&mut self) {
        self.clip = self.bounds();
    }

    pub fn clip(&self) -> Rect {
        self.clip
    }

    pub fn get(&self, x: i32, y: i32) -> Color {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return 0;
        }
        self.px[y as usize * self.stride + x as usize]
    }

    pub fn put(&mut self, x: i32, y: i32, c: Color) {
        if self.clip.contains(x, y) {
            self.px[y as usize * self.stride + x as usize] = c;
        }
    }

    /// Blends `c` over the pixel with opacity `a` in 0..=256.
    #[inline]
    pub fn blend256(&mut self, x: i32, y: i32, c: Color, a: u32) {
        if a == 0 || !self.clip.contains(x, y) {
            return;
        }
        let i = y as usize * self.stride + x as usize;
        self.px[i] = if a >= 256 { c } else { mix256(self.px[i], c, a) };
    }

    /// Blends `c` over the pixel with opacity `a` (0..=1).
    pub fn blend(&mut self, x: i32, y: i32, c: Color, a: f32) {
        self.blend256(x, y, c, unit256(a));
    }

    /// Calls `f(x, y, old) -> new` for every pixel of `r` inside the clip. `f` should use integer maths.
    pub fn shade(&mut self, r: Rect, mut f: impl FnMut(i32, i32, Color) -> Color) {
        let r = r.intersect(&self.clip);
        for y in r.y..r.bottom() {
            let row = y as usize * self.stride;
            for x in r.x..r.right() {
                let i = row + x as usize;
                self.px[i] = f(x, y, self.px[i]);
            }
        }
    }

    /// Fills each row of `r` with the colour `f(y)` (cheap backgrounds and gradients).
    pub fn fill_rows(&mut self, r: Rect, mut f: impl FnMut(i32) -> Color) {
        let r = r.intersect(&self.clip);
        for y in r.y..r.bottom() {
            let s = y as usize * self.stride + r.x as usize;
            self.px[s..s + r.w as usize].fill(f(y));
        }
    }

    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        self.fill_rows(r, |_| c);
    }

    /// Copies a `w` x `h` picture (`src`, `src_stride` pixels per row) with its top-left corner at (`x`, `y`),
    /// inside the clip only. Pixels outside `src` are left alone.
    pub fn copy_from(&mut self, x: i32, y: i32, w: i32, h: i32, src: &[u32], src_stride: usize) {
        let r = Rect::new(x, y, w, h).intersect(&self.clip);
        for yy in r.y..r.bottom() {
            let s = (yy - y) as usize * src_stride + (r.x - x) as usize;
            let Some(from) = src.get(s..s + r.w as usize) else { return };
            let d = yy as usize * self.stride + r.x as usize;
            self.px[d..d + r.w as usize].copy_from_slice(from);
        }
    }

    pub fn blend_rect(&mut self, r: Rect, c: Color, a: f32) {
        let a = unit256(a);
        self.shade(r, |_, _, old| mix256(old, c, a));
    }

    /// Multiplies every pixel of `r` by `k` (0..=1), for fades.
    pub fn dim(&mut self, r: Rect, k: f32) {
        let k = unit256(k);
        self.shade(r, |_, _, old| mix256(0, old, k));
    }

    /// Vertical gradient from `top` (row `r.y`) to `bottom` (row `r.bottom() - 1`).
    pub fn vgradient(&mut self, r: Rect, top: Color, bottom: Color) {
        let span = (r.h - 1).max(1) as u32;
        let ry = r.y;
        self.fill_rows(r, |y| mix256(top, bottom, ((y - ry) as u32 * 256 + span / 2) / span));
    }

    /// Rounded rectangle at float position, anti-aliased, with opacity `a`.
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, c: Color, a: f32) {
        self.round_rect_vgrad(x, y, w, h, radius, c, c, a);
    }

    /// A rounded rectangle filled with a vertical gradient.
    pub fn round_rect_vgrad(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, top: Color, bottom: Color, a: f32) {
        let (hw, hh, r) = (fx(w / 2.0), fx(h / 2.0), fx(radius));
        let (cx, cy) = (fx(x) + hw, fx(y) + hh);
        let (y0, hfx) = (fx(y), fx(h).max(1));
        let a = unit256(a);
        self.shade(bbox(x, y, w, h, 1), |px, py, old| {
            let (pxf, pyf) = (px * ONE + 128, py * ONE + 128);
            let k = (cov256(sd_box_fx(pxf, pyf, cx, cy, hw, hh, r)) * a) >> 8;
            if k == 0 {
                return old;
            }
            let col = if top == bottom { top } else { mix256(top, bottom, (((pyf - y0) as i64 * 256) / hfx as i64).clamp(0, 256) as u32) };
            mix256(old, col, k)
        });
    }

    /// Outline of a rounded rectangle (`width` pixels wide, centred on the edge).
    pub fn round_rect_stroke(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, c: Color, a: f32) {
        let (hw, hh, r) = (fx(w / 2.0), fx(h / 2.0), fx(radius));
        let (cx, cy) = (fx(x) + hw, fx(y) + hh);
        let half = fx(width / 2.0);
        let a = unit256(a);
        // Only the border band needs work: skip the inside of the box.
        let inner = Rect::new(
            math::floor(x + radius.max(width)) as i32 + 1,
            math::floor(y + width) as i32 + 2,
            (w - 2.0 * radius.max(width)) as i32 - 2,
            (h - 2.0 * width) as i32 - 4,
        );
        self.shade(bbox(x, y, w, h, 2), |px, py, old| {
            if inner.contains(px, py) {
                return old;
            }
            let d = sd_box_fx(px * ONE + 128, py * ONE + 128, cx, cy, hw, hh, r);
            let k = (cov256(d.abs() - half) * a) >> 8;
            if k == 0 { old } else { mix256(old, c, k) }
        });
    }

    /// Soft drop shadow under a rounded rectangle: opacity `a` at the edge fading over `blur` pixels.
    pub fn shadow(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, blur: f32, c: Color, a: f32) {
        let (hw, hh, r) = (fx(w / 2.0), fx(h / 2.0), fx(radius));
        let (cx, cy) = (fx(x) + hw, fx(y) + hh);
        let (e0, e1) = (-fx(blur * 0.25), fx(blur));
        let a = unit256(a);
        let b = blur as i32 + 2;
        // Deep inside, the shadow is fully under the box: leave it to the box itself.
        let inner = Rect::new(math::floor(x + radius + blur * 0.25) as i32 + 1, math::floor(y + radius + blur * 0.25) as i32 + 1, (w - 2.0 * (radius + blur * 0.25)) as i32 - 2, (h - 2.0 * (radius + blur * 0.25)) as i32 - 2);
        self.shade(bbox(x, y, w, h, b), |px, py, old| {
            if inner.contains(px, py) {
                return mix256(old, c, a);
            }
            let d = sd_box_fx(px * ONE + 128, py * ONE + 128, cx, cy, hw, hh, r);
            let k = ((256 - smooth256(e0, e1, d)) * a) >> 8;
            if k == 0 { old } else { mix256(old, c, k) }
        });
    }

    /// Anti-aliased filled circle.
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Color, a: f32) {
        let (cxf, cyf, rf) = (fx(cx), fx(cy), fx(r).max(1));
        let a = unit256(a);
        // Squared distances fully inside / fully outside skip the square root.
        let inner = (rf - 128).max(0) as i64;
        let outer = (rf + 128) as i64;
        let (in2, out2) = (inner * inner, outer * outer);
        self.shade(bbox(cx - r, cy - r, 2.0 * r, 2.0 * r, 1), |px, py, old| {
            let (dx, dy) = ((px * ONE + 128 - cxf) as i64, (py * ONE + 128 - cyf) as i64);
            let d2 = dx * dx + dy * dy;
            let k = if d2 <= in2 {
                a
            } else if d2 >= out2 {
                0
            } else {
                (cov256(isqrt(d2 as u64) as i32 - rf) * a) >> 8
            };
            if k == 0 { old } else { mix256(old, c, k) }
        });
    }

    /// Additive radial glow: brightness `k` at the centre falling smoothly to zero at `r`.
    pub fn glow(&mut self, cx: f32, cy: f32, r: f32, c: Color, k: f32) {
        let (cxf, cyf) = (fx(cx) as i64, fx(cy) as i64);
        let r2 = (fx(r) as i64).pow(2).max(1);
        let k = unit256(k);
        if k == 0 {
            return;
        }
        self.shade(bbox(cx - r, cy - r, 2.0 * r, 2.0 * r, 0), |px, py, old| {
            let (dx, dy) = ((px * ONE + 128) as i64 - cxf, (py * ONE + 128) as i64 - cyf);
            let d2 = dx * dx + dy * dy;
            if d2 >= r2 {
                return old;
            }
            let t = (256 - d2 * 256 / r2) as u32; // 1 - d²/r²
            add256(old, c, (((t * t) >> 8) * k) >> 8)
        });
    }

    /// A ring of radius `r` and thickness `t`, drawn from 12 o'clock clockwise over `frac` (0..=1) of the
    /// circle, anti-aliased (countdowns and spinners; small, so the angle test uses floats).
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, t: f32, frac: f32, c: Color, a: f32) {
        self.arc_from(cx, cy, r, t, 0.0, frac, c, a);
    }

    /// Like [`Canvas::arc`], starting at `start` (a fraction of the circle, clockwise from 12 o'clock; it may
    /// be negative) instead of at 12 o'clock.
    pub fn arc_from(&mut self, cx: f32, cy: f32, r: f32, t: f32, start: f32, frac: f32, c: Color, a: f32) {
        let frac = math::clamp01(frac);
        if frac <= 0.0 {
            return;
        }
        let o = r + t;
        let (cxf, cyf, rf, half) = (fx(cx), fx(cy), fx(r), fx(t / 2.0));
        let a = unit256(a);
        let end = frac * math::TAU;
        self.shade(bbox(cx - o, cy - o, 2.0 * o, 2.0 * o, 1), |px, py, old| {
            let (dx, dy) = (px * ONE + 128 - cxf, py * ONE + 128 - cyf);
            let dist = isqrt((dx as i64 * dx as i64 + dy as i64 * dy as i64) as u64) as i32;
            let k = cov256((dist - rf).abs() - half);
            if k == 0 {
                return old;
            }
            let edge = if frac >= 1.0 {
                256
            } else {
                // Angle from 12 o'clock, clockwise, in 0..TAU; soft end over about one pixel of arc length.
                let mut ang = math::atan2(dx as f32, -dy as f32) - start * math::TAU;
                while ang < 0.0 {
                    ang += math::TAU;
                }
                while ang >= math::TAU {
                    ang -= math::TAU;
                }
                unit256((end - ang) * (dist as f32 / ONE as f32) + 0.5)
            };
            let k = (((k * edge) >> 8) * a) >> 8;
            if k == 0 { old } else { mix256(old, c, k) }
        });
    }

    /// Draws `s` with `face`, the top of the line at `y`, blending over what is there. Returns the width.
    pub fn text(&mut self, face: &Face, x: i32, y: i32, s: &str, c: Color, a: f32) -> i32 {
        self.text_tracked(face, x, y, s, c, a, 0)
    }

    /// Like [`Canvas::text`] with `tracking` extra pixels between letters.
    pub fn text_tracked(&mut self, face: &Face, x: i32, y: i32, s: &str, c: Color, a: f32, tracking: i32) -> i32 {
        let a = unit256(a);
        let base = y + face.ascent as i32;
        let mut pen = x;
        let clip = self.clip;
        for ch in s.chars() {
            let g = gfont::glyph(face, ch);
            let gx = pen + g.left as i32;
            let gy = base - g.top as i32;
            let gr = Rect::new(gx, gy, g.w as i32, g.h as i32);
            if !gr.intersect(&clip).is_empty() {
                for row in 0..g.h as i32 {
                    for col in 0..g.w as i32 {
                        let cov = g.cov[(row * g.w as i32 + col) as usize] as u32;
                        if cov != 0 {
                            // 0..=255 coverage to 0..=256, times opacity.
                            self.blend256(gx + col, gy + row, c, ((cov + (cov >> 7)) * a) >> 8);
                        }
                    }
                }
            }
            pen += g.advance as i32 + tracking;
        }
        pen - x - if s.is_empty() { 0 } else { tracking }
    }
}

/// Width of `s` in `face` with `tracking` pixels between letters.
pub fn text_width(face: &Face, s: &str, tracking: i32) -> i32 {
    let n = s.chars().count() as i32;
    gfont::text_width(face, s) as i32 + tracking * (n - 1).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(buf: &mut Vec<u32>, w: usize, h: usize) -> Canvas<'_> {
        buf.resize(w * h, 0);
        Canvas::new(buf, w, h, w).unwrap()
    }

    #[test]
    fn too_small_buffer_is_refused() {
        let mut b = vec![0u32; 10];
        assert!(Canvas::new(&mut b, 4, 4, 4).is_none());
        assert!(Canvas::new(&mut b, 4, 2, 3).is_none(), "stride < width");
    }

    #[test]
    fn isqrt_is_exact_floor() {
        for v in (0u64..5000).chain([u32::MAX as u64, 1 << 40, (1 << 40) + 12345, u64::MAX / 3]) {
            let r = isqrt(v);
            assert!(r * r <= v, "{v}");
            assert!((r + 1).checked_mul(r + 1).is_none_or(|s| s > v), "{v}");
        }
    }

    #[test]
    fn rect_math() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
        assert!(a.intersect(&Rect::new(20, 20, 2, 2)).is_empty());
        assert_eq!(Rect::default().union(&a), a);
        assert_eq!(a.inflate(2), Rect::new(-2, -2, 14, 14));
    }

    #[test]
    fn mix_endpoints_and_rounding() {
        assert_eq!(mix(rgb(0, 0, 0), rgb(255, 255, 255), 0.0), 0);
        assert_eq!(mix(rgb(0, 0, 0), rgb(255, 255, 255), 1.0), 0xFFFFFF);
        assert_eq!(mix(rgb(0, 0, 0), rgb(200, 100, 50), 0.5), rgb(100, 50, 25));
        assert_eq!(mix256(rgb(10, 20, 30), rgb(10, 20, 30), 77), rgb(10, 20, 30), "same colour stays put");
        assert_eq!(add(rgb(250, 0, 0), rgb(100, 0, 0), 1.0), rgb(255, 0, 0), "saturates");
        // No channel bleeds into its neighbour.
        assert_eq!(mix256(rgb(255, 0, 255), rgb(0, 255, 0), 128) & 0xFF, 128);
    }

    #[test]
    fn fill_respects_clip_and_bounds() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 8, 8);
        c.set_clip(Rect::new(2, 2, 4, 4));
        c.fill_rect(Rect::new(-5, -5, 100, 100), 0xFF);
        let lit = b.iter().filter(|&&p| p == 0xFF).count();
        assert_eq!(lit, 16);
    }

    #[test]
    fn round_rect_is_solid_inside_and_soft_at_corners() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 40, 40);
        c.round_rect(5.0, 5.0, 30.0, 30.0, 8.0, 0xFFFFFF, 1.0);
        assert_eq!(c.get(20, 20), 0xFFFFFF);
        assert_eq!(c.get(5, 20), 0xFFFFFF, "straight edge pixel fully covered");
        assert_eq!(c.get(0, 0), 0);
        assert_eq!(c.get(5, 5), 0, "corner is cut by the radius");
        let partial = (5..12).any(|i| {
            let v = c.get(i, 5 + (12 - i) / 2) & 0xFF;
            v > 0 && v < 255
        });
        assert!(partial, "corner edge is anti-aliased");
    }

    #[test]
    fn circle_area_is_about_pi_r_squared() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 64, 64);
        c.circle(32.0, 32.0, 20.0, 0xFF, 1.0);
        let area: f32 = b.iter().map(|&p| (p & 0xFF) as f32 / 255.0).sum();
        let want = math::PI * 400.0;
        assert!((area - want).abs() / want < 0.01, "area {area} want {want}");
    }

    #[test]
    fn arc_fraction_controls_area() {
        let area = |frac: f32| {
            let mut b = Vec::new();
            let mut c = canvas(&mut b, 64, 64);
            c.arc(32.0, 32.0, 20.0, 4.0, frac, 0xFF, 1.0);
            b.iter().map(|&p| (p & 0xFF) as f32).sum::<f32>()
        };
        let full = area(1.0);
        let half = area(0.5);
        let quarter = area(0.25);
        assert!((half / full - 0.5).abs() < 0.03, "half {}", half / full);
        assert!((quarter / full - 0.25).abs() < 0.03, "quarter {}", quarter / full);
        assert_eq!(area(0.0), 0.0);
    }

    #[test]
    fn arc_starts_at_twelve_and_runs_clockwise() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 64, 64);
        c.arc(32.0, 32.0, 20.0, 4.0, 0.25, 0xFF, 1.0);
        assert!(c.get(40, 13) & 0xFF > 128, "upper right lit");
        assert_eq!(c.get(24, 13) & 0xFF, 0, "upper left dark");
        assert_eq!(c.get(32, 52) & 0xFF, 0, "bottom dark");
    }

    #[test]
    fn arc_from_rotates_the_start() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 64, 64);
        // From 10:30 to 1:30 (a quarter centred on 12 o'clock): the top is lit, the right side is not.
        c.arc_from(32.0, 32.0, 20.0, 4.0, -0.125, 0.25, 0xFF, 1.0);
        assert!(c.get(32, 12) & 0xFF > 128, "top lit");
        assert!(c.get(25, 13) & 0xFF > 100, "left of top lit");
        assert_eq!(c.get(52, 32) & 0xFF, 0, "3 o'clock dark");
        assert_eq!(c.get(32, 52) & 0xFF, 0, "6 o'clock dark");
    }

    #[test]
    fn shadow_fades_with_distance() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 80, 80);
        c.fill_rect(c.bounds(), 0xFFFFFF);
        c.shadow(20.0, 20.0, 40.0, 40.0, 6.0, 12.0, 0, 0.5);
        let near = c.get(18, 40) & 0xFF;
        let far = c.get(10, 40) & 0xFF;
        assert!(near < far, "darker close to the box: {near} vs {far}");
        assert_eq!(c.get(0, 0) & 0xFF, 255, "untouched far away");
    }

    #[test]
    fn stroke_draws_only_the_border() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 40, 40);
        c.round_rect_stroke(5.5, 5.5, 29.0, 29.0, 6.0, 1.0, 0xFF, 1.0);
        assert_eq!(c.get(20, 20), 0, "inside untouched");
        assert!(c.get(20, 5) & 0xFF > 100, "top edge drawn");
        assert!(c.get(5, 20) & 0xFF > 100, "left edge drawn");
    }

    #[test]
    fn glow_brightens_the_centre_most() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 50, 50);
        c.glow(25.0, 25.0, 20.0, rgb(0, 0, 200), 1.0);
        assert!(c.get(25, 25) & 0xFF > c.get(35, 25) & 0xFF);
        assert_eq!(c.get(0, 0), 0);
    }

    #[test]
    fn gradient_runs_top_to_bottom() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 4, 11);
        c.vgradient(c.bounds(), rgb(0, 0, 0), rgb(0, 0, 200));
        assert_eq!(c.get(0, 0), 0);
        assert_eq!(c.get(3, 10), rgb(0, 0, 200));
        assert_eq!(c.get(1, 5), rgb(0, 0, 100));
    }

    #[test]
    fn text_draws_inside_its_measured_width() {
        let mut b = Vec::new();
        let mut c = canvas(&mut b, 200, 40);
        let w = c.text(&gfont::SANS_16, 10, 5, "Hello", 0xFFFFFF, 1.0);
        assert_eq!(w, text_width(&gfont::SANS_16, "Hello", 0));
        let lit: Vec<i32> = (0..200).filter(|&x| (0..40).any(|y| c.get(x, y) != 0)).collect();
        assert!(!lit.is_empty());
        assert!(*lit.first().unwrap() >= 9 && *lit.last().unwrap() <= 10 + w + 1);
    }

    #[test]
    fn sd_round_box_signs() {
        assert!(sd_round_box(0.0, 0.0, 0.0, 0.0, 10.0, 5.0, 2.0) < 0.0);
        assert!(sd_round_box(20.0, 0.0, 0.0, 0.0, 10.0, 5.0, 2.0) > 9.0);
        assert!((sd_round_box(10.0, 0.0, 0.0, 0.0, 10.0, 5.0, 2.0)).abs() < 1e-2);
    }

    #[test]
    fn smooth256_endpoints() {
        assert_eq!(smooth256(0, 100, -5), 0);
        assert_eq!(smooth256(0, 100, 100), 256);
        assert_eq!(smooth256(0, 100, 50), 128);
    }
}
