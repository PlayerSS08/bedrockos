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
use crate::apps::{parse_tasks, Effect, Fonts, Help, Notes, Palette, Pick, Settings, Tasks, NOTES_DIR, TASKS_FILE};
use crate::apps2::{Calculator, Clock, Files, FilesReq, Snake};
use crate::browser::Browser;
use crate::desktop::{self, DeskLayout, APPS, BROWSER, CALC, CHATBOX, CLOCK, FILES, HELP, NETWORK, NOTES, SETTINGS, SNAKE, TASKS};
use crate::keymap::Key;
use crate::wififly::{self, WifiFly, WifiReq};
use crate::wm::Wm;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use crate::pointer::{self, Pointer, Texel};
use crate::fb::Console;
use crate::global::Global;
use crate::gui::canvas::{Canvas, Rect};
use crate::gui::wallimg;
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
/// How long a panel message stays up.
const MESSAGE_US: u64 = 3_000_000;

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
    /// The mouse pointer on the desktop (screen coordinates), once a mouse has moved.
    dp: Option<Pointer>,
    /// Where the desktop last painted the pointer (None while the console shows it).
    dp_shown: Option<(i32, i32)>,
    /// Buttons of the previous mouse event (to see presses and releases).
    dp_buttons: u8,
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
    dp: None,
    dp_shown: None,
    dp_buttons: 0,
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
    {
        let (w, h) = (fb.width, fb.height);
        // SAFETY: single-threaded boot path; BOOT_BG is written here once and only read afterwards.
        let bg = unsafe { BOOT_BG.get() };
        if picture_into(wallimg::BOOT, &mut bg[..w * h], w, h, 200) {
            // SAFETY: as above; the slice is never written again.
            let px: &'static [u32] = unsafe { core::slice::from_raw_parts(BOOT_BG.as_ptr() as *const u32, w * h) };
            if let Some(sc) = s.scene.as_mut() {
                sc.set_background(px);
            }
        }
    }
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

/// Redraws the whole screen on the next frame (after the firmware may have drawn over it).
pub fn repaint() {
    if active() {
        render(true, None);
    }
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

/// Draws the sign-in card over the animation (a frame is drawn when due, or at once with `force`).
pub fn login_frame(view: &crate::bootscene::LoginView, force: bool) {
    if !active() {
        return;
    }
    let s = state();
    if !force && s.drawn_once && us_since(s.last_frame) < FRAME_US {
        return;
    }
    if let Some(sc) = s.scene.as_mut() {
        sc.set_menu_items(5);
    }
    render_view(false, None, Some(view), 1.0);
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
    crate::info!("bootfx", "boot animation done: {} frames", n);
    if !enter_desktop() {
        let _ = log::with_console(|c| c.release_to_log());
    }
}

// ---- the desktop: compositor and window manager --------------------------------------------------------------
//
// After boot the console no longer draws on the screen: it draws into TERMBUF. The screen is composed in BACK
// from the wallpaper, the panel, the open windows bottom to top (ChatBox's body is copied from TERMBUF, the other
// apps draw themselves) and the dock; then only the changed rectangle is copied to the framebuffer, and the
// pointer is painted on top. Everything runs with interrupts off (the shell task calls in for the mouse and
// keys, the status task for console changes, the clock and auto-save).

/// The console's own pixels while the desktop runs (`term.w` pixels per row).
static TERMBUF: Global<[u32; MAX_W * MAX_H]> = Global::new([0; MAX_W * MAX_H]);
/// The wallpaper and empty panel, drawn once (copying it is much cheaper than drawing the glows again).
static BACKDROP: Global<[u32; MAX_W * MAX_H]> = Global::new([0; MAX_W * MAX_H]);
/// A decoded background picture (before scaling), and the boot screen's background at screen size.
static PICTURE: Global<[u32; wallimg::SRC_PIXELS]> = Global::new([0; wallimg::SRC_PIXELS]);
static BOOT_BG: Global<[u32; MAX_W * MAX_H]> = Global::new([0; MAX_W * MAX_H]);

/// Decodes background picture `qoi` and scales it to cover `dst` (`w` x `h`). False if it does not decode.
fn picture_into(qoi: &[u8], dst: &mut [u32], w: usize, h: usize, bright: u32) -> bool {
    // SAFETY: single-threaded boot path; PICTURE is only used here, one call at a time.
    let src = unsafe { PICTURE.get() };
    match wallimg::qoi_decode(qoi, &mut src[..]) {
        Some((sw, sh)) => {
            wallimg::cover(&src[..sw * sh], sw, sh, dst, w, h, bright);
            true
        }
        None => false,
    }
}

struct Desk {
    l: DeskLayout,
    wm: Wm,
    notes: Option<Notes>,
    tasks: Option<Tasks>,
    help: Help,
    settings: Settings,
    files: Files,
    browser: Browser,
    calc: Calculator,
    clock: Clock,
    snake: Snake,
    /// Last Snake step and last stopwatch redraw (TSC).
    snake_at: u64,
    watch_at: u64,
    /// The hover tip shown under the top bar: which spot (0 network, 1 battery, 2 clock) and where it is.
    tip: Option<(usize, Rect)>,
    /// The Wi-Fi drop-down under the network icon, while open.
    wifi: Option<WifiFly>,
    /// The achievement toast on screen and when it appeared.
    toast: Option<(&'static str, u64)>,
    /// Wallpaper brightness (day and night), see [`daylight`].
    bright: u32,
    /// The command palette, while open (Ctrl+Space).
    palette: Option<Palette>,
    /// Size of each maximised window before it was maximised.
    maxed: [Option<Rect>; crate::wm::N],
    /// Last time the live Settings page was refreshed (TSC).
    settings_at: u64,
    /// Panel message and when it was shown (TSC).
    msg: String,
    msg_at: Option<u64>,
    /// Minute shown by the panel clock.
    clock_min: u32,
    /// While a window is dragged: which, and where it was grabbed relative to its corner.
    drag: Option<(usize, i32, i32)>,
    drag_drawn: u64,
    /// When notes / tasks last changed and are not saved yet (TSC).
    notes_dirty: Option<u64>,
    tasks_dirty: Option<u64>,
    /// Following the VirtualBox window's size: whether the guest device answered, and the last poll (TSC).
    follow_window: bool,
    size_at: u64,
}

static DESK: Global<Option<Desk>> = Global::new(None);
/// Set when a file changed and the shell should save the RAM file system to the USB stick.
static PERSIST: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Notes and tasks are written to the file system this long after the last key.
const AUTOSAVE_US: u64 = 1_200_000;

/// Only with interrupts off (see the section comment).
fn desk() -> Option<&'static mut Desk> {
    // SAFETY: every caller runs inside `arch::without_interrupts`, on the only CPU, and does not keep the
    // reference past that section.
    unsafe { DESK.get() }.as_mut()
}

/// Draws the desktop, fades it in (about 0.3 s) and gives the text console its off-screen buffer, shown in
/// the ChatBox window. Returns false (nothing changed) when the window is too small for a console.
fn enter_desktop() -> bool {
    let s = state();
    let Some(fb) = s.fb else { return false };
    let (w, h) = (fb.width, fb.height);
    let l = DeskLayout::new(w, h);
    let t = l.term;
    // SAFETY: single-threaded boot path; nothing else uses TERMBUF yet.
    let term = unsafe { TERMBUF.get() };
    let info = FbInfo {
        base: term.as_mut_ptr() as u64,
        size: t.w as usize * t.h as usize * 4,
        width: t.w as usize,
        height: t.h as usize,
        stride: t.w as usize,
        // 0x00RRGGBB, the same as the canvas, so the compositor copies rows unchanged.
        format: PixelFormat::Bgr,
    };
    // SAFETY: TERMBUF is a static, mapped and writable, and holds MAX_W * MAX_H >= w * h pixels; only the
    // console writes it from now on.
    let Some(mut con) = (unsafe { Console::new(&info) }) else {
        crate::warn!("desktop", "ChatBox text area {}x{} too small for the console", t.w, t.h);
        return false;
    };
    con.clear();
    con.fullscreen_log();
    let mut wm = Wm::new(l.app_rects(), l.area());
    wm.show(CHATBOX);
    // SAFETY: single-threaded boot path (see `state`).
    unsafe {
        *DESK.get() = Some(Desk {
            l,
            wm,
            notes: None,
            tasks: None,
            help: Help::new(),
            settings: Settings::default(),
            files: Files::default(),
            browser: Browser::default(),
            calc: Calculator::default(),
            clock: Clock::default(),
            snake: Snake::default(),
            snake_at: 0,
            watch_at: 0,
            maxed: [None; crate::wm::N],
            palette: None,
            tip: None,
            wifi: None,
            toast: None,
            bright: daylight(),
            settings_at: 0,
            msg: String::new(),
            msg_at: None,
            clock_min: u32::MAX,
            drag: None,
            drag_drawn: 0,
            notes_dirty: None,
            tasks_dirty: None,
            follow_window: false,
            size_at: 0,
        });
    }
    let full = Rect::new(0, 0, w as i32, h as i32);
    {
        // SAFETY: single-threaded boot path; BACKDROP is only read from now on.
        let bd = unsafe { BACKDROP.get() };
        let picture = picture_into(wallimg::DESK, &mut bd[..w * h], w, h, daylight());
        let Some(mut c) = Canvas::new(&mut bd[..w * h], w, h, w) else { return false };
        if picture {
            desktop::draw_backdrop_over_picture(&mut c, &l);
        } else {
            desktop::draw_backdrop(&mut c, &l);
        }
    }
    let Some(d) = desk() else { return false };
    compose(d, &fb, full);
    // SAFETY: single-threaded boot path.
    let back = unsafe { BACK.get() };
    let t0 = arch::rdtsc();
    loop {
        let e = us_since(t0);
        if e >= 300_000 {
            break;
        }
        let k = u32::from(anim::ease_in_out((e / 300) as u16)) * 256 / 1000;
        blit_dim(&fb, &back[..w * h], w, full, k);
    }
    blit(&fb, &back[..w * h], w, full);
    log::attach_console(con);
    crate::info!("desktop", "desktop shown; ChatBox window, text area {}x{} at ({}, {})", t.w, t.h, t.x, t.y);
    let follow = crate::vmdisplay::init();
    if let Some(d) = desk() {
        d.follow_window = follow;
    }
    true
}

/// Wallpaper brightness for the local time of day, like Minecraft's day and night: full in the day, dim at
/// night, with an hour-long sunrise (6:00-7:00) and sunset (18:00-19:00). In steps of 10, so it changes
/// only every few minutes.
fn daylight() -> u32 {
    crate::tz::local_now().map_or(240, |t| daylight_at(u32::from(t.hour) * 60 + u32::from(t.min)))
}

/// See [`daylight`]; `min` is minutes after midnight.
pub fn daylight_at(min: u32) -> u32 {
    const DAY: u32 = 240;
    const NIGHT: u32 = 110;
    let v = match min {
        0..=359 | 1140.. => NIGHT,
        360..=419 => NIGHT + (DAY - NIGHT) * (min - 360) / 60,
        420..=1079 => DAY,
        _ => DAY - (DAY - NIGHT) * (min - 1080) / 60,
    };
    v / 10 * 10
}

/// Draws the wallpaper picture (or the gradient) and the empty panel into BACKDROP for the current layout.
fn draw_backdrop_buffer(l: &DeskLayout, w: usize, h: usize) {
    // SAFETY: BACKDROP is only written here (with interrupts off, or on the boot path) and read by `compose`.
    let bd = unsafe { BACKDROP.get() };
    let picture = picture_into(wallimg::DESK, &mut bd[..w * h], w, h, daylight());
    let Some(mut c) = Canvas::new(&mut bd[..w * h], w, h, w) else { return };
    if picture {
        desktop::draw_backdrop_over_picture(&mut c, l);
    } else {
        desktop::draw_backdrop(&mut c, l);
    }
}

/// Switches the screen to `w` x `h` and lays the desktop out again: wallpaper, panel, dock, the open windows
/// at their default places for that size, and ChatBox's text grid. Apps keep their contents.
fn resize_desktop(d: &mut Desk, w: usize, h: usize) -> bool {
    let s = state();
    let Some(mut fb) = s.fb else { return false };
    let Some(stride) = crate::vmdisplay::set_mode(w, h) else {
        crate::warn!("desktop", "the graphics device refused {}x{}", w, h);
        return false;
    };
    fb.width = w;
    fb.height = h;
    fb.stride = stride;
    fb.size = stride * h * 4;
    s.fb = Some(fb);
    s.dp = None;
    s.dp_shown = None;
    let l = DeskLayout::new(w, h);
    let mut wm = Wm::new(l.app_rects(), l.area());
    for &a in d.wm.order() {
        wm.show(a);
    }
    d.wm = wm;
    d.l = l;
    d.maxed = [None; crate::wm::N];
    d.tip = None;
    d.wifi = None;
    d.palette = None;
    d.drag = None;
    let t = d.l.term_in(d.wm.rect(CHATBOX));
    let _ = log::with_console(|c| {
        // SAFETY: the console draws into TERMBUF, which holds MAX_W * MAX_H pixels, more than any text area.
        unsafe { c.resize(t.w as usize, t.h as usize) }
    });
    let _ = crate::fb::take_dirty();
    draw_backdrop_buffer(&d.l, w, h);
    let full = Rect::new(0, 0, w as i32, h as i32);
    compose(d, &fb, full);
    // SAFETY: BACK is only used with interrupts off after boot.
    let back = unsafe { BACK.get() };
    blit(&fb, &back[..w * h], w, full);
    crate::info!("desktop", "screen resized to {}x{} to follow the VirtualBox window", w, h);
    true
}

fn clock_now(buf: &mut [u8; 28]) -> &str {
    match crate::tz::local_now() {
        Some(t) => desktop::clock_text(buf, t.weekday_name(), t.day, t.month_name(), t.hour, t.min, &crate::tz::names(crate::tz::offset()).0),
        None => "--:--",
    }
}

/// Draws the scene clipped to `r` into BACK (no copy to the screen).
fn compose(d: &mut Desk, fb: &FbInfo, r: Rect) {
    let (w, h) = (fb.width, fb.height);
    // SAFETY: BACK and TERMBUF are only used with interrupts off after boot (see the section comment); the
    // console may be writing TERMBUF from another task, which is harmless (a torn row is redrawn next tick).
    let back = unsafe { BACK.get() };
    // SAFETY: TERMBUF is a static of MAX_W * MAX_H pixels; it is only read here (the console writes it through
    // its own raw pointer).
    let term = unsafe { core::slice::from_raw_parts(TERMBUF.as_ptr() as *const u32, MAX_W * MAX_H) };
    let Some(mut c) = Canvas::new(&mut back[..w * h], w, h, w) else { return };
    c.set_clip(r);
    let mut cbuf = [0u8; 28];
    let clock = clock_now(&mut cbuf);
    // SAFETY: BACKDROP is written once in `enter_desktop` and only read afterwards.
    let bd = unsafe { core::slice::from_raw_parts(BACKDROP.as_ptr() as *const u32, MAX_W * MAX_H) };
    c.copy_from(0, 0, w as i32, h as i32, bd, w);
    desktop::draw_panel_text(&mut c, &d.l, clock, &d.msg);
    let focused = d.wm.focused();
    let f = Fonts::for_screen(d.l.big());
    let order: Vec<usize> = d.wm.order().to_vec();
    for app in order {
        let win = d.wm.rect(app);
        let on_top = focused == Some(app);
        desktop::draw_window(&mut c, &d.l, win, app, on_top);
        let body = desktop::app_body(win, d.l.title_h);
        let old = c.clip();
        c.set_clip(old.intersect(&body));
        if !c.clip().is_empty() {
            match app {
                CHATBOX => {
                    let t = d.l.term_in(win);
                    c.copy_from(t.x, t.y, t.w, t.h, term, t.w as usize);
                }
                NOTES => {
                    if let Some(n) = d.notes.as_mut() {
                        n.draw(&mut c, body, &f, on_top);
                    }
                }
                TASKS => {
                    if let Some(t) = d.tasks.as_ref() {
                        t.draw(&mut c, body, &f, on_top);
                    }
                }
                HELP => d.help.draw(&mut c, body, &f, on_top),
                SETTINGS => d.settings.draw(&mut c, body, &f, &settings_lines(d.settings.page, fb)),
                FILES => d.files.draw(&mut c, body, &f),
                BROWSER => d.browser.draw(&mut c, body, &f),
                NETWORK => crate::apps2::draw_network(&mut c, body, &f, crate::netdev::link_up(), &settings_lines(1, fb)[1..]),
                CALC => d.calc.draw(&mut c, body, &f),
                CLOCK => {
                    let utc = crate::rtc::now().map_or(0, |t| t.unix_seconds());
                    let zone = crate::tz::names(crate::tz::offset()).0;
                    d.clock.draw(&mut c, body, &f, (utc, crate::tz::offset(), &zone), log::uptime_us());
                }
                SNAKE => d.snake.draw(&mut c, body, &f),
                _ => {}
            }
        }
        c.set_clip(old);
    }
    if let Some(p) = d.palette.as_ref() {
        let pr = Palette::rect(w as i32, d.l.panel.h, &f);
        p.draw(&mut c, pr, &f);
    }
    if let Some((kind, _)) = d.tip {
        let (lines, tr) = tip_for(d, kind);
        desktop::draw_tip(&mut c, &d.l, tr, &lines);
    }
    if let Some(wf) = d.wifi.as_ref() {
        wf.draw(&mut c, wifi_rect(d), &f);
    }
    if let Some((title, _)) = d.toast {
        desktop::draw_toast(&mut c, &d.l, title);
    }
    let mut open = [false; APPS.len()];
    for &a in d.wm.order() {
        open[a] = true;
    }
    desktop::draw_dock(&mut c, &d.l, &open);
}

/// Redraws rectangle `r` of the screen: composes it, copies it out, puts the pointer back on top.
fn redraw(d: &mut Desk, fb: &FbInfo, r: Rect) {
    let r = r.intersect(&Rect::new(0, 0, fb.width as i32, fb.height as i32));
    if r.is_empty() {
        return;
    }
    compose(d, fb, r);
    // SAFETY: as in `compose`.
    let back = unsafe { BACK.get() };
    blit(fb, &back[..fb.width * fb.height], fb.width, r);
    if let Some((x, y)) = state().dp_shown {
        if !r.intersect(&Rect::new(x, y, pointer::SPRITE_W as i32, pointer::SPRITE_H as i32)).is_empty() {
            sprite(fb, x, y);
        }
    }
}

/// A window and its shadow.
fn win_area(d: &Desk, app: usize) -> Rect {
    d.wm.rect(app).inflate(desktop::SHADOW)
}

/// Keeps the desktop current; called about 30 times a second by the status task: copies what the console
/// drew, updates the clock, clears an old panel message, and auto-saves notes and tasks.
pub fn desk_tick() {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return };
        // Twice a second: does the VirtualBox window want another screen size?
        if d.follow_window && d.drag.is_none() && us_since(d.size_at) >= 500_000 {
            d.size_at = arch::rdtsc();
            if let Some((w, h)) = crate::vmdisplay::wanted().and_then(|(w, h)| crate::vmdisplay::fit(w, h, (MAX_W, MAX_H))) {
                if (w, h) != (fb.width, fb.height) && resize_desktop(d, w, h) {
                    return;
                }
            }
        }
        console_to_screen(d, &fb);
        if d.msg_at.is_some_and(|t| us_since(t) >= MESSAGE_US) {
            d.msg.clear();
            d.msg_at = None;
            let r = d.l.message;
            redraw(d, &fb, r);
        }
        let min = crate::rtc::now().map_or(0, |t| u32::from(t.hour) * 60 + u32::from(t.min));
        if min != d.clock_min {
            d.clock_min = min;
            let r = d.l.clock;
            redraw(d, &fb, r);
            // Day and night: the wallpaper follows the time of day.
            let b = daylight();
            if b != d.bright {
                d.bright = b;
                draw_backdrop_buffer(&d.l, fb.width, fb.height);
                redraw(d, &fb, Rect::new(0, 0, fb.width as i32, fb.height as i32));
            }
        }
        // Achievement toasts: each shows for 4 s, then the next one waiting.
        if d.toast.is_some_and(|(_, at)| us_since(at) >= 4_000_000) {
            d.toast = None;
            let r = desktop::toast_rect(&d.l).inflate(desktop::SHADOW);
            redraw(d, &fb, r);
        }
        if d.toast.is_none() {
            if let Some(t) = crate::achieve::next_toast() {
                d.toast = Some((t, arch::rdtsc()));
                let r = desktop::toast_rect(&d.l).inflate(desktop::SHADOW);
                redraw(d, &fb, r);
            }
        }
        let up = crate::netdev::link_up();
        if desktop::NET_UP.swap(up, core::sync::atomic::Ordering::Relaxed) != up {
            let r = d.l.clock;
            redraw(d, &fb, r);
        }
        // Snake steps every 110 ms while its window is in front; a running stopwatch redraws 10 times a second.
        if d.wm.focused() == Some(SNAKE) && us_since(d.snake_at) >= 110_000 {
            d.snake_at = arch::rdtsc();
            if d.snake.score >= 5 {
                crate::achieve::unlock(crate::achieve::Id::Snake);
            }
            if d.snake.tick() {
                let r = desktop::app_body(d.wm.rect(SNAKE), d.l.title_h);
                redraw(d, &fb, r);
            }
        }
        if d.wm.is_open(CLOCK) && d.clock.running() && us_since(d.watch_at) >= 100_000 {
            d.watch_at = arch::rdtsc();
            let r = desktop::app_body(d.wm.rect(CLOCK), d.l.title_h);
            redraw(d, &fb, r);
        }
        // Every second: battery, hover tip, and the Settings window's live values (uptime, memory, tasks).
        if us_since(d.settings_at) >= 1_000_000 {
            d.settings_at = arch::rdtsc();
            second_tick(d, &fb);
            if d.wm.is_open(SETTINGS) {
                let r = desktop::app_body(d.wm.rect(SETTINGS), d.l.title_h);
                redraw(d, &fb, r);
            }
        }
        if d.notes_dirty.is_some_and(|t| us_since(t) >= AUTOSAVE_US) {
            d.notes_dirty = None;
            if let Some(n) = d.notes.as_mut() {
                save_notes(n);
            }
            let body = desktop::app_body(d.wm.rect(NOTES), d.l.title_h);
            if d.wm.is_open(NOTES) {
                redraw(d, &fb, body);
            }
        }
        if d.tasks_dirty.is_some_and(|t| us_since(t) >= AUTOSAVE_US) {
            d.tasks_dirty = None;
            if let Some(t) = d.tasks.as_ref() {
                let _ = crate::shell::fs().write(TASKS_FILE, &t.to_text());
                PERSIST.store(true, core::sync::atomic::Ordering::Relaxed);
            }
        }
    });
}

/// Shows what the console drew since last time. When ChatBox is in front nothing covers its text, so the
/// changed rows are copied straight from TERMBUF (fast); otherwise they are composed with the windows above.
fn console_to_screen(d: &mut Desk, fb: &FbInfo) {
    let Some((a, b)) = crate::fb::take_dirty() else { return };
    if !d.wm.is_open(CHATBOX) {
        return;
    }
    let t = d.l.term_in(d.wm.rect(CHATBOX));
    let r = Rect::new(t.x, t.y + a as i32, t.w, (b - a + 1) as i32).intersect(&Rect::new(0, 0, fb.width as i32, fb.height as i32));
    if d.wm.focused() != Some(CHATBOX) || d.palette.is_some() {
        redraw(d, fb, r);
        return;
    }
    let (w, h) = (fb.width, fb.height);
    // SAFETY: as in `compose`.
    let back = unsafe { BACK.get() };
    // SAFETY: TERMBUF holds MAX_W * MAX_H pixels and is only read here.
    let term = unsafe { core::slice::from_raw_parts(TERMBUF.as_ptr() as *const u32, MAX_W * MAX_H) };
    if let Some(mut c) = Canvas::new(&mut back[..w * h], w, h, w) {
        c.set_clip(r);
        c.copy_from(t.x, t.y, t.w, t.h, term, t.w as usize);
    }
    blit(fb, &back[..w * h], w, r);
    if let Some((x, y)) = state().dp_shown {
        if !r.intersect(&Rect::new(x, y, pointer::SPRITE_W as i32, pointer::SPRITE_H as i32)).is_empty() {
            sprite(fb, x, y);
        }
    }
}

/// Shows console output right away (the shell calls this after handling keys, so typing feels immediate).
pub fn desk_flush() {
    arch::without_interrupts(|| {
        if let (Some(d), Some(fb)) = (desk(), state().fb) {
            console_to_screen(d, &fb);
        }
    });
}

/// The text of Settings page `page` (see `apps::SETTINGS_PAGES`), with live values.
fn settings_lines(page: usize, fb: &FbInfo) -> Vec<String> {
    let mut v = Vec::new();
    let mut push = |s: String| v.push(s);
    match page {
        0 => {
            push(format!("# BedrockOS {}", crate::VERSION));
            push(String::from("A small Rust operating system for UEFI PCs and VirtualBox."));
            push(String::from("Desktop: ChatBox (shell), Notes, Tasks, Help, Settings."));
            push(String::from("# Licences"));
            push(String::from("Based on Bedrock 0.11.0."));
            push(String::from("Fonts: Inter and JetBrains Mono, SIL Open Font License 1.1."));
        }
        1 => {
            let (ip, mask, gw, dns, dhcp) = crate::netstack::get_config();
            let up = crate::netdev::link_up();
            push(String::from("# Host connection (laptop Wi-Fi)"));
            push(format!("State: {}", if up { "connected" } else { "no link" }));
            push(format!("Address: {} / {} ({})", ip, mask, if dhcp { "DHCP" } else { "static" }));
            push(format!("Gateway: {}    DNS: {}", gw, dns));
            push(String::from("VirtualBox passes the laptop's connection to this network card;"));
            push(String::from("the laptop chooses the Wi-Fi network in this mode."));
            push(String::from("# OS Wi-Fi"));
            push(String::from("No supported Wi-Fi chip yet (milestone 8)."));
            push(String::from("Type `nettest` in ChatBox for a full check."));
        }
        2 => {
            let s = log::uptime_us() / 1_000_000;
            let f = crate::mm::frame_stats();
            let h = crate::mm::heap_stats();
            push(String::from("# System"));
            push(format!("Uptime: {:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60));
            push(format!("Memory: {} MB free of {} MB", f.free as u64 * 4 / 1024, f.total as u64 * 4 / 1024));
            push(format!("Kernel heap: {} KiB used", h.used / 1024));
            push(String::from("# Tasks (id, name, state, CPU ticks)"));
            crate::sched::for_each(|t| push(format!("{:>3}   {:<14} {:<9} {}", t.id, t.name, t.state.name(), t.ticks)));
        }
        _ => {
            push(String::from("# Display"));
            push(format!("Screen: {} x {} pixels", fb.width, fb.height));
            push(String::from("Theme: dark"));
            push(String::from("The screen mode is chosen at boot: the firmware's mode when it is"));
            push(String::from("at least 1024 x 720 (set the VM's resolution in VirtualBox)."));
            push(String::from("Fonts: Inter (text), JetBrains Mono (ChatBox)."));
        }
    }
    v
}

/// Writes every note to `/notes/<title>.txt` and removes files of deleted notes.
fn save_notes(n: &mut Notes) {
    let fs = crate::shell::fs();
    let _ = fs.mkdir(NOTES_DIR);
    let files = n.files();
    let stale: Vec<String> = fs.list(NOTES_DIR).filter(|(name, size)| size.is_some() && name.ends_with(".txt") && !files.iter().any(|f| f.0 == *name)).map(|(name, _)| String::from(name)).collect();
    for name in stale {
        fs.remove(&format!("{NOTES_DIR}/{name}"));
    }
    let mut ok = true;
    for (name, data) in files {
        ok &= fs.write(&format!("{NOTES_DIR}/{name}"), &data).is_ok();
    }
    n.unsaved = false;
    n.save_note = if ok { "Saved" } else { "Not saved: the file system is full" };
    PERSIST.store(true, core::sync::atomic::Ordering::Relaxed);
}

fn load_notes() -> Notes {
    let fs = crate::shell::fs();
    let names: Vec<String> = fs.list(NOTES_DIR).filter(|(n, size)| size.is_some() && n.ends_with(".txt")).map(|(n, _)| String::from(n)).collect();
    let found = names
        .into_iter()
        .map(|n| {
            let data = fs.read(&format!("{NOTES_DIR}/{n}")).map(<[u8]>::to_vec).unwrap_or_default();
            (String::from(n.trim_end_matches(".txt")), data)
        })
        .collect();
    Notes::new(found)
}

fn load_tasks() -> Tasks {
    Tasks::new(parse_tasks(crate::shell::fs().read(TASKS_FILE).unwrap_or(b"")))
}

/// True once (and then false) when the desktop changed files that should be saved to the USB stick.
pub fn take_persist() -> bool {
    PERSIST.swap(false, core::sync::atomic::Ordering::Relaxed)
}

/// After the shell tried to save to the USB stick: shows in Notes whether the files are only in memory.
pub fn persist_done(only_in_memory: bool) {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return };
        let Some(n) = d.notes.as_mut() else { return };
        let note = if only_in_memory { "Saved in memory (no USB stick to keep it after power-off)" } else { "Saved" };
        if n.save_note != note && !n.unsaved {
            n.save_note = note;
            if d.wm.is_open(NOTES) {
                let r = desktop::app_body(d.wm.rect(NOTES), d.l.title_h);
                redraw(d, &fb, r);
            }
        }
    });
}

/// The `task` command changed the task file: the Tasks window reads it again.
/// Redraws the top bar (after the time zone changed).
pub fn panel_refresh() {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return };
        let r = d.l.panel;
        redraw(d, &fb, r);
    });
}

/// Saves the Notes window's edits now (before `note import` adds a file under /notes).
pub fn notes_save_now() {
    arch::without_interrupts(|| {
        if let Some(n) = desk().and_then(|d| d.notes.as_mut()) {
            save_notes(n);
        }
    });
}

/// Reloads the Notes window from /notes (after `note import`).
pub fn notes_reload() {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return };
        if d.notes.is_some() {
            d.notes = Some(load_notes());
            if d.wm.is_open(NOTES) {
                let r = desktop::app_body(d.wm.rect(NOTES), d.l.title_h);
                redraw(d, &fb, r);
            }
        }
    });
}

pub fn tasks_reload() {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return };
        if d.tasks.is_some() {
            d.tasks = Some(load_tasks());
            if d.wm.is_open(TASKS) {
                let r = desktop::app_body(d.wm.rect(TASKS), d.l.title_h);
                redraw(d, &fb, r);
            }
        }
    });
}

/// True once the desktop is on screen (the mouse then goes to [`desk_pointer`]).
pub fn desktop_active() -> bool {
    arch::without_interrupts(|| desk().is_some())
}

fn set_message(d: &mut Desk, fb: &FbInfo, msg: &str) {
    d.msg = String::from(msg);
    d.msg_at = Some(arch::rdtsc());
    let r = d.l.message;
    redraw(d, fb, r);
}

/// Opens `app`'s window (loading its data the first time) or brings it to the front.
fn open_app(d: &mut Desk, fb: &FbInfo, app: usize) {
    use crate::achieve::{unlock, Id};
    if app != CHATBOX {
        unlock(Id::FirstApp);
    }
    match app {
        FILES => unlock(Id::Explorer),
        CLOCK => unlock(Id::TimeTraveller),
        _ => false,
    };
    match app {
        NOTES if d.notes.is_none() => d.notes = Some(load_notes()),
        FILES => {
            let p = d.files.path.clone();
            files_req(d, Some(FilesReq::List(p)));
        }
        SNAKE => d.snake.seed(arch::rdtsc()),
        TASKS if d.tasks.is_none() => d.tasks = Some(load_tasks()),
        _ => {}
    }
    let before = d.wm.focused();
    d.wm.show(app);
    let r = win_area(d, app);
    redraw(d, fb, r);
    if let Some(b) = before.filter(|&b| b != app) {
        let r = desktop::title_bar(d.wm.rect(b), d.l.title_h);
        redraw(d, fb, r);
    }
    let r = d.l.dock;
    redraw(d, fb, r);
}

fn hide_app(d: &mut Desk, fb: &FbInfo, app: usize) {
    let area = win_area(d, app);
    d.wm.hide(app);
    redraw(d, fb, area);
    if let Some(f) = d.wm.focused() {
        let r = win_area(d, f);
        redraw(d, fb, r);
    }
    let r = d.l.dock;
    redraw(d, fb, r);
}

/// Moves the desktop pointer by a mouse movement and handles clicks: the dock opens apps, a click raises a
/// window, the title bar drags it, its buttons close or minimise it, and clicks inside Notes and Tasks go
/// to them.
pub fn desk_pointer(m: &crate::pointer::MouseMotion) {
    let buttons = m.buttons;
    arch::without_interrupts(|| {
        let s = state();
        let (Some(d), Some(fb)) = (desk(), s.fb) else { return };
        let p = s.dp.get_or_insert_with(|| Pointer::new(fb.width, fb.height));
        p.motion(m);
        let (x, y) = (p.x, p.y);
        if let Some((ox, oy)) = s.dp_shown.take() {
            unsprite(&fb, ox, oy);
        }
        let (left, was_left) = (buttons & 1 != 0, s.dp_buttons & 1 != 0);
        s.dp_buttons = buttons;
        if left && !was_left {
            press(d, &fb, x, y);
        }
        if let Some((app, gx, gy)) = d.drag {
            // Redraw at most every 40 ms while dragging, and always on release, so the drop is exact.
            if !left || us_since(d.drag_drawn) >= 40_000 {
                let before = win_area(d, app);
                d.wm.move_to(app, x - gx, y - gy);
                let after = win_area(d, app);
                if before != after {
                    redraw(d, &fb, before.union(&after));
                }
                d.drag_drawn = arch::rdtsc();
            }
            if !left {
                d.drag = None;
            }
        }
        if d.drag.is_none() {
            hover(d, &fb, x, y);
        }
        sprite(&fb, x, y);
        state().dp_shown = Some((x, y));
    });
}

/// The lines of hover tip `kind` (0 network, 1 battery, 2 clock) and where the tip goes.
fn tip_for(d: &Desk, kind: usize) -> (Vec<String>, Rect) {
    let mut cbuf = [0u8; 28];
    let clock = clock_now(&mut cbuf);
    let spots = desktop::panel_spots(&d.l, clock);
    let mut v = Vec::new();
    match kind {
        0 => {
            let (ip, _, gw, _, _) = crate::netstack::get_config();
            if crate::netdev::link_up() {
                v.push(String::from("Connected through the laptop's network"));
                v.push(format!("Address {}   gateway {}", ip, gw));
            } else {
                v.push(String::from("Not connected"));
            }
            v.push(String::from("Click to see Wi-Fi networks and connect"));
        }
        1 => match crate::battery::read() {
            Some(b) => {
                match b.percent {
                    Some(p) => v.push(format!("Battery {}%", p)),
                    None => v.push(String::from("No battery")),
                }
                v.push(String::from(match (b.on_ac, b.charging) {
                    (true, true) => "Plugged in, charging",
                    (true, false) => "Plugged in",
                    (false, _) => "On battery",
                }));
                v.push(String::from("The laptop's battery, as VirtualBox reports it"));
            }
            None => {
                v.push(String::from("Battery not known"));
                v.push(String::from("This machine does not report a battery to BedrockOS"));
            }
        },
        _ => {
            let off = crate::tz::offset();
            let (abbr, region) = crate::tz::names(off);
            match crate::tz::local_now() {
                Some(t) => {
                    v.push(format!("{} {} {} {}", t.weekday_name(), t.day, t.month_name(), t.year));
                    v.push(format!("{:02}:{:02}:{:02} {}", t.hour, t.min, t.sec, abbr));
                }
                None => v.push(String::from("The clock cannot be read")),
            }
            v.push(format!("Time zone: {} ({})", region, crate::tz::utc_text(off)));
            if let Some(u) = crate::rtc::now() {
                v.push(format!("UTC {:02}:{:02}   (change the zone with `tz` in ChatBox)", u.hour, u.min));
            }
        }
    }
    let r = desktop::tip_rect(&d.l, spots[kind], &v);
    (v, r)
}

/// Shows or hides the hover tip as the pointer enters or leaves a top-bar spot.
fn hover(d: &mut Desk, fb: &FbInfo, x: i32, y: i32) {
    let mut cbuf = [0u8; 28];
    let clock = clock_now(&mut cbuf);
    // No tip over the open Wi-Fi drop-down.
    let kind = desktop::panel_spots(&d.l, clock).iter().position(|r| r.inflate(4).contains(x, y)).filter(|_| d.wifi.is_none());
    if kind == d.tip.map(|t| t.0) {
        return;
    }
    if let Some((_, old)) = d.tip.take() {
        redraw(d, fb, old.inflate(desktop::SHADOW / 2));
    }
    if let Some(k) = kind {
        let r = tip_for(d, k).1;
        d.tip = Some((k, r));
        redraw(d, fb, r.inflate(desktop::SHADOW / 2));
    }
}

/// Carries out what the Files window asked for: a folder listing (and the preview of the selected file),
/// or deleting a file.
fn files_req(d: &mut Desk, req: Option<FilesReq>) {
    let fs = crate::shell::fs();
    let path = match req {
        Some(FilesReq::Delete(p)) => {
            fs.remove(&p);
            PERSIST.store(true, core::sync::atomic::Ordering::Relaxed);
            d.files.path.clone()
        }
        Some(FilesReq::List(p)) => p,
        None => {
            files_preview(d);
            return;
        }
    };
    let path = if path == "/" || fs.is_dir(&path) { path } else { String::from("/") };
    let entries: Vec<(String, Option<usize>)> = fs.list(&path).map(|(n, s)| (String::from(n), s)).collect();
    d.files.set_listing(&path, entries);
    files_preview(d);
}

fn files_preview(d: &mut Desk) {
    d.files.preview = d.files.selected_file().and_then(|p| crate::shell::fs().read(&p).map(|b| b[..b.len().min(4096)].to_vec())).unwrap_or_default();
}

/// Every second: the battery level and an open hover tip (its clock shows seconds).
fn second_tick(d: &mut Desk, fb: &FbInfo) {
    if d.wm.is_open(CLOCK) {
        let r = desktop::app_body(d.wm.rect(CLOCK), d.l.title_h);
        redraw(d, fb, r);
    }
    if d.wm.is_open(NETWORK) {
        let r = desktop::app_body(d.wm.rect(NETWORK), d.l.title_h);
        redraw(d, fb, r);
    }
    if let Some(b) = crate::battery::read() {
        let w = desktop::battery_word(b.percent, b.charging, b.on_ac);
        if desktop::BATTERY.swap(w, core::sync::atomic::Ordering::Relaxed) != w {
            let r = d.l.panel;
            redraw(d, fb, r);
        }
    }
    if let Some((k, old)) = d.tip {
        let r = tip_for(d, k).1;
        d.tip = Some((k, r));
        redraw(d, fb, old.union(&r).inflate(desktop::SHADOW / 2));
    }
}

/// Where the Wi-Fi drop-down goes: under the panel, by the network icon.
fn wifi_rect(d: &Desk) -> Rect {
    let mut cbuf = [0u8; 28];
    let clock = clock_now(&mut cbuf);
    let net = desktop::panel_spots(&d.l, clock)[0];
    WifiFly::rect(net, d.l.panel.bottom(), d.l.w, &Fonts::for_screen(d.l.big()))
}

/// Work the Wi-Fi drop-down wants done; the shell task runs it (the helper link blocks for seconds).
static WIFI_REQ: Global<Option<WifiReq>> = Global::new(None);

/// After a key or click in the drop-down: hands its request on, closes it or redraws it.
fn wifi_after(d: &mut Desk, fb: &FbInfo, out: wififly::Out) {
    let r = wifi_rect(d).inflate(desktop::SHADOW);
    if out == wififly::Out::Close {
        d.wifi = None;
    } else if let Some(req) = d.wifi.as_mut().and_then(|w| w.req.take()) {
        // SAFETY: WIFI_REQ is only touched with interrupts off (here and in `take_wifi_request`).
        *unsafe { WIFI_REQ.get() } = Some(req);
    }
    if out != wififly::Out::None || d.wifi.is_none() {
        redraw(d, fb, r);
    }
}

/// Opens the drop-down (it starts a scan) or closes it.
fn toggle_wifi(d: &mut Desk, fb: &FbInfo) {
    if d.wifi.is_some() {
        wifi_after(d, fb, wififly::Out::Close);
        return;
    }
    if let Some((_, old)) = d.tip.take() {
        redraw(d, fb, old.inflate(desktop::SHADOW / 2));
    }
    d.wifi = Some(WifiFly::default());
    wifi_after(d, fb, wififly::Out::Redraw);
}

/// The scan or connection the Wi-Fi drop-down asked for, if any (called by the shell task).
pub fn take_wifi_request() -> Option<WifiReq> {
    // SAFETY: see `wifi_after`.
    arch::without_interrupts(|| unsafe { WIFI_REQ.get() }.take())
}

/// Hands a finished scan to the drop-down (if it is still open) and redraws it.
pub fn wifi_scanned(r: Result<Vec<crate::wifihost::Net>, String>) {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return };
        if let Some(w) = d.wifi.as_mut() {
            w.scanned(r);
            wifi_after(d, &fb, wififly::Out::Redraw);
        }
    })
}

/// Hands a finished connection attempt to the drop-down and redraws it (and the panel's bars).
pub fn wifi_connected(ssid: &str, r: Result<(), String>) {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return };
        if let Some(w) = d.wifi.as_mut() {
            w.connected(ssid, r);
            wifi_after(d, &fb, wififly::Out::Redraw);
        }
        let p = d.l.panel;
        redraw(d, &fb, p);
    })
}

fn press(d: &mut Desk, fb: &FbInfo, x: i32, y: i32) {
    // The network icon opens or closes the Wi-Fi drop-down; while it is open, clicks inside go to it and a
    // click anywhere else closes it first.
    let mut cbuf = [0u8; 28];
    let net = desktop::panel_spots(&d.l, clock_now(&mut cbuf))[0];
    if net.inflate(6).contains(x, y) {
        toggle_wifi(d, fb);
        return;
    }
    if d.wifi.is_some() {
        let r = wifi_rect(d);
        if r.contains(x, y) {
            let f = Fonts::for_screen(d.l.big());
            let out = d.wifi.as_mut().map_or(wififly::Out::None, |w| w.click(r, &f, x, y));
            wifi_after(d, fb, out);
            return;
        }
        wifi_after(d, fb, wififly::Out::Close);
    }
    if let Some(app) = d.l.dock_hit(x, y) {
        if APPS[app].1.is_none() {
            open_app(d, fb, app);
        } else {
            let mut mbuf = [0u8; 48];
            set_message(d, fb, desktop::dock_message(&mut mbuf, app));
        }
        return;
    }
    let Some(app) = d.wm.hit(x, y) else { return };
    let win = d.wm.rect(app);
    let th = d.l.title_h;
    if d.wm.focused() != Some(app) {
        open_app(d, fb, app);
    }
    let [min, max, close] = desktop::title_buttons(win, th);
    if min.contains(x, y) || close.contains(x, y) {
        hide_app(d, fb, app);
    } else if max.contains(x, y) {
        toggle_max(d, fb, app);
    } else if desktop::title_bar(win, th).contains(x, y) {
        d.drag = Some((app, x - win.x, y - win.y));
        d.drag_drawn = 0;
    } else {
        let body = desktop::app_body(win, th);
        let f = Fonts::for_screen(d.l.big());
        let e = match app {
            NOTES => d.notes.as_mut().map_or(Effect::None, |n| n.click(body, &f, x, y)),
            TASKS => d.tasks.as_mut().map_or(Effect::None, |t| t.click(body, &f, x, y)),
            SETTINGS => d.settings.click(body, &f, x, y),
            CALC => {
                crate::achieve::unlock(crate::achieve::Id::Calculator);
                d.calc.click(body, x, y)
            }
            BROWSER => {
                let e = d.browser.click(body, x, y);
                browser_req(d);
                e
            }
            FILES => {
                let (e, req) = d.files.click(body, &f, x, y);
                files_req(d, req);
                e
            }
            _ => Effect::None,
        };
        after_effect(d, fb, app, e);
    }
}

/// Maximises `app` or restores its size. For ChatBox the console's text grid follows the new size.
fn toggle_max(d: &mut Desk, fb: &FbInfo, app: usize) {
    let old = d.wm.toggle_max(app, &mut d.maxed);
    crate::achieve::unlock(crate::achieve::Id::BigWindow);
    if app == CHATBOX {
        let t = d.l.term_in(d.wm.rect(CHATBOX));
        // SAFETY: TERMBUF holds MAX_W * MAX_H pixels, more than any text area on this screen.
        let ok = log::with_console(|c| unsafe { c.resize(t.w as usize, t.h as usize) }).unwrap_or(false);
        if !ok {
            d.wm.toggle_max(app, &mut d.maxed);
            set_message(d, fb, "ChatBox cannot change size right now");
            return;
        }
        let _ = crate::fb::take_dirty();
    }
    let r = old.inflate(desktop::SHADOW).union(&win_area(d, app));
    redraw(d, fb, r);
}

fn after_effect(d: &mut Desk, fb: &FbInfo, app: usize, e: Effect) {
    if e == Effect::None {
        return;
    }
    if e == Effect::Changed {
        let now = Some(arch::rdtsc());
        match app {
            NOTES => d.notes_dirty = now,
            TASKS => d.tasks_dirty = now,
            _ => {}
        }
    }
    let r = desktop::app_body(d.wm.rect(app), d.l.title_h);
    redraw(d, fb, r);
}

/// A key while the desktop runs. Ctrl+T switches between open windows (like Alt+Tab). Keys for Notes,
/// Tasks or Help go to that window when it is in front; returns false when the key is for the shell (ChatBox
/// in front).
/// Opens (and focuses) the desktop app named `name` (case-insensitive, as in the dock). For `open`.
pub fn open_named(name: &str) -> Result<(), &'static str> {
    let app = match name.to_ascii_lowercase().as_str() {
        "browser" | "crafting" | "craftingtable" | "web" => BROWSER,
        _ => desktop::APPS.iter().position(|(n, m)| m.is_none() && n.eq_ignore_ascii_case(name)).ok_or("no such app")?,
    };
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return Err("the desktop is not shown") };
        open_app(d, &fb, app);
        Ok(())
    })
}

/// A page the browser wants; the shell task fetches it (networking cannot run with interrupts off).
static BROWSER_REQ: Global<Option<String>> = Global::new(None);

fn browser_req(d: &mut Desk) {
    if let Some(u) = d.browser.pending.take() {
        // SAFETY: BROWSER_REQ is only touched with interrupts off (here and in `take_browser_request`).
        *unsafe { BROWSER_REQ.get() } = Some(u);
    }
}

/// The page the browser is waiting for, if any (called by the shell task).
pub fn take_browser_request() -> Option<String> {
    // SAFETY: see `browser_req`.
    arch::without_interrupts(|| unsafe { BROWSER_REQ.get() }.take())
}

/// Hands a fetched page (final address and HTML) or an error to the browser and redraws it.
pub fn browser_loaded(url: &str, result: Result<(String, String), String>) {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return };
        if result.is_ok() {
            crate::achieve::unlock(crate::achieve::Id::Surfer);
        }
        d.browser.loaded(url, result);
        if d.wm.is_open(BROWSER) {
            let r = desktop::app_body(d.wm.rect(BROWSER), d.l.title_h);
            redraw(d, &fb, r);
        }
    })
}

/// Keys the palette queued for the shell (a chosen command, typed into ChatBox).
static INJECT: Global<alloc::collections::VecDeque<Key>> = Global::new(alloc::collections::VecDeque::new());

/// The next key the command palette typed for the shell, if any.
pub fn injected_key() -> Option<Key> {
    // SAFETY: rule A: INJECT is only touched by the shell task (here and in `desk_key`, which it calls).
    arch::without_interrupts(|| unsafe { INJECT.get() }.pop_front())
}

fn palette_key(d: &mut Desk, fb: &FbInfo, k: Key) {
    let f = Fonts::for_screen(d.l.big());
    let pr = Palette::rect(fb.width as i32, d.l.panel.h, &f).inflate(40);
    let pick = match (k, d.palette.as_mut()) {
        (Key::Esc | Key::Ctrl(b' '), _) => {
            d.palette = None;
            redraw(d, fb, pr);
            return;
        }
        (_, Some(p)) => p.key(k),
        (_, None) => None,
    };
    if let Some(pick) = pick {
        d.palette = None;
        redraw(d, fb, pr);
        match pick {
            Pick::App(a) => open_app(d, fb, a),
            Pick::Command(name) => {
                open_app(d, fb, CHATBOX);
                // SAFETY: as in `injected_key`.
                let q = unsafe { INJECT.get() };
                q.extend(name.bytes().map(Key::Char));
                q.push_back(Key::Enter);
            }
        }
    } else {
        redraw(d, fb, pr);
    }
}

pub fn desk_key(k: Key) -> bool {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return false };
        if k == Key::Ctrl(b' ') && d.palette.is_none() {
            d.palette = Some(Palette::default());
            let f = Fonts::for_screen(d.l.big());
            let pr = Palette::rect(fb.width as i32, d.l.panel.h, &f).inflate(40);
            redraw(d, &fb, pr);
            return true;
        }
        if d.palette.is_some() {
            palette_key(d, &fb, k);
            return true;
        }
        // The open Wi-Fi drop-down takes every key (its password box, arrows, Enter, Esc).
        if let Some(w) = d.wifi.as_mut() {
            let out = w.key(k);
            wifi_after(d, &fb, out);
            return true;
        }
        if k == Key::Ctrl(b'm') {
            if let Some(top) = d.wm.focused() {
                toggle_max(d, &fb, top);
            }
            return true;
        }
        if k == Key::Ctrl(b't') {
            if let Some(top) = d.wm.focused() {
                d.wm.cycle();
                let r = win_area(d, top);
                redraw(d, &fb, r);
                if let Some(f) = d.wm.focused() {
                    let r = win_area(d, f);
                    redraw(d, &fb, r);
                }
            }
            return true;
        }
        let Some(app) = d.wm.focused() else {
            // No window open: the keys would go to a hidden shell, so they are dropped.
            return true;
        };
        match (app, k) {
            (NOTES, Key::Char(_)) => {
                crate::achieve::unlock(crate::achieve::Id::Writer);
            }
            (CALC, Key::Enter | Key::Char(b'=')) => {
                crate::achieve::unlock(crate::achieve::Id::Calculator);
            }
            _ => {}
        }
        let e = match app {
            NOTES => d.notes.as_mut().map_or(Effect::None, |n| n.key(k)),
            TASKS => d.tasks.as_mut().map_or(Effect::None, |t| t.key(k)),
            HELP => d.help.key(k),
            SETTINGS => d.settings.key(k),
            CALC => d.calc.key(k),
            BROWSER => {
                let e = d.browser.key(k);
                browser_req(d);
                e
            }
            CLOCK => d.clock.key(k, log::uptime_us()),
            SNAKE => d.snake.key(k),
            FILES => {
                let (e, req) = d.files.key(k);
                files_req(d, req);
                e
            }
            NETWORK => {
                if k == Key::Enter {
                    open_app(d, &fb, CHATBOX);
                    // SAFETY: as in `injected_key`.
                    let q = unsafe { INJECT.get() };
                    q.extend(b"nettest".iter().map(|&b| Key::Char(b)));
                    q.push_back(Key::Enter);
                }
                Effect::None
            }
            _ => return false,
        };
        after_effect(d, &fb, app, e);
        true
    })
}

/// Paints the arrow at (`x`, `y`) on the framebuffer.
fn sprite(fb: &FbInfo, x: i32, y: i32) {
    for sy in 0..pointer::SPRITE_H {
        for sx in 0..pointer::SPRITE_W {
            let c = match pointer::sprite_texel(sx, sy) {
                Texel::Clear => continue,
                Texel::Outline => 0x000000,
                Texel::Fill => 0xFFFFFF,
            };
            let (px, py) = (x + sx as i32, y + sy as i32);
            if px >= 0 && py >= 0 {
                put_px(fb, px as usize, py as usize, c);
            }
        }
    }
}

/// Puts back the desktop pixels under an arrow painted by [`sprite`].
fn unsprite(fb: &FbInfo, x: i32, y: i32) {
    // SAFETY: after boot, BACK holds the composed screen and is only used with interrupts off.
    let back = unsafe { BACK.get() };
    for sy in 0..pointer::SPRITE_H {
        for sx in 0..pointer::SPRITE_W {
            if pointer::sprite_texel(sx, sy) == Texel::Clear {
                continue;
            }
            let (px, py) = (x + sx as i32, y + sy as i32);
            if px >= 0 && py >= 0 && (px as usize) < fb.width && (py as usize) < fb.height {
                put_px(fb, px as usize, py as usize, back[py as usize * fb.width + px as usize]);
            }
        }
    }
}

fn put_px(fb: &FbInfo, x: usize, y: usize, c: u32) {
    if x >= fb.width || y >= fb.height {
        return;
    }
    let v = match fb.format {
        PixelFormat::Bgr => c,
        f => f.pack((c >> 16) as u8, (c >> 8) as u8, c as u8),
    };
    // SAFETY: (x, y) is inside the framebuffer (checked above), which is identity-mapped for the kernel.
    unsafe { core::ptr::write_volatile((fb.base as *mut u32).add(y * fb.stride + x), v) };
}

/// Like `blit`, with every pixel scaled by `k` / 256 (fade in).
fn blit_dim(fb: &FbInfo, back: &[u32], w: usize, r: Rect, k: u32) {
    let r = r.intersect(&Rect::new(0, 0, fb.width as i32, fb.height as i32));
    let (x, rw) = (r.x as usize, r.w as usize);
    for y in r.y as usize..r.bottom() as usize {
        let src = &back[y * w + x..y * w + x + rw];
        // SAFETY: as in `blit`.
        let dst = unsafe { core::slice::from_raw_parts_mut((fb.base as *mut u32).add(y * fb.stride + x), rw) };
        for (d, &p) in dst.iter_mut().zip(src) {
            let p = crate::gui::canvas::mix256(0, p, k);
            *d = match fb.format {
                PixelFormat::Bgr => p,
                f => f.pack((p >> 16) as u8, (p >> 8) as u8, p as u8),
            };
        }
    }
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
    render_view(false, None, None, fade);
}

fn render(full: bool, menu: Option<&MenuView>) {
    render_view(full, menu, None, 1.0);
}

fn render_view(full: bool, menu: Option<&MenuView>, login: Option<&crate::bootscene::LoginView>, fade: f32) {
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
        login: login.copied(),
        hint: if menu.is_some() || login.is_some() { "" } else { s.hint },
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
