//! The window manager's bookkeeping: which windows are open, their stacking order (the last one is on top and
//! has the keyboard), where each one is, and moving them within the free area of the screen. Windows are
//! named by their app index in [`crate::desktop::APPS`]. Pure and host-tested; drawing is in `desktop`.

use alloc::vec::Vec;

use crate::desktop::APPS;
use crate::gui::canvas::Rect;

pub const N: usize = APPS.len();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wm {
    /// Open windows, bottom to top.
    order: Vec<usize>,
    rects: [Rect; N],
    /// Where windows may be: below the panel, above the dock.
    area: Rect,
}

impl Wm {
    pub fn new(rects: [Rect; N], area: Rect) -> Wm {
        Wm { order: Vec::new(), rects, area }
    }

    pub fn rect(&self, app: usize) -> Rect {
        self.rects[app]
    }

    pub fn is_open(&self, app: usize) -> bool {
        self.order.contains(&app)
    }

    /// Open windows, bottom to top.
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// The window on top, which gets the keyboard.
    pub fn focused(&self) -> Option<usize> {
        self.order.last().copied()
    }

    /// Opens `app` (or brings it to the front).
    pub fn show(&mut self, app: usize) {
        self.order.retain(|&a| a != app);
        self.order.push(app);
    }

    /// Closes or minimises `app`; the window below gets the keyboard.
    pub fn hide(&mut self, app: usize) {
        self.order.retain(|&a| a != app);
    }

    /// The top-most open window under (`x`, `y`).
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        self.order.iter().rev().copied().find(|&a| self.rects[a].contains(x, y))
    }

    /// Maximises `app` to the whole free area, or puts it back to its size before. Returns the old rectangle.
    pub fn toggle_max(&mut self, app: usize, saved: &mut [Option<Rect>; N]) -> Rect {
        let old = self.rects[app];
        self.rects[app] = match saved[app].take() {
            Some(r) => r,
            None => {
                saved[app] = Some(old);
                self.area
            }
        };
        old
    }

    /// Moves `app`'s top-left corner to (`x`, `y`), kept inside the free area. Returns the old rectangle.
    pub fn move_to(&mut self, app: usize, x: i32, y: i32) -> Rect {
        let r = self.rects[app];
        let a = self.area;
        let x = x.clamp(a.x, (a.right() - r.w).max(a.x));
        let y = y.clamp(a.y, (a.bottom() - r.h).max(a.y));
        self.rects[app] = Rect::new(x, y, r.w, r.h);
        r
    }

    /// Switches the keyboard to the next open window (the bottom one comes to the top), like Alt+Tab.
    pub fn cycle(&mut self) {
        if self.order.len() > 1 {
            let a = self.order.remove(0);
            self.order.push(a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wm() -> Wm {
        let mut r = [Rect::new(0, 0, 0, 0); N];
        r[0] = Rect::new(100, 100, 200, 100);
        r[1] = Rect::new(150, 150, 200, 100);
        r[3] = Rect::new(50, 50, 400, 300);
        Wm::new(r, Rect::new(0, 40, 800, 500))
    }

    #[test]
    fn maximise_and_restore() {
        let mut w = wm();
        let before = w.rect(1);
        let mut saved = [None; N];
        assert_eq!(w.toggle_max(1, &mut saved), before);
        assert_eq!(w.rect(1), w.area);
        w.toggle_max(1, &mut saved);
        assert_eq!(w.rect(1), before);
        assert_eq!(saved[1], None);
    }

    #[test]
    fn stacking_focus_and_hits() {
        let mut w = wm();
        assert_eq!(w.focused(), None);
        w.show(3);
        w.show(0);
        w.show(1);
        assert_eq!(w.order(), &[3, 0, 1]);
        assert_eq!(w.focused(), Some(1));
        assert_eq!(w.hit(160, 160), Some(1), "top-most wins where windows overlap");
        assert_eq!(w.hit(110, 110), Some(0));
        assert_eq!(w.hit(60, 60), Some(3));
        assert_eq!(w.hit(700, 500), None);
        w.show(3);
        assert_eq!(w.order(), &[0, 1, 3], "showing an open window raises it");
        w.hide(3);
        assert_eq!(w.focused(), Some(1));
        assert!(!w.is_open(3));
        w.cycle();
        assert_eq!(w.order(), &[1, 0]);
    }

    #[test]
    fn moving_stays_in_the_area() {
        let mut w = wm();
        let old = w.move_to(0, -50, -50);
        assert_eq!(old, Rect::new(100, 100, 200, 100));
        assert_eq!((w.rect(0).x, w.rect(0).y), (0, 40));
        w.move_to(0, 5000, 5000);
        assert_eq!((w.rect(0).right(), w.rect(0).bottom()), (800, 540));
        assert_eq!((w.rect(0).w, w.rect(0).h), (200, 100), "size kept");
    }
}
