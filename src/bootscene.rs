//! The animated boot screen: a drifting, twinkling starfield over a night gradient, the BedrockOS mark (a
//! layered hexagon) with a pulsing glow, the wordmark, a progress bar with a live status line, and the boot
//! menu card. Pure: it draws into a [`Canvas`] and reports which rectangles changed; the kernel copies only
//! those to the screen (no flicker, little copying).
//!
//! What is shown (progress, status text, menu state) comes from the caller, which takes it from the real
//! boot stages. The scene only animates the look between those updates.
//!
//! Per-pixel work is integer only (see `gui::canvas`): floats are used once per shape or per frame.

use crate::gfont::{self, Face};
use crate::gui::canvas::{self, rgb, Canvas, Rect};
use crate::gui::math::{self, max, min, Rng};
use crate::gui::theme::DARK;

const MAX_STARS: usize = 160;
pub const MAX_DIRTY: usize = MAX_STARS + 8;
pub const MAX_MENU_ITEMS: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
struct Star {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    r: f32,
    base: f32,
    phase: f32,
    speed: f32,
    tint: f32,
}

/// The boot menu as the scene draws it.
#[derive(Clone, Copy, Debug)]
pub struct MenuView<'a> {
    pub items: &'a [&'a str],
    pub selected: usize,
    pub description: &'a str,
    pub footer: &'a str,
    /// Countdown left, 0..=1 (`None` when stopped).
    pub ring: Option<f32>,
}

/// The sign-in card as the scene draws it.
#[derive(Clone, Copy, Debug)]
pub struct LoginView<'a> {
    pub user: &'a str,
    /// The password as stars.
    pub stars: &'a str,
    /// 0 user name, 1 password.
    pub field: usize,
    pub message: &'a str,
    pub error: bool,
    /// The text cursor is lit (it blinks).
    pub caret: bool,
}

/// Everything that changes between frames.
#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    /// Milliseconds since the scene started (drives the animation only).
    pub t_ms: u32,
    /// Logo fade-in, 0..=1.
    pub intro: f32,
    /// Bar fill, 0..=1, from the real boot stages.
    pub progress: f32,
    /// Live status line under the bar.
    pub status: &'a str,
    pub show_bar: bool,
    pub menu: Option<MenuView<'a>>,
    /// The sign-in card (shown instead of the bar and the menu).
    pub login: Option<LoginView<'a>>,
    /// Small line at the bottom ("Esc: show boot messages").
    pub hint: &'a str,
    /// Whole-screen brightness for the closing fade, 1 = normal, 0 = black.
    pub fade: f32,
}

impl Default for View<'_> {
    fn default() -> Self {
        View { t_ms: 0, intro: 1.0, progress: 0.0, status: "", show_bar: true, menu: None, login: None, hint: "", fade: 1.0 }
    }
}

/// Positions of everything, derived from the screen size.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    pub logo_cx: f32,
    pub logo_cy: f32,
    pub logo_r: f32,
    pub glow_r: f32,
    /// Logo, glow and wordmark: redrawn every frame.
    pub logo_box: Rect,
    pub word_y: i32,
    pub bar: Rect,
    /// Bar, status line and percentage: redrawn every frame.
    pub bar_box: Rect,
    pub card: Rect,
    pub item_h: i32,
    pub hint_y: i32,
    pub big_text: bool,
}

impl Layout {
    pub fn new(w: usize, h: usize, menu_items: usize) -> Layout {
        let (wf, hf) = (w as f32, h as f32);
        let big_text = w >= 1600;
        // Shorter screens get a smaller mark placed higher, so the menu card still fits under it.
        let compact = h < 900;
        let logo_r = (min(wf, hf) * if compact { 0.068 } else { 0.085 }).clamp(30.0, 150.0);
        let logo_cx = wf / 2.0;
        let logo_cy = hf * if compact { 0.21 } else { 0.27 };
        let glow_r = logo_r * 2.6;
        let word_face = &gfont::WORDMARK;
        let word_y = (logo_cy + logo_r * 1.25) as i32;
        let word_h = word_face.height as i32;
        let gy0 = (logo_cy - glow_r) as i32 - 2;
        let word_w = canvas::text_width(word_face, "BEDROCKOS", word_tracking(w)) + 8;
        let box_w = ((2.0 * glow_r) as i32 + 4).max(word_w);
        let logo_box = Rect::new(w as i32 / 2 - box_w / 2, gy0, box_w, (word_y + word_h) - gy0 + 4);

        let bar_w = (wf * 0.30).clamp(240.0, 560.0) as i32;
        let bar_y = (word_y + word_h + (hf * 0.07) as i32).min(h as i32 - 90);
        let bar = Rect::new(w as i32 / 2 - bar_w / 2, bar_y, bar_w, if big_text { 8 } else { 6 });
        let line_h = text_face(big_text).height as i32;
        let bar_box = Rect::new(bar.x - 24, bar.y - 14, bar.w + 48, bar.h + 14 + 14 + line_h + 12);

        let item_h = if big_text {
            46
        } else if h < 700 {
            30
        } else {
            34
        };
        let card_w = (wf * 0.34).clamp(340.0, 620.0) as i32;
        let small_h = gfont::SANS_16.height as i32;
        let card_h = 20 + menu_items as i32 * item_h + 14 + small_h + 14 + small_h + 22;
        let card_y = (word_y + word_h + (hf * 0.045) as i32).min(h as i32 - card_h - 8).max(0);
        let card = Rect::new(w as i32 / 2 - card_w / 2, card_y, card_w, card_h);
        let hint_y = h as i32 - small_h - 18;
        Layout { w: w as i32, h: h as i32, logo_cx, logo_cy, logo_r, glow_r, logo_box, word_y, bar, bar_box, card, item_h, hint_y, big_text }
    }

    /// The menu card plus room for its shadow.
    pub fn card_box(&self) -> Rect {
        self.card.inflate(28)
    }

    /// The footer line of the card (countdown ring and text): the only part of an unchanged menu that moves.
    pub fn footer_row(&self, items: usize) -> Rect {
        let small_h = gfont::SANS_16.height as i32;
        let fy = self.card.y + 20 + items as i32 * self.item_h + 14 + small_h + 14;
        Rect::new(self.card.x + 2, fy - 2, self.card.w - 4, small_h + 4)
    }

    pub fn hint_box(&self) -> Rect {
        Rect::new(0, self.hint_y - 4, self.w, gfont::SANS_16.height as i32 + 8)
    }
}

fn word_tracking(w: usize) -> i32 {
    if w >= 1600 { 10 } else { 6 }
}

fn text_face(big: bool) -> &'static Face {
    if big { &gfont::SANS_24 } else { &gfont::SANS_16 }
}

/// Fixed list of changed rectangles for one frame.
pub struct Dirty {
    rects: [Rect; MAX_DIRTY],
    n: usize,
}

impl Default for Dirty {
    fn default() -> Self {
        Self::new()
    }
}

impl Dirty {
    pub const fn new() -> Dirty {
        Dirty { rects: [Rect::new(0, 0, 0, 0); MAX_DIRTY], n: 0 }
    }

    pub fn clear(&mut self) {
        self.n = 0;
    }

    /// Adds `r`, skipping empty ones and ones already covered by an earlier rectangle. When full, the last
    /// slot grows to hold the rest.
    pub fn add(&mut self, r: Rect) {
        if r.is_empty() || self.rects[..self.n].iter().any(|o| o.intersect(&r) == r) {
            return;
        }
        if self.n < MAX_DIRTY {
            self.rects[self.n] = r;
            self.n += 1;
        } else {
            self.rects[MAX_DIRTY - 1] = self.rects[MAX_DIRTY - 1].union(&r);
        }
    }

    pub fn as_slice(&self) -> &[Rect] {
        &self.rects[..self.n]
    }

    /// Total area, for tests and statistics.
    pub fn area(&self) -> i64 {
        self.as_slice().iter().map(|r| r.w as i64 * r.h as i64).sum()
    }
}

pub struct Scene {
    pub layout: Layout,
    stars: [Star; MAX_STARS],
    /// A background picture at screen size (rows packed), drawn instead of the gradient.
    bg: Option<&'static [u32]>,
    star_boxes: [Rect; MAX_STARS],
    n_stars: usize,
    last_t: u32,
    menu_items: usize,
    menu_was_shown: bool,
    /// Hash of the menu content last drawn; the whole card is redrawn only when it changes.
    menu_key: u64,
    hint_was: bool,
    login_was: bool,
}

/// FNV-1a over what the menu card shows, except the countdown ring.
fn menu_key(m: &MenuView) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h = (h ^ x as u64).wrapping_mul(0x100_0000_01B3);
        }
        h = (h ^ 0xFF).wrapping_mul(0x100_0000_01B3);
    };
    eat(&(m.selected as u32).to_le_bytes());
    eat(&(m.items.len() as u32).to_le_bytes());
    for it in m.items {
        eat(it.as_bytes());
    }
    eat(m.description.as_bytes());
    eat(m.footer.as_bytes());
    eat(&[m.ring.is_some() as u8]);
    h
}

impl Scene {
    pub fn new(w: usize, h: usize, seed: u64) -> Scene {
        let mut rng = Rng::new(seed ^ 0x000B_0075_CE2E);
        let mut stars = [Star::default(); MAX_STARS];
        // About one star per 13 000 pixels: 60 at 1024x768, 159 at 1920x1080.
        let n = ((w * h) / 13_000).clamp(40, MAX_STARS);
        for s in stars.iter_mut().take(n) {
            *s = Star {
                x: rng.range(0.0, w as f32),
                y: rng.range(0.0, h as f32),
                vx: rng.range(-1.5, 1.5),
                vy: rng.range(-6.0, -1.5),
                r: rng.range(0.55, 1.5),
                base: rng.range(0.25, 0.9),
                phase: rng.range(0.0, math::TAU),
                speed: rng.range(0.6, 2.2),
                tint: rng.unit(),
            };
        }
        let mut sc = Scene {
            layout: Layout::new(w, h, 6),
            stars,
            bg: None,
            star_boxes: [Rect::default(); MAX_STARS],
            n_stars: n,
            last_t: 0,
            menu_items: 6,
            menu_was_shown: false,
            menu_key: 0,
            hint_was: false,
            login_was: false,
        };
        for i in 0..n {
            sc.star_boxes[i] = sc.star_box(i);
        }
        sc
    }

    /// Re-lays out for a menu with `items` entries.
    /// Draws `px` (the screen's size, rows packed) behind everything instead of the gradient.
    pub fn set_background(&mut self, px: &'static [u32]) {
        let (w, h) = (self.layout.w as usize, self.layout.h as usize);
        if px.len() >= w * h {
            self.bg = Some(px);
        }
    }

    pub fn set_menu_items(&mut self, items: usize) {
        let items = items.min(MAX_MENU_ITEMS);
        if items != self.menu_items {
            self.menu_items = items;
            self.layout = Layout::new(self.layout.w as usize, self.layout.h as usize, items);
        }
    }

    fn star_box(&self, i: usize) -> Rect {
        let s = &self.stars[i];
        let e = (s.r * 3.0) as i32 + 2;
        Rect::new(s.x as i32 - e, s.y as i32 - e, 2 * e + 1, 2 * e + 1)
    }

    /// Advances the animation to `v.t_ms` and redraws what changed. With `full`, everything is redrawn.
    /// The changed rectangles are written to `dirty`.
    pub fn frame(&mut self, c: &mut Canvas, v: &View, full: bool, dirty: &mut Dirty) {
        dirty.clear();
        let dt = v.t_ms.saturating_sub(self.last_t).min(200) as f32 / 1000.0;
        self.last_t = v.t_ms;
        let (w, h) = (self.layout.w as f32, self.layout.h as f32);
        for i in 0..self.n_stars {
            let old = self.star_boxes[i];
            let s = &mut self.stars[i];
            s.x += s.vx * dt;
            s.y += s.vy * dt;
            let mut wrapped = false;
            if s.y < -4.0 {
                s.y += h + 8.0;
                wrapped = true;
            }
            if s.x < -4.0 {
                s.x += w + 8.0;
                wrapped = true;
            } else if s.x > w + 4.0 {
                s.x -= w + 8.0;
                wrapped = true;
            }
            let new = self.star_box(i);
            self.star_boxes[i] = new;
            if !full {
                if wrapped {
                    dirty.add(old);
                    dirty.add(new);
                } else {
                    dirty.add(old.union(&new));
                }
            }
        }
        let menu_shown = v.menu.is_some() || v.login.is_some();
        let hint_now = !v.hint.is_empty();
        if full || v.fade < 1.0 {
            dirty.clear();
            dirty.add(c.bounds());
        } else {
            dirty.add(self.layout.logo_box);
            if v.show_bar && !menu_shown {
                dirty.add(self.layout.bar_box);
            }
            if v.login.is_some() || self.login_was {
                // The card is small; redraw it every frame (the cursor blinks, the text changes).
                dirty.add(self.layout.card_box());
                if v.login.is_none() {
                    dirty.add(self.layout.bar_box);
                }
            }
            match &v.menu {
                Some(m) => {
                    let key = menu_key(m);
                    if !self.menu_was_shown || key != self.menu_key {
                        dirty.add(self.layout.card_box());
                    } else {
                        // Same menu: only the countdown ring moves.
                        dirty.add(self.layout.footer_row(m.items.len()));
                    }
                }
                None if self.menu_was_shown => {
                    dirty.add(self.layout.card_box());
                    // The bar comes back where the card was.
                    dirty.add(self.layout.bar_box);
                }
                None => {}
            }
            if hint_now || self.hint_was {
                dirty.add(self.layout.hint_box());
            }
        }
        self.menu_was_shown = v.menu.is_some();
        self.login_was = v.login.is_some();
        self.menu_key = v.menu.as_ref().map_or(0, menu_key);
        self.hint_was = hint_now;
        let screen = c.bounds();
        for i in 0..dirty.n {
            let r = dirty.rects[i].intersect(&screen);
            dirty.rects[i] = r;
            if !r.is_empty() {
                self.paint(c, v, r);
            }
        }
        c.reset_clip();
    }

    /// Paints everything that overlaps `r` (clipped to it).
    fn paint(&self, c: &mut Canvas, v: &View, r: Rect) {
        c.set_clip(r);
        let t = v.t_ms as f32 / 1000.0;
        if let Some(bg) = self.bg {
            let (w, h) = (self.layout.w, self.layout.h);
            c.copy_from(0, 0, w, h, bg, w as usize);
        } else {
            let (top, bottom) = (DARK.bg_top, DARK.bg_bottom);
            let hh = self.layout.h.max(1) as u32;
            c.fill_rows(r, |y| canvas::mix256(top, bottom, y.max(0) as u32 * 256 / hh));
        }

        // Stars.
        for i in 0..self.n_stars {
            if self.star_boxes[i].intersect(&r).is_empty() {
                continue;
            }
            let s = &self.stars[i];
            let tw = 0.6 + 0.4 * math::sin(s.phase + t * s.speed);
            let col = canvas::mix(rgb(200, 215, 255), rgb(255, 236, 210), s.tint);
            let k = s.base * tw * (0.3 + 0.7 * v.intro);
            c.circle(s.x, s.y, s.r, col, k);
            if s.r > 1.2 {
                c.glow(s.x, s.y, s.r * 3.0, col, 0.12 * k);
            }
        }

        let l = &self.layout;
        if !l.logo_box.intersect(&r).is_empty() {
            self.paint_logo(c, v, t);
        }
        if v.show_bar && v.menu.is_none() && v.login.is_none() && !l.bar_box.intersect(&r).is_empty() {
            self.paint_bar(c, v, t);
        }
        if let Some(m) = &v.menu {
            if !l.card_box().intersect(&r).is_empty() {
                self.paint_menu(c, m);
            }
        }
        if let Some(lv) = &v.login {
            if !l.card_box().intersect(&r).is_empty() {
                self.paint_login(c, lv);
            }
        }
        if !v.hint.is_empty() && !l.hint_box().intersect(&r).is_empty() {
            let f = &gfont::SANS_16;
            let tw = canvas::text_width(f, v.hint, 0);
            c.text(f, l.w / 2 - tw / 2, l.hint_y, v.hint, DARK.text_dim, 0.75 * v.intro);
        }
        if v.fade < 1.0 {
            c.dim(r, v.fade);
        }
    }

    fn paint_logo(&self, c: &mut Canvas, v: &View, t: f32) {
        let l = &self.layout;
        let (cx, cy, r) = (l.logo_cx, l.logo_cy, l.logo_r);
        let a = math::smoothstep(0.0, 1.0, v.intro);
        // Glow breathes slowly (period about 2.6 s).
        let pulse = 0.5 + 0.5 * math::sin(t * math::TAU / 2.6);
        c.glow(cx, cy, l.glow_r * (0.92 + 0.08 * pulse), DARK.accent, (0.16 + 0.10 * pulse) * a);
        c.glow(cx, cy - r * 0.2, l.glow_r * 0.55, DARK.accent_2, (0.10 + 0.06 * pulse) * a);

        // Three strata of a flat-topped hexagon, with gaps between them and a light sweep every few seconds.
        // Per pixel everything is integer, in 1/256 pixel: the hexagon distance is the larger of two
        // half-plane distances (exact along the edges, which is what anti-aliasing needs).
        let hr = (r * 0.88 * 256.0) as i32; // centre to flat side
        let h3 = hr / 3;
        let gap2 = (max(2.0, r * 0.07) * 128.0) as i32; // half the gap
        let (cxf, cyf) = ((cx * 256.0) as i32, (cy * 256.0) as i32);
        let bands = [(rgb(110, 236, 214), rgb(64, 200, 190)), (rgb(110, 176, 255), rgb(70, 130, 240)), (rgb(86, 98, 220), rgb(56, 64, 170))];
        let sweep_period = 3.2;
        // Sweep position along the diagonal, -1.5 .. 2.5 radii, in 1/256 px.
        let sweep = (((t % sweep_period) / sweep_period * 4.0 - 1.5) * r * 256.0) as i32;
        let sweep_w = ((0.35 * r * 256.0) as i32).max(1);
        let a256 = canvas::unit256(a);
        let rim_w = 640; // 2.5 px
        let bb = Rect::new((cx - r * 1.1) as i32 - 2, (cy - r * 1.1) as i32 - 2, (r * 2.2) as i32 + 5, (r * 2.2) as i32 + 5);
        c.shade(bb, |px, py, old| {
            let x = px * 256 + 128 - cxf;
            let y = py * 256 + 128 - cyf;
            let d = sd_hexagon_fx(x, y, hr);
            let mut k = (128 - d).clamp(0, 256) as u32;
            if k == 0 {
                return old;
            }
            let g1 = ((y + h3).abs() - gap2 + 128).clamp(0, 256) as u32;
            let g2 = ((y - h3).abs() - gap2 + 128).clamp(0, 256) as u32;
            k = (k * g1) >> 8;
            k = (k * g2) >> 8;
            if k == 0 {
                return old;
            }
            let band = if y < -h3 {
                0
            } else if y < h3 {
                1
            } else {
                2
            };
            let (ct, cb) = bands[band];
            // Gradient within the band, top light to bottom deeper.
            let (b0, b1) = match band {
                0 => (-hr, -h3),
                1 => (-h3, h3),
                _ => (h3, hr),
            };
            let by = (((y - b0) as i64 * 256) / (b1 - b0).max(1) as i64).clamp(0, 256) as u32;
            let mut col = canvas::mix256(ct, cb, by);
            // Light sweep along the diagonal.
            let diag = x + y * 154 / 256;
            let dist = (diag - sweep).abs();
            if dist < sweep_w {
                let sh = 256 - (dist as i64 * 256 / sweep_w as i64) as u32;
                col = canvas::add256(col, 0xFFFFFF, (((sh * sh) >> 8) * 90) >> 8);
            }
            // Bright rim along the top of the upper band.
            if band == 0 && -d < rim_w {
                col = canvas::add256(col, 0xFFFFFF, ((256 - ((-d).max(0) * 256 / rim_w) as u32) * 64) >> 8);
            }
            canvas::mix256(old, col, (k * a256) >> 8)
        });

        // Wordmark.
        let f = &gfont::WORDMARK;
        let tr = word_tracking(l.w as usize);
        let ww = canvas::text_width(f, "BEDROCKOS", tr);
        let wy = l.word_y + ((1.0 - a) * 8.0) as i32;
        c.text_tracked(f, l.w / 2 - ww / 2, wy, "BEDROCKOS", DARK.text, a, tr);
    }

    fn paint_bar(&self, c: &mut Canvas, v: &View, t: f32) {
        let l = &self.layout;
        let a = v.intro;
        let b = l.bar;
        let (x, y, w, h) = (b.x as f32, b.y as f32, b.w as f32, b.h as f32);
        c.round_rect(x, y, w, h, h / 2.0, rgb(40, 48, 74), 0.85 * a);
        let p = math::clamp01(v.progress);
        let fw = max(h, w * p);
        if p > 0.0 {
            // Filled part: teal to blue, with a shimmer running along it (integer per pixel).
            let shimmer = ((t * 0.9) % 1.0 * fw) as i32; // pixels from the left
            let a256 = canvas::unit256(a);
            let (bx, bw) = (b.x, b.w.max(1));
            let (fwf, hf) = ((fw * 256.0) as i32, (h * 256.0) as i32);
            let (cxf, cyf) = ((x * 256.0) as i32 + fwf / 2, (y * 256.0) as i32 + hf / 2);
            let sw = ((fw * 0.08) as i32).max(6);
            c.shade(Rect::new(bx, b.y, fw as i32 + 1, b.h), |px, py, old| {
                let dx = (px * 256 + 128 - cxf).abs() - (fwf / 2 - hf / 2);
                let dy = (py * 256 + 128 - cyf).abs();
                let d = if dx > 0 { canvas::isqrt((dx as i64 * dx as i64 + dy as i64 * dy as i64) as u64) as i32 } else { dy } - hf / 2;
                let k = ((128 - d).clamp(0, 256) as u32 * a256) >> 8;
                if k == 0 {
                    return old;
                }
                let mut col = canvas::mix256(DARK.accent_2, DARK.accent, ((px - bx) * 256 / bw).clamp(0, 256) as u32);
                let sd = (px - bx - shimmer).abs();
                if sd < sw {
                    col = canvas::add256(col, 0xFFFFFF, ((256 - (sd * 256 / sw) as u32) * 90) >> 8);
                }
                canvas::mix256(old, col, k)
            });
            c.glow(x + fw, y + h / 2.0, h * 3.0, DARK.accent, 0.35 * a);
        }
        let f = text_face(l.big_text);
        let ty = b.bottom() + 12;
        let mut pct = [0u8; 4];
        let pct = percent(&mut pct, (p * 100.0 + 0.5) as u32);
        let pw = canvas::text_width(f, pct, 0);
        c.text(f, b.right() - pw, ty, pct, DARK.text_dim, a);
        // The status never runs into the percentage: long lines end in "...".
        let room = b.w - pw - 16;
        let (head, dots) = fit_text(f, v.status, room);
        let sw = c.text(f, b.x, ty, head, DARK.text_dim, a);
        if dots {
            c.text(f, b.x + sw, ty, "...", DARK.text_dim, a);
        }
    }

    fn paint_login(&self, c: &mut Canvas, lv: &LoginView) {
        let l = &self.layout;
        let card = l.card;
        let (x, y, w, h) = (card.x as f32, card.y as f32, card.w as f32, card.h as f32);
        c.shadow(x, y + 6.0, w, h, 16.0, 26.0, 0, 0.55);
        c.round_rect_vgrad(x, y, w, h, 16.0, rgb(30, 36, 58), rgb(22, 27, 44), 0.92);
        c.round_rect_stroke(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 16.0, 1.0, rgb(80, 94, 132), 0.7);
        let f = text_face(l.big_text);
        let small = &gfont::SANS_16;
        let mut iy = card.y + 20;
        let title = "Sign in to BedrockOS";
        let tw = canvas::text_width(f, title, 0);
        c.text(f, card.x + (card.w - tw) / 2, iy, title, DARK.text, 1.0);
        c.text(f, card.x + (card.w - tw) / 2 + 1, iy, title, DARK.text, 1.0);
        iy += f.height as i32 + 16;
        for (i, (label, value)) in [("User name", lv.user), ("Password", lv.stars)].iter().enumerate() {
            let focused = i == lv.field;
            c.text(small, card.x + 28, iy, label, if focused { DARK.accent } else { DARK.text_dim }, 1.0);
            iy += small.height as i32 + 6;
            let b = Rect::new(card.x + 24, iy, card.w - 48, l.item_h);
            c.round_rect(b.x as f32, b.y as f32, b.w as f32, b.h as f32, 8.0, rgb(14, 17, 28), 0.95);
            let edge = if focused { DARK.accent } else { rgb(70, 80, 110) };
            c.round_rect_stroke(b.x as f32 + 0.5, b.y as f32 + 0.5, b.w as f32 - 1.0, b.h as f32 - 1.0, 8.0, if focused { 1.5 } else { 1.0 }, edge, 1.0);
            let ty = b.y + (b.h - f.height as i32) / 2;
            let shown = fit_text(f, value, b.w - 40).0;
            let vw = c.text(f, b.x + 14, ty, shown, DARK.text, 1.0);
            if focused && lv.caret {
                c.fill_rect(Rect::new(b.x + 15 + vw, b.y + 7, 2, b.h - 14), DARK.accent);
            }
            iy += l.item_h + 14;
        }
        let col = if lv.error { rgb(255, 128, 116) } else { DARK.text_dim };
        let mw = canvas::text_width(small, lv.message, 0);
        c.text(small, card.x + (card.w - mw) / 2, iy, lv.message, col, 1.0);
        iy += small.height as i32 + 10;
        let hint = "Tab switches field   Enter signs in";
        let hw = canvas::text_width(small, hint, 0);
        c.text(small, card.x + (card.w - hw) / 2, iy, hint, rgb(150, 160, 190), 0.8);
    }

    fn paint_menu(&self, c: &mut Canvas, m: &MenuView) {
        let l = &self.layout;
        let card = l.card;
        let (x, y, w, h) = (card.x as f32, card.y as f32, card.w as f32, card.h as f32);
        c.shadow(x, y + 6.0, w, h, 16.0, 26.0, 0, 0.55);
        c.round_rect_vgrad(x, y, w, h, 16.0, rgb(30, 36, 58), rgb(22, 27, 44), 0.92);
        c.round_rect_stroke(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 16.0, 1.0, rgb(80, 94, 132), 0.7);
        let f = text_face(l.big_text);
        let mut iy = card.y + 20;
        for (i, label) in m.items.iter().enumerate() {
            let sel = i == m.selected;
            if sel {
                let (px, pw) = (x + 14.0, w - 28.0);
                c.round_rect_vgrad(px, iy as f32 + 2.0, pw, l.item_h as f32 - 4.0, 10.0, rgb(78, 140, 250), rgb(56, 108, 222), 1.0);
                // Marker dot on the left.
                c.circle(px + 16.0, iy as f32 + l.item_h as f32 / 2.0, 3.5, rgb(255, 255, 255), 0.95);
            }
            let ty = iy + (l.item_h - f.height as i32) / 2;
            c.text(f, card.x + 46, ty, label, if sel { rgb(255, 255, 255) } else { DARK.text }, if sel { 1.0 } else { 0.85 });
            iy += l.item_h;
        }
        let small = &gfont::SANS_16;
        let dy = iy + 14;
        let dw = canvas::text_width(small, m.description, 0);
        c.text(small, card.x + (card.w - dw) / 2, dy, m.description, DARK.text_dim, 1.0);
        // Footer: countdown ring plus text.
        let fy = dy + small.height as i32 + 14;
        let fw = canvas::text_width(small, m.footer, 0);
        let ring_w = if m.ring.is_some() { 26 } else { 0 };
        let fx = card.x + (card.w - fw - ring_w) / 2;
        if let Some(left) = m.ring {
            let (rcx, rcy) = (fx as f32 + 8.0, fy as f32 + small.height as f32 / 2.0);
            c.arc(rcx, rcy, 7.0, 2.2, 1.0, rgb(60, 70, 100), 1.0);
            c.arc(rcx, rcy, 7.0, 2.2, left, DARK.accent_2, 1.0);
        }
        c.text(small, fx + ring_w, fy, m.footer, rgb(170, 180, 205), 1.0);
    }
}

/// Signed distance (1/256 px, negative inside) to a flat-topped hexagon with inner radius `hr` (centre to
/// flat side): the larger of the top/bottom and slanted-side half-plane distances (cos 30 deg ~ 222/256).
pub fn sd_hexagon_fx(x: i32, y: i32, hr: i32) -> i32 {
    let (ax, ay) = (x.abs(), y.abs());
    (ay - hr).max((ax * 222 + ay * 128) / 256 - hr)
}

/// The longest prefix of `s` that fits in `room` pixels, and whether it was cut (then "..." must follow,
/// and room for it is kept).
fn fit_text<'a>(f: &Face, s: &'a str, room: i32) -> (&'a str, bool) {
    if canvas::text_width(f, s, 0) <= room {
        return (s, false);
    }
    let room = room - canvas::text_width(f, "...", 0);
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let next = i + ch.len_utf8();
        if canvas::text_width(f, &s[..next], 0) > room {
            break;
        }
        end = next;
    }
    (s[..end].trim_end(), true)
}

fn percent(buf: &mut [u8; 4], v: u32) -> &str {
    let v = v.min(100);
    let n = if v >= 100 {
        buf[..3].copy_from_slice(b"100");
        3
    } else if v >= 10 {
        buf[0] = b'0' + (v / 10) as u8;
        buf[1] = b'0' + (v % 10) as u8;
        2
    } else {
        buf[0] = b'0' + v as u8;
        1
    };
    buf[n] = b'%';
    core::str::from_utf8(&buf[..n + 1]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(w: usize, h: usize, v: &View, frames: u32) -> (Vec<u32>, Dirty, Scene) {
        let mut buf = vec![0u32; w * h];
        let mut sc = Scene::new(w, h, 7);
        let mut d = Dirty::new();
        {
            let mut c = Canvas::new(&mut buf, w, h, w).unwrap();
            sc.frame(&mut c, v, true, &mut d);
            for i in 1..=frames {
                let vv = View { t_ms: v.t_ms + i * 33, ..*v };
                sc.frame(&mut c, &vv, false, &mut d);
            }
        }
        (buf, d, sc)
    }

    #[test]
    fn hexagon_sdf_signs() {
        // Flat top and bottom at |y| = r, corners on the horizontal axis at r / cos 30.
        let r = 10 * 256;
        assert!(sd_hexagon_fx(0, 0, r) < 0);
        assert!(sd_hexagon_fx(0, r, r).abs() < 4, "flat top at y = r");
        assert!(sd_hexagon_fx(0, 12 * 256, r) > 500);
        assert!(sd_hexagon_fx((r as f32 / 0.866) as i32, 0, r).abs() < 40, "corner");
    }

    #[test]
    fn layout_fits_common_screens() {
        for &(w, h) in &[(1024, 600), (1024, 768), (1280, 720), (1366, 768), (1920, 1080), (2560, 1600), (3840, 2160)] {
            let l = Layout::new(w, h, 6);
            let screen = Rect::new(0, 0, w as i32, h as i32);
            for (name, r) in [("logo", l.logo_box), ("bar", l.bar_box), ("card", l.card)] {
                assert_eq!(r.intersect(&screen), r, "{name} off screen at {w}x{h}: {r:?}");
            }
            assert!(l.card.y >= l.word_y + gfont::WORDMARK.height as i32, "card below the wordmark at {w}x{h}");
            assert!(l.bar.y > l.word_y, "bar below the wordmark at {w}x{h}");
            if h >= 720 {
                assert!(l.card.bottom() <= l.hint_y, "card above the hint at {w}x{h}");
            }
        }
    }

    #[test]
    fn full_frame_draws_logo_and_bar() {
        let v = View { progress: 0.5, status: "Starting network", ..View::default() };
        let (buf, _, sc) = run(1024, 768, &v, 0);
        let l = sc.layout;
        let at = |x: i32, y: i32| buf[y as usize * 1024 + x as usize];
        // Logo centre is the bright middle band, not the background.
        let (r, g, b) = canvas::channels(at(l.logo_cx as i32, l.logo_cy as i32));
        assert!(b > 180 && r < 200, "logo centre colour {r},{g},{b}");
        // Bar: left part filled (bright), right part track (dark).
        let fy = l.bar.y + l.bar.h / 2;
        let left = canvas::channels(at(l.bar.x + l.bar.w / 4, fy));
        let right = canvas::channels(at(l.bar.x + l.bar.w * 3 / 4, fy));
        assert!(left.1 as u32 + left.2 as u32 > right.1 as u32 + right.2 as u32 + 100, "filled {left:?} vs track {right:?}");
    }

    #[test]
    fn later_frames_only_touch_small_regions() {
        let v = View { progress: 0.3, ..View::default() };
        let (_, d, _) = run(1920, 1080, &v, 3);
        let full = 1920i64 * 1080;
        assert!(d.area() < full / 3, "dirty area {} of {}", d.area(), full);
        assert!(d.as_slice().len() > 2);
    }

    #[test]
    fn incremental_frames_match_a_full_redraw() {
        // Drawing frame by frame must give the same picture as drawing that frame from scratch.
        let (w, h) = (640, 480);
        let v0 = View { progress: 0.4, status: "Storage", t_ms: 1000, ..View::default() };
        let mut inc = vec![0u32; w * h];
        let mut sc = Scene::new(w, h, 3);
        let mut d = Dirty::new();
        {
            let mut c = Canvas::new(&mut inc, w, h, w).unwrap();
            sc.frame(&mut c, &v0, true, &mut d);
            for i in 1..=5 {
                sc.frame(&mut c, &View { t_ms: 1000 + i * 33, ..v0 }, false, &mut d);
            }
        }
        let mut full = vec![0u32; w * h];
        {
            let mut c = Canvas::new(&mut full, w, h, w).unwrap();
            sc.last_t = 1000 + 5 * 33; // no extra star movement
            sc.frame(&mut c, &View { t_ms: 1000 + 5 * 33, ..v0 }, true, &mut d);
        }
        let differ = inc.iter().zip(&full).filter(|(a, b)| a != b).count();
        assert!(differ < w * h / 2000, "{differ} pixels differ between incremental and full redraw");
    }

    #[test]
    fn menu_draws_a_card_and_highlight() {
        let items = ["Boot BedrockOS", "Recovery", "Safe mode (no network)", "Firmware setup", "Restart", "Power off"];
        let m = MenuView { items: &items, selected: 1, description: "Messages on screen.", footer: "Boot BedrockOS in 5 seconds", ring: Some(0.6) };
        let v = View { menu: Some(m), ..View::default() };
        let (buf, _, sc) = run(1024, 768, &v, 1);
        let l = sc.layout;
        let at = |x: i32, y: i32| canvas::channels(buf[y as usize * 1024 + x as usize]);
        // Selected row (index 1) is accent blue; row 3 is the dark card.
        let row = |i: i32| l.card.y + 20 + i * l.item_h + l.item_h / 2;
        let sel = at(l.card.x + l.card.w - 40, row(1));
        let other = at(l.card.x + l.card.w - 40, row(3));
        assert!(sel.2 > 180 && sel.0 < 120, "selected row {sel:?}");
        assert!(other.2 < 90, "unselected row {other:?}");
    }

    #[test]
    fn unchanged_menu_only_redraws_its_footer() {
        let items = ["Boot BedrockOS", "Recovery", "Safe mode (no network)", "Firmware setup", "Restart", "Power off"];
        let mk = |sel: usize, ring: f32| MenuView { items: &items, selected: sel, description: "d", footer: "f", ring: Some(ring) };
        let (w, h) = (1920, 1080);
        let mut buf = vec![0u32; w * h];
        let mut sc = Scene::new(w, h, 1);
        let mut d = Dirty::new();
        let mut c = Canvas::new(&mut buf, w, h, w).unwrap();
        sc.frame(&mut c, &View { menu: Some(mk(0, 0.9)), ..View::default() }, true, &mut d);
        sc.frame(&mut c, &View { t_ms: 33, menu: Some(mk(0, 0.8)), ..View::default() }, false, &mut d);
        let card = sc.layout.card_box();
        let card_area = card.w as i64 * card.h as i64;
        assert!(!d.as_slice().contains(&card), "whole card redrawn for a ring tick");
        sc.frame(&mut c, &View { t_ms: 66, menu: Some(mk(1, 0.8)), ..View::default() }, false, &mut d);
        assert!(d.as_slice().contains(&card) && d.area() >= card_area, "selection change redraws the card");
    }

    #[test]
    fn fade_darkens_everything() {
        let v = View { fade: 0.0, ..View::default() };
        let (buf, d, _) = run(320, 240, &v, 1);
        assert!(buf.iter().all(|&p| p == 0));
        assert_eq!(d.area(), 320 * 240);
    }

    /// Writes preview frames to `out/preview/` as PPM when `BEDROCK_PREVIEW` is set
    /// (`BEDROCK_PREVIEW=1 cargo test --lib --target <host> bootscene::tests::preview -- --ignored`).
    #[test]
    #[ignore]
    fn preview() {
        if std::env::var("BEDROCK_PREVIEW").is_err() {
            return;
        }
        let items = ["Boot BedrockOS", "Recovery", "Safe mode (no network)", "Firmware setup", "Restart", "Power off"];
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/out/preview");
        std::fs::create_dir_all(dir).unwrap();
        for &(w, h) in &[(1024usize, 768usize), (1920, 1080)] {
            let shots: [(&str, View); 3] = [
                ("splash", View { t_ms: 1400, progress: 0.62, status: "Network: wired and virtual adapters", hint: "Esc or V: show boot messages", ..View::default() }),
                ("intro", View { t_ms: 300, intro: 0.35, progress: 0.05, status: "Firmware hand-off", ..View::default() }),
                ("menu", View { t_ms: 2000, menu: Some(MenuView { items: &items, selected: 0, description: "Start BedrockOS normally.", footer: "Boot BedrockOS in 4 seconds", ring: Some(0.8) }), ..View::default() }),
            ];
            for (name, v) in shots {
                let (buf, _, _) = run(w, h, &v, 0);
                let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
                for p in buf {
                    let (r, g, b) = canvas::channels(p);
                    out.extend_from_slice(&[r, g, b]);
                }
                std::fs::write(format!("{dir}/{name}-{w}x{h}.ppm"), out).unwrap();
            }
        }
    }

    #[test]
    fn long_status_is_cut_to_fit() {
        let f = &gfont::SANS_16;
        let s = "Filesystem: RAM filesystem and programs";
        assert_eq!(fit_text(f, s, 10_000), (s, false));
        let (head, cut) = fit_text(f, s, 200);
        assert!(cut && s.starts_with(head) && head.len() < s.len());
        assert!(canvas::text_width(f, head, 0) + canvas::text_width(f, "...", 0) <= 200);
    }

    #[test]
    fn percent_text() {
        let mut b = [0u8; 4];
        assert_eq!(percent(&mut b, 0), "0%");
        assert_eq!(percent(&mut b, 42), "42%");
        assert_eq!(percent(&mut b, 100), "100%");
        assert_eq!(percent(&mut b, 900), "100%");
    }

    #[test]
    fn dirty_list_skips_covered_and_overflows_into_the_last_slot() {
        let mut d = Dirty::new();
        d.add(Rect::new(0, 0, 100, 100));
        d.add(Rect::new(10, 10, 5, 5));
        assert_eq!(d.as_slice().len(), 1);
        for i in 0..(MAX_DIRTY as i32 + 20) {
            d.add(Rect::new(200 + i * 3, 0, 2, 2));
        }
        assert_eq!(d.as_slice().len(), MAX_DIRTY);
        assert!(d.as_slice()[MAX_DIRTY - 1].w > 2);
    }
}
