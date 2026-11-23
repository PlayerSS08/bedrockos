//! Colour palettes. Dark is the default; light is switched with `theme light`. Both share the same accent.

use super::canvas::{rgb, Color};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    /// Wallpaper / boot background, top and bottom of the gradient.
    pub bg_top: Color,
    pub bg_bottom: Color,
    /// Panels, windows and cards.
    pub surface: Color,
    pub surface_hi: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub accent: Color,
    pub accent_2: Color,
    pub good: Color,
    pub warn: Color,
    pub bad: Color,
    pub shadow: Color,
}

pub const DARK: Theme = Theme {
    name: "dark",
    bg_top: rgb(10, 14, 28),
    bg_bottom: rgb(22, 30, 54),
    surface: rgb(24, 29, 44),
    surface_hi: rgb(36, 43, 64),
    border: rgb(58, 68, 96),
    text: rgb(232, 236, 246),
    text_dim: rgb(140, 150, 175),
    accent: rgb(92, 160, 255),
    accent_2: rgb(64, 214, 190),
    good: rgb(80, 210, 130),
    warn: rgb(245, 190, 70),
    bad: rgb(240, 90, 90),
    shadow: rgb(0, 0, 0),
};

pub const LIGHT: Theme = Theme {
    name: "light",
    bg_top: rgb(214, 226, 244),
    bg_bottom: rgb(240, 244, 252),
    surface: rgb(250, 251, 254),
    surface_hi: rgb(232, 238, 248),
    border: rgb(196, 206, 224),
    text: rgb(24, 30, 44),
    text_dim: rgb(96, 108, 132),
    accent: rgb(40, 110, 230),
    accent_2: rgb(20, 160, 140),
    good: rgb(30, 150, 80),
    warn: rgb(200, 140, 20),
    bad: rgb(210, 50, 50),
    shadow: rgb(40, 50, 80),
};

pub fn by_name(name: &str) -> Option<&'static Theme> {
    match name {
        "dark" => Some(&DARK),
        "light" => Some(&LIGHT),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::canvas::channels;

    fn luma(c: Color) -> u32 {
        let (r, g, b) = channels(c);
        (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000
    }

    #[test]
    fn text_contrasts_with_its_surface() {
        for t in [&DARK, &LIGHT] {
            let d = luma(t.text).abs_diff(luma(t.surface));
            assert!(d > 150, "{}: text/surface contrast {d}", t.name);
            assert!(luma(t.text_dim).abs_diff(luma(t.surface)) > 70, "{}: dim text still readable", t.name);
        }
    }

    #[test]
    fn names_resolve() {
        assert_eq!(by_name("dark"), Some(&DARK));
        assert_eq!(by_name("light"), Some(&LIGHT));
        assert_eq!(by_name("pink"), None);
    }
}
