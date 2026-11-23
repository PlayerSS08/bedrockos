//! Saving the RAM filesystem to the USB stick and bringing it back at the next boot. Pure and host-tested.
//!
//! BedrockOS keeps files in memory, so without this everything typed into a file is gone at power-off. The whole
//! tree is written as one file (`AEONFS.BIN`) on the AEON volume: a header, one record per folder or file, and
//! a SHA-256 of everything before it. Reading it back treats the file as hostile (it came from a removable
//! stick): sizes are capped, paths are checked, and a wrong checksum means nothing is restored at all.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sha256;

pub const MAGIC: &[u8; 8] = b"AEONFS1\0";
/// A tree bigger than this is not saved (the RAM filesystem itself is smaller than this).
pub const MAX_TOTAL: usize = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_PATH: usize = 255;

/// One folder (`data` is `None`) or file of the tree. Paths are absolute, like `/home/note.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub data: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistError {
    TooBig,
    BadHeader,
    BadChecksum,
    Corrupt,
}

impl core::fmt::Display for PersistError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            PersistError::TooBig => "the files are too big to save",
            PersistError::BadHeader => "not an BedrockOS file snapshot",
            PersistError::BadChecksum => "the snapshot is damaged (checksum mismatch)",
            PersistError::Corrupt => "the snapshot is malformed",
        })
    }
}

/// A path that is safe to recreate: absolute, no empty or `.`/`..` parts, printable, not absurdly long.
pub fn path_is_sane(p: &str) -> bool {
    p.starts_with('/')
        && p.len() > 1
        && p.len() <= MAX_PATH
        && p[1..].split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.bytes().all(|b| (0x20..0x7F).contains(&b)))
}

/// Only the user's own files are saved and restored. System and package-managed places are never touched: the
/// snapshot is read from a removable stick that anyone can edit and its checksum catches accidents, not forgery,
/// so it must not be able to rewrite what the signed `apt` flow manages (package files, apt's status, serial and
/// index, `/etc`). Packages come back only through `apt restore`.
pub fn is_user_path(p: &str) -> bool {
    !["/bin", "/etc", "/usr", "/var", "/usb"].iter().any(|d| p.strip_prefix(d).is_some_and(|rest| rest.is_empty() || rest.starts_with('/')))
}

/// The full rule: a user path that no installed package owns. The folder list alone cannot match everything a
/// package may write (its files can land anywhere), so ownership decides too.
pub fn is_saved_path(path: &str, owned: &alloc::collections::BTreeSet<String>) -> bool {
    is_user_path(path) && !owned.contains(path)
}

pub fn encode(entries: &[Entry]) -> Result<Vec<u8>, PersistError> {
    let total: usize = entries.iter().map(|e| e.path.len() + e.data.as_ref().map_or(0, Vec::len) + 7).sum();
    if entries.len() > MAX_ENTRIES || total > MAX_TOTAL {
        return Err(PersistError::TooBig);
    }
    let mut out = Vec::with_capacity(total + 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        if !path_is_sane(&e.path) {
            return Err(PersistError::Corrupt);
        }
        out.push(u8::from(e.data.is_some()));
        out.extend_from_slice(&(e.path.len() as u16).to_le_bytes());
        out.extend_from_slice(&(e.data.as_ref().map_or(0, Vec::len) as u32).to_le_bytes());
        out.extend_from_slice(e.path.as_bytes());
        if let Some(d) = &e.data {
            out.extend_from_slice(d);
        }
    }
    let sum = sha256::digest(&out);
    out.extend_from_slice(&sum);
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Entry>, PersistError> {
    if bytes.len() < MAGIC.len() + 4 + 32 || &bytes[..8] != MAGIC {
        return Err(PersistError::BadHeader);
    }
    let (body, sum) = bytes.split_at(bytes.len() - 32);
    if sha256::digest(body) != sum {
        return Err(PersistError::BadChecksum);
    }
    let count = u32::from_le_bytes([body[8], body[9], body[10], body[11]]) as usize;
    if count > MAX_ENTRIES {
        return Err(PersistError::Corrupt);
    }
    let mut at = 12usize;
    let mut out = Vec::with_capacity(count);
    let mut total = 0usize;
    for _ in 0..count {
        let head = body.get(at..at + 7).ok_or(PersistError::Corrupt)?;
        let (kind, plen, dlen) = (head[0], usize::from(u16::from_le_bytes([head[1], head[2]])), u32::from_le_bytes([head[3], head[4], head[5], head[6]]) as usize);
        at += 7;
        if kind > 1 || plen > MAX_PATH || (kind == 0 && dlen != 0) {
            return Err(PersistError::Corrupt);
        }
        total = total.saturating_add(dlen);
        if total > MAX_TOTAL {
            return Err(PersistError::TooBig);
        }
        let path = core::str::from_utf8(body.get(at..at + plen).ok_or(PersistError::Corrupt)?).map_err(|_| PersistError::Corrupt)?;
        at += plen;
        if !path_is_sane(path) {
            return Err(PersistError::Corrupt);
        }
        let data = if kind == 1 {
            let d = body.get(at..at.checked_add(dlen).ok_or(PersistError::Corrupt)?).ok_or(PersistError::Corrupt)?;
            at += dlen;
            Some(d.to_vec())
        } else {
            None
        };
        out.push(Entry { path: String::from(path), data });
    }
    if at != body.len() {
        return Err(PersistError::Corrupt);
    }
    Ok(out)
}

/// A fingerprint of a tree (FNV-1a over every path and byte): different when anything was added, removed or
/// changed, so the shell can tell cheaply whether there is anything new to save.
pub fn fingerprint(entries: impl Iterator<Item = (impl AsRef<str>, Option<impl AsRef<[u8]>>)>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |b: &[u8]| {
        for &x in b {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for (path, data) in entries {
        feed(path.as_ref().as_bytes());
        match data {
            Some(d) => {
                feed(&[1]);
                feed(&(d.as_ref().len() as u64).to_le_bytes());
                feed(d.as_ref());
            }
            None => feed(&[0]),
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Vec<Entry> {
        alloc::vec![
            Entry { path: "/home".into(), data: None },
            Entry { path: "/home/note.txt".into(), data: Some(b"hello\nworld".to_vec()) },
            Entry { path: "/home/empty".into(), data: Some(Vec::new()) },
            Entry { path: "/tmp".into(), data: None },
        ]
    }

    #[test]
    fn a_tree_survives_the_round_trip() {
        let enc = encode(&tree()).unwrap();
        assert_eq!(&enc[..8], MAGIC);
        assert_eq!(decode(&enc).unwrap(), tree());
        assert_eq!(decode(&encode(&[]).unwrap()).unwrap(), Vec::<Entry>::new());
    }

    #[test]
    fn any_damage_is_refused_and_never_panics() {
        let enc = encode(&tree()).unwrap();
        for i in 0..enc.len() {
            let mut c = enc.clone();
            c[i] ^= 0x01;
            assert!(decode(&c).is_err(), "flipping byte {i} was accepted");
        }
        for n in 0..enc.len() {
            assert!(decode(&enc[..n]).is_err());
        }
        assert_eq!(decode(b"not a snapshot at all, just text on the stick........"), Err(PersistError::BadHeader));
    }

    #[test]
    fn a_hostile_but_well_signed_snapshot_is_still_refused() {
        // Valid checksum, bad content: the checksum only proves the file is whole, not that it is safe.
        let sign = |mut body: Vec<u8>| {
            let s = sha256::digest(&body);
            body.extend_from_slice(&s);
            body
        };
        let record = |kind: u8, path: &[u8], data: &[u8]| {
            let mut r = alloc::vec![kind];
            r.extend_from_slice(&(path.len() as u16).to_le_bytes());
            r.extend_from_slice(&(data.len() as u32).to_le_bytes());
            r.extend_from_slice(path);
            r.extend_from_slice(data);
            r
        };
        let snap = |recs: &[Vec<u8>]| {
            let mut b = MAGIC.to_vec();
            b.extend_from_slice(&(recs.len() as u32).to_le_bytes());
            for r in recs {
                b.extend_from_slice(r);
            }
            sign(b)
        };
        assert!(decode(&snap(&[record(1, b"/ok.txt", b"fine")])).is_ok());
        for bad in [&b"/../etc/passwd"[..], b"relative.txt", b"/a//b", b"/a/./b", b"/", b"", b"/tab\there", b"/\xFF\xFE"] {
            assert_eq!(decode(&snap(&[record(1, bad, b"x")])), Err(PersistError::Corrupt), "{:?}", bad);
        }
        assert_eq!(decode(&snap(&[record(0, b"/dir", b"data in a folder")])), Err(PersistError::Corrupt));
        assert_eq!(decode(&snap(&[record(7, b"/x", b"")])), Err(PersistError::Corrupt));
        // Claims more data than the file holds, and a count bigger than the records present.
        let mut lie = record(1, b"/x", b"abc");
        lie[3..7].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(decode(&snap(&[lie])), Err(PersistError::Corrupt));
        let mut b = MAGIC.to_vec();
        b.extend_from_slice(&5u32.to_le_bytes());
        assert_eq!(decode(&sign(b)), Err(PersistError::Corrupt));
        // Trailing garbage before the checksum.
        let mut b = MAGIC.to_vec();
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(b"junk");
        assert_eq!(decode(&sign(b)), Err(PersistError::Corrupt));
    }

    #[test]
    fn system_and_package_paths_are_never_user_data() {
        for p in ["/home/a.txt", "/note.txt", "/tmp/x", "/home", "/usrdata/x", "/etcetera", "/binary", "/variable/x"] {
            assert!(is_user_path(p), "{p}");
        }
        for p in ["/etc", "/etc/hostname", "/usr/share/greet/README", "/var/lib/aeonpkg/status", "/var", "/bin/tool", "/usb/AEONFS.BIN"] {
            assert!(!is_user_path(p), "{p}");
        }
    }

    #[test]
    fn a_path_a_package_owns_is_never_saved_wherever_it_is() {
        let owned: alloc::collections::BTreeSet<String> = ["/opt/tool/run", "/note.txt"].iter().map(|s| String::from(*s)).collect();
        assert!(!is_saved_path("/opt/tool/run", &owned)); // outside the system folders but package-owned
        assert!(!is_saved_path("/note.txt", &owned)); // even at the root
        assert!(!is_saved_path("/etc/hostname", &owned)); // system folder
        assert!(is_saved_path("/opt/tool/mine.txt", &owned)); // the user's own file next to it
        assert!(is_saved_path("/home/a.txt", &alloc::collections::BTreeSet::new()));
    }

    #[test]
    fn too_big_is_refused_when_saving() {
        let big = alloc::vec![Entry { path: "/big".into(), data: Some(alloc::vec![0u8; MAX_TOTAL + 1]) }];
        assert_eq!(encode(&big), Err(PersistError::TooBig));
        let bad = alloc::vec![Entry { path: "../x".into(), data: None }];
        assert_eq!(encode(&bad), Err(PersistError::Corrupt));
    }

    #[test]
    fn the_fingerprint_changes_with_any_change() {
        let fp = |t: &[Entry]| fingerprint(t.iter().map(|e| (e.path.as_str(), e.data.as_deref())));
        let a = tree();
        let base = fp(&a);
        assert_eq!(base, fp(&tree()));
        let mut b = tree();
        b[1].data = Some(b"hello\nworlD".to_vec()); // same length, one byte different
        assert_ne!(base, fp(&b));
        let mut c = tree();
        c.pop();
        assert_ne!(base, fp(&c));
        let mut d = tree();
        d[3].data = Some(Vec::new()); // a folder that became an empty file
        assert_ne!(base, fp(&d));
    }
}
