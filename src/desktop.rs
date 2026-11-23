//! The desktop picture: wallpaper, the top panel (name, clock) and the frame of the Terminal window, whose
//! inside is left to the text console. Pure: it draws into a [`Canvas`]; the kernel copies the result to
//! the screen and places the console in [`DeskLayout::term`].
//!
//! Milestone 3 (first part): one fixed window. Moving, resizing and more apps come later.

use crate::gfont;
use crate::gui::canvas::{self, rgb, Canvas, Color, Rect};
use crate::gui::theme::DARK;

/// The console's background (black), so the window body matches the text area exactly.
pub const TERM_BG: Color = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeskLayout {
    pub w: i32,
    pub h: i32,
    pub panel: Rect,
    /// Whole window including the title bar.
    pub window: Rect,
    pub title_h: i32,
    /// Inside of the window below the title bar (filled with the console colour).
    pub body: Rect,
    /// Where the console draws: the body less a small padding.
    pub term: Rect,
    /// Where the clock text is drawn in the panel (redrawn every few seconds).
    pub clock: Rect,
}

impl DeskLayout {
    pub fn new(w: usize, h: usize) -> DeskLayout {
        let (wi, hi) = (w as i32, h as i32);
        let big = w >= 1600;
        let panel_h = if big { 40 } else { 32 };
        let panel = Rect::new(0, 0, wi, panel_h);
        // The window takes most of the screen below the panel, with a margin that shows the wallpaper.
        let margin_x = (wi / 14).clamp(16, 160);
        let top = panel_h + (hi / 22).clamp(12, 60);
        let bottom_margin = (hi / 18).clamp(12, 70);
        let window = Rect::new(margin_x, top, wi - 2 * margin_x, hi - top - bottom_margin);
        let title_h = if big { 40 } else { 32 };
        let body = Rect::new(window.x + 1, window.y + title_h, window.w - 2, window.h - title_h - 10);
        let pad = if big { 12 } else { 8 };
        let term = Rect::new(body.x + pad, body.y + pad / 2, body.w - 2 * pad, body.h - pad / 2);
        let clock_w = if big { 360 } else { 260 };
        let clock = Rect::new(wi - clock_w - 12, 0, clock_w, panel_h);
        DeskLayout { w: wi, h: hi, panel, window, title_h, body, term, clock }
    }

    fn big(&self) -> bool {
        self.w >= 1600
    }

    /// Who draws the pointer at screen position (`x`, `y`): the console (over the terminal text, with the
    /// position in console coordinates) or the desktop (everywhere else).
    pub fn zone(&self, x: i32, y: i32) -> Zone {
        if self.term.contains(x, y) {
            Zone::Terminal { x: x - self.term.x, y: y - self.term.y }
        } else {
            Zone::Desktop
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Terminal { x: i32, y: i32 },
    Desktop,
}

/// Whether the desktop may paint pointer pixel (`x`, `y`): on screen and outside the terminal text area,
/// which belongs to the console.
pub fn desktop_owns(l: &DeskLayout, x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && x < l.w && y < l.h && !l.term.contains(x, y)
}

fn face(big: bool) -> &'static gfont::Face {
    if big { &gfont::SANS_24 } else { &gfont::SANS_16 }
}

/// Draws the whole desktop. The console then paints the inside of the window.
pub fn draw(c: &mut Canvas, l: &DeskLayout, clock: &str) {
    c.reset_clip();
    wallpaper(c, l);
    panel(c, l, clock);
    window_frame(c, l, "Terminal");
}

fn wallpaper(c: &mut Canvas, l: &DeskLayout) {
    let hh = l.h.max(1) as u32;
    let (top, bottom) = (DARK.bg_top, DARK.bg_bottom);
    c.fill_rows(c.bounds(), |y| canvas::mix256(top, bottom, y.max(0) as u32 * 256 / hh));
    // Two soft colour washes, like the boot screen's glow, so the desktop continues its look.
    let (w, h) = (l.w as f32, l.h as f32);
    c.glow(w * 0.18, h * 0.85, h * 0.75, DARK.accent_2, 0.16);
    c.glow(w * 0.85, h * 0.2, h * 0.8, DARK.accent, 0.18);
}

fn panel(c: &mut Canvas, l: &DeskLayout, clock: &str) {
    let p = l.panel;
    c.blend_rect(p, rgb(8, 10, 20), 0.72);
    c.fill_rect(Rect::new(0, p.bottom() - 1, p.w, 1), rgb(48, 58, 86));
    let f = face(l.big());
    let ty = p.y + (p.h - f.height as i32) / 2;
    // Small hexagon mark, then the name.
    let r = p.h as f32 * 0.26;
    let (mx, my) = (18.0 + r, p.h as f32 / 2.0);
    c.circle(mx, my, r, DARK.accent, 1.0);
    c.circle(mx, my, r * 0.45, DARK.accent_2, 1.0);
    c.text(f, (mx + r) as i32 + 10, ty, "BedrockOS", DARK.text, 1.0);
    draw_clock(c, l, clock);
}

/// Redraws only the clock area of the panel.
pub fn draw_clock(c: &mut Canvas, l: &DeskLayout, clock: &str) {
    let r = l.clock;
    c.set_clip(r);
    // The panel background under the clock: wallpaper, then the same dark wash as `panel`.
    let hh = l.h.max(1) as u32;
    let (top, bottom) = (DARK.bg_top, DARK.bg_bottom);
    c.fill_rows(r, |y| canvas::mix256(top, bottom, y.max(0) as u32 * 256 / hh));
    let (w, h) = (l.w as f32, l.h as f32);
    c.glow(w * 0.85, h * 0.2, h * 0.8, DARK.accent, 0.18);
    c.blend_rect(r, rgb(8, 10, 20), 0.72);
    c.fill_rect(Rect::new(r.x, l.panel.bottom() - 1, r.w, 1), rgb(48, 58, 86));
    let f = face(l.big());
    let tw = canvas::text_width(f, clock, 0);
    c.text(f, r.right() - tw - 8, r.y + (l.panel.h - f.height as i32) / 2, clock, DARK.text, 1.0);
    c.reset_clip();
}

fn window_frame(c: &mut Canvas, l: &DeskLayout, title: &str) {
    let w = l.window;
    let (x, y, ww, wh) = (w.x as f32, w.y as f32, w.w as f32, w.h as f32);
    c.shadow(x, y + 8.0, ww, wh, 12.0, 30.0, 0, 0.6);
    c.round_rect(x, y, ww, wh, 12.0, rgb(30, 36, 56), 1.0);
    // Title bar: slightly lighter, with the three window dots and the title centred.
    c.round_rect_vgrad(x, y, ww, l.title_h as f32 + 12.0, 12.0, rgb(44, 52, 78), rgb(34, 41, 62), 1.0);
    c.fill_rect(Rect::new(w.x, w.y + l.title_h - 1, w.w, 1), rgb(58, 68, 96));
    let cy = y + l.title_h as f32 / 2.0;
    let dr = if l.big() { 6.5 } else { 5.5 };
    for (i, col) in [DARK.bad, DARK.warn, DARK.good].into_iter().enumerate() {
        c.circle(x + 20.0 + i as f32 * dr * 3.2, cy, dr, col, 1.0);
    }
    let f = face(l.big());
    let tw = canvas::text_width(f, title, 0);
    c.text(f, w.x + (w.w - tw) / 2, w.y + (l.title_h - f.height as i32) / 2, title, DARK.text, 0.9);
    // Body: the console's own background, square at the top, so the text area lines up with it.
    c.fill_rect(l.body, TERM_BG);
    c.round_rect_stroke(x + 0.5, y + 0.5, ww - 1.0, wh - 1.0, 12.0, 1.0, rgb(70, 82, 116), 0.8);
}

/// Formats the panel clock, such as `Mon 23 Nov  14:05 UTC` (the hardware clock is read as UTC; there is no
/// time zone setting yet).
pub fn clock_text<'a>(buf: &'a mut [u8; 28], weekday: &str, day: u8, month: &str, hour: u8, min: u8) -> &'a str {
    let mut n = 0;
    let mut put = |b: &[u8]| {
        for &x in b {
            if n < buf.len() {
                buf[n] = x;
                n += 1;
            }
        }
    };
    put(weekday.as_bytes().get(..3).unwrap_or(weekday.as_bytes()));
    put(b" ");
    if day >= 10 {
        put(&[b'0' + day / 10]);
    }
    put(&[b'0' + day % 10]);
    put(b" ");
    put(month.as_bytes().get(..3).unwrap_or(month.as_bytes()));
    put(b"  ");
    put(&[b'0' + hour / 10 % 10, b'0' + hour % 10, b':', b'0' + min / 10 % 10, b'0' + min % 10]);
    put(b" UTC");
    core::str::from_utf8(&buf[..n]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_fits_and_window_sits_below_the_panel() {
        for &(w, h) in &[(640, 480), (1024, 768), (1280, 720), (1920, 1080), (2560, 1600)] {
            let l = DeskLayout::new(w, h);
            let screen = Rect::new(0, 0, w as i32, h as i32);
            assert_eq!(l.window.intersect(&screen), l.window, "{w}x{h}");
            assert!(l.window.y > l.panel.bottom(), "{w}x{h}");
            assert_eq!(l.window.intersect(&l.body), l.body, "body inside the window at {w}x{h}");
            assert_eq!(l.body.intersect(&l.term), l.term, "terminal inside the body at {w}x{h}");
            assert!(l.term.w >= w as i32 / 2 && l.term.h >= h as i32 / 2, "terminal is a usable size at {w}x{h}");
            assert_eq!(l.clock.intersect(&l.panel), l.clock);
        }
    }

    #[test]
    fn desktop_draws_panel_window_and_black_terminal() {
        let (w, h) = (1024, 768);
        let mut buf = vec![0x123456u32; w * h];
        let l = DeskLayout::new(w, h);
        {
            let mut c = Canvas::new(&mut buf, w, h, w).unwrap();
            draw(&mut c, &l, "Mon 23 Nov  14:05");
        }
        let at = |x: i32, y: i32| buf[y as usize * w + x as usize];
        assert_eq!(at(l.term.x + 10, l.term.y + 10), TERM_BG, "terminal body is the console colour");
        assert_ne!(at(5, l.h - 5), 0x123456, "wallpaper drawn");
        // Title bar is lighter than the wallpaper next to the window.
        let tb = canvas::channels(at(l.window.x + l.window.w / 3, l.window.y + l.title_h / 2));
        assert!(tb.2 > 60, "title bar {tb:?}");
        // Clock text: some bright pixels in the clock box.
        let lit = (l.clock.x..l.clock.right()).filter(|&x| (0..l.panel.h).any(|y| canvas::channels(at(x, y)).0 > 180)).count();
        assert!(lit > 20, "clock text drawn ({lit} columns)");
    }

    #[test]
    fn clock_redraw_matches_full_draw() {
        let (w, h) = (1280, 720);
        let l = DeskLayout::new(w, h);
        let mut a = vec![0u32; w * h];
        let mut b = vec![0u32; w * h];
        {
            let mut c = Canvas::new(&mut a, w, h, w).unwrap();
            draw(&mut c, &l, "Mon 23 Nov  14:05");
            draw_clock(&mut c, &l, "Mon 23 Nov  14:06");
        }
        {
            let mut c = Canvas::new(&mut b, w, h, w).unwrap();
            draw(&mut c, &l, "Mon 23 Nov  14:06");
        }
        let differ = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        assert!(differ < 50, "{differ} pixels differ");
    }

    #[test]
    fn pointer_zones() {
        let l = DeskLayout::new(1024, 768);
        assert_eq!(l.zone(l.term.x, l.term.y), Zone::Terminal { x: 0, y: 0 });
        assert_eq!(l.zone(l.term.x + 5, l.term.y + 7), Zone::Terminal { x: 5, y: 7 });
        assert_eq!(l.zone(l.term.right(), l.term.y), Zone::Desktop, "right edge is outside");
        assert_eq!(l.zone(10, 10), Zone::Desktop, "panel");
        assert_eq!(l.zone(l.window.x + 30, l.window.y + 5), Zone::Desktop, "title bar");
        assert!(desktop_owns(&l, 10, 10));
        assert!(!desktop_owns(&l, l.term.x + 1, l.term.y + 1));
        assert!(!desktop_owns(&l, -1, 5) && !desktop_owns(&l, 5, 768));
    }

    #[test]
    fn clock_format() {
        let mut b = [0u8; 28];
        assert_eq!(clock_text(&mut b, "Monday", 23, "November", 14, 5), "Mon 23 Nov  14:05 UTC");
        assert_eq!(clock_text(&mut b, "Tue", 1, "Jan", 0, 0), "Tue 1 Jan  00:00 UTC");
    }

    /// `BEDROCK_PREVIEW=1 cargo test --lib --target <host> desktop::tests::preview -- --ignored`
    #[test]
    #[ignore]
    fn preview() {
        if std::env::var("BEDROCK_PREVIEW").is_err() {
            return;
        }
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/out/preview");
        std::fs::create_dir_all(dir).unwrap();
        for &(w, h) in &[(1024usize, 768usize), (1920, 1080)] {
            let l = DeskLayout::new(w, h);
            let mut buf = vec![0u32; w * h];
            {
                let mut c = Canvas::new(&mut buf, w, h, w).unwrap();
                draw(&mut c, &l, "Mon 23 Nov  14:05");
            }
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in buf {
                let (r, g, b) = canvas::channels(p);
                out.extend_from_slice(&[r, g, b]);
            }
            std::fs::write(format!("{dir}/desktop-{w}x{h}.ppm"), out).unwrap();
        }
    }
}
