//! Crafting Table, BedrockOS's web browser. It reads HTML and shows it as text: headings, paragraphs, lists and
//! links you can follow (click one, or Tab to it and press Enter). There are no images, styles or scripts.
//! The address bar takes a web address or words to search for (DuckDuckGo's light page).
//!
//! Pure like the other apps: the kernel fetches pages (`bootfx` hands the request to the shell task, which
//! fetches through the laptop's helper or, for `http://`, directly) and calls [`Browser::loaded`].

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::apps::{clipped, text, Effect, Fonts, BG, SIDEBAR};
use crate::gui::canvas::{self, rgb, Canvas, Rect};
use crate::gui::theme::DARK;
use crate::keymap::Key;

pub const HOME: &str = "about:home";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Para,
    Heading,
    Item,
    Pre,
}

/// A block of the page: its kind and its words, each with the link it belongs to (an index into `links`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: Kind,
    pub words: Vec<(String, Option<usize>)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub title: String,
    pub blocks: Vec<Block>,
    pub links: Vec<String>,
}

/// `href` made absolute against the page address `base`. `None` for links the browser cannot follow
/// (mailto:, javascript:, a fragment of the same page).
pub fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(href.to_string());
    }
    let slash = lower.find(['/', '?', '#']).unwrap_or(lower.len());
    if lower.find(':').is_some_and(|c| c < slash) {
        return None; // mailto:, javascript:, tel:, ...
    }
    let scheme_end = base.find("://")? + 3;
    let scheme = &base[..scheme_end];
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("{}{}", scheme, rest));
    }
    let host_end = base[scheme_end..].find('/').map_or(base.len(), |i| scheme_end + i);
    let origin = &base[..host_end];
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    // Relative to the folder of the base path (query and fragment dropped).
    let path = base[host_end..].split(['?', '#']).next().unwrap_or("");
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    let (rel, tail) = match href.find(['?', '#']) {
        Some(i) => (&href[..i], &href[i..]),
        None => (href, ""),
    };
    let ends_with_slash = rel.ends_with('/') || rel.ends_with("/.") || rel.ends_with("/..") || rel == "." || rel == "..";
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = String::from(origin);
    for p in &parts {
        out.push('/');
        out.push_str(p);
    }
    if parts.is_empty() || ends_with_slash {
        out.push('/');
    }
    out.push_str(tail);
    Some(out)
}

/// What the address bar text means: a web address (http:// added when there is none), or a search.
pub fn address_for(input: &str) -> String {
    let t = input.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("about:") {
        return t.to_string();
    }
    if !t.contains(' ') && t.contains('.') {
        return format!("https://{t}");
    }
    let mut q = String::new();
    for b in t.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => q.push(b as char),
            b' ' => q.push('+'),
            _ => q.push_str(&format!("%{b:02X}")),
        }
    }
    format!("https://lite.duckduckgo.com/lite/?q={q}")
}

/// Text the fonts can draw: common typographic characters become ASCII, other non-ASCII ones are dropped.
fn ascii(c: char, out: &mut String) {
    match c {
        ' '..='~' => out.push(c),
        '\u{2018}' | '\u{2019}' | '\u{2032}' => out.push('\''),
        '\u{201C}' | '\u{201D}' | '\u{2033}' => out.push('"'),
        '\u{2013}' | '\u{2014}' | '\u{2212}' => out.push('-'),
        '\u{2026}' => out.push_str("..."),
        '\u{00A0}' | '\t' | '\n' | '\r' => out.push(' '),
        '\u{2022}' | '\u{00B7}' => out.push('*'),
        '\u{00A9}' => out.push_str("(c)"),
        '\u{00E9}' | '\u{00E8}' | '\u{00EA}' => out.push('e'),
        '\u{00E1}' | '\u{00E0}' | '\u{00E2}' | '\u{00E4}' => out.push('a'),
        '\u{00F3}' | '\u{00F6}' | '\u{00F4}' => out.push('o'),
        '\u{00FA}' | '\u{00FC}' => out.push('u'),
        '\u{00ED}' => out.push('i'),
        '\u{00F1}' => out.push('n'),
        _ => {}
    }
}

/// Decodes character references (`&amp;`, `&#39;`, `&#x2014;`, ...) and maps the text to ASCII.
pub fn decode_text(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        for c in rest[..i].chars() {
            ascii(c, &mut out);
        }
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| c == ';' || c == '&' || c.is_whitespace()).map(|e| e + 1);
        let (ent, after) = match end {
            Some(e) if rest.as_bytes()[e] == b';' && e <= 10 => (&rest[1..e], &rest[e + 1..]),
            _ => {
                out.push('&');
                rest = &rest[1..];
                continue;
            }
        };
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" | "ndash" | "minus" => Some('-'),
            "hellip" => Some('\u{2026}'),
            "copy" => Some('\u{00A9}'),
            "rsquo" | "lsquo" => Some('\''),
            "rdquo" | "ldquo" => Some('"'),
            "bull" | "middot" => Some('*'),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => ascii(c, &mut out),
            None => {
                out.push('&');
                out.push_str(ent);
                out.push(';');
            }
        }
        rest = after;
    }
    for c in rest.chars() {
        ascii(c, &mut out);
    }
    out
}

struct Builder {
    page: Page,
    cur: Block,
    link: Option<usize>,
}

impl Builder {
    fn end_block(&mut self, next: Kind) {
        if !self.cur.words.is_empty() {
            let b = core::mem::replace(&mut self.cur, Block { kind: next, words: Vec::new() });
            self.page.blocks.push(b);
        } else {
            self.cur.kind = next;
        }
    }

    fn words(&mut self, s: &str, pre: bool) {
        let t = decode_text(s);
        if pre {
            for (i, line) in t.split('\n').enumerate() {
                if i > 0 {
                    self.end_block(Kind::Pre);
                }
                if !line.trim().is_empty() {
                    self.cur.words.push((line.trim_end().to_string(), self.link));
                }
            }
            return;
        }
        for w in t.split_whitespace() {
            self.cur.words.push((w.to_string(), self.link));
        }
    }
}

/// Reads `html` (the page at `base`) into blocks of words and a list of links.
pub fn parse(html: &str, base: &str) -> Page {
    let mut b = Builder { page: Page::default(), cur: Block { kind: Kind::Para, words: Vec::new() }, link: None };
    let mut skip: Option<String> = None;
    let mut in_title = false;
    let mut pre = false;
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip.is_none() {
                b.words(rest, pre);
            }
            break;
        };
        let text_part = &rest[..lt];
        if in_title {
            b.page.title.push_str(&decode_text(text_part));
        } else if skip.is_none() {
            b.words(text_part, pre);
        }
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = tag.starts_with('/');
        let body = tag.trim_start_matches('/');
        let name_end = body.find(|c: char| c.is_whitespace() || c == '/' || c == '>').unwrap_or(body.len());
        let name = body[..name_end].to_ascii_lowercase();
        if let Some(s) = &skip {
            if closing && name == *s {
                skip = None;
            }
            continue;
        }
        match name.as_str() {
            "script" | "style" | "noscript" | "svg" | "template" | "select" | "button" if !closing => {
                if !tag.ends_with('/') {
                    skip = Some(name.clone());
                }
            }
            "title" => in_title = !closing,
            "h1" | "h2" | "h3" | "h4" => b.end_block(if closing { Kind::Para } else { Kind::Heading }),
            "li" | "dt" if !closing => {
                b.end_block(Kind::Item);
            }
            "pre" => {
                pre = !closing;
                b.end_block(if closing { Kind::Para } else { Kind::Pre });
            }
            "p" | "div" | "br" | "tr" | "ul" | "ol" | "dl" | "dd" | "table" | "section" | "article" | "header" | "footer"
            | "nav" | "main" | "blockquote" | "hr" | "form" | "aside" | "figure" | "li" | "dt" | "center" | "td" | "th" => {
                let k = if pre { Kind::Pre } else { Kind::Para };
                if name == "td" || name == "th" {
                    // Table cells on one row stay together, separated by a space.
                    if !closing && !b.cur.words.is_empty() {
                        b.cur.words.push((String::from("|"), None));
                    }
                } else {
                    b.end_block(k);
                }
            }
            "a" => {
                if closing {
                    b.link = None;
                } else if let Some(h) = attr(body, "href").and_then(|h| resolve(base, &decode_text(&h))) {
                    b.page.links.push(h);
                    b.link = Some(b.page.links.len() - 1);
                }
            }
            "img" if !closing => {
                if let Some(alt) = attr(body, "alt").filter(|a| !a.trim().is_empty()) {
                    b.words(&format!("[{}]", alt.trim()), false);
                }
            }
            "input" if !closing => {
                if let Some(v) = attr(body, "value").filter(|a| !a.trim().is_empty()) {
                    b.words(&format!("[{}]", v.trim()), false);
                }
            }
            _ => {}
        }
    }
    b.end_block(Kind::Para);
    b.page.title = b.page.title.split_whitespace().collect::<Vec<_>>().join(" ");
    b.page
}

/// The value of attribute `name` in the inside of a tag (`a href="x" class=y`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let vstart = tag.len() - rest.len() + 1;
        let v = tag[vstart..].trim_start();
        return Some(match v.chars().next() {
            Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or("").to_string(),
            _ => v.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or("").to_string(),
        });
    }
    None
}

/// The built-in start page.
pub fn home_page() -> Page {
    let html = "<title>Crafting Table</title>\
        <h1>Crafting Table</h1>\
        <p>The BedrockOS web browser. Type an address or words to search for in the bar above (Ctrl+L), then Enter.</p>\
        <p>Pages are shown as text: headings, paragraphs and links. Click a link, or press Tab to pick one and Enter to open it. Backspace goes back.</p>\
        <h2>Try these</h2>\
        <ul><li><a href=\"http://info.cern.ch/hypertext/WWW/TheProject.html\">The first web page (CERN, 1991)</a></li>\
        <li><a href=\"http://frogfind.com/\">FrogFind: the web made simple for old browsers</a></li>\
        <li><a href=\"https://en.wikipedia.org/wiki/Minecraft\">Wikipedia: Minecraft</a></li>\
        <li><a href=\"https://lite.duckduckgo.com/lite/?q=bedrock\">Search: bedrock</a></li>\
        <li><a href=\"https://news.ycombinator.com/\">Hacker News</a></li>\
        <li><a href=\"https://www.rust-lang.org/\">The Rust language</a></li></ul>\
        <h2>How pages arrive</h2>\
        <p>http:// pages are fetched by BedrockOS itself. https:// pages need encryption (TLS), which BedrockOS does not have: \
        they are fetched for it by the helper on the laptop (python tools/bedrock-host.py), which also does Wi-Fi.</p>";
    parse(html, HOME)
}

/// One laid-out line: x offset per word and where it is drawn.
struct Word<'a> {
    x: i32,
    text: &'a str,
    link: Option<usize>,
}

pub struct Browser {
    pub url: String,
    pub input: String,
    pub editing: bool,
    pub page: Page,
    history: Vec<String>,
    /// Scroll position in lines and the selected link.
    top: usize,
    sel: Option<usize>,
    pub status: String,
    /// A page the kernel should fetch.
    pub pending: Option<String>,
    pub loading: bool,
    /// Clickable areas from the last drawing.
    hits: Vec<(Rect, usize)>,
    rows_shown: usize,
    rows_total: usize,
}

impl Default for Browser {
    fn default() -> Self {
        Browser {
            url: String::from(HOME),
            input: String::new(),
            editing: false,
            page: home_page(),
            history: Vec::new(),
            top: 0,
            sel: None,
            status: String::new(),
            pending: None,
            loading: false,
            hits: Vec::new(),
            rows_shown: 1,
            rows_total: 0,
        }
    }
}

impl Browser {
    /// Starts loading `url` (built-in pages show at once).
    pub fn go(&mut self, url: &str, remember: bool) {
        if remember && self.url != url {
            self.history.push(self.url.clone());
        }
        self.editing = false;
        if url == HOME {
            self.url = String::from(HOME);
            self.page = home_page();
            self.top = 0;
            self.sel = None;
            self.status.clear();
            return;
        }
        self.pending = Some(url.to_string());
        self.loading = true;
        self.status = format!("Loading {url} ...");
    }

    /// The kernel's answer for a fetch: the final address and the HTML, or an error.
    pub fn loaded(&mut self, url: &str, result: Result<(String, String), String>) {
        self.loading = false;
        match result {
            Ok((final_url, html)) => {
                self.url = if final_url.is_empty() { url.to_string() } else { final_url };
                self.page = parse(&html, &self.url);
                self.top = 0;
                self.sel = None;
                self.status = format!("{} links", self.page.links.len());
            }
            Err(e) => {
                self.url = url.to_string();
                self.page = parse(&format!("<h1>Could not open this page</h1><p>{}</p><p>{}</p>", url, e), HOME);
                self.top = 0;
                self.sel = None;
                self.status = String::from("Error");
            }
        }
    }

    fn back(&mut self) {
        if let Some(u) = self.history.pop() {
            self.go(&u, false);
        }
    }

    fn scroll(&mut self, d: i32) {
        let max = self.rows_total.saturating_sub(self.rows_shown);
        self.top = (self.top as i32 + d).clamp(0, max as i32) as usize;
    }

    pub fn key(&mut self, k: Key) -> Effect {
        if self.editing {
            match k {
                Key::Char(b) if (32..127).contains(&b) && self.input.len() < 200 => self.input.push(b as char),
                Key::Backspace => {
                    self.input.pop();
                }
                Key::Enter => {
                    let u = address_for(&self.input);
                    if !self.input.trim().is_empty() {
                        self.go(&u, true);
                    }
                }
                Key::Esc => self.editing = false,
                _ => return Effect::None,
            }
            return Effect::Redraw;
        }
        let n = self.page.links.len();
        match k {
            Key::ClearScreen | Key::Ctrl(b'l') | Key::Char(b'/') => {
                self.editing = true;
                self.input = if self.url == HOME { String::new() } else { self.url.clone() };
            }
            Key::Tab if n > 0 => self.sel = Some(self.sel.map_or(0, |s| (s + 1) % n)),
            Key::Enter => match self.sel.and_then(|s| self.page.links.get(s)).cloned() {
                Some(u) => self.go(&u, true),
                None => return Effect::None,
            },
            Key::Backspace | Key::Left => self.back(),
            Key::Up => self.scroll(-3),
            Key::Down => self.scroll(3),
            Key::PageUp => self.scroll(-(self.rows_shown as i32 - 2).max(1)),
            Key::PageDown | Key::Char(b' ') => self.scroll((self.rows_shown as i32 - 2).max(1)),
            Key::Home => self.top = 0,
            Key::End => self.scroll(i32::MAX / 2),
            Key::Char(b'h') => self.go(HOME, true),
            Key::Char(b'r') if self.url != HOME => {
                let u = self.url.clone();
                self.go(&u, false);
            }
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    fn bar(body: Rect) -> Rect {
        Rect::new(body.x + 12, body.y + 8, body.w - 24, 34)
    }

    pub fn click(&mut self, body: Rect, x: i32, y: i32) -> Effect {
        if Self::bar(body).contains(x, y) {
            if !self.editing {
                self.editing = true;
                self.input = if self.url == HOME { String::new() } else { self.url.clone() };
            }
            return Effect::Redraw;
        }
        self.editing = false;
        if let Some(&(_, l)) = self.hits.iter().find(|(r, _)| r.contains(x, y)) {
            if let Some(u) = self.page.links.get(l).cloned() {
                self.sel = Some(l);
                self.go(&u, true);
            }
        }
        Effect::Redraw
    }

    pub fn draw(&mut self, c: &mut Canvas, body: Rect, f: &Fonts) {
        c.fill_rect(body, BG);
        // Address bar.
        c.fill_rect(Rect::new(body.x, body.y, body.w, 50), SIDEBAR);
        let bar = Self::bar(body);
        let edge = if self.editing { DARK.accent } else { rgb(60, 70, 100) };
        c.round_rect(bar.x as f32, bar.y as f32, bar.w as f32, bar.h as f32, 8.0, rgb(14, 17, 28), 1.0);
        c.round_rect_stroke(bar.x as f32 + 0.5, bar.y as f32 + 0.5, bar.w as f32 - 1.0, bar.h as f32 - 1.0, 8.0, 1.0, edge, 1.0);
        let ty = bar.y + (bar.h - f.sans.height as i32) / 2;
        clipped(c, bar.inflate(-10), |c| {
            if self.editing {
                let s = format!("{}_", self.input);
                let w = canvas::text_width(f.sans, &s, 0);
                let x = (bar.x + 12).min(bar.right() - 12 - w);
                text(c, f.sans, x, ty, &s, DARK.text);
                if self.input.is_empty() {
                    text(c, f.sans, bar.x + 24, ty, "Type an address or words to search for", DARK.text_dim);
                }
            } else {
                let lock = if self.url.starts_with("https://") { "[secure] " } else { "" };
                let shown = if self.url == HOME { String::from("Crafting Table start page") } else { format!("{lock}{}", self.url) };
                text(c, f.sans, bar.x + 12, ty, &shown, DARK.text_dim);
            }
        });
        // Page.
        let area = Rect::new(body.x + 24, body.y + 60, body.w - 48, body.h - 60 - 34);
        let rh = f.sans.height as i32 + 8;
        let space = canvas::text_width(f.sans, " ", 0).max(4);
        let mut rows: Vec<(Kind, Vec<Word>)> = Vec::new();
        let first_heading: String = match self.page.blocks.first() {
            Some(b) if b.kind == Kind::Heading => b.words.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        };
        if !self.page.title.is_empty() && first_heading != self.page.title {
            rows.push((Kind::Heading, alloc::vec![Word { x: 0, text: self.page.title.as_str(), link: None }]));
            rows.push((Kind::Para, Vec::new()));
        }
        for blk in &self.page.blocks {
            let indent = if blk.kind == Kind::Item { 22 } else { 0 };
            let mut line: Vec<Word> = Vec::new();
            let mut x = indent;
            for (w, link) in &blk.words {
                let ww = canvas::text_width(f.sans, w, 0);
                if x > indent && x + ww > area.w {
                    rows.push((blk.kind, core::mem::take(&mut line)));
                    x = indent;
                }
                line.push(Word { x, text: w.as_str(), link: *link });
                x += ww + space;
            }
            if !line.is_empty() {
                rows.push((blk.kind, line));
            }
            if matches!(blk.kind, Kind::Para | Kind::Heading) {
                rows.push((Kind::Para, Vec::new()));
            }
        }
        self.rows_total = rows.len();
        self.rows_shown = (area.h / rh).max(1) as usize;
        self.top = self.top.min(self.rows_total.saturating_sub(self.rows_shown));
        // Keep the selected link in view.
        if let Some(s) = self.sel {
            if let Some(i) = rows.iter().position(|(_, ws)| ws.iter().any(|w| w.link == Some(s))) {
                if i < self.top || i >= self.top + self.rows_shown {
                    self.top = i.saturating_sub(2).min(self.rows_total.saturating_sub(self.rows_shown));
                }
            }
        }
        self.hits.clear();
        let mut hits = Vec::new();
        let (top, sel) = (self.top, self.sel);
        clipped(c, area, |c| {
            for (i, (kind, ws)) in rows.iter().enumerate().skip(top).take(self.rows_shown + 1) {
                let y = area.y + (i - top) as i32 * rh;
                if *kind == Kind::Item {
                    c.circle((area.x + 8) as f32, (y + rh / 2 - 3) as f32, 3.0, DARK.text_dim, 1.0);
                }
                for w in ws {
                    let x = area.x + w.x;
                    let ww = canvas::text_width(f.sans, w.text, 0);
                    let col = match (w.link, kind) {
                        (Some(l), _) if Some(l) == sel => {
                            c.round_rect((x - 3) as f32, (y - 1) as f32, (ww + 6) as f32, (rh - 4) as f32, 4.0, rgb(48, 64, 104), 1.0);
                            rgb(170, 210, 255)
                        }
                        (Some(_), _) => rgb(110, 176, 255),
                        (None, Kind::Heading) => DARK.accent,
                        (None, Kind::Pre) => rgb(200, 210, 190),
                        _ => DARK.text,
                    };
                    let face = if *kind == Kind::Pre { f.mono } else { f.sans };
                    text(c, face, x, y, w.text, col);
                    if *kind == Kind::Heading {
                        text(c, face, x + 1, y, w.text, col);
                    }
                    if let Some(l) = w.link {
                        c.fill_rect(Rect::new(x, y + f.sans.height as i32 + 1, ww, 1), col);
                        hits.push((Rect::new(x - 2, y - 2, ww + 4 + space, rh), l));
                    }
                }
            }
        });
        self.hits = hits;
        // Status line.
        let fy = body.bottom() - 26;
        c.fill_rect(Rect::new(body.x, fy - 6, body.w, 1), DARK.border);
        let pct = if self.rows_total > self.rows_shown { (self.top + self.rows_shown).min(self.rows_total) * 100 / self.rows_total } else { 100 };
        let hint = if self.loading {
            self.status.clone()
        } else {
            format!("{}   {}%   Ctrl+L address   Tab link   Enter open   Backspace back   H home", self.status, pct)
        };
        let col = if self.loading { DARK.warn } else { DARK.text_dim };
        clipped(c, Rect::new(body.x, fy - 4, body.w, 30), |c| text(c, f.sans, body.x + 14, fy, &hint, col));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_links() {
        let b = "https://en.wikipedia.org/wiki/Minecraft?x=1";
        assert_eq!(resolve(b, "/wiki/Steve").as_deref(), Some("https://en.wikipedia.org/wiki/Steve"));
        assert_eq!(resolve(b, "Creeper").as_deref(), Some("https://en.wikipedia.org/wiki/Creeper"));
        assert_eq!(resolve(b, "../a/b?c=d").as_deref(), Some("https://en.wikipedia.org/a/b?c=d"));
        assert_eq!(resolve(b, "//upload.wikimedia.org/x.png").as_deref(), Some("https://upload.wikimedia.org/x.png"));
        assert_eq!(resolve(b, "http://example.com").as_deref(), Some("http://example.com"));
        assert_eq!(resolve(b, "#top"), None);
        assert_eq!(resolve(b, "mailto:a@b.c"), None);
        assert_eq!(resolve("http://info.cern.ch", "x.html").as_deref(), Some("http://info.cern.ch/x.html"));
    }

    #[test]
    fn address_bar() {
        assert_eq!(address_for("example.com"), "https://example.com");
        assert_eq!(address_for("http://info.cern.ch"), "http://info.cern.ch");
        assert_eq!(address_for("minecraft wiki"), "https://lite.duckduckgo.com/lite/?q=minecraft+wiki");
        assert_eq!(address_for("c++"), "https://lite.duckduckgo.com/lite/?q=c%2B%2B");
    }

    #[test]
    fn parses_html() {
        let p = parse(
            "<html><head><title>T &amp; U</title><style>p{}</style><script>var a='<p>';</script></head>\
             <body><h1>Head</h1><p>Hello&nbsp;<a href='/x'>the link</a> &#8212; done</p><ul><li>one<li>two</ul>\
             <!-- <p>hidden</p> --><img alt=\"a cat\"></body></html>",
            "http://h.com/dir/page",
        );
        assert_eq!(p.title, "T & U");
        assert_eq!(p.links, ["http://h.com/x"]);
        assert_eq!(p.blocks[0].kind, Kind::Heading);
        let words: Vec<&str> = p.blocks[1].words.iter().map(|w| w.0.as_str()).collect();
        assert_eq!(words, ["Hello", "the", "link", "-", "done"]);
        assert_eq!(p.blocks[1].words[1].1, Some(0));
        assert_eq!(p.blocks[2].kind, Kind::Item);
        assert_eq!(p.blocks[3].words[0].0, "two");
        assert!(p.blocks.iter().all(|b| b.words.iter().all(|w| w.0 != "hidden" && !w.0.contains("var"))));
        assert_eq!(p.blocks.last().unwrap().words[0].0, "[a");
    }

    #[test]
    fn keys_and_loading() {
        let mut b = Browser::default();
        assert!(b.page.links.len() >= 5, "home page has links");
        b.key(Key::ClearScreen);
        for c in b"rust lang" {
            b.key(Key::Char(*c));
        }
        b.key(Key::Enter);
        assert_eq!(b.pending.as_deref(), Some("https://lite.duckduckgo.com/lite/?q=rust+lang"));
        let u = b.pending.take().unwrap();
        b.loaded(&u, Ok((u.clone(), String::from("<a href='http://a.b/'>A</a>"))));
        assert_eq!(b.page.links, ["http://a.b/"]);
        b.key(Key::Tab);
        b.key(Key::Enter);
        assert_eq!(b.pending.as_deref(), Some("http://a.b/"));
        b.pending = None;
        b.loaded("http://a.b/", Err(String::from("no route")));
        b.key(Key::Backspace);
        assert_eq!(b.pending.as_deref(), Some(u.as_str()), "back to the search");
    }
}
