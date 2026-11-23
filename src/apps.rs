//! The desktop apps that are drawn by the desktop itself (not by the text console): Notes, Tasks and Help.
//! Pure: each app keeps its state, takes keys and clicks, and draws into a [`Canvas`] rectangle. Loading and
//! saving are done by the kernel through [`Notes::files`] / [`Tasks::to_text`], so this is host-tested.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::cmdhelp;
use crate::editbuf::Doc;
use crate::gfont::{self, Face};
use crate::gui::canvas::{self, rgb, Canvas, Color, Rect};
use crate::gui::theme::DARK;
use crate::keymap::Key;

/// What a key or click did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Not handled; nothing changed.
    None,
    /// The window must be redrawn.
    Redraw,
    /// Redraw, and the app's data changed (save it soon).
    Changed,
}

/// Fonts for a window: the sans text face, and the monospace face with its cell size.
#[derive(Clone, Copy)]
pub struct Fonts {
    pub sans: &'static Face,
    pub mono: &'static Face,
    pub cell: (i32, i32),
}

impl Fonts {
    pub fn for_screen(big: bool) -> Fonts {
        if big {
            Fonts { sans: &gfont::SANS_24, mono: &gfont::MONO_L, cell: (gfont::MONO_L_CELL.0 as i32, gfont::MONO_L_CELL.1 as i32) }
        } else {
            Fonts { sans: &gfont::SANS_16, mono: &gfont::MONO, cell: (gfont::MONO_CELL.0 as i32, gfont::MONO_CELL.1 as i32) }
        }
    }
}

const BG: Color = rgb(22, 26, 40);
const SIDEBAR: Color = rgb(30, 35, 54);
const SELECTED: Color = rgb(48, 64, 104);
/// The most bytes one note may hold.
pub const NOTE_MAX: usize = 32 * 1024;
/// Where notes are kept in the file system.
pub const NOTES_DIR: &str = "/notes";
/// Where tasks are kept.
pub const TASKS_FILE: &str = "/tasks.txt";

/// Draws `s` and returns nothing; text is clipped by the canvas clip.
fn text(c: &mut Canvas, f: &Face, x: i32, y: i32, s: &str, col: Color) {
    c.text(f, x, y, s, col, 1.0);
}

/// Runs `draw` with the clip narrowed to `r`, then restores it.
fn clipped(c: &mut Canvas, r: Rect, draw: impl FnOnce(&mut Canvas)) {
    let old = c.clip();
    c.set_clip(old.intersect(&r));
    if !c.clip().is_empty() {
        draw(c);
    }
    c.set_clip(old);
}

// ---- Notes -------------------------------------------------------------------------------------------------

pub struct Notes {
    /// Note titles (file names without `.txt`), in sidebar order.
    names: Vec<String>,
    /// The saved text of every note except the open one, which lives in `doc`.
    texts: Vec<Vec<u8>>,
    open: usize,
    doc: Doc,
    /// Set when something changed that is not saved yet.
    pub unsaved: bool,
    /// Shown in the footer: "Saved", "Saved in memory only", ...
    pub save_note: &'static str,
    /// Earlier states of the open note (text, cursor) for Ctrl+Z, newest last; Ctrl+Y redoes.
    undo: Vec<(Vec<u8>, (usize, usize))>,
    redo: Vec<(Vec<u8>, (usize, usize))>,
    /// The last edit was typing a letter (typing a word is one undo step).
    typing: bool,
    /// Ctrl+C / Ctrl+X copy or cut the current line; Ctrl+V pastes it.
    clip: Vec<u8>,
    /// The Ctrl+F (find) or Ctrl+R (rename) prompt, with what was typed so far.
    prompt: Option<(Prompt, String)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Prompt {
    Find,
    Rename,
}

const UNDO_MAX: usize = 100;

impl Notes {
    /// Opens the notes found as (title, text) pairs; with none, starts one empty "Note 1".
    pub fn new(mut found: Vec<(String, Vec<u8>)>) -> Notes {
        if found.is_empty() {
            found.push((String::from("Note 1"), Vec::new()));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let (names, texts): (Vec<String>, Vec<Vec<u8>>) = found.into_iter().unzip();
        let doc = Doc::new(&texts[0], NOTE_MAX);
        Notes { names, texts, open: 0, doc, unsaved: false, save_note: "Saved", undo: Vec::new(), redo: Vec::new(), typing: false, clip: Vec::new(), prompt: None }
    }

    pub fn count(&self) -> usize {
        self.names.len()
    }

    pub fn title(&self, i: usize) -> &str {
        &self.names[i]
    }

    pub fn open_index(&self) -> usize {
        self.open
    }

    /// Every note as (file name, text), the open one included, for saving.
    pub fn files(&self) -> Vec<(String, Vec<u8>)> {
        (0..self.names.len())
            .map(|i| (format!("{}.txt", self.names[i]), if i == self.open { self.doc.text() } else { self.texts[i].clone() }))
            .collect()
    }

    fn switch(&mut self, to: usize) {
        if to == self.open || to >= self.names.len() {
            return;
        }
        self.texts[self.open] = self.doc.text();
        self.open = to;
        self.doc = Doc::new(&self.texts[to], NOTE_MAX);
        self.undo.clear();
        self.redo.clear();
    }

    /// Puts the open note back to `text` with the cursor at `cur` (line, column).
    fn restore(&mut self, text: &[u8], cur: (usize, usize)) {
        self.doc = Doc::new(text, NOTE_MAX);
        self.doc.top_of_file();
        for _ in 0..cur.0 {
            self.doc.down();
        }
        self.doc.home();
        for _ in 0..cur.1 {
            self.doc.right();
        }
    }

    fn snapshot(&mut self) {
        if self.undo.len() == UNDO_MAX {
            self.undo.remove(0);
        }
        self.undo.push((self.doc.text(), self.doc.cursor()));
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some((t, c)) = self.undo.pop() else { return false };
        self.redo.push((self.doc.text(), self.doc.cursor()));
        self.restore(&t, c);
        self.typing = false;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some((t, c)) = self.redo.pop() else { return false };
        self.undo.push((self.doc.text(), self.doc.cursor()));
        self.restore(&t, c);
        self.typing = false;
        true
    }

    /// Moves the cursor to the next place `q` appears after it (wrapping to the top). False when nowhere.
    pub fn find_next(&mut self, q: &str) -> bool {
        if q.is_empty() {
            return false;
        }
        let q = q.as_bytes();
        let (cl, cc) = self.doc.cursor();
        let n = self.doc.line_count().max(1);
        for step in 0..=n {
            let li = (cl + step) % n;
            let line = self.doc.line(li);
            let from = if step == 0 { cc + 1 } else { 0 };
            if from > line.len() {
                continue;
            }
            if let Some(pos) = line[from..].windows(q.len()).position(|w| w.eq_ignore_ascii_case(q)) {
                let t = self.doc.text();
                self.restore(&t, (li, from + pos));
                return true;
            }
        }
        false
    }

    /// Renames the open note. Refuses empty names, names with `/`, and names already used.
    pub fn rename(&mut self, to: &str) -> Result<(), &'static str> {
        let to = to.trim();
        if to.is_empty() || to.contains('/') || to.len() > 40 {
            return Err("a name needs 1 to 40 characters and no /");
        }
        if self.names.iter().enumerate().any(|(i, n)| i != self.open && n == to) {
            return Err("another note has that name");
        }
        self.names[self.open] = String::from(to);
        self.unsaved = true;
        Ok(())
    }

    pub fn prompt(&self) -> Option<&(Prompt, String)> {
        self.prompt.as_ref()
    }

    fn paste(&mut self, data: &[u8]) -> bool {
        let mut ok = false;
        for &b in data {
            ok |= if b == b'\n' { self.doc.enter().is_ok() } else { self.doc.insert(b).is_ok() };
        }
        ok
    }

    /// Keys while the find or rename prompt is open.
    fn prompt_key(&mut self, k: Key) -> Effect {
        let Some((kind, mut q)) = self.prompt.take() else { return Effect::None };
        match k {
            Key::Esc => return Effect::Redraw,
            Key::Char(b) if (32..127).contains(&b) && q.len() < 40 => q.push(b as char),
            Key::Backspace => {
                q.pop();
            }
            Key::Enter if kind == Prompt::Find => {
                if !self.find_next(&q) {
                    self.save_note = "Not found";
                }
            }
            Key::Enter => {
                return match self.rename(&q) {
                    Ok(()) => Effect::Changed,
                    Err(e) => {
                        self.save_note = e;
                        Effect::Redraw
                    }
                };
            }
            _ => {}
        }
        self.prompt = Some((kind, q));
        Effect::Redraw
    }

    /// Adds an empty note named "Note N" (the first free N) and opens it.
    pub fn new_note(&mut self) {
        let mut n = self.names.len() + 1;
        while self.names.iter().any(|x| *x == format!("Note {n}")) {
            n += 1;
        }
        self.texts[self.open] = self.doc.text();
        self.names.push(format!("Note {n}"));
        self.texts.push(Vec::new());
        let last = self.names.len() - 1;
        self.open = last;
        self.doc = Doc::new(b"", NOTE_MAX);
        self.unsaved = true;
    }

    /// Deletes the open note (the last one left is emptied instead). Returns the deleted file name.
    pub fn delete_open(&mut self) -> Option<String> {
        if self.names.len() == 1 {
            self.doc = Doc::new(b"", NOTE_MAX);
            self.unsaved = true;
            return None;
        }
        let gone = format!("{}.txt", self.names.remove(self.open));
        self.texts.remove(self.open);
        self.open = self.open.min(self.names.len() - 1);
        self.doc = Doc::new(&self.texts[self.open], NOTE_MAX);
        self.unsaved = true;
        Some(gone)
    }

    pub fn key(&mut self, k: Key) -> Effect {
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        // Remember the state before anything that edits, so Ctrl+Z can bring it back. A run of typed
        // letters is one step.
        let edits = matches!(k, Key::Char(_) | Key::Enter | Key::Tab | Key::Backspace | Key::Delete | Key::Ctrl(b'x' | b'v'));
        let letter = matches!(k, Key::Char(b) if b != b' ');
        if edits && !(letter && self.typing) {
            self.snapshot();
        }
        if edits {
            self.typing = letter;
        }
        let edited = match k {
            Key::Ctrl(b'z') => self.undo(),
            Key::Ctrl(b'y') => self.redo(),
            Key::Ctrl(b'c') | Key::Interrupt => {
                self.clip = self.doc.line(self.doc.cursor().0).to_vec();
                self.clip.push(b'\n');
                self.save_note = "Line copied";
                return Effect::Redraw;
            }
            Key::Ctrl(b'x') => {
                self.clip = self.doc.line(self.doc.cursor().0).to_vec();
                self.clip.push(b'\n');
                self.doc.cut_line();
                true
            }
            Key::Ctrl(b'v') => {
                let c = self.clip.clone();
                self.doc.home();
                self.paste(&c)
            }
            Key::Ctrl(b'f') => {
                self.prompt = Some((Prompt::Find, String::new()));
                return Effect::Redraw;
            }
            Key::Ctrl(b'r') => {
                self.prompt = Some((Prompt::Rename, self.names[self.open].clone()));
                return Effect::Redraw;
            }
            Key::Ctrl(b'n') => {
                self.new_note();
                return Effect::Changed;
            }
            Key::HistoryPrev => {
                self.switch(self.open.saturating_sub(1));
                return Effect::Redraw;
            }
            Key::HistoryNext => {
                self.switch(self.open + 1);
                return Effect::Redraw;
            }
            Key::Ctrl(b's') => return Effect::Changed,
            Key::Ctrl(b'd') => {
                self.delete_open();
                return Effect::Changed;
            }
            Key::Char(b) => self.doc.insert(b).is_ok(),
            Key::Enter => self.doc.enter().is_ok(),
            Key::Tab => self.doc.tab().is_ok(),
            Key::Backspace => self.doc.backspace().is_ok(),
            Key::Delete => self.doc.delete().is_ok(),
            Key::Left => {
                self.doc.left();
                false
            }
            Key::Right => {
                self.doc.right();
                false
            }
            Key::Up => {
                self.doc.up();
                false
            }
            Key::Down => {
                self.doc.down();
                false
            }
            Key::Home => {
                self.doc.home();
                false
            }
            Key::End => {
                self.doc.end();
                false
            }
            Key::PageUp => {
                self.doc.page_up(10);
                false
            }
            Key::PageDown => {
                self.doc.page_down(10);
                false
            }
            _ => return Effect::None,
        };
        if edited {
            self.unsaved = true;
            Effect::Changed
        } else {
            Effect::Redraw
        }
    }

    fn sidebar_w(body: Rect) -> i32 {
        (body.w / 4).clamp(140, 260)
    }

    /// A click at (`x`, `y`) inside `body`: picks a note from the sidebar, or the "+ New note" row.
    pub fn click(&mut self, body: Rect, f: &Fonts, x: i32, y: i32) -> Effect {
        let sw = Self::sidebar_w(body);
        if x >= body.x + sw {
            return Effect::None;
        }
        let row_h = f.sans.height as i32 + 14;
        let i = (y - body.y - 10) / row_h;
        if i < 0 {
            return Effect::None;
        }
        let i = i as usize;
        if i < self.names.len() {
            self.switch(i);
            Effect::Redraw
        } else if i == self.names.len() {
            self.new_note();
            Effect::Changed
        } else {
            Effect::None
        }
    }

    pub fn draw(&mut self, c: &mut Canvas, body: Rect, f: &Fonts, focused: bool) {
        c.fill_rect(body, BG);
        let sw = Self::sidebar_w(body);
        let side = Rect::new(body.x, body.y, sw, body.h);
        c.fill_rect(side, SIDEBAR);
        c.fill_rect(Rect::new(side.right() - 1, body.y, 1, body.h), DARK.border);
        let row_h = f.sans.height as i32 + 14;
        clipped(c, side, |c| {
            for (i, name) in self.names.iter().enumerate() {
                let ry = body.y + 10 + i as i32 * row_h;
                if i == self.open {
                    c.round_rect((body.x + 6) as f32, ry as f32, (sw - 12) as f32, (row_h - 2) as f32, 6.0, SELECTED, 1.0);
                }
                text(c, f.sans, body.x + 16, ry + 7, name, DARK.text);
            }
            let ry = body.y + 10 + self.names.len() as i32 * row_h;
            text(c, f.sans, body.x + 16, ry + 7, "+ New note", DARK.accent);
        });
        // Editor area: title, text, footer.
        let ed = Rect::new(side.right() + 16, body.y + 12, body.right() - side.right() - 28, body.h - 24);
        let foot_h = f.sans.height as i32 + 10;
        match &self.prompt {
            Some((Prompt::Rename, q)) => text(c, f.sans, ed.x, ed.y, &format!("Rename to: {q}_   (Enter saves, Esc cancels)"), DARK.accent),
            Some((Prompt::Find, q)) => text(c, f.sans, ed.x, ed.y, &format!("Find: {q}_   (Enter finds next, Esc closes)"), DARK.accent),
            None => text(c, f.sans, ed.x, ed.y, &self.names[self.open], DARK.text),
        }
        let area = Rect::new(ed.x, ed.y + f.sans.height as i32 + 12, ed.w, ed.h - f.sans.height as i32 - 12 - foot_h);
        let (cw, ch) = f.cell;
        let rows = (area.h / ch).max(1) as usize;
        let cols = (area.w / cw).max(2) as usize;
        self.doc.fit(rows, cols);
        clipped(c, area, |c| {
            for r in 0..rows {
                let line = self.doc.window_row(r, cols);
                let s = core::str::from_utf8(&line).unwrap_or("");
                text(c, f.mono, area.x, area.y + r as i32 * ch, s, rgb(222, 226, 236));
            }
            if focused {
                let (cr, cc) = self.doc.window_cursor();
                c.fill_rect(Rect::new(area.x + cc as i32 * cw, area.y + cr as i32 * ch, 2, ch), DARK.accent);
            }
        });
        // Footer: save state and the keys.
        let fy = ed.bottom() - foot_h + 6;
        c.fill_rect(Rect::new(ed.x, fy - 6, ed.w, 1), DARK.border);
        let (state, col) = if self.unsaved { ("Saving...", DARK.warn) } else { (self.save_note, if matches!(self.save_note, "Saved" | "Line copied") { DARK.good } else { DARK.warn }) };
        c.circle((ed.x + 5) as f32, (fy + f.sans.height as i32 / 2) as f32, 4.0, col, 1.0);
        text(c, f.sans, ed.x + 16, fy, state, DARK.text_dim);
        let hint = "Ctrl+N new  Ctrl+R rename  Ctrl+F find  Ctrl+Z/Y undo/redo  Ctrl+C/X/V line";
        let hw = canvas::text_width(f.sans, hint, 0);
        if hw < ed.w - 200 {
            text(c, f.sans, ed.right() - hw, fy, hint, DARK.text_dim);
        }
    }
}

// ---- Tasks -------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub text: String,
    pub done: bool,
}

/// Reads the task file: one task per line, `[x] text` when done, `[ ] text` when not.
pub fn parse_tasks(data: &[u8]) -> Vec<Task> {
    let s = core::str::from_utf8(data).unwrap_or("");
    s.lines()
        .filter_map(|l| {
            let (done, t) = if let Some(t) = l.strip_prefix("[x] ") {
                (true, t)
            } else {
                (false, l.strip_prefix("[ ] ").unwrap_or(l))
            };
            let t = t.trim();
            (!t.is_empty()).then(|| Task { text: String::from(t), done })
        })
        .collect()
}

pub fn tasks_text(tasks: &[Task]) -> Vec<u8> {
    let mut o = String::new();
    for t in tasks {
        o.push_str(if t.done { "[x] " } else { "[ ] " });
        o.push_str(&t.text);
        o.push('\n');
    }
    o.into_bytes()
}

pub struct Tasks {
    pub items: Vec<Task>,
    input: String,
    /// The highlighted task; `None` = the "add a task" line has the keyboard.
    sel: Option<usize>,
}

const TASK_MAX: usize = 120;

impl Tasks {
    pub fn new(items: Vec<Task>) -> Tasks {
        Tasks { items, input: String::new(), sel: None }
    }

    pub fn to_text(&self) -> Vec<u8> {
        tasks_text(&self.items)
    }

    pub fn done_count(&self) -> usize {
        self.items.iter().filter(|t| t.done).count()
    }

    pub fn key(&mut self, k: Key) -> Effect {
        match (self.sel, k) {
            (None, Key::Char(b)) if self.input.len() < TASK_MAX && (0x20..0x7f).contains(&b) => {
                self.input.push(b as char);
                Effect::Redraw
            }
            (None, Key::Backspace) => {
                self.input.pop();
                Effect::Redraw
            }
            (None, Key::Enter) => {
                let t = self.input.trim();
                if t.is_empty() {
                    return Effect::None;
                }
                self.items.push(Task { text: String::from(t), done: false });
                self.input.clear();
                Effect::Changed
            }
            (None, Key::Down) if !self.items.is_empty() => {
                self.sel = Some(0);
                Effect::Redraw
            }
            (Some(i), Key::Down) => {
                self.sel = Some((i + 1).min(self.items.len() - 1));
                Effect::Redraw
            }
            (Some(0), Key::Up) | (Some(_), Key::Esc) => {
                self.sel = None;
                Effect::Redraw
            }
            (Some(i), Key::Up) => {
                self.sel = Some(i - 1);
                Effect::Redraw
            }
            (Some(i), Key::Enter | Key::Char(b' ')) => {
                self.items[i].done = !self.items[i].done;
                Effect::Changed
            }
            (Some(i), Key::Delete | Key::Backspace) => {
                self.items.remove(i);
                self.sel = if self.items.is_empty() { None } else { Some(i.min(self.items.len() - 1)) };
                Effect::Changed
            }
            _ => Effect::None,
        }
    }

    fn row_h(f: &Fonts) -> i32 {
        f.sans.height as i32 + 16
    }

    fn list_top(body: Rect, f: &Fonts) -> i32 {
        body.y + 20 + f.sans.height as i32 + 20 + Self::row_h(f) + 12
    }

    /// A click on a task's checkbox toggles it; on its text selects it; on the input line focuses it.
    pub fn click(&mut self, body: Rect, f: &Fonts, x: i32, y: i32) -> Effect {
        let top = Self::list_top(body, f);
        if y < top {
            self.sel = None;
            return Effect::Redraw;
        }
        let i = ((y - top) / Self::row_h(f)) as usize;
        if i >= self.items.len() {
            return Effect::None;
        }
        if x < body.x + 24 + 28 {
            self.items[i].done = !self.items[i].done;
            return Effect::Changed;
        }
        self.sel = Some(i);
        Effect::Redraw
    }

    pub fn draw(&self, c: &mut Canvas, body: Rect, f: &Fonts, focused: bool) {
        c.fill_rect(body, BG);
        let x = body.x + 24;
        let w = body.w - 48;
        let mut y = body.y + 20;
        let head = format!("{} tasks, {} done", self.items.len(), self.done_count());
        text(c, f.sans, x, y, &head, DARK.text);
        // Progress bar.
        let pw = (w / 3).max(60);
        let px = x + w - pw;
        c.round_rect(px as f32, (y + 6) as f32, pw as f32, 8.0, 4.0, DARK.surface_hi, 1.0);
        if !self.items.is_empty() {
            let fill = pw * self.done_count() as i32 / self.items.len() as i32;
            if fill > 0 {
                c.round_rect(px as f32, (y + 6) as f32, fill as f32, 8.0, 4.0, DARK.good, 1.0);
            }
        }
        y += f.sans.height as i32 + 20;
        // Input line.
        let rh = Self::row_h(f);
        let input_active = focused && self.sel.is_none();
        c.round_rect(x as f32, y as f32, w as f32, rh as f32, 8.0, DARK.surface_hi, 1.0);
        if input_active {
            c.round_rect_stroke(x as f32 + 0.5, y as f32 + 0.5, w as f32 - 1.0, rh as f32 - 1.0, 8.0, 1.0, DARK.accent, 1.0);
        }
        let ty = y + (rh - f.sans.height as i32) / 2;
        clipped(c, Rect::new(x + 8, y, w - 16, rh), |c| {
            if self.input.is_empty() {
                text(c, f.sans, x + 14, ty, "Add a task and press Enter", DARK.text_dim);
            } else {
                text(c, f.sans, x + 14, ty, &self.input, DARK.text);
            }
            if input_active {
                let cx = x + 14 + canvas::text_width(f.sans, &self.input, 0) + 1;
                c.fill_rect(Rect::new(cx, ty, 2, f.sans.height as i32), DARK.accent);
            }
        });
        // The list.
        let top = Self::list_top(body, f);
        clipped(c, Rect::new(body.x, top, body.w, body.bottom() - top - f.sans.height as i32 - 16), |c| {
            for (i, t) in self.items.iter().enumerate() {
                let ry = top + i as i32 * rh;
                if focused && self.sel == Some(i) {
                    c.round_rect(x as f32, ry as f32, w as f32, (rh - 2) as f32, 6.0, SELECTED, 1.0);
                }
                let (bx, by) = (x + 10, ry + rh / 2 - 9);
                if t.done {
                    c.round_rect(bx as f32, by as f32, 18.0, 18.0, 5.0, DARK.good, 1.0);
                    c.text(f.sans, bx + 3, by + (18 - f.sans.height as i32) / 2, "v", rgb(255, 255, 255), 1.0);
                } else {
                    c.round_rect_stroke(bx as f32 + 0.5, by as f32 + 0.5, 17.0, 17.0, 5.0, 1.5, DARK.text_dim, 1.0);
                }
                let col = if t.done { DARK.text_dim } else { DARK.text };
                let tx = x + 42;
                let tty = ry + (rh - f.sans.height as i32) / 2;
                text(c, f.sans, tx, tty, &t.text, col);
                if t.done {
                    let tw = canvas::text_width(f.sans, &t.text, 0);
                    c.fill_rect(Rect::new(tx, tty + f.sans.height as i32 / 2 + 1, tw, 1), DARK.text_dim);
                }
            }
        });
        let hint = "Up/Down select   Space done   Delete remove   Esc back to input";
        text(c, f.sans, x, body.bottom() - f.sans.height as i32 - 10, hint, DARK.text_dim);
    }
}

// ---- Command palette (Ctrl+Space) ---------------------------------------------------------------------------

/// What a palette entry does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pick {
    /// Open the desktop app with this index (`desktop::APPS`).
    App(usize),
    /// Type this command into ChatBox and run it.
    Command(&'static str),
}

#[derive(Default)]
pub struct Palette {
    pub query: String,
    pub sel: usize,
}

pub const PALETTE_ROWS: usize = 8;

impl Palette {
    /// The entries matching the query (apps first, then commands), at most `PALETTE_ROWS`.
    pub fn matches(&self) -> Vec<(String, &'static str, Pick)> {
        let q = self.query.trim().to_ascii_lowercase();
        let mut v = Vec::new();
        for (i, (name, later)) in crate::desktop::APPS.iter().enumerate() {
            if later.is_none() && name.to_ascii_lowercase().contains(q.as_str()) {
                v.push((format!("Open {name}"), "app", Pick::App(i)));
            }
        }
        for c in crate::cmdhelp::COMMANDS {
            if c.name.contains(q.as_str()) || (!q.is_empty() && c.about.to_ascii_lowercase().contains(q.as_str())) {
                v.push((format!("{}  -  {}", c.name, c.about), "command", Pick::Command(c.name)));
            }
        }
        v.truncate(PALETTE_ROWS);
        v
    }

    /// A key; `Some(pick)` when Enter chose an entry. Esc is handled by the caller (closes the palette).
    pub fn key(&mut self, k: Key) -> Option<Pick> {
        match k {
            Key::Char(b) if (32..127).contains(&b) && self.query.len() < 40 => {
                self.query.push(b as char);
                self.sel = 0;
            }
            Key::Backspace => {
                self.query.pop();
                self.sel = 0;
            }
            Key::Up => self.sel = self.sel.saturating_sub(1),
            Key::Down | Key::Tab => self.sel = (self.sel + 1).min(self.matches().len().saturating_sub(1)),
            Key::Enter => return self.matches().get(self.sel).map(|m| m.2),
            _ => {}
        }
        None
    }

    /// Where the palette is drawn on a `w` x `h` screen below a panel `top` pixels high.
    pub fn rect(w: i32, top: i32, f: &Fonts) -> Rect {
        let pw = (w - 80).min(720);
        let row = f.sans.height as i32 + 14;
        Rect::new((w - pw) / 2, top + 80, pw, row * (PALETTE_ROWS as i32 + 1) + 28)
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect, f: &Fonts) {
        let row = f.sans.height as i32 + 14;
        c.shadow(r.x as f32, r.y as f32 + 6.0, r.w as f32, r.h as f32, 12.0, 24.0, 0, 0.55);
        c.round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 12.0, rgb(28, 33, 52), 1.0);
        c.round_rect_stroke(r.x as f32 + 0.5, r.y as f32 + 0.5, r.w as f32 - 1.0, r.h as f32 - 1.0, 12.0, 1.0, DARK.accent, 0.8);
        let x = r.x + 18;
        let q = if self.query.is_empty() { String::from("Type an app or command...") } else { format!("{}_", self.query) };
        text(c, f.sans, x, r.y + 14, &q, if self.query.is_empty() { DARK.text_dim } else { DARK.text });
        c.fill_rect(Rect::new(r.x + 10, r.y + row + 10, r.w - 20, 1), DARK.border);
        clipped(c, r, |c| {
            for (i, (label, kind, _)) in self.matches().iter().enumerate() {
                let ry = r.y + row + 16 + i as i32 * row;
                if i == self.sel {
                    c.round_rect((r.x + 8) as f32, ry as f32, (r.w - 16) as f32, (row - 2) as f32, 6.0, SELECTED, 1.0);
                }
                let kw = canvas::text_width(f.sans, kind, 0);
                clipped(c, Rect::new(x, ry, r.right() - kw - 40 - x, row), |c| text(c, f.sans, x, ry + 7, label, DARK.text));
                text(c, f.sans, r.right() - kw - 18, ry + 7, kind, DARK.text_dim);
            }
        });
    }
}

// ---- Settings ---------------------------------------------------------------------------------------------

/// The pages of the Settings window. Their text comes from the kernel (live values) on every redraw.
pub const SETTINGS_PAGES: [&str; 4] = ["About", "Network", "System", "Display"];

#[derive(Default)]
pub struct Settings {
    pub page: usize,
}

impl Settings {
    pub fn key(&mut self, k: Key) -> Effect {
        match k {
            Key::Up | Key::Left => self.page = self.page.saturating_sub(1),
            Key::Down | Key::Right | Key::Tab => self.page = (self.page + 1) % SETTINGS_PAGES.len(),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    fn side_w(body: Rect) -> i32 {
        (body.w / 4).clamp(140, 220)
    }

    pub fn click(&mut self, body: Rect, f: &Fonts, x: i32, y: i32) -> Effect {
        if x >= body.x + Self::side_w(body) {
            return Effect::None;
        }
        let row_h = f.sans.height as i32 + 14;
        let i = (y - body.y - 10) / row_h;
        if (0..SETTINGS_PAGES.len() as i32).contains(&i) {
            self.page = i as usize;
            return Effect::Redraw;
        }
        Effect::None
    }

    /// `lines`: the text of the current page; a line starting with `# ` is a heading.
    pub fn draw(&self, c: &mut Canvas, body: Rect, f: &Fonts, lines: &[String]) {
        c.fill_rect(body, BG);
        let sw = Self::side_w(body);
        let side = Rect::new(body.x, body.y, sw, body.h);
        c.fill_rect(side, SIDEBAR);
        c.fill_rect(Rect::new(side.right() - 1, body.y, 1, body.h), DARK.border);
        let row_h = f.sans.height as i32 + 14;
        for (i, name) in SETTINGS_PAGES.iter().enumerate() {
            let ry = body.y + 10 + i as i32 * row_h;
            if i == self.page {
                c.round_rect((body.x + 6) as f32, ry as f32, (sw - 12) as f32, (row_h - 2) as f32, 6.0, SELECTED, 1.0);
            }
            text(c, f.sans, body.x + 16, ry + 7, name, DARK.text);
        }
        let x = side.right() + 20;
        let area = Rect::new(x, body.y + 14, body.right() - x - 16, body.h - 28);
        clipped(c, area, |c| {
            let mut y = area.y;
            for l in lines {
                if let Some(h) = l.strip_prefix("# ") {
                    y += 6;
                    text(c, f.sans, x, y, h, DARK.accent);
                    y += f.sans.height as i32 + 8;
                } else {
                    text(c, f.sans, x, y, l, DARK.text);
                    y += f.sans.height as i32 + 6;
                }
            }
        });
    }
}

// ---- Help --------------------------------------------------------------------------------------------------

pub struct Help {
    query: String,
    top: usize,
}

impl Default for Help {
    fn default() -> Self {
        Self::new()
    }
}

/// True when `needle` (lower case) is in `hay`, ignoring case.
fn contains_ci(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    h.windows(n.len()).any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

impl Help {
    pub fn new() -> Help {
        Help { query: String::new(), top: 0 }
    }

    /// The commands matching the search, from the shell's own command table.
    pub fn matches(&self) -> Vec<&'static cmdhelp::Cmd> {
        let q = self.query.to_ascii_lowercase();
        cmdhelp::COMMANDS.iter().filter(|c| contains_ci(c.name, &q) || contains_ci(c.about, &q) || contains_ci(c.group, &q)).collect()
    }

    pub fn key(&mut self, k: Key) -> Effect {
        match k {
            Key::Char(b) if (0x20..0x7f).contains(&b) && self.query.len() < 40 => {
                self.query.push(b as char);
                self.top = 0;
            }
            Key::Backspace => {
                self.query.pop();
                self.top = 0;
            }
            Key::Esc => {
                self.query.clear();
                self.top = 0;
            }
            Key::Down => self.top = (self.top + 1).min(self.matches().len().saturating_sub(1)),
            Key::Up => self.top = self.top.saturating_sub(1),
            Key::PageDown => self.top = (self.top + 10).min(self.matches().len().saturating_sub(1)),
            Key::PageUp => self.top = self.top.saturating_sub(10),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    pub fn draw(&self, c: &mut Canvas, body: Rect, f: &Fonts, focused: bool) {
        c.fill_rect(body, BG);
        let x = body.x + 24;
        let w = body.w - 48;
        let mut y = body.y + 18;
        let rh = f.sans.height as i32 + 16;
        c.round_rect(x as f32, y as f32, w as f32, rh as f32, 8.0, DARK.surface_hi, 1.0);
        if focused {
            c.round_rect_stroke(x as f32 + 0.5, y as f32 + 0.5, w as f32 - 1.0, rh as f32 - 1.0, 8.0, 1.0, DARK.accent, 1.0);
        }
        let ty = y + (rh - f.sans.height as i32) / 2;
        if self.query.is_empty() {
            text(c, f.sans, x + 14, ty, "Search commands (type to filter)", DARK.text_dim);
        } else {
            text(c, f.sans, x + 14, ty, &self.query, DARK.text);
        }
        if focused {
            let cx = x + 14 + canvas::text_width(f.sans, &self.query, 0) + 1;
            c.fill_rect(Rect::new(cx, ty, 2, f.sans.height as i32), DARK.accent);
        }
        y += rh + 14;
        let m = self.matches();
        let count = format!("{} of {} commands  -  type them in ChatBox", m.len(), cmdhelp::COMMANDS.len());
        text(c, f.sans, x, y, &count, DARK.text_dim);
        y += f.sans.height as i32 + 10;
        let (cw, ch) = f.cell;
        let line_h = ch + 4;
        let name_w = 12 * cw;
        let list = Rect::new(body.x, y, body.w, body.bottom() - y - 8);
        clipped(c, list, |c| {
            for (i, cmd) in m.iter().skip(self.top).enumerate() {
                let ry = y + i as i32 * line_h;
                if ry > list.bottom() {
                    break;
                }
                text(c, f.mono, x, ry, cmd.name, DARK.accent_2);
                text(c, f.sans, x + name_w, ry + (ch - f.sans.height as i32) / 2, cmd.about, DARK.text);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(n: &mut Notes, s: &str) {
        for b in s.bytes() {
            n.key(Key::Char(b));
        }
    }

    #[test]
    fn palette_finds_apps_and_commands() {
        let mut p = Palette::default();
        assert!(p.matches().iter().any(|m| m.2 == Pick::App(crate::desktop::SETTINGS)));
        for b in b"set" {
            p.key(Key::Char(*b));
        }
        assert_eq!(p.key(Key::Enter), Some(Pick::App(crate::desktop::SETTINGS)), "apps come first");
        let mut p = Palette::default();
        for b in b"nettest" {
            p.key(Key::Char(*b));
        }
        assert_eq!(p.key(Key::Enter), Some(Pick::Command("nettest")));
        p.key(Key::Backspace);
        assert_eq!(p.query, "nettes");
        let mut p = Palette::default();
        for b in b"zzzz" {
            p.key(Key::Char(*b));
        }
        assert_eq!(p.key(Key::Enter), None);
    }

    #[test]
    fn notes_undo_redo_find_rename_and_paste() {
        let mut n = Notes::new(alloc::vec![(String::from("A"), Vec::new()), (String::from("B"), Vec::new())]);
        typed(&mut n, "hello world");
        assert_eq!(n.doc.text(), b"hello world\n");
        n.key(Key::Ctrl(b'z'));
        assert_eq!(n.doc.text(), b"hello \n", "one undo removes the last word");
        n.key(Key::Ctrl(b'y'));
        assert_eq!(n.doc.text(), b"hello world\n");
        // Copy the line, paste it as a new line above.
        n.key(Key::Ctrl(b'c'));
        n.key(Key::Ctrl(b'v'));
        assert_eq!(n.doc.text(), b"hello world\nhello world\n");
        n.key(Key::Ctrl(b'z'));
        assert_eq!(n.doc.text(), b"hello world\n");
        // Find.
        n.restore(b"one\ntwo three\n", (0, 0));
        n.key(Key::Ctrl(b'f'));
        typed(&mut n, "THREE");
        n.key(Key::Enter);
        assert_eq!(n.doc.cursor(), (1, 4));
        n.key(Key::Esc);
        assert!(n.prompt().is_none());
        // Rename: refused when taken, accepted otherwise; the file name follows.
        assert_eq!(n.rename("B"), Err("another note has that name"));
        n.key(Key::Ctrl(b'r'));
        for _ in 0..5 {
            n.key(Key::Backspace);
        }
        typed(&mut n, "Shopping");
        n.key(Key::Enter);
        assert!(n.files().iter().any(|(f, _)| f == "Shopping.txt"));
    }

    #[test]
    fn tasks_round_trip_and_keys() {
        let mut t = Tasks::new(parse_tasks(b"[ ] buy milk\n[x] write report\n\n  \nplain line\n"));
        assert_eq!(t.items.len(), 3);
        assert!(t.items[1].done && !t.items[2].done);
        assert_eq!(parse_tasks(&t.to_text()), t.items, "round trip");
        for b in b"call mum" {
            t.key(Key::Char(*b));
        }
        assert_eq!(t.key(Key::Enter), Effect::Changed);
        assert_eq!(t.items.last().unwrap().text, "call mum");
        assert_eq!(t.key(Key::Enter), Effect::None, "empty input adds nothing");
        t.key(Key::Down); // select the first task
        assert_eq!(t.key(Key::Char(b' ')), Effect::Changed);
        assert!(t.items[0].done);
        assert_eq!(t.done_count(), 2);
        t.key(Key::Delete);
        assert_eq!(t.items.len(), 3);
        assert_eq!(t.items[0].text, "write report");
        t.key(Key::Esc);
        t.key(Key::Char(b'x'));
        assert_eq!(t.items.len(), 3, "typing goes to the input again");
    }

    #[test]
    fn notes_new_switch_delete_and_files() {
        let mut n = Notes::new(Vec::new());
        assert_eq!(n.count(), 1);
        assert_eq!(n.title(0), "Note 1");
        for b in b"hello" {
            assert_eq!(n.key(Key::Char(*b)), Effect::Changed);
        }
        n.key(Key::Enter);
        n.key(Key::Char(b'!'));
        n.key(Key::Ctrl(b'n'));
        assert_eq!((n.count(), n.open_index()), (2, 1));
        n.key(Key::Char(b'z'));
        n.key(Key::HistoryPrev);
        assert_eq!(n.open_index(), 0);
        let files = n.files();
        assert_eq!(files[0], (String::from("Note 1.txt"), b"hello\n!\n".to_vec()));
        assert_eq!(files[1], (String::from("Note 2.txt"), b"z\n".to_vec()));
        assert_eq!(n.delete_open(), Some(String::from("Note 1.txt")));
        assert_eq!(n.count(), 1);
        assert_eq!(n.files()[0].1, b"z\n".to_vec());
        assert_eq!(n.delete_open(), None, "the last note is emptied, not removed");
        assert_eq!(n.files()[0].1, Vec::<u8>::new());
        assert_eq!(n.key(Key::Esc), Effect::None);
    }

    #[test]
    fn notes_open_sorted_existing() {
        let n = Notes::new(alloc::vec![(String::from("b"), b"2".to_vec()), (String::from("a"), b"1".to_vec())]);
        assert_eq!(n.title(0), "a");
        assert_eq!(n.files()[0].1, b"1\n".to_vec());
    }

    #[test]
    fn help_searches_the_command_table() {
        let mut h = Help::new();
        assert_eq!(h.matches().len(), cmdhelp::COMMANDS.len());
        for b in b"PING" {
            h.key(Key::Char(*b));
        }
        assert!(h.matches().iter().any(|c| c.name == "ping"), "case-insensitive");
        assert!(h.matches().len() < cmdhelp::COMMANDS.len());
        h.key(Key::Esc);
        assert_eq!(h.matches().len(), cmdhelp::COMMANDS.len());
    }

    #[test]
    fn apps_draw_without_panicking() {
        let (w, h) = (900usize, 600usize);
        let mut buf = alloc::vec![0u32; w * h];
        let mut c = Canvas::new(&mut buf, w, h, w).unwrap();
        let body = Rect::new(10, 10, 700, 500);
        for big in [false, true] {
            let f = Fonts::for_screen(big);
            let mut n = Notes::new(Vec::new());
            n.key(Key::Char(b'a'));
            n.draw(&mut c, body, &f, true);
            let mut t = Tasks::new(parse_tasks(b"[x] a\n[ ] b\n"));
            t.draw(&mut c, body, &f, true);
            assert_eq!(t.click(body, &f, body.x + 30, Tasks::list_top(body, &f) + 5), Effect::Changed);
            assert!(!t.items[0].done, "checkbox click toggles");
            Help::new().draw(&mut c, body, &f, false);
        }
        assert_eq!(c.get(5, 5), 0, "nothing drawn outside the body");
    }
}
