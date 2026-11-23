//! BedrockOS package manager core, pure and host-testable: the signed index, dependency resolution, a defensive tar
//! reader and the installed-package database. The shell (`apt`) does the downloading and writes the files.
//!
//! Trust model: a package index is trusted only if its Ed25519 signature verifies against a key compiled into
//! the kernel (`pkgkey`). The index lists the SHA-256 and size of every package file, so a package is trusted
//! exactly when its bytes match the signed index. Plain HTTP is therefore fine for transport. There is no
//! revocation, no key rotation protocol and no rollback protection beyond what the index contents say.
//!
//! A package is an uncompressed `ustar` archive: `PKGINFO` first, then files with paths relative to `/`.
//! BedrockOS runs no Linux binaries, so packages carry data, scripts and BedrockOS programs, never `.deb` contents.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use crate::ed25519;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PkgError {
    BadIndex(&'static str),
    BadSignature,
    NotFound(String),
    MissingDependency { pkg: String, dep: String },
    Cycle(String),
    BadTar(&'static str),
    UnsafePath(String),
    HashMismatch,
    SizeMismatch,
    InfoMismatch,
    Conflict { path: String, owner: String },
    /// The signed index carries no `Serial:` line.
    NoSerial,
    /// The repository offered an older package list than one already seen.
    Rollback { offered: u64, seen: u64 },
}

impl core::fmt::Display for PkgError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PkgError::BadIndex(why) => write!(f, "the package index is malformed ({why})"),
            PkgError::BadSignature => write!(f, "the package index signature is not valid for any trusted key"),
            PkgError::NotFound(n) => write!(f, "no package called '{n}' (try `apt update` and `apt search`)"),
            PkgError::MissingDependency { pkg, dep } => write!(f, "'{pkg}' needs '{dep}', which the repository does not have"),
            PkgError::Cycle(n) => write!(f, "dependency loop through '{n}'"),
            PkgError::BadTar(why) => write!(f, "the package file is not a valid archive ({why})"),
            PkgError::UnsafePath(p) => write!(f, "the package contains an unsafe path '{p}'"),
            PkgError::HashMismatch => write!(f, "the downloaded file does not match the SHA-256 in the signed index"),
            PkgError::SizeMismatch => write!(f, "the downloaded file has a different size than the index says"),
            PkgError::InfoMismatch => write!(f, "the package's own PKGINFO disagrees with the index"),
            PkgError::Conflict { path, owner } => write!(f, "'{path}' already belongs to package '{owner}'"),
            PkgError::NoSerial => write!(f, "the package index has no Serial line (rebuild it with a current tools/aeon_repo.py)"),
            PkgError::Rollback { offered, seen } => write!(f, "the repository offered an OLDER package list (serial {offered}, already seen {seen}): refusing, this could be a rollback attack"),
        }
    }
}

pub fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'+' | b'.' | b'-'))
}

fn valid_version(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'-' | b'~' | b'_'))
}

/// A repository file name: relative, no `..`, no odd characters.
fn valid_filename(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('/')
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'-' | b'_' | b'/'))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub version: String,
    pub depends: Vec<String>,
    pub size: usize,
    pub sha256: [u8; 32],
    pub filename: String,
    pub description: String,
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parses an index: stanzas of `Key: value` lines separated by blank lines.
pub fn parse_index(text: &str) -> Result<Vec<Entry>, PkgError> {
    parse_index_full(text).map(|(_, entries)| entries)
}

/// Like `parse_index`, also returning the `Serial:` of the optional header stanza (`Serial: 7` on its own).
pub fn parse_index_full(text: &str) -> Result<(Option<u64>, Vec<Entry>), PkgError> {
    let mut out: Vec<Entry> = Vec::new();
    let mut serial = None;
    for stanza in text.split("\n\n") {
        let stanza = stanza.trim();
        if stanza.is_empty() {
            continue;
        }
        if let Some(n) = stanza.strip_prefix("Serial:").filter(|r| !r.contains('\n')) {
            if serial.is_some() {
                return Err(PkgError::BadIndex("Serial given twice"));
            }
            serial = Some(n.trim().parse::<u64>().map_err(|_| PkgError::BadIndex("bad Serial"))?);
            continue;
        }
        let (mut name, mut version, mut depends, mut size, mut sha, mut file, mut desc) = (None, None, Vec::new(), None, None, None, String::new());
        for line in stanza.lines() {
            let (k, v) = line.split_once(':').ok_or(PkgError::BadIndex("a line without a colon"))?;
            let v = v.trim();
            match k.trim() {
                "Package" => name = Some(v.to_string()),
                "Version" => version = Some(v.to_string()),
                "Depends" => depends = v.split(',').map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).collect(),
                "Size" => size = Some(v.parse::<usize>().map_err(|_| PkgError::BadIndex("bad Size"))?),
                "SHA256" => sha = Some(parse_hex32(v).ok_or(PkgError::BadIndex("bad SHA256"))?),
                "Filename" => file = Some(v.to_string()),
                "Description" => desc = v.chars().filter(|c| !c.is_control()).take(100).collect(),
                _ => {}
            }
        }
        let name = name.ok_or(PkgError::BadIndex("missing Package"))?;
        let version = version.ok_or(PkgError::BadIndex("missing Version"))?;
        let filename = file.ok_or(PkgError::BadIndex("missing Filename"))?;
        if !valid_name(&name) || !valid_version(&version) || !valid_filename(&filename) || depends.iter().any(|d| !valid_name(d)) {
            return Err(PkgError::BadIndex("an invalid name, version or file name"));
        }
        if out.iter().any(|e| e.name == name) {
            return Err(PkgError::BadIndex("a package is listed twice"));
        }
        out.push(Entry {
            name,
            version,
            depends,
            size: size.ok_or(PkgError::BadIndex("missing Size"))?,
            sha256: sha.ok_or(PkgError::BadIndex("missing SHA256"))?,
            filename,
            description: desc,
        });
    }
    Ok((serial, out))
}

/// Does `sig` (64 raw bytes, or 128 hex characters with optional whitespace) sign `msg` under one of `keys`?
pub fn verify_signature(keys: &[[u8; 32]], msg: &[u8], sig: &[u8]) -> bool {
    let raw: [u8; 64] = if sig.len() == 64 {
        sig.try_into().unwrap()
    } else {
        let t = core::str::from_utf8(sig).unwrap_or("").trim();
        if t.len() != 128 {
            return false;
        }
        let mut out = [0u8; 64];
        for (i, o) in out.iter_mut().enumerate() {
            match t.get(i * 2..i * 2 + 2).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => *o = v,
                None => return false,
            }
        }
        out
    };
    keys.iter().any(|k| ed25519::verify(k, msg, &raw))
}

/// Parses the index only if its signature is valid. Returns its serial (which grows with every new index the
/// repository publishes) and the packages.
pub fn parse_signed_index(keys: &[[u8; 32]], index: &[u8], sig: &[u8]) -> Result<(u64, Vec<Entry>), PkgError> {
    if !verify_signature(keys, index, sig) {
        return Err(PkgError::BadSignature);
    }
    let (serial, entries) = parse_index_full(core::str::from_utf8(index).map_err(|_| PkgError::BadIndex("not text"))?)?;
    Ok((serial.ok_or(PkgError::NoSerial)?, entries))
}

/// Refuses an index older than the newest one seen before (`floor`, 0 if none). The same serial is fine: it is
/// the same list again. This blunts replay of an old, validly signed list that hides newer package versions.
pub fn check_serial(offered: u64, floor: u64) -> Result<(), PkgError> {
    if offered < floor { Err(PkgError::Rollback { offered, seen: floor }) } else { Ok(()) }
}

/// Checks a downloaded package file against its index entry.
pub fn verify_download(e: &Entry, data: &[u8]) -> Result<(), PkgError> {
    if data.len() != e.size {
        return Err(PkgError::SizeMismatch);
    }
    if crate::sha256::digest(data) != e.sha256 {
        return Err(PkgError::HashMismatch);
    }
    Ok(())
}

/// Returns indices into `index` of everything that must be installed, dependencies before dependents.
/// Packages for which `is_installed` is true are left out.
pub fn resolve(index: &[Entry], is_installed: &dyn Fn(&str) -> bool, targets: &[&str]) -> Result<Vec<usize>, PkgError> {
    fn visit(index: &[Entry], inst: &dyn Fn(&str) -> bool, name: &str, parent: Option<&str>, order: &mut Vec<usize>, stack: &mut Vec<usize>) -> Result<(), PkgError> {
        if inst(name) {
            return Ok(());
        }
        let Some(i) = index.iter().position(|e| e.name == name) else {
            return Err(match parent {
                Some(p) => PkgError::MissingDependency { pkg: p.to_string(), dep: name.to_string() },
                None => PkgError::NotFound(name.to_string()),
            });
        };
        if order.contains(&i) {
            return Ok(());
        }
        if stack.contains(&i) {
            return Err(PkgError::Cycle(name.to_string()));
        }
        stack.push(i);
        for d in &index[i].depends {
            visit(index, inst, d, Some(name), order, stack)?;
        }
        stack.pop();
        order.push(i);
        Ok(())
    }
    let (mut order, mut stack) = (Vec::new(), Vec::new());
    for t in targets {
        visit(index, is_installed, t, None, &mut order, &mut stack)?;
    }
    Ok(order)
}

// --- tar --------------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub struct TarEntry<'a> {
    pub path: String,
    pub dir: bool,
    pub data: &'a [u8],
}

fn octal(field: &[u8]) -> Option<usize> {
    let s: &[u8] = field.split(|&b| b == 0 || b == b' ').next().unwrap_or(&[]);
    if s.is_empty() {
        return Some(0);
    }
    let mut v = 0usize;
    for &c in s {
        if !(b'0'..=b'7').contains(&c) {
            return None;
        }
        v = v.checked_mul(8)?.checked_add(usize::from(c - b'0'))?;
    }
    Some(v)
}

/// Reads a `ustar` archive: regular files and directories only. Symbolic and hard links, devices, long-name
/// extensions and anything else are refused, as are bad checksums, truncated data and totals above `max_total`.
pub fn read_tar(data: &[u8], max_total: usize) -> Result<Vec<TarEntry<'_>>, PkgError> {
    let mut out = Vec::new();
    let (mut pos, mut total) = (0usize, 0usize);
    loop {
        let Some(h) = data.get(pos..pos + 512) else {
            // A well-formed archive ends with zero blocks; running out of data early is a truncation.
            return Err(PkgError::BadTar("no end-of-archive marker"));
        };
        if h.iter().all(|&b| b == 0) {
            return Ok(out);
        }
        let stored = octal(&h[148..156]).ok_or(PkgError::BadTar("bad checksum field"))?;
        let sum: usize = h.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { usize::from(b) }).sum();
        if sum != stored {
            return Err(PkgError::BadTar("header checksum mismatch"));
        }
        let kind = h[156];
        let size = octal(&h[124..136]).ok_or(PkgError::BadTar("bad size"))?;
        if &h[257..262] != b"ustar" {
            return Err(PkgError::BadTar("not a ustar archive"));
        }
        let name_end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let prefix_end = h[345..500].iter().position(|&b| b == 0).unwrap_or(155);
        if prefix_end != 0 {
            return Err(PkgError::BadTar("long names are not supported"));
        }
        let name = core::str::from_utf8(&h[..name_end]).map_err(|_| PkgError::BadTar("name is not text"))?;
        let dir = match kind {
            b'0' | 0 => false,
            b'5' => true,
            _ => return Err(PkgError::BadTar("only plain files and directories are allowed")),
        };
        let path = name.trim_end_matches('/').to_string();
        total = total.checked_add(size).ok_or(PkgError::BadTar("too big"))?;
        if total > max_total {
            return Err(PkgError::BadTar("the archive is too big"));
        }
        let start = pos + 512;
        let padded = size.div_ceil(512) * 512;
        let body = data.get(start..start + size).ok_or(PkgError::BadTar("file data is cut off"))?;
        if start + padded > data.len() && size > 0 {
            return Err(PkgError::BadTar("file data is cut off"));
        }
        out.push(TarEntry { path, dir, data: body });
        pos = start + padded;
    }
}

/// Maps an archive path to its place in the filesystem (`bin/x` becomes `/bin/x`), refusing anything unsafe.
pub fn install_path(p: &str) -> Result<String, PkgError> {
    let bad = || PkgError::UnsafePath(p.to_string());
    if p.is_empty() || p.starts_with('/') || p.contains('\\') || p.contains('\0') || p.len() > 120 {
        return Err(bad());
    }
    let mut out = String::new();
    for c in p.split('/') {
        if c.is_empty() || c == "." || c == ".." || c.len() > crate::ramfs::MAX_NAME || !crate::ramfs::name_ok(c) {
            return Err(bad());
        }
        out.push('/');
        out.push_str(c);
    }
    Ok(out)
}

/// The files to create for a package archive, after checking `PKGINFO` against the index entry.
pub struct Plan {
    pub dirs: Vec<String>,
    pub files: Vec<(String, Vec<u8>)>,
}

pub fn plan_install(entry: &Entry, tar: &[u8]) -> Result<Plan, PkgError> {
    let entries = read_tar(tar, 4 * 1024 * 1024)?;
    let info = entries.first().filter(|e| e.path == "PKGINFO" && !e.dir).ok_or(PkgError::BadTar("PKGINFO must come first"))?;
    let text = core::str::from_utf8(info.data).map_err(|_| PkgError::InfoMismatch)?;
    let get = |key: &str| text.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')).map(str::trim));
    if get("Package") != Some(entry.name.as_str()) || get("Version") != Some(entry.version.as_str()) {
        return Err(PkgError::InfoMismatch);
    }
    let mut plan = Plan { dirs: Vec::new(), files: Vec::new() };
    for e in &entries[1..] {
        let path = install_path(&e.path)?;
        if e.dir {
            plan.dirs.push(path);
        } else {
            if plan.files.iter().any(|(p, _)| *p == path) {
                return Err(PkgError::BadTar("a file appears twice"));
            }
            plan.files.push((path, e.data.to_vec()));
        }
    }
    Ok(plan)
}

// --- installed database ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub depends: Vec<String>,
    pub files: Vec<String>,
    /// Installed only to satisfy another package's dependency.
    pub auto: bool,
}

pub fn format_status(list: &[Installed]) -> String {
    let mut s = String::new();
    for p in list {
        let _ = write!(s, "Package: {}\nVersion: {}\nAuto: {}\nDepends: {}\n", p.name, p.version, if p.auto { "yes" } else { "no" }, p.depends.join(", "));
        for f in &p.files {
            let _ = writeln!(s, "File: {f}");
        }
        s.push('\n');
    }
    s
}

pub fn parse_status(text: &str) -> Vec<Installed> {
    let mut out = Vec::new();
    for stanza in text.split("\n\n") {
        let mut p = Installed { name: String::new(), version: String::new(), depends: Vec::new(), files: Vec::new(), auto: false };
        for line in stanza.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let v = v.trim();
            match k {
                "Package" => p.name = v.to_string(),
                "Version" => p.version = v.to_string(),
                "Auto" => p.auto = v == "yes",
                "Depends" => p.depends = v.split(',').map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).collect(),
                "File" => p.files.push(v.to_string()),
                _ => {}
            }
        }
        if valid_name(&p.name) {
            out.push(p);
        }
    }
    out
}

/// Automatically installed packages that no installed package depends on any more.
pub fn orphans(list: &[Installed]) -> Vec<String> {
    list.iter()
        .filter(|p| p.auto && !list.iter().any(|q| q.name != p.name && q.depends.contains(&p.name)))
        .map(|p| p.name.clone())
        .collect()
}

/// Packages that depend on `name`.
pub fn dependents<'a>(list: &'a [Installed], name: &str) -> Vec<&'a str> {
    list.iter().filter(|q| q.depends.iter().any(|d| d == name)).map(|q| q.name.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a ustar header the way GNU tar and Python's tarfile do.
    fn header(name: &str, size: usize, kind: u8) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(alloc::format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].fill(b' ');
        let sum: usize = h.iter().map(|&b| usize::from(b)).sum();
        h[148..156].copy_from_slice(alloc::format!("{sum:06o}\0 ").as_bytes());
        h
    }

    fn tar(files: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data, kind) in files {
            out.extend_from_slice(&header(name, data.len(), *kind));
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    fn entry(name: &str, version: &str, deps: &[&str]) -> Entry {
        Entry { name: name.into(), version: version.into(), depends: deps.iter().map(|d| d.to_string()).collect(), size: 1, sha256: [0; 32], filename: alloc::format!("pool/{name}.tar"), description: String::new() }
    }

    #[test]
    fn header_checksum_matches_what_python_tarfile_wrote() {
        // Python: TarInfo("PKGINFO"), mode 0o644, mtime 0, size 27, regular file -> checksum field "006761".
        let h = header("PKGINFO", 27, b'0');
        assert_eq!(&h[148..155], b"006761\0");
    }

    #[test]
    fn tar_round_trip_and_padding() {
        let t = tar(&[("PKGINFO", b"Package: demo\n", b'0'), ("usr", b"", b'5'), ("usr/share/readme", &[7u8; 600], b'0')]);
        let e = read_tar(&t, 10_000).unwrap();
        assert_eq!(e.len(), 3);
        assert_eq!((e[0].path.as_str(), e[0].dir, e[0].data), ("PKGINFO", false, &b"Package: demo\n"[..]));
        assert_eq!((e[1].path.as_str(), e[1].dir), ("usr", true));
        assert_eq!((e[2].path.as_str(), e[2].data.len()), ("usr/share/readme", 600));
    }

    #[test]
    fn tar_defences() {
        let good = tar(&[("a", b"x", b'0')]);
        assert!(read_tar(&good, 10).is_ok());
        assert_eq!(read_tar(&good, 0), Err(PkgError::BadTar("the archive is too big")));
        assert_eq!(read_tar(&tar(&[("l", b"", b'2')]), 10), Err(PkgError::BadTar("only plain files and directories are allowed")));
        assert_eq!(read_tar(&tar(&[("h", b"", b'1')]), 10), Err(PkgError::BadTar("only plain files and directories are allowed")));
        let mut bad = good.clone();
        bad[0] ^= 1;
        assert_eq!(read_tar(&bad, 10), Err(PkgError::BadTar("header checksum mismatch")));
        assert!(read_tar(&good[..600], 10).is_err(), "cut off");
        assert!(read_tar(&good[..512], 10).is_err(), "no end marker");
        let mut big = header("big", 0o7777_7777_777, b'0').to_vec();
        big.extend_from_slice(&[0u8; 1024]);
        assert!(read_tar(&big, usize::MAX).is_err());
        let mut seed = 1u32;
        for len in 0..1500usize {
            let junk: Vec<u8> = (0..len).map(|_| { seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345); (seed >> 16) as u8 }).collect();
            let _ = read_tar(&junk, 100_000);
        }
    }

    #[test]
    fn install_paths() {
        assert_eq!(install_path("bin/hello").unwrap(), "/bin/hello");
        assert_eq!(install_path("usr/share/doc/x.txt").unwrap(), "/usr/share/doc/x.txt");
        for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", "a/./b", "a\\b", "a/b\0c", &"x".repeat(33)] {
            assert!(install_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn plan_checks_pkginfo_against_the_index() {
        let e = entry("demo", "1.2", &[]);
        let t = tar(&[("PKGINFO", b"Package: demo\nVersion: 1.2\n", b'0'), ("usr", b"", b'5'), ("usr/readme", b"hi", b'0')]);
        let p = plan_install(&e, &t).unwrap();
        assert_eq!(p.dirs, ["/usr"]);
        assert_eq!(p.files, [("/usr/readme".to_string(), b"hi".to_vec())]);
        // Wrong name or version: a package that was swapped for another one is refused.
        assert_eq!(plan_install(&entry("other", "1.2", &[]), &t).err(), Some(PkgError::InfoMismatch));
        assert_eq!(plan_install(&entry("demo", "9", &[]), &t).err(), Some(PkgError::InfoMismatch));
        // PKGINFO not first, path traversal, duplicate file.
        let late = tar(&[("a", b"x", b'0'), ("PKGINFO", b"Package: demo\nVersion: 1.2\n", b'0')]);
        assert!(plan_install(&e, &late).is_err());
        let evil = tar(&[("PKGINFO", b"Package: demo\nVersion: 1.2\n", b'0'), ("../evil", b"x", b'0')]);
        assert!(matches!(plan_install(&e, &evil), Err(PkgError::UnsafePath(_))));
        let dup = tar(&[("PKGINFO", b"Package: demo\nVersion: 1.2\n", b'0'), ("a", b"1", b'0'), ("a", b"2", b'0')]);
        assert!(plan_install(&e, &dup).is_err());
    }

    const INDEX: &str = "Package: greeter\nVersion: 1.0\nDepends: greet-data, base\nSize: 2048\nSHA256: 0000000000000000000000000000000000000000000000000000000000000001\nFilename: pool/greeter_1.0.tar\nDescription: Says hello\n\nPackage: greet-data\nVersion: 2.1\nSize: 1024\nSHA256: 0000000000000000000000000000000000000000000000000000000000000002\nFilename: pool/greet-data_2.1.tar\nDescription: Data for greeter\n\nPackage: base\nVersion: 1\nSize: 10\nSHA256: 0000000000000000000000000000000000000000000000000000000000000003\nFilename: pool/base_1.tar\n";

    #[test]
    fn index_parsing() {
        let idx = parse_index(INDEX).unwrap();
        assert_eq!(idx.len(), 3);
        assert_eq!(idx[0].depends, ["greet-data", "base"]);
        assert_eq!((idx[1].size, idx[1].sha256[31]), (1024, 2));
        assert_eq!(idx[2].description, "");
        for bad in [
            "Package: x\nVersion: 1\n",
            "Package: X\nVersion: 1\nSize: 1\nSHA256: 0000000000000000000000000000000000000000000000000000000000000001\nFilename: a",
            "Package: x\nVersion: 1\nSize: 1\nSHA256: zz\nFilename: a",
            "Package: x\nVersion: 1\nSize: 1\nSHA256: 0000000000000000000000000000000000000000000000000000000000000001\nFilename: ../a",
            "Package: x\nVersion: 1\nSize: 1\nSHA256: 0000000000000000000000000000000000000000000000000000000000000001\nFilename: /abs",
            "no colon here",
        ] {
            assert!(parse_index(bad).is_err(), "{bad}");
        }
        let dup = alloc::format!("{INDEX}\n\n{INDEX}");
        assert!(parse_index(&dup).is_err());
    }

    #[test]
    fn dependencies_come_first_and_installed_ones_are_skipped() {
        let idx = parse_index(INDEX).unwrap();
        let order = resolve(&idx, &|_| false, &["greeter"]).unwrap();
        let names: Vec<&str> = order.iter().map(|&i| idx[i].name.as_str()).collect();
        assert_eq!(names, ["greet-data", "base", "greeter"]);
        let order = resolve(&idx, &|n| n == "greet-data", &["greeter", "base"]).unwrap();
        assert_eq!(order.iter().map(|&i| idx[i].name.as_str()).collect::<Vec<_>>(), ["base", "greeter"]);
        assert_eq!(resolve(&idx, &|_| false, &["nope"]), Err(PkgError::NotFound("nope".into())));
        assert!(resolve(&idx, &|_| true, &["greeter"]).unwrap().is_empty());
        let mut broken = idx.clone();
        broken[0].depends.push("ghost".into());
        assert_eq!(resolve(&broken, &|_| false, &["greeter"]), Err(PkgError::MissingDependency { pkg: "greeter".into(), dep: "ghost".into() }));
        let mut cyc = vec![entry("a", "1", &["b"]), entry("b", "1", &["a"])];
        assert!(matches!(resolve(&cyc, &|_| false, &["a"]), Err(PkgError::Cycle(_))));
        cyc[1].depends.clear();
        assert_eq!(resolve(&cyc, &|_| false, &["a"]).unwrap(), [1, 0]);
    }

    #[test]
    fn downloads_must_match_the_signed_index() {
        let data = b"package bytes";
        let mut e = entry("p", "1", &[]);
        e.size = data.len();
        e.sha256 = crate::sha256::digest(data);
        assert_eq!(verify_download(&e, data), Ok(()));
        let mut tampered = data.to_vec();
        tampered[0] ^= 1;
        assert_eq!(verify_download(&e, &tampered), Err(PkgError::HashMismatch));
        assert_eq!(verify_download(&e, b"short"), Err(PkgError::SizeMismatch));
    }

    #[test]
    fn signature_formats() {
        let pk: [u8; 32] = [
            0x3d, 0x40, 0x17, 0xc3, 0xe8, 0x43, 0x89, 0x5a, 0x92, 0xb7, 0x0a, 0xa7, 0x4d, 0x1b, 0x7e, 0xbc, 0x9c, 0x98, 0x2c, 0xcf, 0x2e, 0xc4, 0x96, 0x8c, 0xc0, 0xcd, 0x55, 0xf1, 0x2a, 0xf4, 0x66, 0x0c,
        ];
        let hexsig = "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00";
        assert!(verify_signature(&[pk], &[0x72], hexsig.as_bytes()));
        assert!(verify_signature(&[pk], &[0x72], alloc::format!("  {hexsig}\n").as_bytes()));
        let raw: Vec<u8> = (0..64).map(|i| u8::from_str_radix(&hexsig[i * 2..i * 2 + 2], 16).unwrap()).collect();
        assert!(verify_signature(&[pk], &[0x72], &raw));
        assert!(!verify_signature(&[pk], &[0x73], &raw));
        assert!(!verify_signature(&[], &[0x72], &raw), "no trusted key, nothing verifies");
        assert!(!verify_signature(&[pk], &[0x72], b"nothex"));
        assert!(!verify_signature(&[pk], &[0x72], &raw[..63]));
        assert_eq!(parse_signed_index(&[pk], b"Package: x\n", &raw), Err(PkgError::BadSignature));
        // A correctly signed index without a serial is refused (nothing signed for the test key here, so only the
        // parsing rules are checked): the header stanza must be a lone `Serial:` line.
        assert_eq!(parse_index_full("Serial: 12\n\nPackage: a\nVersion: 1\nSize: 1\nSHA256: 0000000000000000000000000000000000000000000000000000000000000001\nFilename: a\n").unwrap().0, Some(12));
        assert_eq!(parse_index_full(INDEX).unwrap().0, None);
        assert!(parse_index_full("Serial: x\n\n").is_err());
        assert!(parse_index_full("Serial: 1\n\nSerial: 2\n").is_err());
    }

    #[test]
    fn rollback_check() {
        assert_eq!(check_serial(5, 5), Ok(()));
        assert_eq!(check_serial(6, 5), Ok(()));
        assert_eq!(check_serial(0, 0), Ok(()));
        assert_eq!(check_serial(4, 5), Err(PkgError::Rollback { offered: 4, seen: 5 }));
    }

    #[test]
    fn status_database_round_trip_and_orphans() {
        let list = vec![
            Installed { name: "greeter".into(), version: "1.0".into(), depends: vec!["greet-data".into()], files: vec!["/usr/a".into(), "/usr/b".into()], auto: false },
            Installed { name: "greet-data".into(), version: "2.1".into(), depends: vec![], files: vec!["/usr/d".into()], auto: true },
            Installed { name: "lonely-lib".into(), version: "1".into(), depends: vec![], files: vec![], auto: true },
        ];
        assert_eq!(parse_status(&format_status(&list)), list);
        assert_eq!(orphans(&list), ["lonely-lib"]);
        assert_eq!(dependents(&list, "greet-data"), ["greeter"]);
        assert!(dependents(&list, "greeter").is_empty());
        assert!(parse_status("garbage\n\nPackage: BAD NAME\n").is_empty());
    }
}
