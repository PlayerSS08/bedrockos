//! The Wi-Fi drop-down under the top bar's network icon: a click on the icon lists the networks the host
//! sees; a click on one joins it (asking for the password of a locked network). Pure like
//! [`crate::apps2`]: state, keys, clicks and drawing. The kernel runs the scan and the connection (through
//! [`crate::wifihost`]) when [`WifiFly::req`] is set, and hands back the result.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::apps::{clipped, text, Fonts};
use crate::gui::canvas::{self, rgb, Canvas, Rect};
use crate::gui::theme::DARK;
use crate::keymap::Key;
use crate::wifihost::Net;

/// What the drop-down asks the kernel to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiReq {
    Scan,
    /// Join this network with this password (empty: open, or the laptop knows it).
    Connect(String, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Scanning,
    List,
    /// Typing the password for network `n`.
    Password(usize),
    Connecting(String),
}

/// What a key or click did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    None,
    Redraw,
    Close,
}

/// Rows shown at most (the host sends up to `wifihost::MAX_NETS`).
pub const ROWS: usize = 8;
const W: i32 = 400;
const HEAD: i32 = 46;
const FOOT: i32 = 92;

pub struct WifiFly {
    pub nets: Vec<Net>,
    pub phase: Phase,
    pub sel: usize,
    pass: String,
    /// A line under the list: an error, or what happened.
    pub status: String,
    status_bad: bool,
    /// Work for the kernel, taken by it.
    pub req: Option<WifiReq>,
}

impl Default for WifiFly {
    fn default() -> Self {
        WifiFly { nets: Vec::new(), phase: Phase::Scanning, sel: 0, pass: String::new(), status: String::new(), status_bad: false, req: Some(WifiReq::Scan) }
    }
}

fn row_h(f: &Fonts) -> i32 {
    f.sans.height as i32 + 16
}

impl WifiFly {
    /// The drop-down's place: under the panel, right edge near the network icon's (kept on screen).
    pub fn rect(anchor: Rect, panel_bottom: i32, screen_w: i32, f: &Fonts) -> Rect {
        let h = HEAD + ROWS as i32 * row_h(f) + FOOT;
        let x = (anchor.right() + 60 - W).clamp(8, (screen_w - W - 8).max(8));
        Rect::new(x, panel_bottom + 6, W, h)
    }

    fn list_rect(r: Rect, f: &Fonts) -> Rect {
        Rect::new(r.x, r.y + HEAD, r.w, ROWS as i32 * row_h(f))
    }

    fn foot_rect(r: Rect) -> Rect {
        Rect::new(r.x, r.bottom() - FOOT, r.w, FOOT)
    }

    /// The right-hand button in the footer: Refresh (list) or Connect (password).
    fn button(r: Rect) -> Rect {
        let fr = Self::foot_rect(r);
        Rect::new(fr.right() - 112, fr.bottom() - 42, 98, 30)
    }

    /// The password box (password phase).
    fn field(r: Rect) -> Rect {
        let fr = Self::foot_rect(r);
        Rect::new(fr.x + 14, fr.bottom() - 42, fr.w - 14 - 124, 30)
    }

    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Scanning | Phase::Connecting(_))
    }

    /// The scan finished (networks strongest first) or failed.
    pub fn scanned(&mut self, r: Result<Vec<Net>, String>) {
        self.phase = Phase::List;
        match r {
            Ok(n) => {
                self.status = if n.is_empty() { String::from("No networks found") } else { String::new() };
                self.status_bad = false;
                self.nets = n;
                self.nets.truncate(ROWS);
                self.sel = self.nets.iter().position(|n| n.current).unwrap_or(0);
            }
            Err(e) => {
                self.status = e;
                self.status_bad = true;
            }
        }
    }

    /// The connection to `ssid` finished.
    pub fn connected(&mut self, ssid: &str, r: Result<(), String>) {
        self.phase = Phase::List;
        match r {
            Ok(()) => {
                for n in self.nets.iter_mut() {
                    n.current = n.ssid() == ssid;
                }
                self.status = format!("Connected to {ssid}");
                self.status_bad = false;
            }
            Err(e) => {
                self.status = format!("Not connected: {e}");
                self.status_bad = true;
            }
        }
    }

    fn wipe_pass(&mut self) {
        // SAFETY: zero bytes keep the string valid UTF-8.
        unsafe { self.pass.as_bytes_mut().fill(0) };
        self.pass.clear();
    }

    /// Network `i` was chosen: join it, or ask for its password first.
    fn choose(&mut self, i: usize) -> Out {
        let Some(n) = self.nets.get(i).copied() else { return Out::None };
        self.sel = i;
        if n.current {
            self.status = format!("Already connected to {}", n.ssid());
            self.status_bad = false;
            return Out::Redraw;
        }
        if n.secure {
            self.wipe_pass();
            self.status.clear();
            self.phase = Phase::Password(i);
        } else {
            self.connect(i);
        }
        Out::Redraw
    }

    fn connect(&mut self, i: usize) {
        let ssid = String::from(self.nets[i].ssid());
        let pass = core::mem::take(&mut self.pass);
        self.req = Some(WifiReq::Connect(ssid.clone(), pass));
        self.status = format!("Connecting to {ssid}...");
        self.status_bad = false;
        self.phase = Phase::Connecting(ssid);
    }

    fn submit_password(&mut self, i: usize) -> Out {
        if !self.pass.is_empty() && self.pass.len() < 8 {
            self.status = String::from("A Wi-Fi password has at least 8 characters");
            self.status_bad = true;
            return Out::Redraw;
        }
        self.connect(i);
        Out::Redraw
    }

    fn refresh(&mut self) -> Out {
        self.phase = Phase::Scanning;
        self.status.clear();
        self.req = Some(WifiReq::Scan);
        Out::Redraw
    }

    pub fn key(&mut self, k: Key) -> Out {
        if k == Key::Esc {
            if let Phase::Password(_) = self.phase {
                self.wipe_pass();
                self.status.clear();
                self.phase = Phase::List;
                return Out::Redraw;
            }
            return Out::Close;
        }
        if self.busy() {
            return Out::None;
        }
        if let Phase::Password(i) = self.phase {
            return match k {
                Key::Enter => self.submit_password(i),
                Key::Backspace => {
                    self.pass.pop();
                    Out::Redraw
                }
                Key::Char(c) if (0x20..0x7F).contains(&c) && self.pass.len() < 63 => {
                    self.pass.push(c as char);
                    Out::Redraw
                }
                _ => Out::None,
            };
        }
        let n = self.nets.len();
        match k {
            Key::Up | Key::HistoryPrev => self.sel = self.sel.saturating_sub(1),
            Key::Down | Key::HistoryNext => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            Key::Enter => return self.choose(self.sel),
            Key::Char(b'r') | Key::Char(b'R') => return self.refresh(),
            _ => return Out::None,
        }
        Out::Redraw
    }

    /// A click at (`x`, `y`) inside the drop-down `r`.
    pub fn click(&mut self, r: Rect, f: &Fonts, x: i32, y: i32) -> Out {
        if self.busy() {
            return Out::None;
        }
        if Self::button(r).contains(x, y) {
            return match self.phase {
                Phase::Password(i) => self.submit_password(i),
                _ => self.refresh(),
            };
        }
        let lr = Self::list_rect(r, f);
        if lr.contains(x, y) {
            let i = ((y - lr.y) / row_h(f)) as usize;
            if let Phase::Password(p) = self.phase {
                if i == p {
                    return Out::None;
                }
                self.wipe_pass();
                self.phase = Phase::List;
            }
            return self.choose(i);
        }
        Out::None
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect, f: &Fonts) {
        c.shadow(r.x as f32, r.y as f32 + 4.0, r.w as f32, r.h as f32, 10.0, 16.0, 0, 0.55);
        c.round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 10.0, rgb(24, 29, 46), 1.0);
        c.round_rect_stroke(r.x as f32 + 0.5, r.y as f32 + 0.5, r.w as f32 - 1.0, r.h as f32 - 1.0, 10.0, 1.0, rgb(70, 82, 116), 1.0);
        let fh = f.sans.height as i32;
        text(c, f.sans, r.x + 16, r.y + (HEAD - fh) / 2, "Wi-Fi", DARK.text);
        let sub = "through the laptop";
        let sw = canvas::text_width(f.sans, sub, 0);
        text(c, f.sans, r.right() - sw - 16, r.y + (HEAD - fh) / 2, sub, DARK.text_dim);
        c.fill_rect(Rect::new(r.x + 1, r.y + HEAD - 1, r.w - 2, 1), DARK.border);
        let lr = Self::list_rect(r, f);
        let rh = row_h(f);
        clipped(c, lr, |c| {
            if self.phase == Phase::Scanning {
                text(c, f.sans, lr.x + 16, lr.y + 14, "Looking for networks...", DARK.text_dim);
                return;
            }
            if self.nets.is_empty() {
                text(c, f.sans, lr.x + 16, lr.y + 14, "No networks to show. Press Refresh.", DARK.text_dim);
            }
            let typing = match self.phase {
                Phase::Password(i) => Some(i),
                _ => None,
            };
            for (i, n) in self.nets.iter().enumerate().take(ROWS) {
                let ry = lr.y + i as i32 * rh;
                if i == self.sel || typing == Some(i) {
                    c.round_rect((lr.x + 6) as f32, (ry + 2) as f32, (lr.w - 12) as f32, (rh - 4) as f32, 6.0, rgb(48, 64, 104), 1.0);
                }
                bars(c, lr.x + 18, ry + rh / 2 + 8, n.signal, n.current);
                let name_col = if n.current { DARK.accent } else { DARK.text };
                text(c, f.sans, lr.x + 56, ry + (rh - fh) / 2, n.ssid(), name_col);
                let tag = if n.current { "Connected" } else if n.secure { "Locked" } else { "Open" };
                let tw = canvas::text_width(f.sans, tag, 0);
                let tcol = if n.current { DARK.good } else { DARK.text_dim };
                text(c, f.sans, lr.right() - tw - 16, ry + (rh - fh) / 2, tag, tcol);
                if n.secure && !n.current {
                    lock(c, lr.right() - tw - 34, ry + rh / 2 - 7);
                }
            }
        });
        // Footer: a status line, then the password box or the Refresh button.
        let fr = Self::foot_rect(r);
        c.fill_rect(Rect::new(r.x + 1, fr.y, r.w - 2, 1), DARK.border);
        let (line, col) = match &self.phase {
            Phase::Password(i) if self.status.is_empty() => (format!("Password for {}", self.nets[*i].ssid()), DARK.text_dim),
            Phase::Scanning => (String::from("Scanning (the screen pauses a few seconds)..."), DARK.text_dim),
            _ if self.status.is_empty() => (String::from("Click a network to connect"), DARK.text_dim),
            _ => (self.status.clone(), if self.status_bad { DARK.warn } else { DARK.text }),
        };
        clipped(c, Rect::new(fr.x + 14, fr.y + 8, fr.w - 28, fh + 4), |c| text(c, f.sans, fr.x + 14, fr.y + 10, &line, col));
        let label = match self.phase {
            Phase::Password(_) => {
                let fd = Self::field(r);
                c.round_rect(fd.x as f32, fd.y as f32, fd.w as f32, fd.h as f32, 5.0, rgb(14, 17, 28), 1.0);
                c.round_rect_stroke(fd.x as f32 + 0.5, fd.y as f32 + 0.5, fd.w as f32 - 1.0, fd.h as f32 - 1.0, 5.0, 1.0, DARK.accent, 1.0);
                let stars: String = "*".repeat(self.pass.len().min(40));
                let (shown, scol) = if stars.is_empty() { ("(empty: the laptop knows it)", DARK.text_dim) } else { (stars.as_str(), DARK.text) };
                clipped(c, fd.inflate(-4), |c| text(c, f.sans, fd.x + 10, fd.y + (fd.h - fh) / 2, shown, scol));
                "Connect"
            }
            _ => "Refresh",
        };
        let b = Self::button(r);
        let busy = self.busy();
        c.round_rect(b.x as f32, b.y as f32, b.w as f32, b.h as f32, 6.0, if busy { rgb(50, 56, 76) } else { rgb(60, 110, 200) }, 1.0);
        let lw = canvas::text_width(f.sans, label, 0);
        text(c, f.sans, b.x + (b.w - lw) / 2, b.y + (b.h - fh) / 2, label, if busy { DARK.text_dim } else { DARK.text });
    }
}

/// Four signal bars, `signal` 0-100, bottom at `base`.
fn bars(c: &mut Canvas, x: i32, base: i32, signal: u8, current: bool) {
    let lit = match signal {
        0..=24 => 1,
        25..=49 => 2,
        50..=74 => 3,
        _ => 4,
    };
    let on = if current { DARK.good } else { DARK.text };
    for i in 0..4 {
        let h = 4 + i * 4;
        c.fill_rect(Rect::new(x + i * 6, base - h, 4, h), if i < lit { on } else { rgb(70, 78, 100) });
    }
}

/// A small padlock with its top-left at (`x`, `y`).
fn lock(c: &mut Canvas, x: i32, y: i32) {
    c.round_rect_stroke(x as f32 + 2.5, y as f32 + 0.5, 7.0, 8.0, 3.0, 1.5, DARK.text_dim, 1.0);
    c.round_rect(x as f32, (y + 5) as f32, 12.0, 9.0, 2.0, DARK.text_dim, 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifihost::parse_net;

    fn sample() -> WifiFly {
        let mut w = WifiFly::default();
        assert_eq!(w.req.take(), Some(WifiReq::Scan));
        let nets = [&b"NET\t90\t1\t0\tHome"[..], b"NET\t60\t0\t0\tCafe", b"NET\t40\t1\t1\tOffice"].iter().map(|l| parse_net(l).unwrap()).collect();
        w.scanned(Ok(nets));
        w
    }

    #[test]
    fn opens_scanning_and_selects_the_current_network() {
        let w = sample();
        assert_eq!(w.phase, Phase::List);
        assert_eq!(w.sel, 2);
    }

    #[test]
    fn open_network_connects_at_once() {
        let mut w = sample();
        w.key(Key::Up);
        assert_eq!(w.key(Key::Enter), Out::Redraw);
        assert_eq!(w.req.take(), Some(WifiReq::Connect(String::from("Cafe"), String::new())));
        assert_eq!(w.key(Key::Enter), Out::None, "busy while connecting");
        w.connected("Cafe", Ok(()));
        assert!(w.nets[1].current && !w.nets[2].current);
    }

    #[test]
    fn locked_network_asks_for_a_password() {
        let mut w = sample();
        w.sel = 0;
        w.key(Key::Enter);
        assert_eq!(w.phase, Phase::Password(0));
        for b in b"short" {
            w.key(Key::Char(*b));
        }
        w.key(Key::Enter);
        assert!(w.req.is_none() && w.status_bad, "under 8 characters is refused");
        for b in b"er123" {
            w.key(Key::Char(*b));
        }
        w.key(Key::Enter);
        assert_eq!(w.req.take(), Some(WifiReq::Connect(String::from("Home"), String::from("shorter123"))));
        w.connected("Home", Err(String::from("wrong password")));
        assert_eq!(w.phase, Phase::List);
        assert!(w.status.contains("wrong password"));
    }

    #[test]
    fn esc_backs_out_then_closes() {
        let mut w = sample();
        w.sel = 0;
        w.key(Key::Enter);
        w.key(Key::Char(b'x'));
        assert_eq!(w.key(Key::Esc), Out::Redraw);
        assert_eq!(w.phase, Phase::List);
        assert!(w.pass.is_empty());
        assert_eq!(w.key(Key::Esc), Out::Close);
    }

    #[test]
    fn scan_error_is_shown_and_refresh_scans_again() {
        let mut w = WifiFly::default();
        w.scanned(Err(String::from("no helper")));
        assert_eq!(w.phase, Phase::List);
        assert!(w.status_bad);
        assert_eq!(w.key(Key::Char(b'r')), Out::Redraw);
        assert_eq!(w.req, Some(WifiReq::Scan));
    }
}
