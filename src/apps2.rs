//! More desktop apps: Files, Network, Calculator, Clock and Snake. Pure like [`crate::apps`]: state, keys,
//! clicks and drawing; the kernel (`bootfx`) supplies file listings, network values and the time.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::apps::{clipped, text, Effect, Fonts, BG, SELECTED, SIDEBAR};
use crate::gui::canvas::{self, rgb, Canvas, Color, Rect};
use crate::gui::math;
use crate::gui::theme::DARK;
use crate::keymap::Key;

fn row_h(f: &Fonts) -> i32 {
    f.sans.height as i32 + 12
}

// ---- Files ---------------------------------------------------------------------------------------------------

/// What the Files window asks the kernel to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesReq {
    /// List this directory (and show it).
    List(String),
    /// Delete this file.
    Delete(String),
}

/// A browser for the file system: folders first, a preview of the selected file on the right.
pub struct Files {
    pub path: String,
    /// (name, size; `None` for a folder).
    entries: Vec<(String, Option<usize>)>,
    sel: usize,
    top: usize,
    /// The first bytes of the selected file (set by the kernel).
    pub preview: Vec<u8>,
    /// Delete was pressed once on this entry; a second press deletes.
    confirm: bool,
}

impl Default for Files {
    fn default() -> Self {
        Files { path: String::from("/"), entries: Vec::new(), sel: 0, top: 0, preview: Vec::new(), confirm: false }
    }
}

/// `dir` joined with `name`.
pub fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The folder above `path` ("/" stays "/").
pub fn parent(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&path[..i]),
    }
}

impl Files {
    /// Shows `path` with `entries` (folders are sorted first, then files, each by name).
    pub fn set_listing(&mut self, path: &str, mut entries: Vec<(String, Option<usize>)>) {
        entries.sort_by(|a, b| (a.1.is_some(), &a.0).cmp(&(b.1.is_some(), &b.0)));
        if path != self.path {
            self.sel = 0;
            self.top = 0;
        }
        self.path = String::from(path);
        self.entries = entries;
        self.sel = self.sel.min(self.entries.len().saturating_sub(1));
        self.confirm = false;
    }

    /// The full path of the selected entry, when it is a file.
    pub fn selected_file(&self) -> Option<String> {
        self.entries.get(self.sel).filter(|e| e.1.is_some()).map(|e| join(&self.path, &e.0))
    }

    pub fn key(&mut self, k: Key) -> (Effect, Option<FilesReq>) {
        let n = self.entries.len();
        let was = self.sel;
        match k {
            Key::Up => self.sel = self.sel.saturating_sub(1),
            Key::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            Key::PageUp => self.sel = self.sel.saturating_sub(10),
            Key::PageDown => self.sel = (self.sel + 10).min(n.saturating_sub(1)),
            Key::Home => self.sel = 0,
            Key::End => self.sel = n.saturating_sub(1),
            Key::Enter => {
                if let Some((name, None)) = self.entries.get(self.sel) {
                    return (Effect::Redraw, Some(FilesReq::List(join(&self.path, name))));
                }
            }
            Key::Backspace => return (Effect::Redraw, Some(FilesReq::List(parent(&self.path)))),
            Key::Delete => {
                if let Some(p) = self.selected_file() {
                    if self.confirm {
                        self.confirm = false;
                        return (Effect::Changed, Some(FilesReq::Delete(p)));
                    }
                    self.confirm = true;
                    return (Effect::Redraw, None);
                }
            }
            _ => return (Effect::None, None),
        }
        if self.sel != was {
            self.confirm = false;
        }
        (Effect::Redraw, None)
    }

    fn list_rect(body: Rect) -> Rect {
        Rect::new(body.x, body.y + 40, body.w * 11 / 20, body.h - 40 - 30)
    }

    /// A click selects a row; a click on the selected folder opens it.
    pub fn click(&mut self, body: Rect, f: &Fonts, x: i32, y: i32) -> (Effect, Option<FilesReq>) {
        let lr = Self::list_rect(body);
        if !lr.contains(x, y) {
            return (Effect::None, None);
        }
        let i = self.top + ((y - lr.y - 4) / row_h(f)).max(0) as usize;
        if i >= self.entries.len() {
            return (Effect::None, None);
        }
        if i == self.sel {
            return self.key(Key::Enter);
        }
        self.sel = i;
        self.confirm = false;
        (Effect::Redraw, None)
    }

    pub fn draw(&mut self, c: &mut Canvas, body: Rect, f: &Fonts) {
        c.fill_rect(body, BG);
        // Path bar.
        c.fill_rect(Rect::new(body.x, body.y, body.w, 36), SIDEBAR);
        text(c, f.sans, body.x + 14, body.y + (36 - f.sans.height as i32) / 2, &format!("Folder  {}", self.path), DARK.text);
        let lr = Self::list_rect(body);
        let rh = row_h(f);
        let rows = ((lr.h - 8) / rh).max(1) as usize;
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + rows {
            self.top = self.sel + 1 - rows;
        }
        clipped(c, lr, |c| {
            if self.entries.is_empty() {
                text(c, f.sans, lr.x + 16, lr.y + 10, "This folder is empty", DARK.text_dim);
            }
            for (i, (name, size)) in self.entries.iter().enumerate().skip(self.top).take(rows) {
                let ry = lr.y + 4 + (i - self.top) as i32 * rh;
                if i == self.sel {
                    c.round_rect((lr.x + 6) as f32, ry as f32, (lr.w - 12) as f32, (rh - 2) as f32, 6.0, SELECTED, 1.0);
                }
                // Folder: a small tab-topped shape; file: a page.
                let (ix, iy) = (lr.x + 16, ry + rh / 2 - 7);
                match size {
                    None => {
                        c.round_rect(ix as f32, iy as f32, 7.0, 4.0, 1.5, rgb(250, 196, 80), 1.0);
                        c.round_rect(ix as f32, (iy + 3) as f32, 16.0, 11.0, 2.0, rgb(250, 196, 80), 1.0);
                    }
                    Some(_) => c.round_rect((ix + 2) as f32, iy as f32, 12.0, 15.0, 2.0, rgb(200, 210, 230), 1.0),
                }
                text(c, f.sans, lr.x + 42, ry + 6, name, DARK.text);
                let s = match size {
                    None => String::from("folder"),
                    Some(n) if *n < 10 * 1024 => format!("{n} B"),
                    Some(n) => format!("{} KiB", n / 1024),
                };
                let sw = canvas::text_width(f.sans, &s, 0);
                text(c, f.sans, lr.right() - sw - 14, ry + 6, &s, DARK.text_dim);
            }
        });
        // Preview.
        let pr = Rect::new(lr.right() + 1, lr.y, body.right() - lr.right() - 1, lr.h);
        c.fill_rect(Rect::new(pr.x - 1, pr.y, 1, pr.h), DARK.border);
        clipped(c, pr.inflate(-12), |c| {
            match self.entries.get(self.sel) {
                Some((name, Some(n))) => {
                    text(c, f.sans, pr.x + 12, pr.y + 10, name, DARK.accent);
                    text(c, f.sans, pr.x + 12, pr.y + 10 + rh, &format!("{n} bytes"), DARK.text_dim);
                    let printable = self.preview.iter().all(|&b| matches!(b, b'\n' | b'\r' | b'\t' | 32..=126));
                    let (_, ch) = f.cell;
                    let mut y = pr.y + 16 + 2 * rh;
                    if printable {
                        for line in self.preview.split(|&b| b == b'\n').take(40) {
                            text(c, f.mono, pr.x + 12, y, core::str::from_utf8(line).unwrap_or(""), rgb(210, 216, 230));
                            y += ch;
                        }
                    } else {
                        text(c, f.sans, pr.x + 12, y, "Binary file (a program or data)", DARK.text_dim);
                    }
                }
                Some((name, None)) => {
                    text(c, f.sans, pr.x + 12, pr.y + 10, name, DARK.accent);
                    text(c, f.sans, pr.x + 12, pr.y + 10 + rh, "Folder: Enter or click again to open", DARK.text_dim);
                }
                None => {}
            }
        });
        // Footer.
        let fy = body.bottom() - 26;
        c.fill_rect(Rect::new(body.x, fy - 4, body.w, 1), DARK.border);
        let hint = if self.confirm { "Press Delete again to delete this file" } else { "Enter open   Backspace up   Delete delete   Up/Down select" };
        text(c, f.sans, body.x + 14, fy, hint, if self.confirm { DARK.warn } else { DARK.text_dim });
    }
}

// ---- Network -------------------------------------------------------------------------------------------------

/// Draws lines of text; `# ` starts a heading (accent colour). Shared by Network and the kernel's pages.
pub fn draw_lines(c: &mut Canvas, area: Rect, f: &Fonts, lines: &[String]) {
    clipped(c, area, |c| {
        let mut y = area.y;
        for l in lines {
            if let Some(h) = l.strip_prefix("# ") {
                y += 6;
                text(c, f.sans, area.x, y, h, DARK.accent);
                y += f.sans.height as i32 + 8;
            } else {
                text(c, f.sans, area.x, y, l, DARK.text);
                y += f.sans.height as i32 + 6;
            }
        }
    });
}

/// The Network window: a big status badge and the connection details (lines from the kernel). Enter runs
/// `nettest` in ChatBox.
pub fn draw_network(c: &mut Canvas, body: Rect, f: &Fonts, up: bool, lines: &[String]) {
    c.fill_rect(body, BG);
    let (cx, cy) = ((body.x + 70) as f32, (body.y + 70) as f32);
    let col = if up { DARK.good } else { DARK.bad };
    c.circle(cx, cy, 40.0, col, 0.18);
    c.circle(cx, cy + 16.0, 5.0, col, 1.0);
    for k in 1..=3 {
        c.arc_from(cx, cy + 16.0, 11.0 * k as f32, 4.0, -0.125, 0.25, col, 1.0);
    }
    text(c, f.sans, body.x + 130, body.y + 46, if up { "Connected" } else { "Not connected" }, col);
    text(c, f.sans, body.x + 130, body.y + 46 + f.sans.height as i32 + 6, "through the laptop's connection (VirtualBox NAT)", DARK.text_dim);
    let area = Rect::new(body.x + 24, body.y + 130, body.w - 48, body.h - 180);
    draw_lines(c, area, f, lines);
    let fy = body.bottom() - 30;
    c.fill_rect(Rect::new(body.x, fy - 6, body.w, 1), DARK.border);
    text(c, f.sans, body.x + 16, fy, "Enter: run the full check (nettest) in ChatBox", DARK.text_dim);
}

// ---- Calculator ----------------------------------------------------------------------------------------------

const KEYS: [[&str; 4]; 5] = [["C", "(", ")", "/"], ["7", "8", "9", "*"], ["4", "5", "6", "-"], ["1", "2", "3", "+"], ["0", ".", "^", "="]];

#[derive(Default)]
pub struct Calculator {
    pub expr: String,
    pub result: String,
    /// Earlier calculations, newest last.
    pub history: Vec<(String, String)>,
}

impl Calculator {
    fn press(&mut self, k: &str) {
        match k {
            "C" => {
                self.expr.clear();
                self.result.clear();
            }
            "=" => {
                if self.expr.trim().is_empty() {
                    return;
                }
                match crate::calc::eval(&self.expr) {
                    Ok(v) => {
                        let r = crate::calc::format(v);
                        self.history.push((self.expr.clone(), r.clone()));
                        if self.history.len() > 6 {
                            self.history.remove(0);
                        }
                        self.result = r.clone();
                        self.expr = r;
                    }
                    Err(e) => self.result = format!("error: {e}"),
                }
            }
            "<" => {
                self.expr.pop();
            }
            k if self.expr.len() < 40 => self.expr.push_str(k),
            _ => {}
        }
    }

    pub fn key(&mut self, k: Key) -> Effect {
        match k {
            Key::Char(b) if b"0123456789.+-*/%^() ".contains(&b) => self.press(core::str::from_utf8(&[b]).unwrap_or("")),
            Key::Char(b'=') | Key::Enter => self.press("="),
            Key::Char(b'c') | Key::Char(b'C') | Key::Esc => self.press("C"),
            Key::Backspace => self.press("<"),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    /// The display and the key grid inside `body`.
    fn layout(body: Rect) -> (Rect, Rect) {
        let pad = 16;
        let disp = Rect::new(body.x + pad, body.y + pad, body.w * 3 / 5 - 2 * pad, 96);
        let grid = Rect::new(disp.x, disp.bottom() + 14, disp.w, body.bottom() - disp.bottom() - 14 - pad);
        (disp, grid)
    }

    fn key_rect(grid: Rect, r: usize, col: usize) -> Rect {
        let (kw, kh) = (grid.w / 4, grid.h / 5);
        Rect::new(grid.x + col as i32 * kw + 3, grid.y + r as i32 * kh + 3, kw - 6, kh - 6)
    }

    pub fn click(&mut self, body: Rect, x: i32, y: i32) -> Effect {
        let (_, grid) = Self::layout(body);
        for (r, row) in KEYS.iter().enumerate() {
            for (col, k) in row.iter().enumerate() {
                if Self::key_rect(grid, r, col).contains(x, y) {
                    self.press(k);
                    return Effect::Redraw;
                }
            }
        }
        Effect::None
    }

    pub fn draw(&self, c: &mut Canvas, body: Rect, f: &Fonts) {
        c.fill_rect(body, BG);
        let (disp, grid) = Self::layout(body);
        c.round_rect(disp.x as f32, disp.y as f32, disp.w as f32, disp.h as f32, 10.0, rgb(14, 17, 28), 1.0);
        clipped(c, disp.inflate(-10), |c| {
            let e = if self.expr.is_empty() { "0" } else { self.expr.as_str() };
            let ew = canvas::text_width(f.sans, e, 0);
            text(c, f.sans, disp.right() - 14 - ew, disp.y + 14, e, DARK.text_dim);
            let r = if self.result.is_empty() { e } else { self.result.as_str() };
            let rw = canvas::text_width(f.mono, r, 0);
            let col = if self.result.starts_with("error") { DARK.bad } else { DARK.text };
            text(c, f.mono, disp.right() - 14 - rw, disp.bottom() - f.mono.height as i32 - 14, r, col);
        });
        for (r, row) in KEYS.iter().enumerate() {
            for (col, k) in row.iter().enumerate() {
                let kr = Self::key_rect(grid, r, col);
                let bg: Color = match *k {
                    "=" => DARK.accent,
                    "C" => rgb(150, 60, 70),
                    "/" | "*" | "-" | "+" | "^" | "(" | ")" => rgb(48, 56, 84),
                    _ => rgb(36, 42, 64),
                };
                c.round_rect(kr.x as f32, kr.y as f32, kr.w as f32, kr.h as f32, 8.0, bg, 1.0);
                let tw = canvas::text_width(f.sans, k, 0);
                text(c, f.sans, kr.x + (kr.w - tw) / 2, kr.y + (kr.h - f.sans.height as i32) / 2, k, DARK.text);
            }
        }
        // History on the right.
        let hx = disp.right() + 24;
        text(c, f.sans, hx, disp.y, "History", DARK.accent);
        let rh = row_h(f);
        clipped(c, Rect::new(hx, disp.y, body.right() - hx - 12, body.h - 32), |c| {
            for (i, (e, r)) in self.history.iter().rev().enumerate() {
                let y = disp.y + rh + 6 + i as i32 * rh * 2;
                text(c, f.sans, hx, y, e, DARK.text_dim);
                text(c, f.sans, hx, y + rh - 4, &format!("= {r}"), DARK.text);
            }
            if self.history.is_empty() {
                text(c, f.sans, hx, disp.y + rh + 6, "Type or click, Enter for =", DARK.text_dim);
            }
        });
    }
}

// ---- Clock ---------------------------------------------------------------------------------------------------

/// Analog and digital clock, a few world clocks, and a stopwatch with laps.
#[derive(Default)]
pub struct Clock {
    /// Stopwatch time from earlier runs, in microseconds.
    elapsed: u64,
    /// When the stopwatch was started (uptime in microseconds), while it runs.
    since: Option<u64>,
    laps: Vec<u64>,
}

const WORLD: [(&str, i32); 4] = [("London", 0), ("New York", -300), ("Dubai", 240), ("Tokyo", 540)];

pub fn fmt_watch(us: u64) -> String {
    let cs = us / 10_000;
    format!("{:02}:{:02}.{:02}", cs / 6000, cs / 100 % 60, cs % 100)
}

impl Clock {
    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    pub fn watch(&self, now_us: u64) -> u64 {
        self.elapsed + self.since.map_or(0, |s| now_us.saturating_sub(s))
    }

    pub fn key(&mut self, k: Key, now_us: u64) -> Effect {
        match k {
            Key::Char(b' ') | Key::Enter => match self.since.take() {
                Some(s) => self.elapsed += now_us.saturating_sub(s),
                None => self.since = Some(now_us),
            },
            Key::Char(b'l') | Key::Char(b'L') if self.running() => {
                self.laps.push(self.watch(now_us));
                if self.laps.len() > 5 {
                    self.laps.remove(0);
                }
            }
            Key::Char(b'r') | Key::Char(b'R') => *self = Clock::default(),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    /// `time`: (Unix time, local offset in minutes, zone name); `now_us`: uptime (for the stopwatch).
    pub fn draw(&self, c: &mut Canvas, body: Rect, f: &Fonts, time: (i64, i32, &str), now_us: u64) {
        let (utc_secs, tz_min, zone) = time;
        c.fill_rect(body, BG);
        let t = crate::rtc::from_unix(utc_secs + i64::from(tz_min) * 60);
        // Analog face.
        let r = ((body.h - 80).min(body.w / 2 - 40) / 2).clamp(40, 150) as f32;
        let (cx, cy) = ((body.x + 30) as f32 + r, (body.y + 30) as f32 + r);
        c.circle(cx, cy, r, rgb(30, 36, 58), 1.0);
        c.arc(cx, cy, r, 3.0, 1.0, DARK.accent, 0.9);
        for k in 0..12 {
            let a = k as f32 * math::TAU / 12.0;
            let rr = if k % 3 == 0 { 5.0 } else { 3.0 };
            c.circle(cx + math::sin(a) * r * 0.85, cy - math::cos(a) * r * 0.85, rr, DARK.text_dim, 1.0);
        }
        let hand = |c: &mut Canvas, frac: f32, len: f32, w: f32, col: Color| {
            let a = frac * math::TAU;
            let n = (len * r / 3.0) as i32;
            for i in 0..=n {
                let d = len * r * i as f32 / n.max(1) as f32;
                c.circle(cx + math::sin(a) * d, cy - math::cos(a) * d, w, col, 1.0);
            }
        };
        let (h, m, s) = (t.hour as f32, t.min as f32, t.sec as f32);
        hand(c, (h % 12.0 + m / 60.0) / 12.0, 0.5, 4.0, DARK.text);
        hand(c, (m + s / 60.0) / 60.0, 0.72, 3.0, DARK.text);
        hand(c, s / 60.0, 0.8, 1.2, DARK.warn);
        c.circle(cx, cy, 5.0, DARK.accent, 1.0);
        // Digital time and date.
        let x = (cx + r) as i32 + 40;
        let mut y = body.y + 30;
        let rh = row_h(f);
        text(c, f.mono, x, y, &format!("{:02}:{:02}:{:02} {}", t.hour, t.min, t.sec, zone), DARK.text);
        y += rh + 4;
        text(c, f.sans, x, y, &format!("{} {} {} {}", t.weekday_name(), t.day, t.month_name(), t.year), DARK.text_dim);
        y += rh + 14;
        text(c, f.sans, x, y, "World clocks", DARK.accent);
        y += rh;
        for (name, off) in WORLD {
            let w = crate::rtc::from_unix(utc_secs + i64::from(off) * 60);
            text(c, f.sans, x, y, name, DARK.text);
            text(c, f.mono, x + 140, y, &format!("{:02}:{:02}", w.hour, w.min), DARK.text);
            y += rh;
        }
        y += 14;
        text(c, f.sans, x, y, "Stopwatch", DARK.accent);
        y += rh;
        let col = if self.running() { DARK.good } else { DARK.text };
        text(c, f.mono, x, y, &fmt_watch(self.watch(now_us)), col);
        y += rh;
        for (i, l) in self.laps.iter().enumerate() {
            text(c, f.sans, x, y, &format!("lap {}  {}", i + 1, fmt_watch(*l)), DARK.text_dim);
            y += rh - 2;
        }
        let fy = body.bottom() - 28;
        c.fill_rect(Rect::new(body.x, fy - 6, body.w, 1), DARK.border);
        text(c, f.sans, body.x + 16, fy, "Space start/stop   L lap   R reset", DARK.text_dim);
    }
}

// ---- Snake ---------------------------------------------------------------------------------------------------

pub const GRID: (i32, i32) = (28, 18);

/// The classic game: arrows steer, eat the food to grow, do not hit the walls or yourself.
pub struct Snake {
    body: VecDeque<(i32, i32)>,
    dir: (i32, i32),
    next: (i32, i32),
    food: (i32, i32),
    pub score: u32,
    pub best: u32,
    pub alive: bool,
    pub paused: bool,
    seed: u64,
}

impl Default for Snake {
    fn default() -> Self {
        let mut s = Snake { body: VecDeque::new(), dir: (1, 0), next: (1, 0), food: (0, 0), score: 0, best: 0, alive: true, paused: true, seed: 0x9E37_79B9 };
        s.restart();
        s
    }
}

impl Snake {
    pub fn restart(&mut self) {
        let (w, h) = GRID;
        self.body = (0..4).map(|i| (w / 2 - i, h / 2)).collect();
        self.dir = (1, 0);
        self.next = (1, 0);
        self.score = 0;
        self.alive = true;
        self.place_food();
    }

    fn rand(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    pub fn seed(&mut self, s: u64) {
        self.seed ^= s | 1;
    }

    fn place_food(&mut self) {
        let (w, h) = GRID;
        for _ in 0..1000 {
            let p = ((self.rand() % w as u64) as i32, (self.rand() % h as u64) as i32);
            if !self.body.contains(&p) {
                self.food = p;
                return;
            }
        }
    }

    /// One step. Returns true when something changed.
    pub fn tick(&mut self) -> bool {
        if !self.alive || self.paused {
            return false;
        }
        self.dir = self.next;
        let (hx, hy) = self.body[0];
        let head = (hx + self.dir.0, hy + self.dir.1);
        let (w, h) = GRID;
        let grows = head == self.food;
        let tail_moves = if grows { 0 } else { 1 };
        let hits_self = self.body.iter().take(self.body.len() - tail_moves).any(|&p| p == head);
        if head.0 < 0 || head.1 < 0 || head.0 >= w || head.1 >= h || hits_self {
            self.alive = false;
            self.best = self.best.max(self.score);
            return true;
        }
        self.body.push_front(head);
        if grows {
            self.score += 10;
            self.place_food();
        } else {
            self.body.pop_back();
        }
        true
    }

    pub fn key(&mut self, k: Key) -> Effect {
        let want = match k {
            Key::Up => (0, -1),
            Key::Down => (0, 1),
            Key::Left => (-1, 0),
            Key::Right => (1, 0),
            Key::Char(b' ') | Key::Enter | Key::Char(b'p') => {
                if !self.alive {
                    self.restart();
                    self.paused = false;
                } else {
                    self.paused = !self.paused;
                }
                return Effect::Redraw;
            }
            _ => return Effect::None,
        };
        // No turning straight back into yourself.
        if want != (-self.dir.0, -self.dir.1) {
            self.next = want;
        }
        if self.paused && self.alive {
            self.paused = false;
        }
        Effect::Redraw
    }

    pub fn draw(&self, c: &mut Canvas, body: Rect, f: &Fonts) {
        c.fill_rect(body, BG);
        let (gw, gh) = GRID;
        let cell = ((body.w - 40) / gw).min((body.h - 80) / gh).max(4);
        let (bw, bh) = (cell * gw, cell * gh);
        let ox = body.x + (body.w - bw) / 2;
        let oy = body.y + 44;
        let rh = row_h(f);
        text(c, f.sans, ox, body.y + 12, &format!("Score {}", self.score), DARK.text);
        let best = format!("Best {}", self.best.max(self.score));
        let tw = canvas::text_width(f.sans, &best, 0);
        text(c, f.sans, ox + bw - tw, body.y + 12, &best, DARK.text_dim);
        c.round_rect((ox - 3) as f32, (oy - 3) as f32, (bw + 6) as f32, (bh + 6) as f32, 6.0, rgb(14, 18, 30), 1.0);
        for (i, &(x, y)) in self.body.iter().enumerate() {
            let col = if i == 0 { rgb(140, 240, 160) } else { rgb(70, 190, 110) };
            c.round_rect((ox + x * cell + 1) as f32, (oy + y * cell + 1) as f32, (cell - 2) as f32, (cell - 2) as f32, 3.0, col, 1.0);
        }
        let (fx, fy) = self.food;
        c.circle((ox + fx * cell) as f32 + cell as f32 / 2.0, (oy + fy * cell) as f32 + cell as f32 / 2.0, cell as f32 * 0.4, rgb(250, 90, 90), 1.0);
        let msg = if !self.alive {
            Some("Game over: Space plays again")
        } else if self.paused {
            Some("Arrows to play, Space pauses")
        } else {
            None
        };
        if let Some(m) = msg {
            let mw = canvas::text_width(f.sans, m, 0) + 32;
            let r = Rect::new(ox + (bw - mw) / 2, oy + bh / 2 - rh, mw, rh * 2);
            c.round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 10.0, rgb(24, 29, 46), 0.95);
            text(c, f.sans, r.x + 16, r.y + (r.h - f.sans.height as i32) / 2, m, DARK.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_navigation_and_delete_confirm() {
        let mut f = Files::default();
        f.set_listing("/", alloc::vec![(String::from("b.txt"), Some(3)), (String::from("notes"), None), (String::from("a.txt"), Some(1))]);
        assert_eq!(f.entries[0].0, "notes", "folders first");
        assert_eq!(f.key(Key::Enter).1, Some(FilesReq::List(String::from("/notes"))));
        f.key(Key::Down);
        assert_eq!(f.selected_file().as_deref(), Some("/a.txt"));
        assert_eq!(f.key(Key::Delete).1, None, "first press asks");
        assert_eq!(f.key(Key::Delete).1, Some(FilesReq::Delete(String::from("/a.txt"))));
        f.set_listing("/notes", Vec::new());
        assert_eq!(f.key(Key::Backspace).1, Some(FilesReq::List(String::from("/"))));
        assert_eq!(parent("/a/b"), "/a");
        assert_eq!(parent("/a"), "/");
    }

    #[test]
    fn calculator_keys() {
        let mut c = Calculator::default();
        for b in b"(2+3)*4" {
            c.key(Key::Char(*b));
        }
        c.key(Key::Enter);
        assert_eq!(c.result, "20");
        assert_eq!(c.history.len(), 1);
        c.key(Key::Char(b'/'));
        c.key(Key::Char(b'0'));
        c.key(Key::Enter);
        assert!(c.result.starts_with("error"));
        c.key(Key::Esc);
        assert!(c.expr.is_empty());
        // Clicking the "7" key (row 1, column 0).
        let body = Rect::new(0, 0, 600, 500);
        let (_, grid) = Calculator::layout(body);
        let k = Calculator::key_rect(grid, 1, 0);
        c.click(body, k.x + 2, k.y + 2);
        assert_eq!(c.expr, "7");
    }

    #[test]
    fn stopwatch() {
        let mut w = Clock::default();
        w.key(Key::Char(b' '), 1_000_000);
        assert_eq!(w.watch(3_500_000), 2_500_000);
        w.key(Key::Char(b'l'), 2_000_000);
        w.key(Key::Char(b' '), 4_000_000);
        assert_eq!(w.watch(9_000_000), 3_000_000, "stopped");
        assert_eq!(fmt_watch(61_230_000), "01:01.23");
        w.key(Key::Char(b'r'), 0);
        assert_eq!(w.watch(5), 0);
    }

    #[test]
    fn snake_moves_eats_and_dies() {
        let mut s = Snake::default();
        assert!(!s.tick(), "starts paused");
        s.key(Key::Right);
        let head = s.body[0];
        s.food = (head.0 + 1, head.1);
        s.tick();
        assert_eq!(s.score, 10);
        assert_eq!(s.body.len(), 5);
        s.key(Key::Left);
        assert_eq!(s.next, (1, 0), "cannot reverse");
        for _ in 0..GRID.0 {
            s.food = (-5, -5);
            s.tick();
        }
        assert!(!s.alive, "hit the wall");
        assert_eq!(s.best, 10);
        s.key(Key::Char(b' '));
        assert!(s.alive && s.score == 0);
    }
}
