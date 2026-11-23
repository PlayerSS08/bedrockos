//! The sign-in screen shown before the boot menu: a user name and a password, checked before BedrockOS goes on.
//! The password is not stored in the image, only its SHA-256 hash. Three wrong tries lock the screen for
//! a few seconds. Pure logic (no screen, no allocation), so it is tested on the host.

use crate::sha256;

pub const USER: &str = "BedrockOS";
/// SHA-256 of the password.
const PASS_SHA256: [u8; 32] = [
    0xaa, 0xd4, 0x37, 0xbd, 0xa4, 0x86, 0xef, 0x72, 0xe3, 0xa2, 0x3d, 0xf9, 0x92, 0x91, 0x9d, 0x95, 0x58, 0x96, 0x9b, 0x7e, 0xe2, 0xdd, 0xcd,
    0x0a, 0x4b, 0x2a, 0x63, 0xe8, 0x6b, 0xee, 0x4e, 0xe0,
];
pub const MAX: usize = 32;
const TRIES: u32 = 3;
pub const LOCK_MS: u32 = 10_000;
/// Enough stars for the longest password.
const STARS: &str = "********************************";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(u8),
    Backspace,
    Tab,
    Enter,
    Up,
    Down,
    Esc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Idle,
    Redraw,
    /// Both are right: go on to the boot menu.
    SignedIn,
}

pub struct Login {
    user: [u8; MAX],
    ulen: usize,
    pass: [u8; MAX],
    plen: usize,
    /// 0 user name, 1 password.
    pub field: usize,
    fails: u32,
    locked_ms: u32,
    pub message: &'static str,
    pub error: bool,
}

impl Default for Login {
    fn default() -> Self {
        Self::new()
    }
}

/// Compares in time that does not depend on where the first difference is.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Login {
    pub const fn new() -> Login {
        Login {
            user: [0; MAX],
            ulen: 0,
            pass: [0; MAX],
            plen: 0,
            field: 0,
            fails: 0,
            locked_ms: 0,
            message: "Type your user name and password, then press Enter.",
            error: false,
        }
    }

    pub fn user(&self) -> &str {
        core::str::from_utf8(&self.user[..self.ulen]).unwrap_or("")
    }

    /// The password as stars.
    pub fn stars(&self) -> &'static str {
        &STARS[..self.plen]
    }

    pub fn locked(&self) -> bool {
        self.locked_ms > 0
    }

    fn check(&self) -> bool {
        let user_ok = same(&self.user[..self.ulen], USER.as_bytes());
        let pass_ok = same(&sha256::digest(&self.pass[..self.plen]), &PASS_SHA256);
        user_ok & pass_ok
    }

    fn clear_password(&mut self) {
        self.pass.fill(0);
        self.plen = 0;
    }

    /// Time passing (for the lock after too many wrong tries).
    pub fn tick(&mut self, ms: u32) -> Outcome {
        if self.locked_ms == 0 {
            return Outcome::Idle;
        }
        let before = self.locked_ms.div_ceil(1000);
        self.locked_ms = self.locked_ms.saturating_sub(ms);
        if self.locked_ms == 0 {
            self.message = "Try again: type your password, then press Enter.";
            self.error = false;
            return Outcome::Redraw;
        }
        if self.locked_ms.div_ceil(1000) != before { Outcome::Redraw } else { Outcome::Idle }
    }

    /// Seconds left of the lock.
    pub fn lock_seconds(&self) -> u32 {
        self.locked_ms.div_ceil(1000)
    }

    pub fn key(&mut self, k: Key) -> Outcome {
        if self.locked() {
            return Outcome::Idle;
        }
        match k {
            Key::Char(c) if (0x20..0x7F).contains(&c) => {
                let (buf, len) = if self.field == 0 { (&mut self.user, &mut self.ulen) } else { (&mut self.pass, &mut self.plen) };
                if *len < MAX {
                    buf[*len] = c;
                    *len += 1;
                }
            }
            Key::Char(_) => return Outcome::Idle,
            Key::Backspace => {
                if self.field == 0 {
                    self.ulen = self.ulen.saturating_sub(1);
                } else if self.plen > 0 {
                    self.plen -= 1;
                    self.pass[self.plen] = 0;
                }
            }
            Key::Tab | Key::Down | Key::Up => self.field ^= 1,
            Key::Esc => {
                if self.field == 0 {
                    self.ulen = 0;
                } else {
                    self.clear_password();
                }
            }
            Key::Enter => {
                if self.field == 0 {
                    self.field = 1;
                    return Outcome::Redraw;
                }
                if self.check() {
                    self.clear_password();
                    self.message = "Welcome back. Starting BedrockOS...";
                    self.error = false;
                    return Outcome::SignedIn;
                }
                self.clear_password();
                self.fails += 1;
                self.error = true;
                if self.fails % TRIES == 0 {
                    self.locked_ms = LOCK_MS;
                    self.message = "Too many wrong tries. Wait a few seconds.";
                } else {
                    self.message = "Wrong user name or password. Try again.";
                }
            }
        }
        Outcome::Redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typ(l: &mut Login, s: &str) {
        for b in s.bytes() {
            l.key(Key::Char(b));
        }
    }

    #[test]
    fn right_name_and_password_sign_in() {
        let mut l = Login::new();
        typ(&mut l, "BedrockOS");
        assert_eq!(l.key(Key::Enter), Outcome::Redraw, "Enter in the name field moves to the password");
        assert_eq!(l.field, 1);
        typ(&mut l, "MinecraftSS_08");
        assert_eq!(l.stars(), "**************");
        assert_eq!(l.key(Key::Enter), Outcome::SignedIn);
        assert_eq!(l.stars(), "", "the password is wiped");
    }

    #[test]
    fn wrong_tries_fail_and_lock() {
        let mut l = Login::new();
        typ(&mut l, "bedrockos"); // the name is case-sensitive
        l.key(Key::Tab);
        typ(&mut l, "MinecraftSS_08");
        assert_eq!(l.key(Key::Enter), Outcome::Redraw);
        assert!(l.error && !l.locked());
        l.key(Key::Tab);
        for _ in 0..9 {
            l.key(Key::Backspace);
        }
        typ(&mut l, "BedrockOS");
        l.key(Key::Tab);
        for bad in ["minecraftss_08", "MinecraftSS_8"] {
            typ(&mut l, bad);
            l.key(Key::Enter);
        }
        assert!(l.locked(), "three wrong tries lock");
        typ(&mut l, "MinecraftSS_08");
        assert_eq!(l.key(Key::Enter), Outcome::Idle, "keys are ignored while locked");
        assert_eq!(l.lock_seconds(), 10);
        assert_eq!(l.tick(400), Outcome::Idle);
        assert_eq!(l.tick(600), Outcome::Redraw, "the seconds shown change");
        assert_eq!(l.tick(LOCK_MS), Outcome::Redraw);
        assert!(!l.locked());
        typ(&mut l, "MinecraftSS_08");
        assert_eq!(l.key(Key::Enter), Outcome::SignedIn);
    }

    #[test]
    fn editing_keys() {
        let mut l = Login::new();
        typ(&mut l, "abc");
        l.key(Key::Backspace);
        assert_eq!(l.user(), "ab");
        l.key(Key::Esc);
        assert_eq!(l.user(), "");
        for _ in 0..40 {
            l.key(Key::Char(b'x'));
        }
        assert_eq!(l.user().len(), MAX);
        assert_eq!(l.key(Key::Char(0x07)), Outcome::Idle);
    }
}
