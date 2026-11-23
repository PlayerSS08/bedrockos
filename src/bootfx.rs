//! The animated boot screen on real hardware: owns the back buffer, turns real boot stages into the status
//! line and progress bar, paces frames at about 30 per second, and copies only changed rectangles to the
//! framebuffer (drawing happens off-screen, so nothing flickers). The picture itself is `bootscene` (pure,
//! host-tested).
//!
//! Boot is single-threaded until the scheduler runs, so frames are drawn from the boot path itself: each
//! `stage` call lets the bar glide to its new value (at most about 90 ms), and the boot menu loop draws a
//! frame on every pass. Long driver waits inside one stage (such as USB port settling) hold the last frame.
//!
//! Esc or V while the animation runs switches to the scrolling boot log (like Plymouth versus a text boot).

use crate::anim::{self, Smoother};
use crate::arch;
use crate::bootinfo::FbInfo;
use crate::bootscene::{Dirty, MenuView, Scene, View};
use crate::global::Global;
use crate::gui::canvas::{Canvas, Rect};
use crate::log;
use crate::logfmt::Level;
use crate::pixel::PixelFormat;

/// Largest screen the back buffer holds (16 MiB). Bigger screens keep the plain text boot.
pub const MAX_W: usize = 2560;
pub const MAX_H: usize = 1600;

static BACK: Global<[u32; MAX_W * MAX_H]> = Global::new([0; MAX_W * MAX_H]);
static DIRTY: Global<Dirty> = Global::new(Dirty::new());

const FRAME_US: u64 = 33_000;
const INTRO_MS: u32 = 900;
const STATUS_MAX: usize = 96;

struct State {
    active: bool,
    fb: Option<FbInfo>,
    scene: Option<Scene>,
    t0: u64,
    last_frame: u64,
    drawn_once: bool,
    bar: Smoother,
    status: [u8; STATUS_MAX],
    status_len: usize,
    hint: &'static str,
    key_poll: Option<fn() -> bool>,
    frames: u32,
}

static STATE: Global<State> = Global::new(State {
    active: false,
    fb: None,
    scene: None,
    t0: 0,
    last_frame: 0,
    drawn_once: false,
    bar: Smoother::new(),
    status: [0; STATUS_MAX],
    status_len: 0,
    hint: "",
    key_poll: None,
    frames: 0,
});

fn state() -> &'static mut State {
    // SAFETY: the boot screen only runs on the boot path before the scheduler starts (single CPU, one
    // thread), and every function here takes the reference only for its own duration.
    unsafe { STATE.get() }
}

fn us_since(t: u64) -> u64 {
    let hz = log::tsc_hz().max(1);
    arch::rdtsc().wrapping_sub(t) / (hz / 1_000_000).max(1)
}

/// Starts the animation on `fb`. Returns false (and nothing changes) for screens larger than the back buffer.
pub fn start(fb: &FbInfo, seed: u64) -> bool {
    if fb.width > MAX_W || fb.height > MAX_H || fb.width < 640 || fb.height < 480 || fb.stride < fb.width {
        return false;
    }
    let s = state();
    s.fb = Some(*fb);
    s.scene = Some(Scene::new(fb.width, fb.height, seed));
    s.t0 = arch::rdtsc();
    s.last_frame = 0;
    s.drawn_once = false;
    s.bar = Smoother::new();
    s.active = true;
    s.hint = "Esc or V: show boot messages";
    set_status("Firmware hand-off");
    render(true, None);
    true
}

pub fn active() -> bool {
    state().active
}

/// Frames drawn so far (for the boot log).
pub fn frames() -> u32 {
    state().frames
}

/// A function that returns true when the reader pressed Esc or V. Set before and after ExitBootServices
/// (firmware keyboard, then the PS/2 controller).
pub fn set_key_poll(f: Option<fn() -> bool>) {
    state().key_poll = f;
}

fn set_status(text: &str) {
    let s = state();
    let b = text.as_bytes();
    let n = b.len().min(STATUS_MAX);
    s.status[..n].copy_from_slice(&b[..n]);
    s.status_len = n;
}

/// A real boot stage started: shows `label` and moves the bar toward `permille`, drawing frames while it
/// glides (bounded to about 90 ms, so the boot is never held back for show).
pub fn stage(label: &str, permille: u32) {
    if !active() {
        return;
    }
    set_status(label);
    state().bar.target(permille);
    let t = arch::rdtsc();
    while active() && state().bar.shown() < permille.min(1000) && us_since(t) < 90_000 {
        pump();
        arch::pause();
    }
    pump();
}

/// Draws a frame if one is due (about 30 per second). Cheap to call often.
pub fn pump() {
    if !active() {
        return;
    }
    if let Some(f) = state().key_poll {
        if f() {
            to_log("boot screen: switched to boot messages (Esc/V)");
            return;
        }
    }
    let s = state();
    if s.drawn_once && us_since(s.last_frame) < FRAME_US {
        return;
    }
    render(false, None);
}

/// Draws the boot menu over the animation (a frame is drawn when due, or at once with `force`).
pub fn menu_frame(view: &MenuView, force: bool) {
    if !active() {
        return;
    }
    let s = state();
    if !force && s.drawn_once && us_since(s.last_frame) < FRAME_US {
        return;
    }
    if let Some(sc) = s.scene.as_mut() {
        sc.set_menu_items(view.items.len());
    }
    render(false, Some(view));
}

/// The boot is done: the bar fills, holds briefly, and the screen fades to black (about 0.5 s in all).
pub fn finish() {
    if !active() {
        return;
    }
    stage("Starting desktop", 1000);
    let t = arch::rdtsc();
    while us_since(t) < 150_000 {
        pump();
        arch::pause();
    }
    let t = arch::rdtsc();
    loop {
        let e = us_since(t);
        if e >= 350_000 {
            break;
        }
        let f = 1.0 - f32::from(anim::ease_in_out((e / 350) as u16)) / 1000.0;
        render_faded(f);
        // Fades redraw the whole screen; pace them like normal frames.
        while us_since(state().last_frame) < FRAME_US / 2 {
            arch::pause();
        }
    }
    render_faded(0.0);
    let n = state().frames;
    state().active = false;
    let _ = log::with_console(|c| c.release_to_log());
    crate::info!("bootfx", "boot animation done: {} frames", n);
}

/// Leaves the animation for the scrolling log (verbose key, recovery mode): everything logged so far is shown.
pub fn to_log(reason: &str) {
    if !active() {
        return;
    }
    state().active = false;
    let _ = log::with_console(|c| c.release_to_log());
    log::replay(|bytes| {
        let _ = log::with_console(|c| c.write_line(Level::Info, bytes));
    });
    crate::info!("bootfx", "{}", reason);
}

fn render_faded(fade: f32) {
    render_view(false, None, fade);
}

fn render(full: bool, menu: Option<&MenuView>) {
    render_view(full, menu, 1.0);
}

fn render_view(full: bool, menu: Option<&MenuView>, fade: f32) {
    let s = state();
    let Some(fb) = s.fb else { return };
    let now = arch::rdtsc();
    let t_ms = (us_since(s.t0) / 1000).min(u64::from(u32::MAX)) as u32;
    let dt = if s.drawn_once { us_since(s.last_frame) } else { 0 };
    let shown = s.bar.step(dt);
    s.last_frame = now;
    let full = full || !s.drawn_once;
    s.drawn_once = true;
    s.frames = s.frames.wrapping_add(1);
    let intro = f32::from(anim::ease_out_cubic(((t_ms.min(INTRO_MS) * 1000) / INTRO_MS) as u16)) / 1000.0;
    let status = core::str::from_utf8(&s.status[..s.status_len]).unwrap_or("");
    let view = View {
        t_ms,
        intro,
        progress: shown as f32 / 1000.0,
        status,
        show_bar: true,
        menu: menu.copied(),
        hint: if menu.is_some() { "" } else { s.hint },
        fade,
    };
    let (w, h) = (fb.width, fb.height);
    // SAFETY: single-threaded boot path (see `state`); BACK and DIRTY are only used here.
    let (back, dirty) = unsafe { (BACK.get(), DIRTY.get()) };
    let Some(scene) = s.scene.as_mut() else { return };
    {
        let Some(mut c) = Canvas::new(&mut back[..w * h], w, h, w) else { return };
        scene.frame(&mut c, &view, full, dirty);
    }
    let drawn = arch::rdtsc();
    for r in dirty.as_slice() {
        blit(&fb, &back[..w * h], w, *r);
    }
    if s.frames <= 3 || s.frames % 16 == 0 {
        crate::debug!(
            "bootfx",
            "frame {} full={} rects={} area={} draw_us={} blit_us={}",
            s.frames,
            full,
            dirty.as_slice().len(),
            dirty.area(),
            us_between(now, drawn),
            us_since(drawn)
        );
    }
}

fn us_between(a: u64, b: u64) -> u64 {
    let hz = log::tsc_hz().max(1);
    b.wrapping_sub(a) / (hz / 1_000_000).max(1)
}

/// Copies rectangle `r` of the back buffer (`0x00RRGGBB`, `w` pixels per row) to the framebuffer.
fn blit(fb: &FbInfo, back: &[u32], w: usize, r: Rect) {
    let r = r.intersect(&Rect::new(0, 0, fb.width as i32, fb.height as i32));
    if r.is_empty() {
        return;
    }
    let (x, rw) = (r.x as usize, r.w as usize);
    for y in r.y as usize..r.bottom() as usize {
        let src = &back[y * w + x..y * w + x + rw];
        // SAFETY: the framebuffer holds `stride * height` pixels (checked when the console was set up), it is
        // identity-mapped before and after ExitBootServices, and `x + rw <= width <= stride`, `y < height`.
        let dst = unsafe { core::slice::from_raw_parts_mut((fb.base as *mut u32).add(y * fb.stride + x), rw) };
        match fb.format {
            // 0x00RRGGBB in memory is B, G, R, 0: the framebuffer's own order.
            PixelFormat::Bgr => dst.copy_from_slice(src),
            f => {
                for (d, &p) in dst.iter_mut().zip(src) {
                    *d = f.pack((p >> 16) as u8, (p >> 8) as u8, p as u8);
                }
            }
        }
    }
}

/// Esc (scancode 0x01) or V (0x2F) on the PS/2 keyboard, read straight from the controller. Used after
/// ExitBootServices, before the keyboard driver runs. Other keys are dropped.
pub fn ps2_verbose_key() -> bool {
    // SAFETY: reading the i8042 status and data ports has no side effect beyond taking the pending byte.
    unsafe {
        let st = arch::inb(0x64);
        if st & 0x01 != 0 && st & 0x20 == 0 {
            let sc = arch::inb(0x60);
            return sc == 0x01 || sc == 0x2F;
        }
    }
    false
}
