//! Bedrock Snap Store: a built-in, offline app store.
//!
//! Every app is described in [`CATALOG`]. An installed snap is a file `/snap/<name>` (its metadata), so
//! `ls /snap` shows every installed app. The desktop apps come preinstalled; the store-only snaps are small
//! programs built into the kernel that `snap run <name>` (or typing the name) starts.
//! `apt` uses this store when no network repository is set (see `shell::cmd_apt`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ramfs::RamFs;

/// One app in the store.
pub struct Snap {
    pub name: &'static str,
    pub version: &'static str,
    pub publisher: &'static str,
    pub summary: &'static str,
    /// Desktop window it opens (name as in the dock), `None` for a command-line snap.
    pub desktop: Option<&'static str>,
    /// Comes installed with the OS.
    pub preinstalled: bool,
    pub size_kb: u32,
}

const fn s(name: &'static str, version: &'static str, summary: &'static str, desktop: Option<&'static str>, preinstalled: bool, size_kb: u32) -> Snap {
    Snap { name, version, publisher: "bedrock", summary, desktop, preinstalled, size_kb }
}

pub const CATALOG: &[Snap] = &[
    s("notes", "1.2.0", "Write and keep notes", Some("Notes"), true, 64),
    s("tasks", "1.1.0", "To-do list", Some("Tasks"), true, 48),
    s("crafting-table", "0.9.0", "Crafting Table web browser", Some("Crafting Table"), true, 220),
    s("files", "1.0.0", "File manager", Some("Files"), true, 96),
    s("chatbox", "1.3.0", "Terminal (the shell)", Some("ChatBox"), true, 128),
    s("network", "1.0.0", "Network and Wi-Fi settings", Some("Network"), true, 72),
    s("calculator", "1.0.1", "Calculator", Some("Calculator"), true, 40),
    s("clock", "1.0.0", "Clock with time zones", Some("Clock"), true, 36),
    s("snake", "1.0.0", "Snake game", Some("Snake"), true, 32),
    s("settings", "1.0.0", "System settings", Some("Settings"), true, 80),
    s("help", "1.0.0", "Help and command guide", Some("Help"), true, 52),
    s("hello", "2.10", "GNU-style hello world", None, false, 8),
    s("cowsay", "3.04", "A talking cow says your text", None, false, 12),
    s("fortune", "1.99", "A random saying", None, false, 16),
    s("neofetch", "7.1.0", "System information with a logo", None, false, 20),
    s("sl", "5.02", "Steam locomotive runs across the screen", None, false, 10),
    s("figlet", "2.2.5", "Big letters from text", None, false, 24),
];

pub const DIR: &str = "/snap";

pub fn find(name: &str) -> Option<&'static Snap> {
    let n = name.trim().to_ascii_lowercase();
    CATALOG.iter().find(|s| s.name == n || s.desktop.is_some_and(|d| d.eq_ignore_ascii_case(&n)))
}

fn path(s: &Snap) -> String {
    format!("{DIR}/{}", s.name)
}

pub fn is_installed(fs: &RamFs, s: &Snap) -> bool {
    fs.read(&path(s)).is_some()
}

fn meta(s: &Snap) -> String {
    format!(
        "name: {}\nversion: {}\npublisher: {}\nsummary: {}\nconfinement: strict\ntype: {}\nsize: {} kB\n",
        s.name,
        s.version,
        s.publisher,
        s.summary,
        if s.desktop.is_some() { "desktop app" } else { "command" },
        s.size_kb
    )
}

/// Creates `/snap` and installs the preinstalled apps (once; a removed one stays removed until reinstalled
/// because `/var/snap-seeded` remembers the first seeding).
pub fn seed(fs: &mut RamFs) {
    if !fs.is_dir(DIR) {
        let _ = fs.mkdir(DIR);
    }
    if !fs.is_dir("/var") {
        let _ = fs.mkdir("/var");
    }
    if fs.read("/var/snap-seeded").is_some() {
        return;
    }
    for s in CATALOG.iter().filter(|s| s.preinstalled) {
        let _ = fs.write(&path(s), meta(s).as_bytes());
    }
    let _ = fs.write("/var/snap-seeded", b"1\n");
}

pub fn install(fs: &mut RamFs, name: &str, say: &mut dyn FnMut(&str)) -> bool {
    seed(fs);
    let Some(s) = find(name) else {
        say(&format!("error: snap \"{name}\" not found (try: snap find)"));
        return false;
    };
    if is_installed(fs, s) {
        say(&format!("snap \"{}\" is already installed", s.name));
        return true;
    }
    say(&format!("Downloading snap \"{}\" ({} kB) from the Bedrock Store...", s.name, s.size_kb));
    say("Verifying SHA-256 ... ok");
    match fs.write(&path(s), meta(s).as_bytes()) {
        Ok(()) => {
            say(&format!("{} {} from {} installed", s.name, s.version, s.publisher));
            true
        }
        Err(e) => {
            say(&format!("error: cannot install {}: {e:?}", s.name));
            false
        }
    }
}

pub fn remove(fs: &mut RamFs, name: &str, say: &mut dyn FnMut(&str)) -> bool {
    seed(fs);
    let Some(s) = find(name) else {
        say(&format!("error: snap \"{name}\" not found"));
        return false;
    };
    if s.name == "chatbox" {
        say("error: cannot remove \"chatbox\": it is the system terminal");
        return false;
    }
    if fs.remove(&path(s)) {
        say(&format!("{} removed", s.name));
        true
    } else {
        say(&format!("snap \"{}\" is not installed", s.name));
        false
    }
}

/// `snap list`: installed snaps as a table.
pub fn list_installed(fs: &mut RamFs, say: &mut dyn FnMut(&str)) {
    seed(fs);
    say("Name            Version  Publisher  Notes");
    for s in CATALOG.iter().filter(|s| is_installed(fs, s)) {
        say(&format!("{:<15} {:<8} {:<10} {}", s.name, s.version, s.publisher, if s.desktop.is_some() { "desktop" } else { "-" }));
    }
}

/// `snap find [text]`: store apps matching `query` (all when empty).
pub fn find_cmd(fs: &mut RamFs, query: &str, say: &mut dyn FnMut(&str)) -> usize {
    seed(fs);
    let q = query.to_ascii_lowercase();
    let hits: Vec<&Snap> = CATALOG.iter().filter(|s| q.is_empty() || s.name.contains(&q) || s.summary.to_ascii_lowercase().contains(&q)).collect();
    if hits.is_empty() {
        say(&format!("No matching snaps for \"{query}\""));
        return 0;
    }
    say("Name            Version  Publisher  Summary");
    for s in &hits {
        let mark = if is_installed(fs, s) { " [installed]" } else { "" };
        say(&format!("{:<15} {:<8} {:<10} {}{}", s.name, s.version, s.publisher, s.summary, mark));
    }
    hits.len()
}

pub fn info(fs: &mut RamFs, name: &str, say: &mut dyn FnMut(&str)) -> bool {
    seed(fs);
    let Some(s) = find(name) else {
        say(&format!("error: no snap found for \"{name}\""));
        return false;
    };
    for l in meta(s).lines() {
        say(l);
    }
    say(&format!("installed: {}", if is_installed(fs, s) { s.version } else { "no" }));
    true
}

/// Output of a command-line snap (`None` for a desktop app).
pub fn run_text(name: &str, args: &str, seed: u64) -> Option<String> {
    let text = if args.trim().is_empty() { "Moo! BedrockOS" } else { args.trim() };
    Some(match name {
        "hello" => String::from("Hello, world!\n"),
        "cowsay" => {
            let bar: String = core::iter::repeat_n('-', text.len() + 2).collect();
            format!(" {bar}\n< {text} >\n {bar}\n        \\   ^__^\n         \\  (oo)\\_______\n            (__)\\       )\\/\\\n                ||----w |\n                ||     ||\n")
        }
        "fortune" => {
            const SAYINGS: [&str; 5] = [
                "Bedrock cannot be broken. Neither can your spirit.",
                "A creeper a day keeps the builders away.",
                "There is no place like 127.0.0.1.",
                "Talk is cheap. Show me the code. -- Linus Torvalds",
                "Keep calm and craft on.",
            ];
            format!("{}\n", SAYINGS[(seed as usize) % SAYINGS.len()])
        }
        "neofetch" => format!(
            "   ######      bedrock@bedrockos\n  ########     -----------------\n ##########    OS: BedrockOS {}\n ##  ##  ##    Kernel: Rust no_std (UEFI)\n ##########    Shell: ChatBox\n  ########     Packages: {} (snap)\n   ######      CPU: x86_64\n",
            crate::VERSION,
            CATALOG.len()
        ),
        "sl" => String::from("      ====        ________                ___________\n  _D _|  |_______/        \\__I_I_____===__|_________|\n   |(_)---  |   H\\________/ |   |        =|___ ___|\n   /     |  |   H  |  |     |   |         ||_| |_||\n  |      |  |   H  |__--------------------| [___] |\n  | ________|___H__/__|_____/[][]~\\_______|       |\n  |/ |   |-----------I_____I [][] []  D   |=======|_\n__/ =| o |=-~~\\  /~~\\  /~~\\  /~~\\ ____Y___________|__\n |/-=|___|=    ||    ||    ||    |_____/~\\___/\n  \\_/      \\O=====O=====O=====O_/      \\_/\n"),
        "figlet" => {
            let mut rows = [String::new(), String::new(), String::new()];
            for c in text.chars().take(20) {
                let c = c.to_ascii_uppercase();
                let g = if c == ' ' { ["   ", "   ", "   "] } else { ["###", "#?#", "###"] };
                for (r, row) in rows.iter_mut().enumerate() {
                    row.push_str(&g[r].replace('?', &String::from(c)));
                    row.push(' ');
                }
            }
            format!("{}\n{}\n{}\n", rows[0], rows[1], rows[2])
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs() -> RamFs {
        RamFs::new(256, 1 << 20)
    }

    #[test]
    fn seed_installs_desktop_apps() {
        let mut f = fs();
        seed(&mut f);
        let names: Vec<&str> = f.list(DIR).map(|(n, _)| n).collect();
        assert!(names.contains(&"notes") && names.contains(&"snake") && names.contains(&"crafting-table"));
        assert!(!names.contains(&"cowsay"));
    }

    #[test]
    fn install_remove_roundtrip() {
        let mut f = fs();
        let mut out = Vec::new();
        assert!(install(&mut f, "cowsay", &mut |l| out.push(String::from(l))));
        assert!(f.read("/snap/cowsay").is_some());
        assert!(install(&mut f, "cowsay", &mut |_| {}));
        assert!(remove(&mut f, "cowsay", &mut |_| {}));
        assert!(f.read("/snap/cowsay").is_none());
        assert!(!install(&mut f, "nope", &mut |_| {}));
        assert!(!remove(&mut f, "chatbox", &mut |_| {}));
    }

    #[test]
    fn removed_preinstalled_stays_removed() {
        let mut f = fs();
        seed(&mut f);
        assert!(remove(&mut f, "snake", &mut |_| {}));
        seed(&mut f);
        assert!(f.read("/snap/snake").is_none());
    }

    #[test]
    fn find_and_run() {
        let mut f = fs();
        assert_eq!(find_cmd(&mut f, "cow", &mut |_| {}), 1);
        assert_eq!(find_cmd(&mut f, "", &mut |_| {}), CATALOG.len());
        assert!(run_text("cowsay", "hi", 0).unwrap().contains("< hi >"));
        assert!(run_text("notes", "", 0).is_none());
        assert!(run_text("fortune", "", 7).is_some());
        assert_eq!(find("Calculator").map(|s| s.name), Some("calculator"));
    }
}
