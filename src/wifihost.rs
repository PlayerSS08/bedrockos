//! Host-assisted Wi-Fi ("Wi-Fi through the host").
//!
//! When BedrockOS runs in a virtual machine it has no Wi-Fi radio of its own: like every VM it reaches the network
//! through the host. This module lets BedrockOS still choose *which* Wi-Fi network the host joins. A small helper on
//! the host (`tools/wifi-host.ps1`, Windows) listens on the VM's second serial port (COM2) and answers three
//! text commands, one per line, fields separated by a tab:
//!
//! | BedrockOS sends | Helper answers |
//! |---|---|
//! | `HELLO` | `BEDROCKWIFI 1` |
//! | `SCAN` | zero or more `NET <signal 0-100> <secure 0/1> <current 0/1> <ssid>` lines, then `END` (or `ERR <text>`) |
//! | `CONNECT <ssid> <password>` | `OK <text>` or `ERR <text>` |
//!
//! The radio work (scan, WPA2 handshake) is done by the host's driver, not by BedrockOS. BedrockOS's own traffic then goes
//! through the VM's virtual network card (`dhcp`). On a real PC there is no helper on COM2 and BedrockOS says so.
//! The parsing below is pure and host-tested; the serial transport exists only in the UEFI build.

use crate::spin::SpinLock;

pub const MAX_NETS: usize = 12;
pub const SSID_MAX: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Net {
    ssid: [u8; SSID_MAX],
    len: u8,
    pub signal: u8,
    pub secure: bool,
    pub current: bool,
}

impl Net {
    pub const EMPTY: Net = Net { ssid: [0; SSID_MAX], len: 0, signal: 0, secure: false, current: false };

    pub fn ssid(&self) -> &str {
        core::str::from_utf8(&self.ssid[..usize::from(self.len)]).unwrap_or("?")
    }
}

fn field_flag(f: &[u8]) -> Option<bool> {
    match f {
        b"0" => Some(false),
        b"1" => Some(true),
        _ => None,
    }
}

/// Parses one `NET\t<signal>\t<secure>\t<current>\t<ssid>` line (no line ending).
pub fn parse_net(line: &[u8]) -> Option<Net> {
    let mut it = line.splitn(5, |&b| b == b'\t');
    if it.next()? != b"NET" {
        return None;
    }
    let signal = core::str::from_utf8(it.next()?).ok()?.parse::<u8>().ok()?.min(100);
    let secure = field_flag(it.next()?)?;
    let current = field_flag(it.next()?)?;
    let ssid = it.next()?;
    if ssid.is_empty() || ssid.len() > SSID_MAX || core::str::from_utf8(ssid).is_err() || ssid.iter().any(|&b| b < 0x20) {
        return None;
    }
    let mut n = Net { signal, secure, current, ..Net::EMPTY };
    n.ssid[..ssid.len()].copy_from_slice(ssid);
    n.len = ssid.len() as u8;
    Some(n)
}

/// Splits `wifi connect` arguments into the network name and the password. A name with spaces goes in
/// double quotes (`"My Home WiFi" secret pass`); the password is the rest of the line, spaces included.
pub fn split_name_password(args: &str) -> Option<(&str, &str)> {
    let a = args.trim();
    let (name, rest) = if let Some(q) = a.strip_prefix('"') {
        let end = q.find('"')?;
        (&q[..end], &q[end + 1..])
    } else {
        match a.find(char::is_whitespace) {
            Some(i) => (&a[..i], &a[i..]),
            None => (a, ""),
        }
    };
    if name.is_empty() {
        return None;
    }
    Some((name, rest.trim()))
}

/// A short text without allocation (helper replies, error messages).
#[derive(Clone, Copy, Debug)]
pub struct Msg {
    buf: [u8; 96],
    len: usize,
}

impl Msg {
    pub fn new(s: &[u8]) -> Msg {
        let mut m = Msg { buf: [0; 96], len: 0 };
        let mut n = s.len().min(m.buf.len());
        while n > 0 && core::str::from_utf8(&s[..n]).is_err() {
            n -= 1; // never cut a character in half
        }
        m.buf[..n].copy_from_slice(&s[..n]);
        m.len = n;
        m
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

/// Splits a helper reply into `Ok(text)` for `OK ...` and `Err(text)` for anything else.
pub fn parse_reply(line: &[u8]) -> Result<Msg, Msg> {
    let (head, rest) = match line.iter().position(|&b| b == b'\t') {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, &b""[..]),
    };
    match head {
        b"OK" => Ok(Msg::new(rest)),
        b"ERR" => Err(Msg::new(if rest.is_empty() { b"the helper refused" } else { rest })),
        _ => Err(Msg::new(b"unexpected answer from the Wi-Fi helper")),
    }
}

/// The network chosen in the boot menu (or with `wifi connect`) that the host joined.
pub struct Chosen {
    ssid: [u8; SSID_MAX],
    len: u8,
}

static CHOSEN: SpinLock<Chosen> = SpinLock::new(Chosen { ssid: [0; SSID_MAX], len: 0 });

pub fn set_connected(ssid: &str) {
    let b = ssid.as_bytes();
    let n = b.len().min(SSID_MAX);
    let mut c = CHOSEN.lock();
    c.ssid[..n].copy_from_slice(&b[..n]);
    c.len = n as u8;
}

/// The connected network's name, if one was chosen this boot.
pub fn connected(out: &mut [u8; SSID_MAX]) -> Option<usize> {
    let c = CHOSEN.lock();
    let n = usize::from(c.len);
    out[..n].copy_from_slice(&c.ssid[..n]);
    (n > 0).then_some(n)
}

/// Serial transport to the host helper on COM2 (UEFI build only; works before and after ExitBootServices).
#[cfg(target_os = "uefi")]
pub mod link {
    use super::{parse_net, parse_reply, Msg, Net, MAX_NETS};
    use crate::arch::{inb, irq_restore, irq_save, outb, pause, rdtsc};

    /// Runs one exchange with interrupts off. Before ExitBootServices the firmware has its own serial driver on
    /// COM2 whose timer callback would otherwise read the helper's reply first (and turn it into "key presses").
    fn exclusive<T>(f: impl FnOnce() -> T) -> T {
        let was = irq_save();
        let r = f();
        irq_restore(was);
        r
    }

    const COM2: u16 = 0x2F8;

    fn deadline(ms: u64) -> u64 {
        rdtsc().wrapping_add(crate::log::tsc_hz().max(1_000_000) / 1000 * ms)
    }

    fn passed(t: u64) -> bool {
        (rdtsc().wrapping_sub(t) as i64) >= 0
    }

    /// Sets COM2 to 115200 8N1 and checks a UART answers (an absent port reads back 0xFF).
    pub fn present() -> bool {
        // SAFETY: standard 16550 register accesses at COM2; on machines without one they are ignored.
        unsafe {
            outb(COM2 + 1, 0x00);
            outb(COM2 + 3, 0x80);
            outb(COM2, 0x01);
            outb(COM2 + 1, 0x00);
            outb(COM2 + 3, 0x03);
            outb(COM2 + 2, 0xC7);
            outb(COM2 + 4, 0x03);
            for p in [0xA5u8, 0x5A] {
                outb(COM2 + 7, p);
                if inb(COM2 + 7) != p {
                    return false;
                }
            }
        }
        true
    }

    fn put(b: u8) -> bool {
        let t = deadline(200);
        // SAFETY: COM2 was validated by `present`.
        unsafe {
            while inb(COM2 + 5) & 0x20 == 0 {
                if passed(t) {
                    return false;
                }
                pause();
            }
            outb(COM2, b);
        }
        true
    }

    fn get(t: u64) -> Option<u8> {
        // SAFETY: COM2 was validated by `present`.
        unsafe {
            loop {
                if inb(COM2 + 5) & 1 != 0 {
                    return Some(inb(COM2));
                }
                if passed(t) {
                    return None;
                }
                pause();
            }
        }
    }

    /// Drops anything left over from an earlier, timed-out exchange.
    fn drain() {
        // SAFETY: COM2 was validated by `present`.
        unsafe {
            for _ in 0..4096 {
                if inb(COM2 + 5) & 1 == 0 {
                    break;
                }
                let _ = inb(COM2);
            }
        }
    }

    fn send(fields: &[&[u8]]) -> bool {
        for (i, f) in fields.iter().enumerate() {
            if i > 0 && !put(b'\t') {
                return false;
            }
            for &b in *f {
                if !put(b) {
                    return false;
                }
            }
        }
        put(b'\n')
    }

    /// One line (without `\r\n`) into `buf`, or `None` on timeout. Over-long lines are cut.
    fn read_line(buf: &mut [u8], t: u64) -> Option<usize> {
        let mut n = 0;
        loop {
            match get(t)? {
                b'\n' => return Some(n),
                b'\r' => {}
                b if n < buf.len() => {
                    buf[n] = b;
                    n += 1;
                }
                _ => {}
            }
        }
    }

    /// Whether the host helper is there and answering.
    pub fn hello() -> bool {
        exclusive(hello_inner)
    }

    fn hello_inner() -> bool {
        if !present() {
            return false;
        }
        drain();
        let mut buf = [0u8; 32];
        send(&[b"HELLO"]) && matches!(read_line(&mut buf, deadline(1500)), Some(n) if buf[..n].starts_with(b"BEDROCKWIFI"))
    }

    /// Asks the host to scan. Fills `out` with up to `MAX_NETS` networks, strongest first as the host sends them.
    pub fn scan(out: &mut [Net; MAX_NETS]) -> Result<usize, Msg> {
        exclusive(|| scan_inner(out))
    }

    fn scan_inner(out: &mut [Net; MAX_NETS]) -> Result<usize, Msg> {
        if !hello_inner() {
            return Err(Msg::new(b"no helper: run `python tools/bedrock-host.py` on the laptop, then try again"));
        }
        if !send(&[b"SCAN"]) {
            return Err(Msg::new(b"could not talk to the Wi-Fi helper"));
        }
        let t = deadline(15_000);
        let mut n = 0;
        let mut buf = [0u8; 128];
        loop {
            let Some(len) = read_line(&mut buf, t) else { return Err(Msg::new(b"the Wi-Fi helper did not answer in time")) };
            let line = &buf[..len];
            if line == b"END" {
                return Ok(n);
            }
            if line.starts_with(b"ERR") {
                return Err(parse_reply(line).err().unwrap_or(Msg::new(b"scan failed")));
            }
            if let Some(net) = parse_net(line) {
                if n < MAX_NETS {
                    out[n] = net;
                    n += 1;
                }
            }
        }
    }

    /// Asks the host to join `ssid` (empty `password` for open networks or ones the host already knows).
    pub fn connect(ssid: &str, password: &[u8]) -> Result<Msg, Msg> {
        exclusive(|| connect_inner(ssid, password))
    }

    fn connect_inner(ssid: &str, password: &[u8]) -> Result<Msg, Msg> {
        if !hello_inner() {
            return Err(Msg::new(b"no helper: run `python tools/bedrock-host.py` on the laptop, then try again"));
        }
        if !send(&[b"CONNECT", ssid.as_bytes(), password]) {
            return Err(Msg::new(b"could not talk to the Wi-Fi helper"));
        }
        let mut buf = [0u8; 128];
        match read_line(&mut buf, deadline(60_000)) {
            Some(n) => parse_reply(&buf[..n]),
            None => Err(Msg::new(b"no answer: the connection took too long")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_name_and_password() {
        assert_eq!(split_name_password("MOJO-JAN-B-F7 hunter22"), Some(("MOJO-JAN-B-F7", "hunter22")));
        assert_eq!(split_name_password("\"My Home WiFi\" pass with space "), Some(("My Home WiFi", "pass with space")));
        assert_eq!(split_name_password("Cafe"), Some(("Cafe", "")));
        assert_eq!(split_name_password("\"Open Net\""), Some(("Open Net", "")));
        assert_eq!(split_name_password("  "), None);
        assert_eq!(split_name_password("\"unclosed"), None);
    }

    #[test]
    fn parses_network_lines() {
        let n = parse_net(b"NET\t87\t1\t0\tMOJO-JAN-B-F6").unwrap();
        assert_eq!(n.ssid(), "MOJO-JAN-B-F6");
        assert_eq!((n.signal, n.secure, n.current), (87, true, false));
        let n = parse_net(b"NET\t100\t0\t1\tFree WiFi Guest").unwrap();
        assert_eq!(n.ssid(), "Free WiFi Guest");
        assert!(n.current && !n.secure);
        assert!(parse_net(b"NET\t100\t0\t1\tname\twith tab").is_none(), "control characters are refused");
        assert_eq!(parse_net(b"NET\t250\t1\t0\tx").unwrap().signal, 100, "signal is capped");
    }

    #[test]
    fn rejects_bad_lines() {
        for bad in [
            &b""[..],
            b"END",
            b"NET",
            b"NET\t50\t1\t0\t",
            b"NET\tx\t1\t0\tname",
            b"NET\t50\t2\t0\tname",
            b"NET\t50\t1\t0\t0123456789012345678901234567890123",
            b"NET\t50\t1\t0\tbad\x01ctl",
            b"NET\t50\t1\t0\t\xff\xfe",
            b"OK\t50\t1\t0\tname",
        ] {
            assert!(parse_net(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn replies() {
        assert_eq!(parse_reply(b"OK\tconnected to X").unwrap().as_str(), "connected to X");
        assert_eq!(parse_reply(b"OK").unwrap().as_str(), "");
        assert_eq!(parse_reply(b"ERR\twrong password").err().unwrap().as_str(), "wrong password");
        assert_eq!(parse_reply(b"ERR").err().unwrap().as_str(), "the helper refused");
        assert!(parse_reply(b"garbage").is_err());
    }

    #[test]
    fn msg_cuts_long_text_on_a_character_boundary() {
        let long = "é".repeat(80); // 160 bytes
        let m = Msg::new(long.as_bytes());
        assert!(m.as_str().len() <= 96 && !m.as_str().is_empty());
        assert!(m.as_str().chars().all(|c| c == 'é'));
    }

    #[test]
    fn chosen_network_round_trips() {
        set_connected("Hostel");
        let mut b = [0u8; SSID_MAX];
        let n = connected(&mut b).unwrap();
        assert_eq!(&b[..n], b"Hostel");
    }

    #[test]
    fn random_lines_never_panic() {
        let mut x: u64 = 0x1234_5678_9abc_def0;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let len = (x % 60) as usize;
            let mut line = b"NET\t".to_vec();
            for i in 0..len {
                line.push(match (x >> (i % 56)) % 6 {
                    0 => b'\t',
                    1 => b'1',
                    2 => b'0',
                    3 => (x >> 3) as u8,
                    _ => b'a' + (i as u8 % 26),
                });
            }
            if let Some(n) = parse_net(&line) {
                assert!(!n.ssid().is_empty() && n.ssid().len() <= SSID_MAX && n.signal <= 100);
            }
            let _ = parse_reply(&line);
        }
    }
}
