//! `calc`: a small expression calculator. Numbers (with decimals), + - * / % ^, parentheses and unary minus,
//! with the usual precedence. Pure and host-tested.

use alloc::string::String;

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i] == b' ' {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn expr(&mut self) -> Result<f64, &'static str> {
        let mut v = self.term()?;
        while let Some(c @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if c == b'+' { v + r } else { v - r };
        }
        Ok(v)
    }

    fn term(&mut self) -> Result<f64, &'static str> {
        let mut v = self.power()?;
        while let Some(c @ (b'*' | b'/' | b'%')) = self.peek() {
            self.i += 1;
            let r = self.power()?;
            v = match c {
                b'*' => v * r,
                _ if r == 0.0 => return Err("division by zero"),
                b'/' => v / r,
                _ => v % r,
            };
        }
        Ok(v)
    }

    fn power(&mut self) -> Result<f64, &'static str> {
        let base = self.unary()?;
        if self.peek() == Some(b'^') {
            self.i += 1;
            let e = self.power()?;
            if e != (e as i32) as f64 || !(-64.0..=64.0).contains(&e) {
                return Err("powers must be whole numbers from -64 to 64");
            }
            let mut r = 1.0;
            for _ in 0..(e as i32).unsigned_abs() {
                r *= base;
            }
            return Ok(if e < 0.0 { 1.0 / r } else { r });
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<f64, &'static str> {
        match self.peek() {
            Some(b'-') => {
                self.i += 1;
                Ok(-self.unary()?)
            }
            Some(b'+') => {
                self.i += 1;
                self.unary()
            }
            Some(b'(') => {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() != Some(b')') {
                    return Err("missing )");
                }
                self.i += 1;
                Ok(v)
            }
            Some(b'0'..=b'9' | b'.') => self.number(),
            Some(_) => Err("unexpected character"),
            None => Err("expression ends too early"),
        }
    }

    fn number(&mut self) -> Result<f64, &'static str> {
        let (mut v, mut scale, mut dot, mut digits) = (0.0f64, 1.0f64, false, 0);
        while let Some(&c) = self.s.get(self.i) {
            match c {
                b'0'..=b'9' => {
                    if dot {
                        scale /= 10.0;
                        v += f64::from(c - b'0') * scale;
                    } else {
                        v = v * 10.0 + f64::from(c - b'0');
                    }
                    digits += 1;
                }
                b'.' if !dot => dot = true,
                _ => break,
            }
            self.i += 1;
        }
        if digits == 0 { Err("bad number") } else { Ok(v) }
    }
}

/// Evaluates `expr`.
pub fn eval(expr: &str) -> Result<f64, &'static str> {
    let mut p = P { s: expr.as_bytes(), i: 0 };
    let v = p.expr()?;
    if p.peek().is_some() {
        return Err("unexpected text after the expression");
    }
    Ok(v)
}

/// Formats a result: whole numbers without decimals, others with up to 6 decimals (trailing zeros cut).
pub fn format(v: f64) -> String {
    use core::fmt::Write;
    let mut s = String::new();
    if v == (v as i64) as f64 && v.abs() < 1e15 {
        let _ = write!(s, "{}", v as i64);
    } else {
        let _ = write!(s, "{:.6}", v);
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_and_parentheses() {
        assert_eq!(eval("1 + 2 * 3"), Ok(7.0));
        assert_eq!(eval("(1 + 2) * 3"), Ok(9.0));
        assert_eq!(eval("-2 ^ 2"), Ok(4.0));
        assert_eq!(eval("2 ^ 3 ^ 2"), Ok(512.0));
        assert_eq!(eval("10 % 4"), Ok(2.0));
        assert_eq!(eval("7 / 2"), Ok(3.5));
        assert_eq!(format(eval("1/3").unwrap()), "0.333333");
        assert_eq!(format(eval("2.50 * 2").unwrap()), "5");
        assert_eq!(eval("1 / 0"), Err("division by zero"));
        assert_eq!(eval("(1 + 2"), Err("missing )"));
        assert_eq!(eval("1 +"), Err("expression ends too early"));
        assert_eq!(eval("2 x"), Err("unexpected text after the expression"));
        assert_eq!(eval("abc"), Err("unexpected character"));
    }
}
