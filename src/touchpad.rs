//! Touchpad report decoder. Pure and host-tested.
//!
//! Input: the raw input reports of a HID pointing device (as `i2chid` or a USB driver delivers them) and
//! its report descriptor. Output: the same `MouseMotion` the USB mouse produces, so the pointer overlay and
//! the shell do not care where movement comes from.
//!
//! A relative mouse report (what many touchpads send until a driver switches them to precision mode) is
//! passed straight through. A Precision Touchpad report carries absolute finger positions, so this turns
//! them into movement itself: one finger moves the pointer, a quick light touch is a click, the pad's
//! button is the left button (right with two fingers down), and two fingers moving up or down scroll.

use crate::hidparse::{self, Field, MouseLayout, TouchLayout};
use crate::pointer::{MouseMotion, BTN_LEFT, BTN_RIGHT};

/// Pointer counts for a swipe across the whole width of the pad (the pointer's own acceleration comes on top).
const COUNTS_PER_WIDTH: i64 = 900;
/// Fixed point: movement is carried in 1/4096 of a count so slow motion is not lost to rounding.
const FRAC: i64 = 4096;
/// A touch shorter than this, that did not travel, is a tap.
const TAP_MS: u64 = 250;
/// How long a tap keeps the button down before the release is delivered.
const TAP_HOLD_MS: u64 = 30;
/// A tap moves less than 1/`TAP_SLOP` of the pad width.
const TAP_SLOP: i32 = 40;
/// Two fingers scroll one notch per 1/`SCROLL_NOTCHES` of the pad height.
const SCROLL_NOTCHES: i32 = 24;
/// No single report moves the pointer (or the wheel) further than this.
const MAX_STEP: i64 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Finger {
    id: i32,
    x: i32,
    y: i32,
}

#[derive(Debug)]
pub struct Touchpad {
    mouse: Option<MouseLayout>,
    touch: Option<TouchLayout>,
    uses_ids: bool,
    /// Primary finger as of the previous report (one-finger tracking).
    last: Option<Finger>,
    /// Mean y of two fingers as of the previous report (scrolling).
    last_pair_y: Option<i32>,
    fingers_before: usize,
    most_fingers: usize,
    down_at: u64,
    down_xy: (i32, i32),
    travelled: bool,
    frac: (i64, i64),
    scroll: i32,
    button: bool,
    /// When a tap's press was delivered, until its release is.
    tap_pressed_at: Option<u64>,
    /// Reports decoded so far, for the diagnostics.
    pub reports: u32,
}

impl Touchpad {
    /// `None` when the descriptor holds neither a mouse nor a touchpad layout.
    pub fn new(report_descriptor: &[u8]) -> Option<Self> {
        let parsed = hidparse::parse(report_descriptor)?;
        let (mouse, touch) = (hidparse::find_mouse(&parsed), hidparse::find_touch(&parsed));
        if mouse.is_none() && touch.is_none() {
            return None;
        }
        Some(Self {
            mouse,
            touch,
            uses_ids: parsed.uses_report_ids,
            last: None,
            last_pair_y: None,
            fingers_before: 0,
            most_fingers: 0,
            down_at: 0,
            down_xy: (0, 0),
            travelled: false,
            frac: (0, 0),
            scroll: 0,
            button: false,
            tap_pressed_at: None,
            reports: 0,
        })
    }

    /// One line for the boot log and the `touchpad` command.
    pub fn describe(&self) -> alloc::string::String {
        use alloc::format;
        match (&self.mouse, &self.touch) {
            (Some(m), Some(t)) => format!("mouse report {} and touchpad report {} ({} contacts)", m.report_id, t.report_id, t.contacts.len()),
            (Some(m), None) => format!("mouse report {}", m.report_id),
            (None, Some(t)) => format!("precision touchpad report {} ({} contacts, x 0..{})", t.report_id, t.contacts.len(), t.contacts[0].x.logical_max),
            (None, None) => alloc::string::String::from("nothing usable"),
        }
    }

    /// Decodes one input report (starting with the report id byte when the device uses ids). Returns the
    /// movement it caused, or `None` when it changed nothing. `now_ms` is any steadily increasing clock.
    pub fn feed(&mut self, report: &[u8], now_ms: u64) -> Option<MouseMotion> {
        let (id, data) = if self.uses_ids { (*report.first()?, report.get(1..)?) } else { (0, report) };
        self.reports = self.reports.wrapping_add(1);
        if let Some(m) = &self.mouse {
            if m.report_id == id {
                return Self::mouse_report(m, data);
            }
        }
        let t = self.touch.clone()?;
        if t.report_id != id {
            return None;
        }
        self.touch_report(&t, data, now_ms)
    }

    /// Call regularly, even without reports: delivers the release of a tap.
    pub fn idle(&mut self, now_ms: u64) -> Option<MouseMotion> {
        let at = self.tap_pressed_at?;
        if now_ms.saturating_sub(at) >= TAP_HOLD_MS {
            self.tap_pressed_at = None;
            return Some(MouseMotion { buttons: 0, ..MouseMotion::default() });
        }
        None
    }

    fn mouse_report(m: &MouseLayout, data: &[u8]) -> Option<MouseMotion> {
        let mut buttons = 0u8;
        for b in &m.buttons {
            if b.value(data)? != 0 {
                buttons |= 1 << (b.usage - 1).min(7);
            }
        }
        let clamp = |f: &Field| f.value(data).map(|v| v.clamp(-1000, 1000));
        Some(MouseMotion { dx: clamp(&m.x)?, dy: clamp(&m.y)?, wheel: m.wheel.as_ref().and_then(clamp).unwrap_or(0), buttons })
    }

    fn touch_report(&mut self, t: &TouchLayout, data: &[u8], now_ms: u64) -> Option<MouseMotion> {
        let mut fingers = [Finger { id: 0, x: 0, y: 0 }; 5];
        let mut n = 0;
        for (slot, c) in t.contacts.iter().enumerate() {
            if n == fingers.len() || c.tip.value(data)? == 0 {
                continue;
            }
            // A contact the pad itself flags as not a finger (a palm) is ignored.
            if c.confidence.is_some_and(|f| f.value(data) == Some(0)) {
                continue;
            }
            let id = c.id.and_then(|f| f.value(data)).unwrap_or(slot as i32);
            fingers[n] = Finger { id, x: c.x.value(data)?, y: c.y.value(data)? };
            n += 1;
        }
        let width = t.contacts[0].x.range();
        let height = t.contacts[0].y.range();
        let mut motion = MouseMotion::default();
        let was_button = self.button;
        self.button = t.button.is_some_and(|b| b.value(data) == Some(1));

        if n == 0 {
            // All fingers up: a short, still touch is a tap.
            if self.fingers_before > 0 && self.most_fingers > 0 && !self.travelled && now_ms.saturating_sub(self.down_at) <= TAP_MS && !self.button && !was_button {
                motion.buttons = if self.most_fingers >= 2 { BTN_RIGHT } else { BTN_LEFT };
                self.tap_pressed_at = Some(now_ms);
            }
            self.end_touch();
        } else {
            if self.fingers_before == 0 {
                self.down_at = now_ms;
                self.down_xy = (fingers[0].x, fingers[0].y);
                self.travelled = false;
                self.most_fingers = 0;
                self.last = None;
                self.last_pair_y = None;
                self.frac = (0, 0);
                self.scroll = 0;
            }
            self.most_fingers = self.most_fingers.max(n);
            if n == 1 {
                let f = fingers[0];
                if let Some(p) = self.last.filter(|p| p.id == f.id) {
                    // The descriptor is untrusted: a 32-bit axis must not overflow, so this is all i64.
                    let (dx, dy) = (i64::from(f.x) - i64::from(p.x), i64::from(f.y) - i64::from(p.y));
                    self.frac.0 += dx * COUNTS_PER_WIDTH * FRAC / i64::from(width);
                    self.frac.1 += dy * COUNTS_PER_WIDTH * FRAC / i64::from(width);
                    let (cx, cy) = ((self.frac.0 / FRAC).clamp(-MAX_STEP, MAX_STEP), (self.frac.1 / FRAC).clamp(-MAX_STEP, MAX_STEP));
                    motion.dx = cx as i32;
                    motion.dy = cy as i32;
                    self.frac.0 -= cx * FRAC;
                    self.frac.1 -= cy * FRAC;
                }
                self.last = Some(f);
                self.last_pair_y = None;
            } else {
                // Two or more fingers: scroll by the vertical movement of the first two.
                let mean = ((i64::from(fingers[0].y) + i64::from(fingers[1].y)) / 2) as i32;
                if let Some(p) = self.last_pair_y {
                    self.scroll = self.scroll.saturating_add(mean.saturating_sub(p));
                    let step = (height / SCROLL_NOTCHES).max(1);
                    motion.wheel = (self.scroll / step).clamp(-MAX_STEP as i32, MAX_STEP as i32);
                    self.scroll -= motion.wheel * step;
                }
                self.last_pair_y = Some(mean);
                self.last = None;
            }
            // Real travel (more than a small slip of the finger) cancels a tap.
            let slop = i64::from(width / TAP_SLOP);
            let (ox, oy) = (i64::from(fingers[0].x) - i64::from(self.down_xy.0), i64::from(fingers[0].y) - i64::from(self.down_xy.1));
            if ox.abs() > slop || oy.abs() > slop {
                self.travelled = true;
            }
        }
        self.fingers_before = n;

        // The pad's own button: left, or right when two fingers are down.
        if self.button {
            motion.buttons |= if n >= 2 { BTN_RIGHT } else { BTN_LEFT };
        }
        if self.tap_pressed_at.is_some() && motion.buttons == 0 {
            // A tap's button stays down until `idle` releases it.
            motion.buttons = if self.most_fingers >= 2 { BTN_RIGHT } else { BTN_LEFT };
        }
        let changed = motion.dx != 0 || motion.dy != 0 || motion.wheel != 0 || motion.buttons != 0 || was_button != self.button;
        changed.then_some(motion)
    }

    fn end_touch(&mut self) {
        self.last = None;
        self.last_pair_y = None;
        self.most_fingers = 0;
        self.travelled = false;
        self.scroll = 0;
        self.frac = (0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hidparse::tests::{touch_desc, MOUSE};

    /// A touchpad report for `touch_desc()`: contacts as (id, x, y), the click button, the contact count.
    fn report(contacts: &[(u8, u16, u16)], button: bool) -> [u8; 15] {
        let mut r = [0u8; 15];
        r[0] = 4;
        for (i, &(id, x, y)) in contacts.iter().take(2).enumerate() {
            let b = 1 + i * 6;
            r[b] = 0b11; // tip and confidence
            r[b + 1] = id;
            r[b + 2..b + 4].copy_from_slice(&x.to_le_bytes());
            r[b + 4..b + 6].copy_from_slice(&y.to_le_bytes());
        }
        r[13] = u8::from(button);
        r[14] = contacts.len() as u8;
        r
    }

    fn pad() -> Touchpad {
        Touchpad::new(&touch_desc()).unwrap()
    }

    #[test]
    fn a_descriptor_without_pointing_is_refused() {
        // A keyboard-ish descriptor: one array input, no X/Y, no touch.
        assert!(Touchpad::new(&[0x05, 0x01, 0x09, 0x06, 0x15, 0x00, 0x25, 0x65, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00]).is_none());
        assert!(Touchpad::new(&[]).is_none());
    }

    #[test]
    fn mouse_reports_pass_straight_through() {
        let mut t = Touchpad::new(MOUSE).unwrap();
        let m = t.feed(&[1, 0x01, 5, 0xFB, 0], 0).unwrap();
        assert_eq!(m, MouseMotion { dx: 5, dy: -5, wheel: 0, buttons: 1 });
        // Another report id is somebody else's.
        assert_eq!(t.feed(&[9, 0, 0, 0, 0], 0), None);
        assert_eq!(t.feed(&[], 0), None);
        assert_eq!(t.feed(&[1, 0], 0), None); // truncated
    }

    #[test]
    fn one_finger_moves_the_pointer_in_proportion() {
        let mut t = pad();
        assert_eq!(t.feed(&report(&[(1, 1000, 1000)], false), 0), None); // first contact: no jump
        // Moving by a quarter of the width (4095 / 4 = 1023 units) is about 225 counts at 900 per width.
        let m = t.feed(&report(&[(1, 2023, 1000)], false), 10).unwrap();
        assert!((m.dx - 225).abs() <= 2, "dx {}", m.dx);
        assert_eq!((m.dy, m.buttons), (0, 0));
        // A different finger id restarts tracking instead of jumping.
        assert_eq!(t.feed(&report(&[(2, 100, 100)], false), 20), None);
    }

    #[test]
    fn slow_motion_is_not_lost_to_rounding() {
        let mut t = pad();
        t.feed(&report(&[(1, 1000, 500)], false), 0);
        let mut total = 0;
        for i in 1..=100u16 {
            // One unit per report is 0.22 count: 100 reports must add up to about 22.
            if let Some(m) = t.feed(&report(&[(1, 1000 + i, 500)], false), u64::from(i) * 8) {
                total += m.dx;
            }
        }
        assert!((total - 22).abs() <= 1, "total {total}");
    }

    #[test]
    fn a_quick_light_touch_is_a_left_click_released_later() {
        let mut t = pad();
        t.feed(&report(&[(1, 2000, 1000)], false), 1000);
        let m = t.feed(&report(&[], false), 1100).unwrap();
        assert_eq!(m.buttons, BTN_LEFT);
        assert_eq!(t.idle(1110), None); // still held
        assert_eq!(t.idle(1140).unwrap().buttons, 0);
        assert_eq!(t.idle(1200), None);
    }

    #[test]
    fn a_long_or_moving_touch_is_not_a_tap() {
        let mut t = pad();
        t.feed(&report(&[(1, 2000, 1000)], false), 1000);
        assert_eq!(t.feed(&report(&[], false), 1400), None); // held too long
        t.feed(&report(&[(1, 2000, 1000)], false), 2000);
        t.feed(&report(&[(1, 2600, 1000)], false), 2050); // travelled
        assert_eq!(t.feed(&report(&[], false), 2100), None);
    }

    #[test]
    fn the_pad_button_clicks_left_and_right_with_two_fingers() {
        let mut t = pad();
        t.feed(&report(&[(1, 2000, 1000)], false), 0);
        assert_eq!(t.feed(&report(&[(1, 2000, 1000)], true), 10).unwrap().buttons, BTN_LEFT);
        assert_eq!(t.feed(&report(&[(1, 2000, 1000)], false), 20).unwrap().buttons, 0);
        t.feed(&report(&[], false), 30); // a short touch: a tap, whose release comes from `idle`
        assert_eq!(t.idle(100).unwrap().buttons, 0);
        t.feed(&report(&[(1, 2000, 1000), (2, 2500, 1000)], false), 1000);
        assert_eq!(t.feed(&report(&[(1, 2000, 1000), (2, 2500, 1000)], true), 1010).unwrap().buttons, BTN_RIGHT);
    }

    #[test]
    fn two_fingers_scroll_and_do_not_move_the_pointer() {
        let mut t = pad();
        t.feed(&report(&[(1, 1000, 500), (2, 1500, 500)], false), 0);
        // A tenth of the height (2047 / 24 = 85 units per notch): 200 units down is two notches.
        let m = t.feed(&report(&[(1, 1000, 700), (2, 1500, 700)], false), 10).unwrap();
        assert_eq!((m.dx, m.dy, m.wheel), (0, 0, 2));
        // Fingers moving up scroll the other way.
        let m = t.feed(&report(&[(1, 1000, 500), (2, 1500, 500)], false), 20).unwrap();
        assert_eq!(m.wheel, -2);
    }

    #[test]
    fn a_palm_is_ignored() {
        let mut t = pad();
        let mut r = report(&[(1, 1000, 1000)], false);
        r[1] = 0b01; // tip without confidence
        assert_eq!(t.feed(&r, 0), None);
        assert_eq!(t.feed(&r, 10), None);
    }

    #[test]
    fn garbage_never_panics() {
        let mut t = pad();
        for len in 0..20 {
            for fill in [0u8, 0xFF, 0x55] {
                let mut r = alloc::vec![fill; len];
                if let Some(first) = r.first_mut() {
                    *first = 4;
                }
                let _ = t.feed(&r, 5);
                let _ = t.idle(500);
            }
        }
    }
}
