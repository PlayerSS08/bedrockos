//! The shell's command table: one place that says what each command is called, how it is used and
//! what it does. `help`, `help <command>` and `help all` all read from it. Pure, host-tested.

pub struct Cmd {
    pub group: &'static str,
    pub name: &'static str,
    pub usage: &'static str,
    pub about: &'static str,
}

const fn c(group: &'static str, name: &'static str, usage: &'static str, about: &'static str) -> Cmd {
    Cmd { group, name, usage, about }
}

/// Groups in the order `help` shows them.
// Aliases people bring from other systems: Windows (cls, dir, ver), Linux (man, free, uname, history).
pub const GROUPS: &[&str] = &[
    "system", "status", "hardware", "memory", "tasks", "network", "packaging", "files", "manage", "text", "disk", "programs", "tools", "power",
];

pub const COMMANDS: &[Cmd] = &[
    c("system", "help", "help [command|all]", "this list; `help <command>` explains one command"),
    c("system", "info", "info", "firmware, CPU clock, Secure Boot"),
    c("system", "version", "version", "BedrockOS version and revision"),
    c("system", "cpu", "cpu", "CPUs, interrupt controllers, address sizes"),
    c("status", "security", "security", "which protections are active right now"),
    c("status", "tpm", "tpm", "measured-boot PCR values"),
    c("status", "lastpanic", "lastpanic", "how the previous boot ended"),
    c("system", "uptime", "uptime", "time since boot"),
    c("system", "uname", "uname", "system name, version and architecture"),
    c("system", "ver", "ver", "same as `version`"),
    c("system", "man", "man <command>", "same as `help <command>`"),
    c("system", "history", "history", "your latest commands (Ctrl+Up/Down recall them)"),
    c("status", "log", "log", "replay the boot log"),
    c("hardware", "pci", "pci", "devices on the PCI bus"),
    c("hardware", "usb", "usb", "USB controller and its ports"),
    c("hardware", "mouse", "mouse", "mouse pointer position and buttons (USB mouse)"),
    c("hardware", "touchpad", "touchpad [scan|dump]", "laptop touchpad (I2C): status, debug"),
    c("hardware", "disks", "disks", "the disk, if there is one"),
    c("hardware", "dread", "dread <n> [off]", "32 bytes of sector n as hex"),
    c("network", "net", "net", "network overview: host connection and OS Wi-Fi"),
    c("network", "nettest", "nettest", "check link, address, gateway, DNS, HTTP"),
    c("system", "about", "about", "what BedrockOS is"),
    c("system", "tz", "tz [zone|+HH:MM]", "show or set the time zone"),
    c("system", "battery", "battery", "the laptop's battery level"),
    c("system", "open", "open <app>", "open an app: notes, files, clock, snake..."),
    c("system", "neofetch", "neofetch", "system summary with a logo"),
    c("system", "achievements", "achievements", "Minecraft-style achievements you have unlocked"),
    c("system", "cmds", "cmds", "every command, by group"),
    c("system", "keys", "keys", "all keyboard shortcuts"),
    c("tools", "calc", "calc <expr>", "calculator: + - * / % ^ and ( )"),
    c("tools", "uuid", "uuid", "a random UUID"),
    c("tools", "rand", "rand [max]", "a random number from 1 to max (100)"),
    c("tools", "fortune", "fortune", "a saying"),
    c("tools", "base64", "base64 [-d] <text>", "encode or decode base64"),
    c("tools", "bench", "bench", "quick CPU speed test"),
    c("tasks", "note", "note <list|export..>", "notes to and from .txt files"),
    c("tasks", "todo-today", "todo-today", "tasks not done yet"),
    c("tasks", "task", "task [add|done|rm]", "your to-do list (the same as the Tasks app)"),
    c("network", "ip", "ip [a|r|l|n]", "configure and view IP, links, routing"),
    c("network", "ifconfig", "ifconfig [if] [ip]", "display or configure network interfaces"),
    c("network", "ping", "ping <ip> [-c N]", "send ICMP Echo Requests and measure latency"),
    c("network", "dhcp", "dhcp [renew|status]", "auto-configure IP, gateway, and DNS"),
    c("network", "connect", "connect [eth|wifi]", "connect to Ethernet or Wi-Fi"),
    c("network", "wifi", "wifi [scan|status]", "choose and join a Wi-Fi network (via host)"),
    c("network", "curl", "curl <url> [-o file]", "download over http (names work; no https)"),
    c("status", "speedtest", "speedtest [url]", "measured download speed"),
    c("hardware", "report", "report", "save a diagnostic report to the USB stick"),
    c("disk", "save", "save", "save your files to the USB stick"),
    c("network", "http", "http <url>", "same as `curl`"),
    c("network", "netstat", "netstat", "interfaces, routes, ARP cache, sockets"),
    c("packaging", "apt", "apt <cmd> [pkg]", "signed package manager (apt help)"),
    c("packaging", "snap", "snap <cmd> [app]", "app store: list, find, install, remove, run"),
    c("packaging", "sudo", "sudo <command>", "run a command as root"),
    c("memory", "mem", "mem", "frames, heap and RAM totals"),
    c("memory", "free", "free", "same as `mem`"),
    c("memory", "alloc", "alloc <KiB>", "allocate and free memory; shows a refusal too"),
    c("memory", "memmap", "memmap", "the firmware memory map"),
    c("memory", "page", "page [fifo|lru|opt]", "page replacement simulation (fifo, lru)"),
    c("tasks", "ps", "ps", "list tasks"),
    c("tasks", "spawn", "spawn [coop]", "start 2 workers (coop: they yield themselves)"),
    c("tasks", "queue", "queue", "the ready queue: who runs next, in order"),
    c("tasks", "trace", "trace [n|all|reset]", "context switches and why (timer, yield, ...)"),
    c("tasks", "job", "job <units> [more]", "tasks that end after N units of work; timed"),
    c("tasks", "race", "race [lock] [n]", "two tasks share a counter; lock fixes lost adds"),
    c("tasks", "counters", "counters", "how far the busy workers have counted"),
    c("tasks", "block", "block", "start a task that waits on a queue"),
    c("tasks", "wake", "wake", "wake the tasks waiting on that queue"),
    c("tasks", "kill", "kill <pid|id>", "stop a program (by pid) or a task"),
    c("tasks", "sched", "sched [policy]", "CPU scheduling policies (fcfs, sjf, rr)"),
    c("files", "ls", "ls [folder]", "list a folder in RAM (default: the current one)"),
    c("files", "dir", "dir [folder]", "same as `ls`"),
    c("files", "tree", "tree [folder]", "show a folder and everything inside it"),
    c("files", "cd", "cd [folder]", "go into a folder (`cd ..` up, `cd` back to /)"),
    c("files", "pwd", "pwd", "show the current folder"),
    c("files", "mkdir", "mkdir <folder>", "make a folder"),
    c("files", "rmdir", "rmdir <folder>", "delete an empty folder"),
    c("files", "df", "df", "space used by the RAM files"),
    c("files", "cat", "cat <file>", "print a file"),
    c("packaging", "sha256sum", "sha256sum <file>...", "SHA-256 of files, to check downloads"),
    c("files", "write", "write [-f] <f> <t>", "create a file (-f replaces; \\n = new line)"),
    c("files", "append", "append <file> <text>", "add a line to the end of a file"),
    c("files", "edit", "edit <file>", "type many lines, end with a line holding only ."),
    c("files", "rm", "rm <file>", "delete a file"),
    c("manage", "cp", "cp [-f] <file> <to>", "copy a file (-f replaces an existing one)"),
    c("manage", "mv", "mv [-f] <file> <to>", "move or rename a file (-f replaces)"),
    c("manage", "touch", "touch <file>", "create an empty file (there is no clock)"),
    c("text", "head", "head [-n N] <file>", "the first N lines (default 10)"),
    c("text", "tail", "tail [-n N] <file>", "the last N lines (default 10)"),
    c("text", "wc", "wc <file> ...", "count lines, words and bytes"),
    c("text", "grep", "grep [-in] <t> <f>", "lines with the text (-i any case, -n numbers)"),
    c("manage", "find", "find [folder] [text]", "names below a folder that contain the text"),
    c("text", "hexdump", "hexdump <file> [n]", "a file as hex and text (first 256 bytes)"),
    c("disk", "dls", "dls [path]", "list a directory on the disk (FAT16/FAT32)"),
    c("disk", "dcat", "dcat <path>", "print a file from the disk (first 64 KiB)"),
    c("disk", "sync", "sync", "flush USB cached writes to disk"),
    c("disk", "disksched", "disksched [algo]", "disk arm scheduling (fcfs, sstf, scan)"),
    c("programs", "run", "run <program>", "ring 3 demo (hello linux ...) or a file"),
    c("tools", "echo", "echo <text>", "print text"),
    c("tools", "sort", "sort <n> <n> ...", "sort numbers (uses the heap)"),
    c("tools", "seq", "seq [first] <last>", "print numbers from first (default 1) to last"),
    c("tools", "sleep", "sleep <seconds>", "wait 1 to 60 seconds"),
    c("status", "whoami", "whoami", "who you are (Notch)"),
    c("status", "hostname", "hostname", "the machine's name"),
    c("status", "date", "date", "date and time from the clock chip"),
    c("tools", "clear", "clear", "clear the screen (or press Ctrl+L)"),
    c("tools", "cls", "cls", "same as `clear`"),
    c("tools", "banker", "banker [run|detect]", "Banker's deadlock avoidance & detection"),
    c("tools", "diners", "diners [steps]", "dining philosophers concurrency simulation"),
    c("tools", "collatz", "collatz <n>", "compute Collatz 3n+1 sequence"),
    c("tools", "pi", "pi [samples]", "Monte Carlo statistical Pi estimation"),
    c("power", "panic", "panic", "trigger a kernel panic, to see the stop screen"),
    c("power", "reboot", "reboot", "restart the machine"),
    c("power", "shutdown", "shutdown", "power off"),
];

/// The commands whose names start with `prefix`.
pub fn matching(prefix: &str) -> impl Iterator<Item = &'static Cmd> + '_ {
    COMMANDS.iter().filter(move |c| c.name.starts_with(prefix))
}

/// The longest text every command starting with `prefix` begins with (`prefix` itself when nothing
/// matches or the matches differ right after it).
pub fn common_prefix(prefix: &str) -> &'static str {
    let mut it = matching(prefix);
    let Some(first) = it.next() else { return "" };
    let mut n = first.name.len();
    for c in it {
        n = n.min(first.name.bytes().zip(c.name.bytes()).take_while(|(a, b)| a == b).count());
    }
    &first.name[..n]
}

/// The closest command name to a mistyped `word` (at most 2 edits away), for "did you mean".
pub fn suggest(word: &str) -> Option<&'static str> {
    const MAX: usize = 16;
    let w = word.as_bytes();
    if w.is_empty() || w.len() > MAX {
        return None;
    }
    let mut best: Option<(usize, &'static str)> = None;
    for c in COMMANDS {
        let d = edit_distance(w, c.name.as_bytes());
        if d <= 2 && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, c.name));
        }
    }
    best.map(|(_, n)| n)
}

/// Levenshtein distance for short ASCII words (both at most 16 bytes; longer names give a large value).
fn edit_distance(a: &[u8], b: &[u8]) -> usize {
    const MAX: usize = 16;
    if a.len() > MAX || b.len() > MAX {
        return usize::MAX / 2;
    }
    let mut prev = [0usize; MAX + 1];
    let mut cur = [0usize; MAX + 1];
    for (j, p) in prev.iter_mut().enumerate().take(b.len() + 1) {
        *p = j;
    }
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The command called `name`, if any.
pub fn find(name: &str) -> Option<&'static Cmd> {
    COMMANDS.iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_usage_starts_with_the_name() {
        for (i, a) in COMMANDS.iter().enumerate() {
            assert!(!a.about.is_empty());
            assert!(a.usage == a.name || a.usage.starts_with(&format!("{} ", a.name)), "{}", a.usage);
            assert!(GROUPS.contains(&a.group), "unknown group {}", a.group);
            for b in &COMMANDS[i + 1..] {
                assert_ne!(a.name, b.name);
            }
        }
    }

    #[test]
    fn every_group_has_commands() {
        for g in GROUPS {
            assert!(COMMANDS.iter().any(|c| c.group == *g), "empty group {}", g);
        }
    }

    #[test]
    fn lines_fit_a_71_column_screen() {
        for c in COMMANDS {
            // `help all` prints "  {usage:<20} {about}". The smallest screen (640 px, 9 px cells) has 71 columns.
            assert!(c.usage.len() <= 20, "{}", c.usage);
            assert!(2 + 20 + 1 + c.about.len() <= 71, "{} is too long for one line ({} columns)", c.name, 23 + c.about.len());
        }
        for g in GROUPS {
            let names: usize = COMMANDS.iter().filter(|c| c.group == *g).map(|c| c.name.len() + 1).sum();
            // A group line may wrap onto a second row on the smallest screen (the console wraps long lines).
            assert!(10 + names <= 142, "group line {} too long", g);
        }
    }

    #[test]
    fn prefix_matching_and_completion() {
        assert_eq!(matching("d").count(), 10); // dir dread dls dcat df disks date dhcp disksched diners
        assert_eq!(common_prefix("d"), "d");
        assert_eq!(common_prefix("dc"), "dcat");
        assert_eq!(common_prefix("hel"), "help");
        assert_eq!(common_prefix("he"), "he"); // help head hexdump
        assert_eq!(common_prefix("sh"), "sh"); // sha256sum shutdown
        assert_eq!(common_prefix("shu"), "shutdown");
        assert_eq!(common_prefix("mem"), "mem"); // mem and memmap
        assert_eq!(common_prefix("zz"), "");
        assert_eq!(common_prefix("").len(), 0, "everything matches, nothing in common");
    }

    #[test]
    fn did_you_mean() {
        assert_eq!(suggest("hlep"), Some("help"));
        assert_eq!(suggest("shutdwon"), Some("shutdown"));
        assert_eq!(suggest("lss"), Some("ls"));
        assert_eq!(suggest("xyzzyplugh"), None);
        assert_eq!(suggest(""), None);
        assert_eq!(suggest("a-very-long-mistyped-command"), None);
        assert_eq!(suggest("ls"), Some("ls"), "an exact name is its own best match");
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance(b"", b""), 0);
        assert_eq!(edit_distance(b"abc", b""), 3);
        assert_eq!(edit_distance(b"kitten", b"sitting"), 3);
        assert_eq!(edit_distance(b"ps", b"pc"), 1);
    }

    #[test]
    fn find_works() {
        assert_eq!(find("ls").map(|c| c.usage), Some("ls [folder]"));
        assert!(find("nosuch").is_none());
        assert!(find("").is_none());
    }

    /// Every command the shell dispatches is in the table, and every table entry is dispatched.
    #[test]
    fn table_matches_what_the_shell_runs() {
        let src = include_str!("shell.rs");
        let start = src.find("fn run(line: &str, ").expect("run() in shell.rs");
        let end = src[start..].find("/// The interactive shell task.").expect("end of run()") + start;
        let mut dispatched = Vec::new();
        for line in src[start..end].lines() {
            let t = line.trim_start();
            if !t.starts_with('"') {
                continue;
            }
            let Some((pat, _)) = t.split_once("=>") else { continue };
            for alt in pat.split('|') {
                let name = alt.trim().trim_matches('"');
                if !name.is_empty() && name != "?" && !name.contains(' ') {
                    dispatched.push(name.to_string());
                }
            }
        }
        assert!(dispatched.len() > 25, "scanner found only {}", dispatched.len());
        for name in &dispatched {
            assert!(find(name).is_some(), "`{}` is run by the shell but missing from the help table", name);
        }
        for c in COMMANDS {
            assert!(dispatched.iter().any(|d| d == c.name), "`{}` is in the help table but the shell does not run it", c.name);
        }
    }
}
