//! Crafting Table, BedrockOS's web browser. It reads HTML and shows it as text: headings, paragraphs, lists and
//! links you can follow (click one, or Tab to it and press Enter). There are no images, styles or scripts.
//! The address bar takes a web address or words to search for (DuckDuckGo's light page).
//!
//! Pure like the other apps: the kernel fetches pages (`bootfx` hands the request to the shell task, which
//! fetches through the laptop's helper or, for `http://`, directly) and calls [`Browser::loaded`].

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::apps::{clipped, text, Effect, Fonts};
use crate::gfont::{self, Face};
use crate::gui::canvas::{self, rgb, Canvas, Color, Rect};
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
            // Site menus come first on most pages; like reader mode, they are left out (when they end).
            "nav" if !closing && rest.contains("</nav") => skip = Some(name.clone()),
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
            "input" if !closing && !attr(body, "type").is_some_and(|t| t.eq_ignore_ascii_case("hidden")) => {
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
        <ul><li><a href=\"https://en.wikipedia.org/wiki/Minecraft\">Wikipedia</a></li>\
        <li><a href=\"https://lite.duckduckgo.com/lite/?q=minecraft\">DuckDuckGo</a></li>\
        <li><a href=\"https://news.ycombinator.com/\">Hacker News</a></li>\
        <li><a href=\"http://info.cern.ch/hypertext/WWW/TheProject.html\">CERN 1991</a></li>\
        <li><a href=\"http://frogfind.com/\">FrogFind</a></li>\
        <li><a href=\"https://www.rust-lang.org/\">Rust</a></li></ul>\
        <h2>How pages arrive</h2>\
        <p>http:// pages are fetched by BedrockOS itself. https:// pages need encryption (TLS), which BedrockOS does not have: \
        they are fetched for it by the helper on the laptop (python tools/bedrock-host.py), which also does Wi-Fi.</p>";
    parse(html, HOME)
}

/// One laid-out word: where it goes on its line.
struct Word<'a> {
    x: i32,
    text: &'a str,
    link: Option<usize>,
}

/// One laid-out line of the page.
struct Row<'a> {
    kind: Kind,
    words: Vec<Word<'a>>,
    h: i32,
}

// Light colours, like the browsers people know.
const PAGE: Color = rgb(255, 255, 255);
const INK: Color = rgb(32, 33, 36);
const INK_DIM: Color = rgb(95, 99, 104);
const LINK: Color = rgb(26, 13, 171);
const LINK_SEL: Color = rgb(232, 240, 254);
const STRIP: Color = rgb(222, 225, 230);
const TOOLBAR: Color = rgb(255, 255, 255);
const OMNI: Color = rgb(241, 243, 244);
const LINE: Color = rgb(218, 220, 224);
const BLUE: Color = rgb(26, 115, 232);
const ICON: Color = rgb(95, 99, 104);
const ICON_OFF: Color = rgb(189, 193, 198);

/// The bookmarks bar.
const BOOKMARKS: [(&str, &str); 5] = [
    ("Wikipedia", "https://en.wikipedia.org/wiki/Minecraft"),
    ("DuckDuckGo", "https://lite.duckduckgo.com/lite/"),
    ("Hacker News", "https://news.ycombinator.com/"),
    ("First web page", "http://info.cern.ch/hypertext/WWW/TheProject.html"),
    ("Rust", "https://www.rust-lang.org/"),
];

/// Where each part of the browser window goes.
#[derive(Clone, Copy, Default)]
struct Chrome {
    tab: Rect,
    tab_close: Rect,
    new_tab: Rect,
    toolbar: Rect,
    back: Rect,
    fwd: Rect,
    reload: Rect,
    home: Rect,
    omni: Rect,
    menu: Rect,
    marks: Rect,
    page: Rect,
    scroll: Rect,
}

fn chrome(body: Rect, f: &Fonts) -> Chrome {
    let th = f.sans.height as i32;
    let strip_h = th + 20;
    let tool_h = th + 24;
    let marks_h = th + 12;
    let tab_w = (body.w / 3).clamp(170, 320);
    let tab = Rect::new(body.x + 10, body.y + 7, tab_w, strip_h - 7);
    let bs = tool_h - 12;
    let toolbar = Rect::new(body.x, body.y + strip_h, body.w, tool_h);
    let by = toolbar.y + 6;
    let back = Rect::new(body.x + 8, by, bs, bs);
    let fwd = Rect::new(back.right() + 2, by, bs, bs);
    let reload = Rect::new(fwd.right() + 2, by, bs, bs);
    let home = Rect::new(reload.right() + 2, by, bs, bs);
    let menu = Rect::new(body.right() - 8 - bs, by, bs, bs);
    let ox = home.right() + 10;
    let omni = Rect::new(ox, toolbar.y + 6, menu.x - 10 - ox, tool_h - 12);
    let marks = Rect::new(body.x, toolbar.bottom(), body.w, marks_h);
    let py = marks.bottom() + 1;
    let page = Rect::new(body.x, py, body.w - 12, body.bottom() - py);
    Chrome {
        tab,
        tab_close: Rect::new(tab.right() - 28, tab.y + (tab.h - 20) / 2, 20, 20),
        new_tab: Rect::new(tab.right() + 8, tab.y + (tab.h - 28) / 2, 28, 28),
        toolbar,
        back,
        fwd,
        reload,
        home,
        omni,
        menu,
        marks,
        page,
        scroll: Rect::new(body.right() - 12, py, 12, page.h),
    }
}

/// A round-ended line made of dots (the canvas has no line primitive); for small icons.
fn seg(c: &mut Canvas, a: (f32, f32), b: (f32, f32), w: f32, col: Color) {
    let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) * 2.0) as i32 + 1;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        c.circle(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, w / 2.0, col, 1.0);
    }
}

fn centre(r: Rect) -> (f32, f32) {
    (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0)
}

/// The site part of an address ("en.wikipedia.org") and the rest.
fn split_url(url: &str) -> (&str, &str) {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    }
}

/// A small coloured square with the first letter of `name`, standing in for a site's icon.
fn site_icon(c: &mut Canvas, f: &Face, x: i32, y: i32, s: i32, name: &str) {
    let h = name.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    let cols = [rgb(66, 133, 244), rgb(219, 68, 55), rgb(244, 180, 0), rgb(15, 157, 88), rgb(171, 71, 188), rgb(0, 172, 193)];
    c.round_rect(x as f32, y as f32, s as f32, s as f32, s as f32 * 0.25, cols[(h % 6) as usize], 1.0);
    let ch = name.get(..1).unwrap_or("?");
    let up = [ch.as_bytes()[0].to_ascii_uppercase()];
    let l = core::str::from_utf8(&up).unwrap_or("?");
    let w = canvas::text_width(f, l, 0);
    c.text(f, x + (s - w) / 2, y + (s - f.height as i32) / 2, l, rgb(255, 255, 255), 1.0);
}

/// The Crafting Table tab icon: a little crafting-table block.
fn table_icon(c: &mut Canvas, x: i32, y: i32, s: i32) {
    c.fill_rect(Rect::new(x, y, s, s), rgb(150, 104, 58));
    c.fill_rect(Rect::new(x, y, s, s / 3), rgb(186, 140, 84));
    c.fill_rect(Rect::new(x, y + s / 3, s, 1), rgb(96, 64, 34));
    c.fill_rect(Rect::new(x + s / 2, y, 1, s / 3), rgb(96, 64, 34));
    c.fill_rect(Rect::new(x + s / 4, y + s / 2, s / 2, s / 3), rgb(110, 76, 40));
}

pub struct Browser {
    pub url: String,
    pub input: String,
    pub editing: bool,
    pub page: Page,
    history: Vec<String>,
    forward: Vec<String>,
    /// Scroll position in lines and the selected link.
    top: usize,
    sel: Option<usize>,
    pub status: String,
    /// A page the kernel should fetch.
    pub pending: Option<String>,
    pub loading: bool,
    /// Clickable links from the last drawing: area, address, link number.
    hits: Vec<(Rect, usize)>,
    marks: Vec<(Rect, &'static str)>,
    ui: Chrome,
    search_box: Rect,
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
            forward: Vec::new(),
            top: 0,
            sel: None,
            status: String::new(),
            pending: None,
            loading: false,
            hits: Vec::new(),
            marks: Vec::new(),
            ui: Chrome::default(),
            search_box: Rect::default(),
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
            self.forward.clear();
        }
        self.editing = false;
        if url == HOME {
            self.url = String::from(HOME);
            self.page = home_page();
            self.top = 0;
            self.sel = None;
            self.status.clear();
            self.loading = false;
            return;
        }
        self.pending = Some(url.to_string());
        self.loading = true;
        self.status = format!("Waiting for {}...", split_url(url).0);
    }

    /// The kernel's answer for a fetch: the final address and the HTML, or an error.
    pub fn loaded(&mut self, url: &str, result: Result<(String, String), String>) {
        self.loading = false;
        match result {
            Ok((final_url, html)) => {
                self.url = if final_url.is_empty() { url.to_string() } else { final_url };
                self.page = parse(&html, &self.url);
                self.status.clear();
            }
            Err(e) => {
                self.url = url.to_string();
                let site = split_url(url).0;
                self.page = parse(
                    &format!("<title>{site}</title><h1>This site can't be reached</h1><p>{site} could not be opened.</p><p>{e}</p><ul><li>Check the address</li><li>Check the connection (ping 8.8.8.8 in ChatBox)</li><li>For https:// pages, check the laptop helper is running</li></ul>"),
                    HOME,
                );
                self.status.clear();
            }
        }
        self.top = 0;
        self.sel = None;
    }

    fn back(&mut self) {
        if let Some(u) = self.history.pop() {
            self.forward.push(self.url.clone());
            self.go(&u, false);
        }
    }

    fn fwd(&mut self) {
        if let Some(u) = self.forward.pop() {
            self.history.push(self.url.clone());
            self.go(&u, false);
        }
    }

    fn reload(&mut self) {
        let u = self.url.clone();
        self.go(&u, false);
    }

    fn edit_address(&mut self) {
        self.editing = true;
        self.input = if self.url == HOME { String::new() } else { self.url.clone() };
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
            Key::ClearScreen | Key::Ctrl(b'l') | Key::Char(b'/') => self.edit_address(),
            Key::Tab if n > 0 => self.sel = Some(self.sel.map_or(0, |s| (s + 1) % n)),
            Key::Enter => match self.sel.and_then(|s| self.page.links.get(s)).cloned() {
                Some(u) => self.go(&u, true),
                None => return Effect::None,
            },
            Key::Backspace | Key::Left => self.back(),
            Key::Right => self.fwd(),
            Key::Up => self.scroll(-3),
            Key::Down => self.scroll(3),
            Key::PageUp => self.scroll(-(self.rows_shown as i32 - 2).max(1)),
            Key::PageDown | Key::Char(b' ') => self.scroll((self.rows_shown as i32 - 2).max(1)),
            Key::Home => self.top = 0,
            Key::End => self.scroll(i32::MAX / 2),
            Key::Char(b'h') => self.go(HOME, true),
            Key::Char(b'r') | Key::Ctrl(b'r') => self.reload(),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    pub fn click(&mut self, _body: Rect, x: i32, y: i32) -> Effect {
        let ui = self.ui;
        if ui.omni.contains(x, y) || (self.url == HOME && self.search_box.contains(x, y)) {
            if !self.editing {
                self.edit_address();
            }
            return Effect::Redraw;
        }
        self.editing = false;
        if ui.back.contains(x, y) {
            self.back();
        } else if ui.fwd.contains(x, y) {
            self.fwd();
        } else if ui.reload.contains(x, y) {
            self.reload();
        } else if ui.home.contains(x, y) || ui.new_tab.contains(x, y) || ui.tab_close.contains(x, y) {
            self.go(HOME, true);
        } else if let Some(&(_, u)) = self.marks.iter().find(|(r, _)| r.contains(x, y)) {
            self.go(u, true);
        } else if ui.scroll.contains(x, y) && self.rows_total > self.rows_shown {
            // A click on the scroll bar jumps there.
            let frac = (y - ui.scroll.y).clamp(0, ui.scroll.h) as usize;
            self.top = (self.rows_total - self.rows_shown) * frac / ui.scroll.h.max(1) as usize;
        } else if let Some(&(_, l)) = self.hits.iter().find(|(r, _)| r.contains(x, y)) {
            if let Some(u) = self.page.links.get(l).cloned() {
                self.sel = Some(l);
                self.go(&u, true);
            }
        }
        Effect::Redraw
    }

    pub fn draw(&mut self, c: &mut Canvas, body: Rect, f: &Fonts) {
        let ui = chrome(body, f);
        self.ui = ui;
        self.draw_frame(c, body, f, &ui);
        c.fill_rect(ui.page, PAGE);
        if self.url == HOME {
            self.draw_start(c, f, &ui);
        } else {
            self.draw_page(c, f, &ui);
        }
        // Status bubble, bottom left (where the page is loading from, or the chosen link's address).
        let bubble = if self.loading {
            Some(self.status.clone())
        } else {
            self.sel.and_then(|s| self.page.links.get(s)).cloned()
        };
        if let Some(s) = bubble {
            let w = (canvas::text_width(f.sans, &s, 0) + 20).min(ui.page.w * 2 / 3);
            let h = f.sans.height as i32 + 10;
            let r = Rect::new(ui.page.x, ui.page.bottom() - h, w, h);
            c.fill_rect(r, OMNI);
            c.fill_rect(Rect::new(r.x, r.y, r.w, 1), LINE);
            c.fill_rect(Rect::new(r.right() - 1, r.y, 1, r.h), LINE);
            clipped(c, r.inflate(-4), |c| text(c, f.sans, r.x + 10, r.y + 5, &s, INK_DIM));
        }
    }

    /// Tab strip, toolbar (buttons and address bar) and bookmarks bar.
    fn draw_frame(&mut self, c: &mut Canvas, body: Rect, f: &Fonts, ui: &Chrome) {
        let th = f.sans.height as i32;
        c.fill_rect(Rect::new(body.x, body.y, body.w, ui.toolbar.y - body.y), STRIP);
        // The tab, joined to the toolbar.
        let t = ui.tab;
        c.round_rect(t.x as f32, t.y as f32, t.w as f32, (t.h + 12) as f32, 9.0, TOOLBAR, 1.0);
        let ix = t.x + 12;
        let is = th.min(18);
        if self.loading {
            let (cx, cy) = (ix as f32 + is as f32 / 2.0, (t.y + t.h / 2) as f32);
            c.arc(cx, cy, is as f32 / 2.0 - 1.0, 2.0, 1.0, LINE, 1.0);
            c.arc(cx, cy, is as f32 / 2.0 - 1.0, 2.0, 0.3, BLUE, 1.0);
        } else {
            table_icon(c, ix, t.y + (t.h - is) / 2, is);
        }
        let title: String = if self.loading {
            String::from("Loading...")
        } else if self.url == HOME {
            String::from("New Tab")
        } else if !self.page.title.is_empty() {
            self.page.title.clone()
        } else {
            String::from(split_url(&self.url).0)
        };
        let tr = Rect::new(ix + is + 8, t.y, t.w - is - 52, t.h);
        clipped(c, tr, |c| text(c, f.sans, tr.x, t.y + (t.h - th) / 2, &title, INK));
        let (cx, cy) = centre(ui.tab_close);
        seg(c, (cx - 4.0, cy - 4.0), (cx + 4.0, cy + 4.0), 1.6, ICON);
        seg(c, (cx - 4.0, cy + 4.0), (cx + 4.0, cy - 4.0), 1.6, ICON);
        let (cx, cy) = centre(ui.new_tab);
        seg(c, (cx - 6.0, cy), (cx + 6.0, cy), 1.8, ICON);
        seg(c, (cx, cy - 6.0), (cx, cy + 6.0), 1.8, ICON);

        // Toolbar.
        c.fill_rect(ui.toolbar, TOOLBAR);
        let s = ui.back.w as f32 * 0.22;
        let col = |on: bool| if on { ICON } else { ICON_OFF };
        let (cx, cy) = centre(ui.back);
        let k = col(!self.history.is_empty());
        seg(c, (cx + s, cy), (cx - s, cy), 2.0, k);
        seg(c, (cx - s, cy), (cx, cy - s), 2.0, k);
        seg(c, (cx - s, cy), (cx, cy + s), 2.0, k);
        let (cx, cy) = centre(ui.fwd);
        let k = col(!self.forward.is_empty());
        seg(c, (cx - s, cy), (cx + s, cy), 2.0, k);
        seg(c, (cx + s, cy), (cx, cy - s), 2.0, k);
        seg(c, (cx + s, cy), (cx, cy + s), 2.0, k);
        let (cx, cy) = centre(ui.reload);
        if self.loading {
            // Stop (an X) while loading, like real browsers.
            seg(c, (cx - s, cy - s), (cx + s, cy + s), 2.0, ICON);
            seg(c, (cx - s, cy + s), (cx + s, cy - s), 2.0, ICON);
        } else {
            c.arc(cx, cy, s * 1.05, 2.0, 0.78, ICON, 1.0);
            seg(c, (cx, cy - s * 1.05), (cx - s * 0.6, cy - s * 1.6), 2.0, ICON);
            seg(c, (cx, cy - s * 1.05), (cx - s * 0.6, cy - s * 0.5), 2.0, ICON);
        }
        let (cx, cy) = centre(ui.home);
        seg(c, (cx - s * 1.2, cy), (cx, cy - s * 1.15), 2.0, ICON);
        seg(c, (cx, cy - s * 1.15), (cx + s * 1.2, cy), 2.0, ICON);
        seg(c, (cx - s * 0.8, cy - s * 0.3), (cx - s * 0.8, cy + s), 2.0, ICON);
        seg(c, (cx + s * 0.8, cy - s * 0.3), (cx + s * 0.8, cy + s), 2.0, ICON);
        seg(c, (cx - s * 0.8, cy + s), (cx + s * 0.8, cy + s), 2.0, ICON);
        let (cx, cy) = centre(ui.menu);
        for d in [-s * 0.9, 0.0, s * 0.9] {
            c.circle(cx, cy + d, 2.0, ICON, 1.0);
        }
        // Address bar.
        let o = ui.omni;
        let rr = o.h as f32 / 2.0;
        if self.editing {
            c.round_rect(o.x as f32, o.y as f32, o.w as f32, o.h as f32, rr, TOOLBAR, 1.0);
            c.round_rect_stroke(o.x as f32 + 1.0, o.y as f32 + 1.0, o.w as f32 - 2.0, o.h as f32 - 2.0, rr, 2.0, BLUE, 1.0);
        } else {
            c.round_rect(o.x as f32, o.y as f32, o.w as f32, o.h as f32, rr, OMNI, 1.0);
        }
        let ty = o.y + (o.h - th) / 2;
        let inner = Rect::new(o.x + 12, o.y, o.w - 24, o.h);
        let secure = self.url.starts_with("https://");
        clipped(c, inner, |c| {
            let mut x = o.x + 16;
            if self.editing {
                let w = canvas::text_width(f.sans, &self.input, 0);
                let sx = x.min(inner.right() - 6 - w);
                text(c, f.sans, sx, ty, &self.input, INK);
                c.fill_rect(Rect::new(sx + w + 1, o.y + 8, 2, o.h - 16), BLUE);
                if self.input.is_empty() {
                    text(c, f.sans, x + 6, ty, "Search DuckDuckGo or type a URL", INK_DIM);
                }
                return;
            }
            if self.url == HOME {
                text(c, f.sans, x, ty, "Search DuckDuckGo or type a URL", INK_DIM);
                return;
            }
            let cy = (o.y + o.h / 2) as f32;
            if secure {
                // Padlock.
                let lx = x as f32;
                seg(c, (lx + 3.0, cy - 1.0), (lx + 3.0, cy - 5.0), 1.6, ICON);
                seg(c, (lx + 3.0, cy - 6.0), (lx + 9.0, cy - 6.0), 1.6, ICON);
                seg(c, (lx + 9.0, cy - 1.0), (lx + 9.0, cy - 5.0), 1.6, ICON);
                c.round_rect(lx, cy - 2.0, 12.0, 9.0, 2.0, ICON, 1.0);
                x += 22;
            } else {
                c.circle(x as f32 + 7.0, cy, 7.0, ICON, 1.0);
                c.circle(x as f32 + 7.0, cy, 5.4, OMNI, 1.0);
                c.fill_rect(Rect::new(x + 6, cy as i32 - 1, 2, 5), ICON);
                c.fill_rect(Rect::new(x + 6, cy as i32 - 4, 2, 2), ICON);
                x += 20;
                x += c.text(f.sans, x, ty, "Not secure", INK_DIM, 1.0);
                c.fill_rect(Rect::new(x + 8, o.y + 9, 1, o.h - 18), INK_DIM);
                x += 18;
            }
            let (site, rest) = split_url(&self.url);
            x += c.text(f.sans, x, ty, site, INK, 1.0);
            text(c, f.sans, x, ty, rest, INK_DIM);
        });
        // Loading bar under the toolbar.
        if self.loading {
            c.fill_rect(Rect::new(body.x, ui.toolbar.bottom() - 3, body.w * 2 / 5, 3), BLUE);
        }
        // Bookmarks bar.
        c.fill_rect(ui.marks, TOOLBAR);
        c.fill_rect(Rect::new(body.x, ui.marks.bottom(), body.w, 1), LINE);
        self.marks.clear();
        let mut x = ui.marks.x + 12;
        let is = th - 2;
        for (name, url) in BOOKMARKS {
            let w = canvas::text_width(f.sans, name, 0) + is + 22;
            if x + w > ui.marks.right() - 8 {
                break;
            }
            let r = Rect::new(x, ui.marks.y + 3, w, ui.marks.h - 6);
            site_icon(c, f.sans, x + 8, r.y + (r.h - is) / 2, is, name);
            text(c, f.sans, x + is + 14, ui.marks.y + (ui.marks.h - th) / 2, name, INK);
            self.marks.push((r, url));
            x += w + 4;
        }
    }

    /// The start page: a big title, a search box and shortcut tiles, like a new tab page.
    fn draw_start(&mut self, c: &mut Canvas, f: &Fonts, ui: &Chrome) {
        let p = ui.page;
        let big = &gfont::SANS_24;
        self.hits.clear();
        self.rows_total = 0;
        let title = "Crafting Table";
        let tw = canvas::text_width(big, title, 0);
        let mut y = p.y + (p.h / 7).max(24);
        let is = big.height as i32 + 8;
        let tx = p.x + (p.w - tw - is - 14) / 2;
        table_icon(c, tx, y - 4, is);
        c.text(big, tx + is + 14, y, title, INK, 1.0);
        c.text(big, tx + is + 15, y, title, INK, 1.0);
        y += big.height as i32 + 26;
        let sw = (p.w - 80).min(560);
        let sb = Rect::new(p.x + (p.w - sw) / 2, y, sw, f.sans.height as i32 + 24);
        self.search_box = sb;
        let rr = sb.h as f32 / 2.0;
        c.shadow(sb.x as f32, sb.y as f32 + 1.0, sb.w as f32, sb.h as f32, rr, 6.0, 0, 0.18);
        c.round_rect(sb.x as f32, sb.y as f32, sb.w as f32, sb.h as f32, rr, PAGE, 1.0);
        c.round_rect_stroke(sb.x as f32 + 0.5, sb.y as f32 + 0.5, sb.w as f32 - 1.0, sb.h as f32 - 1.0, rr, 1.0, LINE, 1.0);
        let (gx, gy) = (sb.x as f32 + 22.0, (sb.y + sb.h / 2) as f32 - 1.0);
        c.circle(gx, gy, 6.5, ICON, 1.0);
        c.circle(gx, gy, 4.8, PAGE, 1.0);
        seg(c, (gx + 4.5, gy + 4.5), (gx + 8.5, gy + 8.5), 2.2, ICON);
        text(c, f.sans, sb.x + 42, sb.y + (sb.h - f.sans.height as i32) / 2, "Search DuckDuckGo or type a URL", INK_DIM);
        y = sb.bottom() + 34;
        // Shortcut tiles: the start page's links.
        let n = self.page.links.len();
        let tile = (f.sans.height as i32 * 7).clamp(110, 176);
        let per_row = ((p.w - 40) / (tile + 16)).clamp(1, 6) as usize;
        let rows = n.div_ceil(per_row);
        for row in 0..rows {
            let k = per_row.min(n - row * per_row);
            let rw = k as i32 * (tile + 16) - 16;
            for col in 0..k {
                let i = row * per_row + col;
                let r = Rect::new(p.x + (p.w - rw) / 2 + col as i32 * (tile + 16), y + row as i32 * (tile + 16), tile, tile);
                let label: Vec<&str> = self.page.blocks.iter().flat_map(|b| b.words.iter()).filter(|w| w.1 == Some(i)).map(|w| w.0.as_str()).collect();
                let label = label.join(" ");
                let label = label.split(':').next().unwrap_or("").split('(').next().unwrap_or("").trim();
                let sel = self.sel == Some(i);
                c.round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 12.0, if sel { LINK_SEL } else { OMNI }, 1.0);
                if sel {
                    c.round_rect_stroke(r.x as f32 + 1.0, r.y as f32 + 1.0, r.w as f32 - 2.0, r.h as f32 - 2.0, 12.0, 2.0, BLUE, 1.0);
                }
                let s = tile * 2 / 5;
                site_icon(c, f.sans, r.x + (r.w - s) / 2, r.y + tile / 6, s, label);
                let lw = canvas::text_width(f.sans, label, 0);
                let lr = Rect::new(r.x + 6, r.bottom() - f.sans.height as i32 - 12, r.w - 12, f.sans.height as i32 + 4);
                clipped(c, lr, |c| text(c, f.sans, r.x + ((r.w - lw) / 2).max(6), lr.y, label, INK));
                self.hits.push((r, i));
            }
        }
        y += rows as i32 * (tile + 16) + 14;
        let note = "Pages are shown as text. https:// pages come through the laptop helper.";
        let nw = canvas::text_width(f.sans, note, 0);
        clipped(c, p, |c| text(c, f.sans, p.x + (p.w - nw) / 2, y, note, INK_DIM));
    }

    /// A web page: headings, paragraphs, lists and links on white, with a scroll bar.
    fn draw_page(&mut self, c: &mut Canvas, f: &Fonts, ui: &Chrome) {
        let p = ui.page;
        let hf = &gfont::SANS_24;
        let margin = ((p.w - 900) / 2).max(32);
        let area = Rect::new(p.x + margin, p.y + 18, p.w - 2 * margin, p.h - 18);
        let body_h = f.sans.height as i32 + 8;
        let space = canvas::text_width(f.sans, " ", 0).max(4);
        let mut rows: Vec<Row> = Vec::new();
        for blk in &self.page.blocks {
            let (face, h) = match blk.kind {
                Kind::Heading => (hf, hf.height as i32 + 14),
                Kind::Pre => (f.mono, f.mono.height as i32 + 4),
                _ => (f.sans, body_h),
            };
            let indent = if blk.kind == Kind::Item { 26 } else { 0 };
            let mut line: Vec<Word> = Vec::new();
            let mut x = indent;
            for (w, link) in &blk.words {
                let ww = canvas::text_width(face, w, 0);
                if x > indent && x + ww > area.w {
                    rows.push(Row { kind: blk.kind, words: core::mem::take(&mut line), h });
                    x = indent;
                }
                line.push(Word { x, text: w.as_str(), link: *link });
                x += ww + space;
            }
            if !line.is_empty() {
                rows.push(Row { kind: blk.kind, words: line, h });
            }
            if matches!(blk.kind, Kind::Para | Kind::Heading) {
                rows.push(Row { kind: Kind::Para, words: Vec::new(), h: body_h / 2 });
            }
        }
        self.rows_total = rows.len();
        self.rows_shown = (area.h / body_h).max(1) as usize;
        self.top = self.top.min(self.rows_total.saturating_sub(self.rows_shown));
        if let Some(s) = self.sel {
            if let Some(i) = rows.iter().position(|r| r.words.iter().any(|w| w.link == Some(s))) {
                if i < self.top || i >= self.top + self.rows_shown {
                    self.top = i.saturating_sub(2).min(self.rows_total.saturating_sub(self.rows_shown));
                }
            }
        }
        let (top, sel) = (self.top, self.sel);
        let mut hits = Vec::new();
        clipped(c, Rect::new(p.x, p.y, p.w, p.h), |c| {
            let mut y = area.y;
            for row in rows.iter().skip(top) {
                if y > area.bottom() {
                    break;
                }
                let face = match row.kind {
                    Kind::Heading => hf,
                    Kind::Pre => f.mono,
                    _ => f.sans,
                };
                if row.kind == Kind::Pre {
                    c.fill_rect(Rect::new(area.x - 8, y, area.w + 16, row.h), rgb(246, 248, 250));
                }
                if row.kind == Kind::Item && row.words.first().is_some_and(|w| w.x == 26) {
                    c.circle((area.x + 10) as f32, (y + row.h / 2 - 2) as f32, 2.8, INK, 1.0);
                }
                for (wi, w) in row.words.iter().enumerate() {
                    let x = area.x + w.x;
                    let tw = canvas::text_width(face, w.text, 0);
                    // A link's words share one underline: reach over the gap to the next word of the same link.
                    let ww = match row.words.get(wi + 1) {
                        Some(n) if w.link.is_some() && n.link == w.link => n.x - w.x,
                        _ => tw,
                    };
                    let col = match (w.link, row.kind) {
                        (Some(l), _) if Some(l) == sel => {
                            c.round_rect((x - 3) as f32, (y - 1) as f32, (ww + 6) as f32, (face.height + 4) as f32, 3.0, LINK_SEL, 1.0);
                            c.round_rect_stroke((x - 3) as f32, (y - 1) as f32, (ww + 6) as f32, (face.height + 4) as f32, 3.0, 1.5, BLUE, 1.0);
                            LINK
                        }
                        (Some(_), _) => LINK,
                        (None, Kind::Pre) => rgb(36, 41, 47),
                        _ => INK,
                    };
                    let _ = tw;
                    text(c, face, x, y, w.text, col);
                    if row.kind == Kind::Heading {
                        text(c, face, x + 1, y, w.text, col);
                    }
                    if let Some(l) = w.link {
                        c.fill_rect(Rect::new(x, y + face.height as i32, ww, 1), col);
                        hits.push((Rect::new(x - 2, y - 2, ww + 4 + space, row.h), l));
                    }
                }
                y += row.h;
            }
        });
        self.hits = hits;
        // Scroll bar.
        let sb = ui.scroll;
        c.fill_rect(sb, rgb(248, 248, 248));
        c.fill_rect(Rect::new(sb.x, sb.y, 1, sb.h), rgb(232, 232, 232));
        if self.rows_total > self.rows_shown {
            let th = (sb.h as usize * self.rows_shown / self.rows_total).max(30) as i32;
            let ty = sb.y + ((sb.h - th) as usize * self.top / (self.rows_total - self.rows_shown)) as i32;
            c.round_rect(sb.x as f32 + 3.0, ty as f32 + 2.0, 6.0, (th - 4) as f32, 3.0, rgb(193, 193, 193), 1.0);
        }
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
             <!-- <p>hidden</p> --><input type=hidden value=\"secret\"><img alt=\"a cat\"></body></html>",
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
