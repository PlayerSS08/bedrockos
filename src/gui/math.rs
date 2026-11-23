//! Small float helpers for drawing (no `std`, no libm): square root, sine, smoothstep, a PRNG.
//! Pure and host-tested. Floats are safe in BedrockOS kernel tasks: the context switch saves SSE state.

/// Square root by a bit-level first guess and three Newton steps (relative error under 1e-6).
pub fn sqrt(x: f32) -> f32 {
    if x <= 0.0 || x.is_nan() {
        return 0.0;
    }
    if x.is_infinite() {
        return x;
    }
    let mut y = f32::from_bits((x.to_bits() >> 1) + 0x1FBD_1DF5);
    for _ in 0..3 {
        y = 0.5 * (y + x / y);
    }
    y
}

pub const PI: f32 = core::f32::consts::PI;
pub const TAU: f32 = core::f32::consts::TAU;

/// Sine with an absolute error under 0.001 (range reduction, then a 5th-order odd polynomial).
pub fn sin(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    // Reduce to [-PI, PI].
    let mut r = x - TAU * floor(x / TAU + 0.5);
    // Fold to [-PI/2, PI/2] using sin(PI - r) = sin(r).
    if r > PI / 2.0 {
        r = PI - r;
    } else if r < -PI / 2.0 {
        r = -PI - r;
    }
    let r2 = r * r;
    // Taylor up to r^7: accurate on [-PI/2, PI/2].
    r * (1.0 - r2 / 6.0 * (1.0 - r2 / 20.0 * (1.0 - r2 / 42.0)))
}

pub fn cos(x: f32) -> f32 {
    sin(x + PI / 2.0)
}

/// Arc tangent of y/x in (-PI, PI], error under 0.005 rad.
pub fn atan2(y: f32, x: f32) -> f32 {
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    let ax = if x < 0.0 { -x } else { x };
    let ay = if y < 0.0 { -y } else { y };
    let (a, swap) = if ay > ax { (ax / ay, true) } else { (ay / ax, false) };
    // atan(a) on [0,1].
    let s = a * a;
    let mut r = ((-0.046_496_475 * s + 0.159_314_22) * s - 0.327_622_76) * s * a + a;
    if swap {
        r = PI / 2.0 - r;
    }
    if x < 0.0 {
        r = PI - r;
    }
    if y < 0.0 { -r } else { r }
}

pub fn floor(x: f32) -> f32 {
    let t = x as i64 as f32;
    if t > x { t - 1.0 } else { t }
}

pub fn clamp01(x: f32) -> f32 {
    if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }
}

pub fn abs(x: f32) -> f32 {
    if x < 0.0 { -x } else { x }
}

pub fn min(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

pub fn max(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

/// Hermite smoothstep between edges `e0` and `e1`.
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp01((x - e0) / (e1 - e0));
    t * t * (3.0 - 2.0 * t)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Small deterministic PRNG (xorshift64*), for starfields and particles.
#[derive(Clone, Copy, Debug)]
pub struct Rng(u64);

impl Rng {
    pub const fn new(seed: u64) -> Rng {
        Rng(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqrt_is_accurate() {
        for &x in &[0.0001f32, 0.25, 1.0, 2.0, 9.0, 1234.5, 1.0e6, 3.0e9] {
            let r = sqrt(x);
            assert!(abs(r * r - x) / x < 1e-5, "{x} -> {r}");
        }
        assert_eq!(sqrt(-4.0), 0.0);
        assert_eq!(sqrt(0.0), 0.0);
    }

    #[test]
    fn sin_and_cos_are_accurate() {
        let mut x = -20.0f32;
        while x < 20.0 {
            let want = (x as f64).sin() as f32;
            assert!(abs(sin(x) - want) < 0.001, "sin({x}) = {} want {want}", sin(x));
            let wc = (x as f64).cos() as f32;
            assert!(abs(cos(x) - wc) < 0.001, "cos({x})");
            x += 0.0137;
        }
    }

    #[test]
    fn atan2_is_accurate() {
        let mut a = -3.1f32;
        while a < 3.1 {
            let (y, x) = ((a as f64).sin() as f32 * 7.0, (a as f64).cos() as f32 * 7.0);
            assert!(abs(atan2(y, x) - a) < 0.005, "atan2 at {a}: {}", atan2(y, x));
            a += 0.01;
        }
        assert_eq!(atan2(0.0, 0.0), 0.0);
    }

    #[test]
    fn floor_handles_negatives() {
        assert_eq!(floor(1.5), 1.0);
        assert_eq!(floor(-1.5), -2.0);
        assert_eq!(floor(-2.0), -2.0);
    }

    #[test]
    fn smoothstep_endpoints() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rng_is_deterministic_and_in_range() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            let u = a.unit();
            assert_eq!(u, b.unit());
            assert!((0.0..1.0).contains(&u));
        }
    }
}
