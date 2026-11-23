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
use crate::apps::{parse_tasks, Effect, Fonts, Help, Notes, Tasks, NOTES_DIR, TASKS_FILE};
use crate::desktop::{self, DeskLayout, APPS, CHATBOX, HELP, NOTES, TASKS};
use crate::keymap::Key;
use crate::wm::Wm;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use crate::pointer::{self, Pointer, Texel};
use crate::fb::Console;
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

struct Desk {
    l: DeskLayout,
    wm: Wm,
    notes: Option<Notes>,
    tasks: Option<Tasks>,
    help: Help,
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
            msg: String::new(),
            msg_at: None,
            clock_min: u32::MAX,
            drag: None,
            drag_drawn: 0,
            notes_dirty: None,
            tasks_dirty: None,
        });
    }
    let full = Rect::new(0, 0, w as i32, h as i32);
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
    true
}

fn clock_now(buf: &mut [u8; 28]) -> &str {
    match crate::rtc::now() {
        Some(t) => desktop::clock_text(buf, t.weekday_name(), t.day, t.month_name(), t.hour, t.min),
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
    desktop::draw_base(&mut c, &d.l, clock, &d.msg);
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
                _ => {}
            }
        }
        c.set_clip(old);
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
        if let Some((a, b)) = crate::fb::take_dirty() {
            if d.wm.is_open(CHATBOX) {
                let t = d.l.term_in(d.wm.rect(CHATBOX));
                redraw(d, &fb, Rect::new(t.x, t.y + a as i32, t.w, (b - a + 1) as i32));
            }
        }
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
    match app {
        NOTES if d.notes.is_none() => d.notes = Some(load_notes()),
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
pub fn desk_pointer(dx: i32, dy: i32, buttons: u8) {
    arch::without_interrupts(|| {
        let s = state();
        let (Some(d), Some(fb)) = (desk(), s.fb) else { return };
        let p = s.dp.get_or_insert_with(|| Pointer::new(fb.width, fb.height));
        p.apply(dx, dy, buttons);
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
        sprite(&fb, x, y);
        state().dp_shown = Some((x, y));
    });
}

fn press(d: &mut Desk, fb: &FbInfo, x: i32, y: i32) {
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
        set_message(d, fb, "Resizing windows comes in a later version");
    } else if desktop::title_bar(win, th).contains(x, y) {
        d.drag = Some((app, x - win.x, y - win.y));
        d.drag_drawn = 0;
    } else {
        let body = desktop::app_body(win, th);
        let f = Fonts::for_screen(d.l.big());
        let e = match app {
            NOTES => d.notes.as_mut().map_or(Effect::None, |n| n.click(body, &f, x, y)),
            TASKS => d.tasks.as_mut().map_or(Effect::None, |t| t.click(body, &f, x, y)),
            _ => Effect::None,
        };
        after_effect(d, fb, app, e);
    }
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
pub fn desk_key(k: Key) -> bool {
    arch::without_interrupts(|| {
        let (Some(d), Some(fb)) = (desk(), state().fb) else { return false };
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
        let e = match app {
            NOTES => d.notes.as_mut().map_or(Effect::None, |n| n.key(k)),
            TASKS => d.tasks.as_mut().map_or(Effect::None, |t| t.key(k)),
            HELP => d.help.key(k),
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
