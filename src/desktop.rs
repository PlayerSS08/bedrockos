//! The desktop picture: wallpaper, the top panel (name, clock) and the frame of the ChatBox window, whose
//! inside is left to the text console. Pure: it draws into a [`Canvas`]; the kernel copies the result to
//! the screen and places the console in [`DeskLayout::term`].
//!
//! Every function draws only inside the canvas clip, so the kernel can redraw any damaged rectangle by
//! drawing the whole scene clipped to it (wallpaper, panel, windows bottom to top, dock).

use crate::gfont;
use crate::gui::canvas::{self, rgb, Canvas, Color, Rect};
use crate::gui::math;
use crate::gui::theme::DARK;
use alloc::string::String;

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
    /// The dock at the bottom (the app launcher).
    pub dock: Rect,
    /// Size of one dock icon tile, in pixels.
    pub icon: i32,
    /// The middle of the panel, for short messages.
    pub message: Rect,
}

/// The apps of the dock, in order, and the milestone that builds each one (`None` = available now).
pub const APPS: [(&str, Option<u8>); 10] = [
    ("Notes", None),
    ("Tasks", None),
    ("Files", None),
    ("ChatBox", None),
    ("Network", None),
    ("Calculator", None),
    ("Clock", None),
    ("Snake", None),
    ("Settings", None),
    ("Help", None),
];

/// Indexes into [`APPS`]. ChatBox is the window holding the shell.
pub const NOTES: usize = 0;
pub const TASKS: usize = 1;
pub const CHATBOX: usize = 3;
pub const FILES: usize = 2;
pub const NETWORK: usize = 4;
pub const CALC: usize = 5;
pub const CLOCK: usize = 6;
pub const SNAKE: usize = 7;
pub const SETTINGS: usize = 8;
pub const HELP: usize = 9;

/// The title bar of a window at `win` with title-bar height `th`.
pub fn title_bar(win: Rect, th: i32) -> Rect {
    Rect::new(win.x, win.y, win.w, th)
}

/// The three title-bar buttons of a window, left to right: minimise, maximise, close.
pub fn title_buttons(win: Rect, th: i32) -> [Rect; 3] {
    let bw = th * 3 / 2;
    let right = win.right() - 1;
    [2, 1, 0].map(|k| Rect::new(right - (k + 1) * bw, win.y + 1, bw, th - 2))
}

/// The inside of an app window below its title bar.
pub fn app_body(win: Rect, th: i32) -> Rect {
    Rect::new(win.x + 1, win.y + th, win.w - 2, win.h - th - 1)
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
        // Dock: a row of icon tiles centred at the bottom, floating a little above the edge.
        let icon = if big { 56 } else { 44 };
        let gap = icon / 4;
        let n = APPS.len() as i32;
        let dock_w = n * icon + (n - 1) * gap + 2 * gap;
        let dock_h = icon + 2 * (gap * 3 / 4);
        let dock = Rect::new((wi - dock_w) / 2, hi - dock_h - gap, dock_w, dock_h);
        let window = Rect::new(margin_x, top, wi - 2 * margin_x, dock.y - gap - top);
        let title_h = if big { 40 } else { 32 };
        let body = Rect::new(window.x + 1, window.y + title_h, window.w - 2, window.h - title_h - 10);
        let pad = if big { 12 } else { 8 };
        let term = Rect::new(body.x + pad, body.y + pad / 2, body.w - 2 * pad, body.h - pad / 2);
        let clock_w = if big { 480 } else { 370 };
        let clock = Rect::new(wi - clock_w - 12, 0, clock_w, panel_h);
        let message = Rect::new(wi / 2 - wi / 8, 0, 2 * (wi / 8), panel_h);
        DeskLayout { w: wi, h: hi, panel, window, title_h, body, term, clock, dock, icon, message }
    }

    pub fn big(&self) -> bool {
        self.w >= 1600
    }

    /// Where windows may be: below the panel, above the dock.
    pub fn area(&self) -> Rect {
        Rect::new(0, self.panel.bottom(), self.w, self.dock.y - self.panel.bottom())
    }

    /// Where each app's window opens: ChatBox fills the main area, the others are smaller and cascade.
    pub fn app_rects(&self) -> [Rect; APPS.len()] {
        let mut r = [self.window; APPS.len()];
        let (w, h) = (self.window.w * 2 / 3, self.window.h * 4 / 5);
        for (k, app) in (0..APPS.len()).filter(|&a| a != CHATBOX).enumerate() {
            let k = (k % 6) as i32;
            // The calculator is a small window; the others share one size.
            let (w, h) = if app == CALC { ((w * 4 / 5).min(640), h) } else { (w, h) };
            let x = (self.window.x + self.window.w / 3 - 40 + k * 44).min(self.w - w);
            let y = (self.window.y + 16 + k * 32).min(self.dock.y - h);
            r[app] = Rect::new(x, y, w, h);
        }
        r
    }

    /// Where the console's text goes when the ChatBox window is at `win`.
    pub fn term_in(&self, win: Rect) -> Rect {
        let (il, it) = (self.term.x - self.window.x, self.term.y - self.window.y);
        let (ir, ib) = (self.window.right() - self.term.right(), self.window.bottom() - self.term.bottom());
        Rect::new(win.x + il, win.y + it, win.w - il - ir, win.h - it - ib)
    }

    /// The tile of dock icon `i`.
    pub fn dock_item(&self, i: usize) -> Rect {
        let gap = self.icon / 4;
        let x = self.dock.x + gap + i as i32 * (self.icon + gap);
        Rect::new(x, self.dock.y + (self.dock.h - self.icon) / 2, self.icon, self.icon)
    }

    /// The three title-bar buttons, left to right: minimise, maximise, close (full title-bar height).
    pub fn title_buttons(&self) -> [Rect; 3] {
        title_buttons(self.window, self.title_h)
    }

    /// Which dock icon is at (`x`, `y`), if any.
    pub fn dock_hit(&self, x: i32, y: i32) -> Option<usize> {
        (0..APPS.len()).find(|&i| self.dock_item(i).contains(x, y))
    }

    /// The title bar, where the window is grabbed to move it.
    pub fn title_bar(&self) -> Rect {
        title_bar(self.window, self.title_h)
    }

    /// The same layout with the window's top-left corner moved to (`x`, `y`), kept fully on screen and below
    /// the panel (the console inside it must stay within the framebuffer).
    pub fn moved(&self, x: i32, y: i32) -> DeskLayout {
        let x = x.clamp(0, (self.w - self.window.w).max(0));
        let y = y.clamp(self.panel.bottom(), (self.dock.y - self.window.h).max(self.panel.bottom()));
        let (dx, dy) = (x - self.window.x, y - self.window.y);
        let shift = |r: Rect| Rect::new(r.x + dx, r.y + dy, r.w, r.h);
        DeskLayout { window: shift(self.window), body: shift(self.body), term: shift(self.term), ..*self }
    }
}

fn face(big: bool) -> &'static gfont::Face {
    if big { &gfont::SANS_24 } else { &gfont::SANS_16 }
}

/// Draws the start-up desktop (ChatBox open, nothing else) in one go; previews and tests use it.
pub fn draw(c: &mut Canvas, l: &DeskLayout, clock: &str) {
    c.reset_clip();
    draw_base(c, l, clock, "");
    draw_window(c, l, l.window, CHATBOX, true);
    let mut open = [false; APPS.len()];
    open[CHATBOX] = true;
    draw_dock(c, l, &open);
}

/// Wallpaper and the top panel with its clock and message, inside the current clip.
pub fn draw_base(c: &mut Canvas, l: &DeskLayout, clock: &str, msg: &str) {
    draw_backdrop(c, l);
    draw_panel_text(c, l, clock, msg);
}

/// Wallpaper and the empty panel (no clock or message): it never changes, so the kernel draws it once and
/// copies it, which keeps redraws cheap.
pub fn draw_backdrop(c: &mut Canvas, l: &DeskLayout) {
    wallpaper(c, l);
    panel(c, l);
}

/// The clock and message text of the panel, drawn over [`draw_backdrop`].
pub fn draw_panel_text(c: &mut Canvas, l: &DeskLayout, clock: &str, msg: &str) {
    if l.panel.intersect(&c.clip()).is_empty() {
        return;
    }
    let f = face(l.big());
    let ty = l.panel.y + (l.panel.h - f.height as i32) / 2;
    let [net, bat, time] = panel_spots(l, clock);
    c.text(f, time.x, ty, clock, DARK.text, 1.0);
    status_icons(c, l, net, bat);
    if !msg.is_empty() {
        let m = l.message;
        let tw = canvas::text_width(f, msg, 0);
        c.text(f, m.x + (m.w - tw) / 2, ty, msg, DARK.warn, 1.0);
    }
}

/// Is the network (the laptop's connection, in VirtualBox) up? Set by the kernel, shown in the panel.
pub static NET_UP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The battery as the kernel last read it: 0 = unknown, otherwise `1 << 31 | on_ac << 9 | charging << 8 |
/// percent` (percent 255 = no battery).
pub static BATTERY: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn battery_word(percent: Option<u8>, charging: bool, on_ac: bool) -> u32 {
    1 << 31 | u32::from(on_ac) << 9 | u32::from(charging) << 8 | u32::from(percent.unwrap_or(255))
}

/// The percentage text shown next to the battery ("87%"), empty when unknown.
fn battery_label(buf: &mut [u8; 8]) -> &str {
    let w = BATTERY.load(core::sync::atomic::Ordering::Relaxed);
    let p = w & 0xFF;
    if w == 0 || p > 100 {
        return "";
    }
    let mut n = 0;
    let mut put = |d: u32| {
        buf[n] = b'0' + d as u8;
        n += 1;
    };
    if p >= 100 {
        put(1);
        put(0);
        put(0);
    } else {
        if p >= 10 {
            put(p / 10);
        }
        put(p % 10);
    }
    buf[n] = b'%';
    core::str::from_utf8(&buf[..=n]).unwrap_or("")
}

/// The panel's hover spots, right to left: network bars, battery (icon and percentage), clock text.
pub fn panel_spots(l: &DeskLayout, clock: &str) -> [Rect; 3] {
    let f = face(l.big());
    let u = (f.height as i32 / 6).max(2);
    let (y, h) = (l.panel.y, l.panel.h);
    let tw = canvas::text_width(f, clock, 0);
    let time = Rect::new(l.clock.right() - tw - 8, y, tw, h);
    let mut b = [0u8; 8];
    let pw = canvas::text_width(f, battery_label(&mut b), 0);
    let bat_w = u * 7 + 2 + if pw > 0 { pw + 6 } else { 0 };
    let bat = Rect::new(time.x - 16 - bat_w, y, bat_w, h);
    let net_w = 4 * u + 3 * (u / 2);
    let net = Rect::new(bat.x - 14 - net_w, y, net_w, h);
    [net, bat, time]
}

/// Wi-Fi signal bars (bright when the network is up) and the battery with its percentage (green fill
/// to the charge level; full and dim when it is not known).
fn status_icons(c: &mut Canvas, l: &DeskLayout, net: Rect, bat: Rect) {
    let f = face(l.big());
    let u = (f.height as i32 / 6).max(2);
    let cy = l.panel.y + l.panel.h / 2;
    let (bw, bh) = (u * 6, u * 3);
    let (bx, by) = (bat.x, cy - bh / 2);
    let w = BATTERY.load(core::sync::atomic::Ordering::Relaxed);
    let pct = (w & 0xFF) as i32;
    let known = w != 0 && pct <= 100;
    c.round_rect_stroke(bx as f32 + 0.5, by as f32 + 0.5, bw as f32, bh as f32, u as f32 * 0.6, 1.5, DARK.text, 0.9);
    c.fill_rect(Rect::new(bx + bw + 1, cy - u / 2 - 1, (u / 2).max(2), u + 2), DARK.text);
    let fill_w = if known { ((bw - 5) * pct / 100).max(1) } else { bw - 5 };
    let col = if !known { DARK.text_dim } else if pct <= 20 && w & 0x300 == 0 { DARK.warn } else { DARK.good };
    c.fill_rect(Rect::new(bx + 3, by + 3, fill_w, bh - 5), col);
    let mut b = [0u8; 8];
    let label = battery_label(&mut b);
    if !label.is_empty() {
        let ty = l.panel.y + (l.panel.h - f.height as i32) / 2;
        c.text(f, bx + bw + u + 6, ty, label, DARK.text, 1.0);
    }
    let up = NET_UP.load(core::sync::atomic::Ordering::Relaxed);
    let col = if up { DARK.text } else { DARK.text_dim };
    let base = cy + bh / 2 + 1;
    let mut x = net.x;
    for i in 1..=4 {
        let h = u * i + u / 2;
        c.fill_rect(Rect::new(x, base - h, u, h), if up || i == 1 { col } else { rgb(70, 78, 100) });
        x += u + u / 2;
    }
}

/// A hover tip box under the panel, right-aligned with `anchor`, holding `lines`.
pub fn tip_rect(l: &DeskLayout, anchor: Rect, lines: &[String]) -> Rect {
    let f = face(l.big());
    let w = lines.iter().map(|s| canvas::text_width(f, s, 0)).max().unwrap_or(0) + 28;
    let h = lines.len() as i32 * (f.height as i32 + 6) + 18;
    let x = (anchor.right() - w).clamp(8, (l.w - w - 8).max(8));
    Rect::new(x, l.panel.bottom() + 6, w, h)
}

pub fn draw_tip(c: &mut Canvas, l: &DeskLayout, r: Rect, lines: &[String]) {
    let f = face(l.big());
    c.shadow(r.x as f32, r.y as f32 + 4.0, r.w as f32, r.h as f32, 8.0, 14.0, 0, 0.5);
    c.round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 8.0, rgb(24, 29, 46), 1.0);
    c.round_rect_stroke(r.x as f32 + 0.5, r.y as f32 + 0.5, r.w as f32 - 1.0, r.h as f32 - 1.0, 8.0, 1.0, rgb(70, 82, 116), 1.0);
    for (i, s) in lines.iter().enumerate() {
        let col = if i == 0 { DARK.text } else { DARK.text_dim };
        c.text(f, r.x + 14, r.y + 9 + i as i32 * (f.height as i32 + 6), s, col, 1.0);
    }
}
/// The dock, with a dot under every app whose window is open.
pub fn draw_dock(c: &mut Canvas, l: &DeskLayout, open: &[bool; APPS.len()]) {
    let d = l.dock;
    if d.intersect(&c.clip()).is_empty() {
        return;
    }
    let (x, y, w, h) = (d.x as f32, d.y as f32, d.w as f32, d.h as f32);
    let r = l.icon as f32 * 0.4;
    c.shadow(x, y + 4.0, w, h, r, 18.0, 0, 0.45);
    c.round_rect(x, y, w, h, r, rgb(22, 27, 44), 0.82);
    c.round_rect_stroke(x + 0.5, y + 0.5, w - 1.0, h - 1.0, r, 1.0, rgb(70, 82, 116), 0.7);
    for (i, &is_open) in open.iter().enumerate() {
        let t = l.dock_item(i);
        icon(c, t, i);
        if is_open {
            c.circle(t.x as f32 + t.w as f32 / 2.0, (t.bottom() + (d.bottom() - t.bottom()) / 2) as f32, 2.5, DARK.text, 0.9);
        }
    }
}

/// One app icon, drawn from shapes on a coloured rounded tile.
fn icon(c: &mut Canvas, t: Rect, app: usize) {
    let (x, y, s) = (t.x as f32, t.y as f32, t.w as f32);
    let (top, bottom) = match app {
        NOTES => (rgb(255, 214, 102), rgb(240, 170, 60)),
        TASKS => (rgb(110, 220, 150), rgb(50, 170, 100)),
        FILES => (rgb(110, 176, 255), rgb(60, 110, 230)),
        CHATBOX => (rgb(70, 78, 100), rgb(36, 40, 56)),
        NETWORK => (rgb(100, 226, 214), rgb(40, 170, 170)),
        CALC => (rgb(255, 150, 90), rgb(225, 90, 50)),
        CLOCK => (rgb(90, 100, 140), rgb(40, 46, 74)),
        SNAKE => (rgb(60, 70, 60), rgb(24, 32, 26)),
        SETTINGS => (rgb(170, 176, 196), rgb(110, 118, 140)),
        _ => (rgb(196, 140, 255), rgb(130, 80, 220)),
    };
    c.round_rect_vgrad(x, y, s, s, s * 0.24, top, bottom, 1.0);
    let (cx, cy) = (x + s / 2.0, y + s / 2.0);
    let white = rgb(255, 255, 255);
    let ink = rgb(40, 44, 60);
    match app {
        NOTES => {
            // A page with ruled lines.
            c.round_rect(x + s * 0.26, y + s * 0.2, s * 0.48, s * 0.6, s * 0.06, white, 1.0);
            for k in 0..4 {
                c.round_rect(x + s * 0.33, y + s * (0.32 + 0.11 * k as f32), s * 0.34, s * 0.04, s * 0.02, rgb(200, 170, 110), 1.0);
            }
        }
        TASKS => {
            // Three rows: a checkbox dot and a bar.
            for k in 0..3 {
                let ry = y + s * (0.3 + 0.2 * k as f32);
                c.circle(x + s * 0.3, ry, s * 0.065, white, 1.0);
                c.round_rect(x + s * 0.42, ry - s * 0.035, s * 0.34, s * 0.07, s * 0.035, white, 0.9);
            }
        }
        FILES => {
            // A folder: tab and body.
            c.round_rect(x + s * 0.2, y + s * 0.28, s * 0.26, s * 0.12, s * 0.04, white, 0.85);
            c.round_rect(x + s * 0.2, y + s * 0.35, s * 0.6, s * 0.38, s * 0.06, white, 1.0);
        }
        CALC => {
            // A 3x3 keypad.
            for k in 0..9 {
                let (kx, ky) = ((k % 3) as f32, (k / 3) as f32);
                c.round_rect(x + s * (0.22 + 0.2 * kx), y + s * (0.22 + 0.2 * ky), s * 0.16, s * 0.16, s * 0.04, white, if k == 8 { 1.0 } else { 0.85 });
            }
        }
        CLOCK => {
            // A face with two hands.
            c.circle(cx, cy, s * 0.3, white, 1.0);
            c.round_rect(cx - s * 0.03, cy - s * 0.22, s * 0.06, s * 0.24, s * 0.03, ink, 1.0);
            c.round_rect(cx - s * 0.02, cy - s * 0.02, s * 0.18, s * 0.05, s * 0.025, ink, 1.0);
            c.circle(cx, cy, s * 0.04, rgb(230, 80, 80), 1.0);
        }
        SNAKE => {
            // A green snake of squares and a red apple.
            for (k, (dx, dy)) in [(0.2, 0.62), (0.32, 0.62), (0.44, 0.62), (0.44, 0.5), (0.44, 0.38), (0.56, 0.38)].iter().enumerate() {
                let col = if k == 5 { rgb(150, 245, 170) } else { rgb(80, 200, 115) };
                c.round_rect(x + s * dx, y + s * dy, s * 0.11, s * 0.11, s * 0.03, col, 1.0);
            }
            c.circle(x + s * 0.74, y + s * 0.3, s * 0.07, rgb(245, 80, 80), 1.0);
        }
        CHATBOX => {
            // A prompt: ">" and a cursor bar.
            let f = &gfont::MONO;
            c.text(f, (x + s * 0.24) as i32, (cy - f.height as f32 / 2.0) as i32, ">", rgb(110, 236, 214), 1.0);
            c.round_rect(x + s * 0.5, cy + s * 0.08, s * 0.22, s * 0.06, 0.0, white, 0.9);
        }
        NETWORK => {
            // Three arcs fanning out from a dot near the bottom.
            let by = y + s * 0.72;
            c.circle(cx, by, s * 0.06, white, 1.0);
            for k in 1..=3 {
                c.arc_from(cx, by, s * 0.16 * k as f32, s * 0.06, -0.125, 0.25, white, 1.0);
            }
        }
        SETTINGS => {
            // A gear: a ring with eight teeth.
            for k in 0..8 {
                let a = k as f32 * math::TAU / 8.0;
                c.circle(cx + math::sin(a) * s * 0.25, cy - math::cos(a) * s * 0.25, s * 0.07, ink, 1.0);
            }
            c.arc(cx, cy, s * 0.18, s * 0.12, 1.0, ink, 1.0);
        }
        _ => {
            // A question mark.
            let f = if s >= 50.0 { &gfont::SANS_24 } else { &gfont::SANS_16 };
            let tw = canvas::text_width(f, "?", 0);
            c.text(f, (cx - tw as f32 / 2.0) as i32, (cy - f.height as f32 / 2.0) as i32, "?", white, 1.0);
        }
    }
}

fn wallpaper(c: &mut Canvas, l: &DeskLayout) {
    let hh = l.h.max(1) as u32;
    let (top, bottom) = (DARK.bg_top, DARK.bg_bottom);
    c.fill_rows(c.clip(), |y| canvas::mix256(top, bottom, y.max(0) as u32 * 256 / hh));
    // Two soft colour washes, like the boot screen's glow, so the desktop continues its look.
    let (w, h) = (l.w as f32, l.h as f32);
    c.glow(w * 0.18, h * 0.85, h * 0.75, DARK.accent_2, 0.16);
    c.glow(w * 0.85, h * 0.2, h * 0.8, DARK.accent, 0.18);
}

fn panel(c: &mut Canvas, l: &DeskLayout) {
    let p = l.panel;
    if p.intersect(&c.clip()).is_empty() {
        return;
    }
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
}

/// Corner radius of the window: small (not the large rounded Mac look).
const WIN_R: f32 = 8.0;

/// How far a window's shadow reaches outside it (to redraw that area when the window moves).
pub const SHADOW: i32 = 34;

/// The window of `app` at `win`: shadow, frame, title bar with icon, name and buttons. The focused window
/// has a blue accent line and bright title; others are dimmed. ChatBox's body is filled with the console
/// colour (the kernel copies the console text over it); other apps draw their own body afterwards.
pub fn draw_window(c: &mut Canvas, l: &DeskLayout, win: Rect, app: usize, focused: bool) {
    if win.inflate(SHADOW).intersect(&c.clip()).is_empty() {
        return;
    }
    let w = win;
    let th = l.title_h;
    let (x, y, ww, wh) = (w.x as f32, w.y as f32, w.w as f32, w.h as f32);
    c.shadow(x, y + 6.0, ww, wh, WIN_R, 26.0, 0, if focused { 0.6 } else { 0.4 });
    c.round_rect(x, y, ww, wh, WIN_R, rgb(26, 30, 46), 1.0);
    // Title bar: flat, with a thin accent line on top; the app icon and name on the left.
    let bar = if focused { rgb(38, 44, 68) } else { rgb(31, 35, 52) };
    c.round_rect(x, y, ww, th as f32 + WIN_R, WIN_R, bar, 1.0);
    c.fill_rect(Rect::new(w.x + WIN_R as i32, w.y, w.w - 2 * WIN_R as i32, 2), if focused { DARK.accent } else { DARK.border });
    c.fill_rect(Rect::new(w.x, w.y + th - 1, w.w, 1), rgb(52, 60, 86));
    let is = th - 12;
    let pad = if l.big() { 14 } else { 10 };
    if app == CHATBOX {
        title_icon(c, w.x + pad, w.y + (th - is) / 2, is);
    } else {
        icon(c, Rect::new(w.x + pad, w.y + (th - is) / 2, is, is), app);
    }
    let f = face(l.big());
    let tcol = if focused { DARK.text } else { DARK.text_dim };
    c.text(f, w.x + pad + is + pad * 3 / 4, w.y + (th - f.height as i32) / 2, APPS[app].0, tcol, 0.95);
    // Minimise, maximise, close: thin line glyphs; close sits on a red square.
    let b = title_buttons(w, th);
    let g = (th / 4) as f32;
    let col = rgb(200, 206, 222);
    let close = b[2];
    // Red square, following the window's rounded top-right corner.
    let old = c.clip();
    c.set_clip(old.intersect(&close));
    let (rx, ry) = (close.x as f32 - WIN_R, close.y as f32 - 1.0);
    let red = if focused { rgb(196, 43, 28) } else { rgb(120, 46, 40) };
    c.round_rect(rx, ry, close.w as f32 + WIN_R + 1.0, close.h as f32 + WIN_R + 1.0, WIN_R, red, 1.0);
    c.set_clip(old);
    let (mx, my) = (b[0].x as f32 + b[0].w as f32 / 2.0, b[0].y as f32 + b[0].h as f32 / 2.0);
    c.fill_rect(Rect::new((mx - g / 2.0) as i32, my as i32, g as i32, 1), col);
    let (qx, qy) = (b[1].x as f32 + b[1].w as f32 / 2.0, b[1].y as f32 + b[1].h as f32 / 2.0);
    c.round_rect_stroke(qx - g / 2.0, qy - g / 2.0, g, g, 1.5, 1.0, col, 1.0);
    let (cx, cy) = (close.x + close.w / 2, close.y + close.h / 2);
    cross(c, cx, cy, th / 8, rgb(255, 255, 255));
    if app == CHATBOX {
        // Body: the console's own background, so the text area lines up with it.
        let body = Rect::new(l.body.x + w.x - l.window.x, l.body.y + w.y - l.window.y, l.body.w, l.body.h);
        c.fill_rect(body, TERM_BG);
    }
    c.round_rect_stroke(x + 0.5, y + 0.5, ww - 1.0, wh - 1.0, WIN_R, 1.0, rgb(64, 74, 104), 0.8);
}

/// An "x" centred on pixel (`cx`, `cy`) reaching `r` pixels out each way, with soft edges (integer maths:
/// the distance to each diagonal is |dx -+ dy| / sqrt 2).
fn cross(c: &mut Canvas, cx: i32, cy: i32, r: i32, col: Color) {
    for dy in -r..=r {
        for dx in -r..=r {
            let d = (dx - dy).abs().min((dx + dy).abs());
            // d = 0 on a diagonal (full), d = 1 next to it (a third, for anti-aliasing).
            let a = match d {
                0 => 256,
                1 => 80,
                _ => continue,
            };
            c.blend256(cx + dx, cy + dy, col, a);
        }
    }
}

/// The ChatBox icon, made by `tools/genicon.py` from `assets/bedrock.jpg` (empty when there is no image).
static ICON_DATA: &[u8] = include_bytes!("gui/icondata.bin");

/// The RGBA pixels of the icon that is `size` pixels square, if `data` has one.
fn icon_pixels(data: &[u8], size: i32) -> Option<&[u8]> {
    let mut o = 0;
    while o + 8 <= data.len() {
        if &data[o..o + 4] != b"BRIC" {
            return None;
        }
        let w = u16::from_le_bytes([data[o + 4], data[o + 5]]) as usize;
        let h = u16::from_le_bytes([data[o + 6], data[o + 7]]) as usize;
        let px = data.get(o + 8..o + 8 + w * h * 4)?;
        if w == h && w as i32 == size {
            return Some(px);
        }
        o += 8 + w * h * 4;
    }
    None
}

/// Draws the ChatBox icon (`s` pixels square) at (`x`, `y`): the bedrock image, or a drawn stone tile.
fn title_icon(c: &mut Canvas, x: i32, y: i32, s: i32) {
    if let Some(px) = icon_pixels(ICON_DATA, s) {
        for j in 0..s {
            for i in 0..s {
                let k = ((j * s + i) * 4) as usize;
                let a = u32::from(px[k + 3]);
                if a > 0 {
                    c.blend256(x + i, y + j, rgb(px[k], px[k + 1], px[k + 2]), (a * 257) >> 8);
                }
            }
        }
        return;
    }
    let (fx, fy, fs) = (x as f32, y as f32, s as f32);
    c.round_rect_vgrad(fx, fy, fs, fs, fs * 0.2, rgb(110, 112, 120), rgb(52, 54, 62), 1.0);
    c.round_rect(fx + fs * 0.2, fy + fs * 0.25, fs * 0.3, fs * 0.18, 1.0, rgb(150, 152, 160), 1.0);
    c.round_rect(fx + fs * 0.5, fy + fs * 0.6, fs * 0.3, fs * 0.16, 1.0, rgb(30, 32, 38), 1.0);
}

/// Formats the panel clock, such as `Mon 23 Nov  14:05 UTC` (the hardware clock is read as UTC; there is no
/// time zone setting yet).
pub fn clock_text<'a>(buf: &'a mut [u8; 28], weekday: &str, day: u8, month: &str, hour: u8, min: u8, zone: &str) -> &'a str {
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
    put(b" ");
    put(zone.as_bytes());
    core::str::from_utf8(&buf[..n]).unwrap_or("")
}

/// The panel message for a click on dock icon `app`: ChatBox is already open; the other apps say which
/// milestone brings them.
pub fn dock_message(buf: &mut [u8; 48], app: usize) -> &str {
    let Some(&(name, ms)) = APPS.get(app) else { return "" };
    let mut n = 0;
    let mut put = |b: &[u8]| {
        for &x in b {
            if n < buf.len() {
                buf[n] = x;
                n += 1;
            }
        }
    };
    put(name.as_bytes());
    match ms {
        None => put(b" is open"),
        Some(m) => {
            put(b" arrives in milestone ");
            if m >= 10 {
                put(&[b'0' + m / 10 % 10]);
            }
            put(&[b'0' + m % 10]);
        }
    }
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
    fn partial_redraw_matches_full_draw() {
        // The compositor redraws a damaged rectangle by drawing everything clipped to it; the result must
        // be the same as drawing the whole screen.
        let (w, h) = (1280, 720);
        let l = DeskLayout::new(w, h);
        let mut a = vec![0u32; w * h];
        let mut b = vec![0u32; w * h];
        {
            let mut c = Canvas::new(&mut a, w, h, w).unwrap();
            draw(&mut c, &l, "Mon 23 Nov  14:05");
            let r = Rect::new(l.window.x - 20, l.window.y - 10, 300, 120);
            c.set_clip(r);
            draw_base(&mut c, &l, "Mon 23 Nov  14:05", "");
            draw_window(&mut c, &l, l.window, CHATBOX, true);
        }
        {
            let mut c = Canvas::new(&mut b, w, h, w).unwrap();
            draw(&mut c, &l, "Mon 23 Nov  14:05");
        }
        assert!(a == b, "clipped redraw differs from the full picture");
    }

    #[test]
    fn app_windows_open_inside_the_free_area() {
        for &(w, h) in &[(1024, 768), (1920, 1080)] {
            let l = DeskLayout::new(w, h);
            let a = l.area();
            for (i, r) in l.app_rects().iter().enumerate() {
                assert_eq!(r.intersect(&a), *r, "app {i} at {w}x{h}");
                assert!(app_body(*r, l.title_h).w > 300, "usable body");
            }
            assert_eq!(l.term_in(l.window), l.term);
            let moved = Rect::new(l.window.x + 10, l.window.y + 5, l.window.w, l.window.h);
            assert_eq!(l.term_in(moved).x, l.term.x + 10);
        }
    }

    #[test]
    fn moving_the_window_keeps_it_on_screen_and_carries_the_terminal() {
        let l = DeskLayout::new(1024, 768);
        let m = l.moved(l.window.x + 40, l.window.y + 30);
        // Down is limited by the dock: the window may not cover it.
        assert_eq!((m.window.x, m.window.y), (l.window.x + 40, (l.window.y + 30).min(l.dock.y - l.window.h)));
        assert_eq!((m.term.x - m.window.x, m.term.y - m.window.y), (l.term.x - l.window.x, l.term.y - l.window.y));
        assert_eq!((m.term.w, m.term.h), (l.term.w, l.term.h), "same size");
        let far = l.moved(5000, 5000);
        assert_eq!(far.window.right(), 1024);
        assert_eq!(far.window.bottom(), l.dock.y);
        let up = l.moved(-50, -50);
        assert_eq!((up.window.x, up.window.y), (0, l.panel.bottom()), "not over the panel");
        assert!(l.title_bar().contains(l.window.x + 50, l.window.y + 5));
        assert!(!l.title_bar().contains(l.term.x + 5, l.term.y + 5));
    }

    #[test]
    fn dock_fits_below_the_window_and_hits_its_icons() {
        for &(w, h) in &[(1024, 768), (1280, 720), (1920, 1080)] {
            let l = DeskLayout::new(w, h);
            let screen = Rect::new(0, 0, w as i32, h as i32);
            assert_eq!(l.dock.intersect(&screen), l.dock, "{w}x{h}");
            assert!(l.window.bottom() <= l.dock.y, "window above the dock at {w}x{h}");
            for i in 0..APPS.len() {
                let t = l.dock_item(i);
                assert_eq!(l.dock.intersect(&t), t, "icon {i} inside the dock");
                assert_eq!(l.dock_hit(t.x + t.w / 2, t.y + t.h / 2), Some(i));
            }
            assert_eq!(l.dock_hit(l.dock.x + 1, l.dock.y + 1), None, "dock padding is not an icon");
            assert!(l.moved(l.window.x, 10_000).window.bottom() <= l.dock.y, "dragging cannot cover the dock");
            assert!(l.message.intersect(&l.clock).is_empty(), "message area clear of the clock");
        }
        assert_eq!(APPS[CHATBOX], ("ChatBox", None));
    }

    #[test]
    fn title_buttons_sit_right_in_the_title_bar() {
        for &(w, h) in &[(1024, 768), (1920, 1080)] {
            let l = DeskLayout::new(w, h);
            let [min, max, close] = l.title_buttons();
            assert_eq!(close.right(), l.window.right() - 1, "close at the right edge");
            assert_eq!(max.right(), close.x);
            assert_eq!(min.right(), max.x);
            let bar = l.title_bar();
            for b in [min, max, close] {
                assert_eq!(bar.intersect(&b), b, "inside the title bar at {w}x{h}");
            }
        }
    }

    #[test]
    fn icon_data_has_both_title_bar_sizes() {
        // Built by tools/genicon.py; empty means "no image" (placeholder drawn).
        if !ICON_DATA.is_empty() {
            for s in [20, 28] {
                assert_eq!(icon_pixels(ICON_DATA, s).map(<[u8]>::len), Some((s * s * 4) as usize), "size {s}");
            }
        }
        let mut one = b"BRIC".to_vec();
        one.extend_from_slice(&[2, 0, 2, 0]);
        one.extend_from_slice(&[9; 16]);
        assert_eq!(icon_pixels(&one, 2), Some(&[9u8; 16][..]));
        assert_eq!(icon_pixels(&one, 3), None);
        assert_eq!(icon_pixels(&one[..10], 2), None, "truncated");
        assert_eq!(icon_pixels(b"JUNKxxxx", 2), None);
    }

    #[test]
    fn dock_messages() {
        let mut b = [0u8; 48];
        assert_eq!(dock_message(&mut b, FILES), "Files is open");
        assert_eq!(dock_message(&mut b, CHATBOX), "ChatBox is open");
        assert_eq!(dock_message(&mut b, 99), "");
        // Every message fits the panel's message area at the smallest supported screen.
        let l = DeskLayout::new(1024, 768);
        for i in 0..APPS.len() {
            let m = dock_message(&mut b, i);
            assert!(canvas::text_width(face(false), m, 0) <= l.message.w, "{m}");
        }
    }

    #[test]
    fn clock_format() {
        let mut b = [0u8; 28];
        assert_eq!(clock_text(&mut b, "Monday", 23, "November", 14, 5, "UTC"), "Mon 23 Nov  14:05 UTC");
        assert_eq!(clock_text(&mut b, "Tue", 1, "Jan", 0, 0, "IST"), "Tue 1 Jan  00:00 IST");
        let mut lb = [0u8; 8];
        BATTERY.store(battery_word(Some(7), false, false), core::sync::atomic::Ordering::Relaxed);
        assert_eq!(battery_label(&mut lb), "7%");
        BATTERY.store(battery_word(Some(100), true, true), core::sync::atomic::Ordering::Relaxed);
        assert_eq!(battery_label(&mut lb), "100%");
        BATTERY.store(battery_word(None, false, true), core::sync::atomic::Ordering::Relaxed);
        assert_eq!(battery_label(&mut lb), "");
        BATTERY.store(0, core::sync::atomic::Ordering::Relaxed);
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
