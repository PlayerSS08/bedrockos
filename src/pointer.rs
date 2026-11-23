//! Mouse pointer: USB boot-protocol report parsing, the pointer position (clamped to the screen, with a little
//! acceleration) and the arrow sprite. Pure and host-testable; the framebuffer painting is in `fb`.

pub const SPRITE_W: usize = 12;
pub const SPRITE_H: usize = 19;

/// Arrow cursor. `#` = outline (black), `.` = fill (white), space = transparent.
const SPRITE: [&str; SPRITE_H] = [
    "#           ",
    "##          ",
    "#.#         ",
    "#..#        ",
    "#...#       ",
    "#....#      ",
    "#.....#     ",
    "#......#    ",
    "#.......#   ",
    "#........#  ",
    "#.........# ",
    "#......#####",
    "#...#..#    ",
    "#..# #..#   ",
    "#.#  #..#   ",
    "##    #..#  ",
    "#     #..#  ",
    "       ##   ",
    "            ",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Texel {
    Clear,
    Outline,
    Fill,
}

pub fn sprite_texel(x: usize, y: usize) -> Texel {
    match SPRITE.get(y).and_then(|row| row.as_bytes().get(x)) {
        Some(b'#') => Texel::Outline,
        Some(b'.') => Texel::Fill,
        _ => Texel::Clear,
    }
}

pub const BTN_LEFT: u8 = 1;
pub const BTN_RIGHT: u8 = 2;
pub const BTN_MIDDLE: u8 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseReport {
    pub buttons: u8,
    pub dx: i8,
    pub dy: i8,
    pub wheel: i8,
}

/// Parses a boot-protocol mouse report: buttons, dx, dy and, if the device sends it, the wheel. Shorter than
/// three bytes is not a report.
pub fn parse_report(r: &[u8]) -> Option<MouseReport> {
    if r.len() < 3 {
        return None;
    }
    Some(MouseReport { buttons: r[0] & 0x07, dx: r[1] as i8, dy: r[2] as i8, wheel: r.get(3).map_or(0, |&w| w as i8) })
}

/// Motion summed over several reports since the last poll.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseMotion {
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
    pub buttons: u8,
}

impl MouseMotion {
    pub fn add(&mut self, r: MouseReport) {
        self.dx = self.dx.saturating_add(i32::from(r.dx));
        self.dy = self.dy.saturating_add(i32::from(r.dy));
        self.wheel = self.wheel.saturating_add(i32::from(r.wheel));
        self.buttons = r.buttons;
    }
}

/// A relative movement scaled so that slow motion stays precise and fast motion crosses the screen.
fn accelerate(d: i32) -> i32 {
    let a = d.abs();
    let scaled = if a <= 2 { a } else if a <= 8 { a * 2 } else { a * 3 };
    scaled * d.signum()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
    width: i32,
    height: i32,
}

impl Pointer {
    pub fn new(width: usize, height: usize) -> Self {
        let (w, h) = (width.max(1) as i32, height.max(1) as i32);
        Self { x: w / 2, y: h / 2, buttons: 0, width: w, height: h }
    }

    /// Applies one report (or the sum of several). Returns true when the position changed.
    pub fn apply(&mut self, dx: i32, dy: i32, buttons: u8) -> bool {
        let (ox, oy) = (self.x, self.y);
        self.x = (self.x + accelerate(dx)).clamp(0, self.width - 1);
        self.y = (self.y + accelerate(dy)).clamp(0, self.height - 1);
        self.buttons = buttons & 0x07;
        (ox, oy) != (self.x, self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_parsing() {
        assert_eq!(parse_report(&[1, 5, 0xFB]), Some(MouseReport { buttons: 1, dx: 5, dy: -5, wheel: 0 }));
        assert_eq!(parse_report(&[0xFF, 0, 0, 0xFF]), Some(MouseReport { buttons: 7, dx: 0, dy: 0, wheel: -1 }));
        assert_eq!(parse_report(&[1, 2]), None);
        assert_eq!(parse_report(&[]), None);
    }

    #[test]
    fn motion_sums_reports_and_keeps_the_latest_buttons() {
        let mut m = MouseMotion::default();
        m.add(MouseReport { buttons: 1, dx: 5, dy: -3, wheel: 1 });
        m.add(MouseReport { buttons: 0, dx: 5, dy: -3, wheel: 0 });
        assert_eq!(m, MouseMotion { dx: 10, dy: -6, wheel: 1, buttons: 0 });
    }

    #[test]
    fn pointer_is_clamped_to_the_screen() {
        let mut p = Pointer::new(100, 50);
        assert_eq!((p.x, p.y), (50, 25));
        for _ in 0..50 {
            p.apply(127, 127, 0);
        }
        assert_eq!((p.x, p.y), (99, 49));
        for _ in 0..50 {
            p.apply(-128, -128, 0);
        }
        assert_eq!((p.x, p.y), (0, 0));
    }

    #[test]
    fn slow_motion_is_one_to_one_and_fast_motion_is_amplified() {
        assert_eq!(accelerate(1), 1);
        assert_eq!(accelerate(-2), -2);
        assert_eq!(accelerate(5), 10);
        assert_eq!(accelerate(-20), -60);
        assert_eq!(accelerate(0), 0);
    }

    #[test]
    fn apply_reports_movement_and_buttons() {
        let mut p = Pointer::new(640, 480);
        assert!(!p.apply(0, 0, BTN_LEFT));
        assert_eq!(p.buttons, BTN_LEFT);
        assert!(p.apply(3, 0, 0));
        assert_eq!(p.buttons, 0);
        // At the edge nothing moves, so nothing needs repainting.
        let mut q = Pointer::new(10, 10);
        q.apply(-100, -100, 0);
        assert!(!q.apply(-5, -5, 0));
    }

    #[test]
    fn sprite_has_a_tip_an_outline_and_fill() {
        assert_eq!(sprite_texel(0, 0), Texel::Outline);
        assert_eq!(sprite_texel(1, 2), Texel::Fill);
        assert_eq!(sprite_texel(11, 0), Texel::Clear);
        assert_eq!(sprite_texel(100, 100), Texel::Clear);
        for row in SPRITE {
            assert_eq!(row.len(), SPRITE_W);
        }
        let fill = (0..SPRITE_H).flat_map(|y| (0..SPRITE_W).map(move |x| (x, y))).filter(|&(x, y)| sprite_texel(x, y) == Texel::Fill).count();
        assert!(fill > 20);
    }
}
