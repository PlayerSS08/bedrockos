//! `apt` for BedrockOS: update, install, remove and query packages from a signed repository (`pkg`), over HTTP
//! (`netstack`). Everything here works on the RAM filesystem and a [`Fetcher`], so the whole flow, including
//! tampering and failures, is unit-tested on the host with a fake server.
//!
//! This is NOT Debian's apt: it does not read `.deb` files and cannot run Linux programs. It uses the same
//! verbs because they are what people know. Honest limits: the filesystem is in RAM (up to 64 entries and
//! 1 MiB unless changed), packages are copied to the AEON USB stick when one is writable, and there is no
//! version solving (a package name is installed once, at the version the index lists).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::pkg::{self, Entry, Installed};
use crate::ramfs::RamFs;

pub const SOURCE_PATH: &str = "/etc/apt/source";
pub const INDEX_PATH: &str = "/var/lib/aeonpkg/index";
pub const SIG_PATH: &str = "/var/lib/aeonpkg/index.sig";
pub const STATUS_PATH: &str = "/var/lib/aeonpkg/status";
/// Highest index serial seen (also kept on the USB stick so it survives power off).
pub const SERIAL_PATH: &str = "/var/lib/aeonpkg/serial";

/// Largest index or package the downloader will keep.
const MAX_INDEX: usize = 256 * 1024;
const MAX_PACKAGE: usize = 4 * 1024 * 1024;

/// Where file contents come from: the network, or (in tests) a table of known answers.
pub trait Fetcher {
    /// `rel` is a path relative to the repository root (`index`, `pool/x_1.tar`).
    fn get(&mut self, rel: &str, max: usize) -> Result<Vec<u8>, String>;
}

/// Downloads from `<source>/<rel>` with the kernel's HTTP client.
pub struct NetFetcher {
    pub base: String,
}

impl Fetcher for NetFetcher {
    fn get(&mut self, rel: &str, max: usize) -> Result<Vec<u8>, String> {
        let url = format!("{}{}", self.base, rel);
        #[cfg(target_os = "uefi")]
        {
            let r = crate::netstack::http_fetch(&url, max, &mut |_, _| {})?;
            if r.status != 200 {
                return Err(format!("{url}: the server answered HTTP {}", r.status));
            }
            Ok(r.body)
        }
        #[cfg(not(target_os = "uefi"))]
        {
            let _ = max;
            Err(format!("{url}: no network in host builds"))
        }
    }
}

fn ensure_parents(fs: &mut RamFs, file_path: &str) {
    let mut acc = String::new();
    let parts: Vec<&str> = file_path.split('/').filter(|s| !s.is_empty()).collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        acc.push('/');
        acc.push_str(part);
        if !fs.is_dir(&acc) {
            let _ = fs.mkdir(&acc);
        }
    }
}

fn write_file(fs: &mut RamFs, path: &str, data: &[u8]) -> Result<(), String> {
    ensure_parents(fs, path);
    fs.write(path, data).map_err(|e| format!("cannot write {path}: {e:?} (the RAM filesystem is small; `df` shows it)"))
}

pub fn get_source(fs: &RamFs) -> Option<String> {
    fs.read(SOURCE_PATH).and_then(|d| core::str::from_utf8(d).ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Sets the repository address (`http://host[:port]/path/`). Only plain `http://` can be used today.
pub fn set_source(fs: &mut RamFs, url: &str) -> Result<String, String> {
    let u = crate::http::parse_url(url).map_err(|_| format!("'{url}' is not a usable http:// address"))?;
    if u.https {
        return Err("https:// needs TLS, which BedrockOS does not have yet. The index is signed, so plain http:// is safe here".to_string());
    }
    let mut norm = url.to_string();
    if !norm.ends_with('/') {
        norm.push('/');
    }
    write_file(fs, SOURCE_PATH, norm.as_bytes())?;
    Ok(norm)
}

/// Every path an installed package owns. The saved-files snapshot never carries these: packages come back
/// only through the signed `apt restore`.
pub fn owned_paths(fs: &RamFs) -> alloc::collections::BTreeSet<String> {
    load_status(fs).into_iter().flat_map(|p| p.files).collect()
}

pub fn load_status(fs: &RamFs) -> Vec<Installed> {
    fs.read(STATUS_PATH).and_then(|d| core::str::from_utf8(d).ok()).map(pkg::parse_status).unwrap_or_default()
}

fn save_status(fs: &mut RamFs, list: &[Installed]) -> Result<(), String> {
    write_file(fs, STATUS_PATH, pkg::format_status(list).as_bytes())
}

/// The stored index, re-verified against the stored signature every time it is used.
pub fn load_index(fs: &RamFs, keys: &[[u8; 32]]) -> Result<Vec<Entry>, String> {
    let (Some(index), Some(sig)) = (fs.read(INDEX_PATH), fs.read(SIG_PATH)) else {
        return Err("no package list yet: run `apt source <http://...>` and `apt update` first".to_string());
    };
    pkg::parse_signed_index(keys, index, sig).map(|(_, entries)| entries).map_err(|e| e.to_string())
}

/// The highest serial remembered in RAM, or 0.
pub fn stored_serial(fs: &RamFs) -> u64 {
    fs.read(SERIAL_PATH).and_then(|d| core::str::from_utf8(d).ok()).and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// Downloads the index and its signature, verifies them and stores them. Returns the number of packages.
/// `floor` is the highest serial remembered elsewhere (the USB stick); the larger of it and the one in RAM must
/// not be exceeded by an older list.
pub fn update(
    fs: &mut RamFs,
    f: &mut dyn Fetcher,
    keys: &[[u8; 32]],
    floor: u64,
    say: &mut dyn FnMut(&str),
    persist: &mut dyn FnMut(&str, &[u8]),
) -> Result<usize, String> {
    say("Get: index");
    let index = f.get("index", MAX_INDEX)?;
    let sig = f.get("index.sig", 1024)?;
    // Nothing is stored unless the signature is valid and the list is not older than one already seen: a bad or
    // replayed index never replaces a good one.
    let (serial, entries) = pkg::parse_signed_index(keys, &index, &sig).map_err(|e| e.to_string())?;
    pkg::check_serial(serial, stored_serial(fs).max(floor)).map_err(|e| e.to_string())?;
    write_file(fs, INDEX_PATH, &index)?;
    write_file(fs, SIG_PATH, &sig)?;
    let serial_text = format!("{serial}\n");
    write_file(fs, SERIAL_PATH, serial_text.as_bytes())?;
    persist("index", &index);
    persist("index.sig", &sig);
    persist("serial", serial_text.as_bytes());
    say(&format!("Signature OK (list serial {serial}). {} package(s) available.", entries.len()));
    Ok(entries.len())
}

/// Installs `names` and whatever they depend on.
pub fn install(
    fs: &mut RamFs,
    f: &mut dyn Fetcher,
    keys: &[[u8; 32]],
    names: &[&str],
    say: &mut dyn FnMut(&str),
    persist: &mut dyn FnMut(&str, &[u8]),
) -> Result<(), String> {
    let index = load_index(fs, keys)?;
    let mut status = load_status(fs);
    let order = pkg::resolve(&index, &|n| status.iter().any(|p| p.name == n), names).map_err(|e| e.to_string())?;
    if order.is_empty() {
        say("Everything asked for is already installed.");
        return Ok(());
    }
    let list: Vec<String> = order.iter().map(|&i| format!("{} {}", index[i].name, index[i].version)).collect();
    say(&format!("The following will be installed: {}", list.join(", ")));

    for &i in &order {
        let e = &index[i];
        say(&format!("Get: {} {} [{} B]", e.name, e.version, e.size));
        let data = f.get(&e.filename, e.size.min(MAX_PACKAGE).saturating_add(1)).map_err(|err| format!("{}: {err}", e.name))?;
        // The signed index is the authority: size and SHA-256 must match it before anything is unpacked.
        pkg::verify_download(e, &data).map_err(|err| format!("{}: {err}", e.name))?;
        let plan = pkg::plan_install(e, &data).map_err(|err| format!("{}: {err}", e.name))?;

        for (path, _) in &plan.files {
            if let Some(owner) = status.iter().find(|p| p.files.contains(path)) {
                return Err(pkg::PkgError::Conflict { path: path.clone(), owner: owner.name.clone() }.to_string());
            }
            if fs.read(path).is_some() || fs.is_dir(path) {
                return Err(pkg::PkgError::Conflict { path: path.clone(), owner: "(a file you created)".to_string() }.to_string());
            }
        }
        // Write everything; if anything fails, take back what was written so the system is not left half-installed.
        let mut written: Vec<String> = Vec::new();
        let mut failure = None;
        for (path, content) in &plan.files {
            match write_file(fs, path, content) {
                Ok(()) => written.push(path.clone()),
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }
        for dir in &plan.dirs {
            if !fs.is_dir(dir) {
                let _ = fs.mkdir(dir);
            }
        }
        if let Some(err) = failure {
            for p in &written {
                fs.remove(p);
            }
            return Err(format!("{}: {err}; nothing was installed", e.name));
        }
        status.push(Installed {
            name: e.name.clone(),
            version: e.version.clone(),
            depends: e.depends.clone(),
            files: written,
            auto: !names.contains(&e.name.as_str()),
        });
        if let Err(err) = save_status(fs, &status) {
            // The record of what is installed could not be saved: undo the files so nothing is left unowned.
            if let Some(p) = status.pop() {
                for f in &p.files {
                    fs.remove(f);
                }
            }
            return Err(format!("{}: {err}; nothing was installed", e.name));
        }
        persist(&e.filename, &data);
        say(&format!("Installed {} {} ({} file(s))", e.name, e.version, plan.files.len()));
    }
    persist("installed", status.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join("\n").as_bytes());
    Ok(())
}

/// Reinstalls packages that are already installed.
pub fn reinstall(
    fs: &mut RamFs,
    f: &mut dyn Fetcher,
    keys: &[[u8; 32]],
    names: &[&str],
    say: &mut dyn FnMut(&str),
    persist: &mut dyn FnMut(&str, &[u8]),
) -> Result<(), String> {
    let index = load_index(fs, keys)?;
    let mut status = load_status(fs);
    for n in names {
        if !status.iter().any(|p| p.name == *n) {
            return Err(format!("'{n}' is not installed; cannot reinstall"));
        }
    }
    for n in names {
        let e = index.iter().find(|e| e.name == *n).ok_or_else(|| pkg::PkgError::NotFound(n.to_string()).to_string())?;
        say(&format!("Get: {} {} [{} B]", e.name, e.version, e.size));
        let data = f.get(&e.filename, e.size.min(MAX_PACKAGE).saturating_add(1)).map_err(|err| format!("{}: {err}", e.name))?;
        pkg::verify_download(e, &data).map_err(|err| format!("{}: {err}", e.name))?;
        let plan = pkg::plan_install(e, &data).map_err(|err| format!("{}: {err}", e.name))?;

        let old_pos = status.iter().position(|p| p.name == e.name).expect("checked above");
        let old_pkg = status[old_pos].clone();
        for (path, _) in &plan.files {
            if let Some(owner) = status.iter().find(|p| p.name != e.name && p.files.contains(path)) {
                return Err(pkg::PkgError::Conflict { path: path.clone(), owner: owner.name.clone() }.to_string());
            }
            // A path this package did not own before must be free: never overwrite a file or folder of the user's.
            if !old_pkg.files.contains(path) && (fs.read(path).is_some() || fs.is_dir(path)) {
                return Err(pkg::PkgError::Conflict { path: path.clone(), owner: "(a file you created)".to_string() }.to_string());
            }
        }

        // Remember what every touched path holds now, so a failure puts the system back exactly as it was.
        let touched: Vec<&String> = plan.files.iter().map(|(p, _)| p).chain(old_pkg.files.iter()).collect();
        let backups: Vec<(String, Option<Vec<u8>>)> = touched.iter().map(|p| ((*p).clone(), fs.read(p).map(<[u8]>::to_vec))).collect();
        let undo = |fs: &mut RamFs| {
            for (path, old) in &backups {
                match old {
                    Some(data) => {
                        let _ = write_file(fs, path, data);
                    }
                    None => {
                        fs.remove(path);
                    }
                }
            }
        };

        let mut written: Vec<String> = Vec::new();
        let mut failure = None;
        for (path, content) in &plan.files {
            match write_file(fs, path, content) {
                Ok(()) => written.push(path.clone()),
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }
        if let Some(err) = failure {
            undo(fs);
            return Err(format!("{}: {err}; nothing was changed", e.name));
        }
        for dir in &plan.dirs {
            if !fs.is_dir(dir) {
                let _ = fs.mkdir(dir);
            }
        }
        // Only now, with the new files safely in place, drop files the new version no longer has.
        for old_file in &old_pkg.files {
            if !plan.files.iter().any(|(p, _)| p == old_file) {
                fs.remove(old_file);
            }
        }

        status[old_pos] = Installed { name: e.name.clone(), version: e.version.clone(), depends: e.depends.clone(), files: written, auto: old_pkg.auto };
        if let Err(err) = save_status(fs, &status) {
            status[old_pos] = old_pkg;
            undo(fs);
            return Err(format!("{}: {err}; nothing was changed", e.name));
        }
        persist(&e.filename, &data);
        say(&format!("Reinstalled {} {} ({} file(s))", e.name, e.version, plan.files.len()));
    }
    persist("installed", status.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join("\n").as_bytes());
    Ok(())
}

/// Upgrades installed packages that have newer versions in the repository index.
/// When `full` is true, also resolves new dependencies and performs autoremove on obsolete packages.
pub fn upgrade(
    fs: &mut RamFs,
    f: &mut dyn Fetcher,
    keys: &[[u8; 32]],
    full: bool,
    say: &mut dyn FnMut(&str),
    persist: &mut dyn FnMut(&str, &[u8]),
) -> Result<usize, String> {
    let index = load_index(fs, keys)?;
    let status = load_status(fs);
    let upgradable: Vec<String> = status
        .iter()
        .filter_map(|p| {
            index.iter().find(|e| e.name == p.name && e.version != p.version).map(|e| e.name.clone())
        })
        .collect();

    if upgradable.is_empty() {
        say("All packages are up to date.");
        return Ok(0);
    }

    say(&format!("The following package(s) will be upgraded: {}", upgradable.join(", ")));
    let refs: Vec<&str> = upgradable.iter().map(String::as_str).collect();

    let mut missing_deps: Vec<&str> = Vec::new();
    for name in &refs {
        if let Some(entry) = index.iter().find(|e| e.name == *name) {
            for dep in &entry.depends {
                if !status.iter().any(|p| p.name == *dep) && !missing_deps.contains(&dep.as_str()) {
                    missing_deps.push(dep.as_str());
                }
            }
        }
    }
    if !missing_deps.is_empty() {
        install(fs, f, keys, &missing_deps, say, persist)?;
    }

    reinstall(fs, f, keys, &refs, say, persist)?;

    if full {
        let removed = autoremove(fs, say)?;
        if removed > 0 {
            say(&format!("full-upgrade removed {} obsolete package(s).", removed));
        }
    }

    Ok(upgradable.len())
}

/// Removes packages; refuses when another installed package still needs one of them.
pub fn remove(fs: &mut RamFs, names: &[&str], say: &mut dyn FnMut(&str)) -> Result<(), String> {
    let mut status = load_status(fs);
    for n in names {
        if !status.iter().any(|p| p.name == *n) {
            return Err(format!("'{n}' is not installed"));
        }
        let needed_by: Vec<&str> = pkg::dependents(&status, n).into_iter().filter(|d| !names.contains(d)).collect();
        if !needed_by.is_empty() {
            return Err(format!("cannot remove '{n}': needed by {}", needed_by.join(", ")));
        }
    }
    for n in names {
        let pos = status.iter().position(|p| p.name == *n).expect("checked above");
        let p = status.remove(pos);
        for file in &p.files {
            fs.remove(file);
        }
        say(&format!("Removed {} {}", p.name, p.version));
    }
    save_status(fs, &status)
}

/// Removes automatically installed packages nothing needs any more.
pub fn autoremove(fs: &mut RamFs, say: &mut dyn FnMut(&str)) -> Result<usize, String> {
    let mut total = 0;
    loop {
        let orphans = pkg::orphans(&load_status(fs));
        if orphans.is_empty() {
            break;
        }
        let refs: Vec<&str> = orphans.iter().map(String::as_str).collect();
        remove(fs, &refs, say)?;
        total += orphans.len();
    }
    if total == 0 {
        say("Nothing to remove.");
    }
    Ok(total)
}

pub fn list(fs: &RamFs, keys: &[[u8; 32]], filter: Option<&str>, installed_only: bool, say: &mut dyn FnMut(&str)) -> Result<(), String> {
    let status = load_status(fs);
    let mut shown = 0;
    if installed_only {
        for p in status.iter().filter(|p| filter.is_none_or(|f| p.name.contains(f))) {
            say(&format!("{}/{} [installed{}]", p.name, p.version, if p.auto { ",automatic" } else { "" }));
            shown += 1;
        }
    } else {
        let index = load_index(fs, keys)?;
        for e in index.iter().filter(|e| filter.is_none_or(|f| e.name.contains(f) || e.description.contains(f))) {
            let mark = if status.iter().any(|p| p.name == e.name) { " [installed]" } else { "" };
            say(&format!("{}/{}{}  {}", e.name, e.version, mark, e.description));
            shown += 1;
        }
    }
    if shown == 0 {
        say("(none)");
    }
    Ok(())
}

pub fn show(fs: &RamFs, keys: &[[u8; 32]], name: &str, say: &mut dyn FnMut(&str)) -> Result<(), String> {
    let index = load_index(fs, keys)?;
    let e = index.iter().find(|e| e.name == name).ok_or_else(|| pkg::PkgError::NotFound(name.to_string()).to_string())?;
    let installed = load_status(fs).iter().any(|p| p.name == name);
    say(&format!("Package: {}\nVersion: {}\nDepends: {}\nSize: {} B\nSHA256: {}\nFilename: {}\nInstalled: {}\nDescription: {}", e.name, e.version, e.depends.join(", "), e.size, pkg::hex(&e.sha256), e.filename, if installed { "yes" } else { "no" }, e.description));
    Ok(())
}

/// Files a package put on the system.
pub fn files_of(fs: &RamFs, name: &str) -> Option<Vec<String>> {
    load_status(fs).into_iter().find(|p| p.name == name).map(|p| p.files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    /// Public half of the fixture repository's (non-secret) test key, from `aeon_repo.py pubkey --test-key`.
    fn real_test_key() -> [u8; 32] {
        let mut k = [0u8; 32];
        let hex = "340f45c1d0fd1cf50d832e9f9a34d6e4aa884cf0ff0139623aa33442d0c38a50";
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        k
    }

    /// A repository on "the server": names to bytes, with a counter of requests.
    struct Fake {
        files: BTreeMap<String, Vec<u8>>,
        requests: Vec<String>,
    }

    impl Fake {
        fn real() -> Fake {
            let mut files = BTreeMap::new();
            files.insert("index".to_string(), include_bytes!("../tests/fixtures/repo/index").to_vec());
            files.insert("index.sig".to_string(), include_bytes!("../tests/fixtures/repo/index.sig").to_vec());
            files.insert("pool/greet-data_2.1.tar".to_string(), include_bytes!("../tests/fixtures/repo/pool/greet-data_2.1.tar").to_vec());
            files.insert("pool/greeter_1.0.tar".to_string(), include_bytes!("../tests/fixtures/repo/pool/greeter_1.0.tar").to_vec());
            files.insert("pool/aeon-cheatsheet_1.0.tar".to_string(), include_bytes!("../tests/fixtures/repo/pool/aeon-cheatsheet_1.0.tar").to_vec());
            Fake { files, requests: Vec::new() }
        }
    }

    impl Fetcher for Fake {
        fn get(&mut self, rel: &str, _max: usize) -> Result<Vec<u8>, String> {
            self.requests.push(rel.to_string());
            self.files.get(rel).cloned().ok_or_else(|| format!("{rel}: HTTP 404"))
        }
    }

    fn new_fs() -> RamFs {
        RamFs::new(64, 1 << 20)
    }

    fn run<T>(f: impl FnOnce(&mut dyn FnMut(&str), &mut dyn FnMut(&str, &[u8])) -> T) -> (T, Vec<String>, Vec<String>) {
        let (mut said, mut saved) = (Vec::new(), Vec::new());
        let r = f(&mut |s| said.push(s.to_string()), &mut |n, _| saved.push(n.to_string()));
        (r, said, saved)
    }

    fn updated() -> (RamFs, Fake) {
        let (mut fs, mut f) = (new_fs(), Fake::real());
        run(|say, persist| update(&mut fs, &mut f, &[real_test_key()], 0, say, persist)).0.unwrap();
        (fs, f)
    }

    #[test]
    fn update_verifies_and_stores_the_signed_index() {
        let (fs, _) = updated();
        assert_eq!(load_index(&fs, &[real_test_key()]).unwrap().len(), 3);
        // The stored index is only usable with the right key.
        assert!(load_index(&fs, &[[7; 32]]).is_err());
    }

    #[test]
    fn an_older_package_list_is_refused_but_the_same_or_newer_is_fine() {
        // The fixture repository's index has serial 1.
        let (mut fs, mut f) = updated();
        assert_eq!(stored_serial(&fs), 1);
        let (r, _, _) = run(|say, persist| update(&mut fs, &mut f, &[real_test_key()], 0, say, persist));
        assert!(r.is_ok(), "the same list again is fine");
        // The stick remembers a newer serial: the repository's list is now a rollback.
        let (mut fresh, mut g) = (new_fs(), Fake::real());
        let (r, _, saved) = run(|say, persist| update(&mut fresh, &mut g, &[real_test_key()], 5, say, persist));
        assert!(r.unwrap_err().contains("OLDER package list"));
        assert!(fresh.read(INDEX_PATH).is_none() && saved.is_empty(), "a refused list stores nothing");
        // RAM remembers too.
        write_file(&mut fresh, SERIAL_PATH, b"9\n").unwrap();
        assert!(run(|say, persist| update(&mut fresh, &mut g, &[real_test_key()], 0, say, persist)).0.is_err());
    }

    #[test]
    fn a_tampered_or_unsigned_index_is_refused_and_changes_nothing() {
        let (mut fs, mut f) = (new_fs(), Fake::real());
        f.files.get_mut("index").unwrap()[40] ^= 1;
        let (r, _, saved) = run(|say, persist| update(&mut fs, &mut f, &[real_test_key()], 0, say, persist));
        assert!(r.unwrap_err().contains("signature"));
        assert!(fs.read(INDEX_PATH).is_none() && saved.is_empty());
        // A good index already stored survives a later bad update.
        let (mut fs, mut good) = updated();
        let mut bad = Fake::real();
        bad.files.get_mut("index.sig").unwrap()[3] = b'0';
        bad.files.get_mut("index.sig").unwrap()[4] = b'0';
        assert!(run(|say, persist| update(&mut fs, &mut bad, &[real_test_key()], 0, say, persist)).0.is_err());
        assert_eq!(load_index(&fs, &[real_test_key()]).unwrap().len(), 3);
        let _ = &mut good;
        // Signed by someone else (the real repository key is not the test key).
        let (mut fs2, mut f2) = (new_fs(), Fake::real());
        assert!(run(|say, persist| update(&mut fs2, &mut f2, &[[9; 32]], 0, say, persist)).0.is_err());
    }

    #[test]
    fn install_resolves_dependencies_checks_hashes_and_writes_files() {
        let (mut fs, mut f) = updated();
        let ((), said, saved) = {
            let (r, s, p) = run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist));
            r.unwrap();
            ((), s, p)
        };
        assert!(said[0].contains("greet-data 2.1, greeter 1.0"));
        assert!(fs.read("/usr/share/greet/en.txt").unwrap().starts_with(b"Hello from the greet-data"));
        assert!(fs.read("/usr/share/greet/README").unwrap().starts_with(b"greeter 1.0"));
        let st = load_status(&fs);
        assert_eq!(st.len(), 2);
        assert!(st.iter().find(|p| p.name == "greet-data").unwrap().auto);
        assert!(!st.iter().find(|p| p.name == "greeter").unwrap().auto);
        assert!(saved.contains(&"pool/greeter_1.0.tar".to_string()) && saved.contains(&"installed".to_string()));
        // Installing again does nothing and fetches nothing.
        let n = f.requests.len();
        let (r, said, _) = run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist));
        r.unwrap();
        assert_eq!(f.requests.len(), n);
        assert!(said[0].contains("already installed"));
    }

    #[test]
    fn a_swapped_or_corrupted_package_is_never_unpacked() {
        let (mut fs, mut f) = updated();
        // Server serves another package's archive under the greeter name.
        let other = f.files["pool/aeon-cheatsheet_1.0.tar"].clone();
        f.files.insert("pool/greeter_1.0.tar".into(), other);
        let (r, _, _) = run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist));
        let err = r.unwrap_err();
        assert!(err.contains("size") || err.contains("SHA-256"), "{err}");
        assert!(fs.read("/usr/share/greet/README").is_none());
        // One flipped bit.
        let (mut fs, mut f) = updated();
        f.files.get_mut("pool/greet-data_2.1.tar").unwrap()[700] ^= 1;
        let (r, _, _) = run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist));
        assert!(r.is_err());
        assert!(load_status(&fs).is_empty(), "no partial install is recorded");
        // A missing file on the server.
        let (mut fs, mut f) = updated();
        f.files.remove("pool/greet-data_2.1.tar");
        assert!(run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist)).0.is_err());
    }

    #[test]
    fn conflicts_with_existing_files_are_refused() {
        let (mut fs, mut f) = updated();
        fs.mkdir("/usr").unwrap();
        fs.mkdir("/usr/share").unwrap();
        fs.mkdir("/usr/share/greet").unwrap();
        fs.write("/usr/share/greet/en.txt", b"mine").unwrap();
        let (r, _, _) = run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greet-data"], say, persist));
        assert!(r.unwrap_err().contains("already belongs"));
        assert_eq!(fs.read("/usr/share/greet/en.txt").unwrap(), b"mine");
    }

    #[test]
    fn unknown_packages_and_missing_index_are_explained() {
        let (mut fs, mut f) = updated();
        let (r, _, _) = run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["nope"], say, persist));
        assert!(r.unwrap_err().contains("no package called 'nope'"));
        let mut empty = new_fs();
        let (r, _, _) = run(|say, persist| install(&mut empty, &mut Fake::real(), &[real_test_key()], &["greeter"], say, persist));
        assert!(r.unwrap_err().contains("apt update"));
    }

    #[test]
    fn remove_respects_dependents_and_autoremove_cleans_up() {
        let (mut fs, mut f) = updated();
        run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist)).0.unwrap();
        let (r, _, _) = run(|say, _| remove(&mut fs, &["greet-data"], say));
        assert!(r.unwrap_err().contains("needed by greeter"));
        assert!(fs.read("/usr/share/greet/en.txt").is_some());
        let (r, _, _) = run(|say, _| remove(&mut fs, &["greeter"], say));
        r.unwrap();
        assert!(fs.read("/usr/share/greet/README").is_none());
        assert!(fs.read("/usr/share/greet/en.txt").is_some(), "the dependency stays until autoremove");
        let (n, _, _) = run(|say, _| autoremove(&mut fs, say));
        assert_eq!(n.unwrap(), 1);
        assert!(fs.read("/usr/share/greet/en.txt").is_none());
        assert!(load_status(&fs).is_empty());
        assert!(run(|say, _| remove(&mut fs, &["greeter"], say)).0.is_err());
    }

    #[test]
    fn reinstall_never_overwrites_a_file_the_package_did_not_own() {
        let (mut fs, mut f) = updated();
        run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist)).0.unwrap();
        // The record says greeter does not own README (as if the new version were adding it) while a file is there.
        let mut st = load_status(&fs);
        st.iter_mut().find(|p| p.name == "greeter").unwrap().files.retain(|p| p != "/usr/share/greet/README");
        save_status(&mut fs, &st).unwrap();
        fs.write("/usr/share/greet/README", b"my own notes").unwrap();
        let (r, _, _) = run(|say, persist| reinstall(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist));
        assert!(r.is_err());
        assert_eq!(fs.read("/usr/share/greet/README").unwrap(), b"my own notes");
        assert_eq!(load_status(&fs), st, "a refused reinstall changes nothing");
    }

    #[test]
    fn reinstall_drops_files_the_new_version_lacks_only_after_writing_the_new_ones() {
        let (mut fs, mut f) = updated();
        run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist)).0.unwrap();
        let mut st = load_status(&fs);
        st.iter_mut().find(|p| p.name == "greeter").unwrap().files.push("/usr/share/greet/old.txt".to_string());
        save_status(&mut fs, &st).unwrap();
        fs.write("/usr/share/greet/old.txt", b"from an older version").unwrap();
        run(|say, persist| reinstall(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist)).0.unwrap();
        assert!(fs.read("/usr/share/greet/old.txt").is_none());
        assert!(fs.read("/usr/share/greet/README").unwrap().starts_with(b"greeter 1.0"));
        let after = load_status(&fs);
        let owned = &after.iter().find(|p| p.name == "greeter").unwrap().files;
        assert!(!owned.iter().any(|p| p.ends_with("old.txt")) && owned.iter().any(|p| p.ends_with("README")));
    }

    #[test]
    fn a_full_filesystem_rolls_the_package_back() {
        let (mut fs_big, mut f) = updated();
        
        let mut tiny = RamFs::new(64, 900); // room for the index and one small file, not for the status record
        for p in [INDEX_PATH, SIG_PATH] {
            let d = fs_big.read(p).unwrap().to_vec();
            write_file(&mut tiny, p, &d).unwrap();
        }
        let (r, _, _) = run(|say, persist| install(&mut tiny, &mut f, &[real_test_key()], &["greeter"], say, persist));
        assert!(r.is_err());
        assert!(tiny.read("/usr/share/greet/en.txt").is_none(), "partially written files are removed");
        assert!(load_status(&tiny).is_empty());
        let _ = &mut fs_big;
    }

    #[test]
    fn list_search_show() {
        let (mut fs, mut f) = updated();
        run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["aeon-cheatsheet"], say, persist)).0.unwrap();
        let (r, said, _) = run(|say, _| list(&fs, &[real_test_key()], None, false, say));
        r.unwrap();
        assert_eq!(said.len(), 3);
        assert!(said.iter().any(|l| l.starts_with("aeon-cheatsheet/1.0 [installed]")));
        let (_, said, _) = run(|say, _| list(&fs, &[real_test_key()], Some("greet"), false, say));
        assert_eq!(said.len(), 2);
        let (_, said, _) = run(|say, _| list(&fs, &[real_test_key()], None, true, say));
        assert_eq!(said, ["aeon-cheatsheet/1.0 [installed]"]);
        let (r, said, _) = run(|say, _| show(&fs, &[real_test_key()], "greeter", say));
        r.unwrap();
        assert!(said[0].contains("Depends: greet-data") && said[0].contains("Installed: no"));
        assert!(run(|say, _| show(&fs, &[real_test_key()], "zzz", say)).0.is_err());
        assert_eq!(files_of(&fs, "aeon-cheatsheet").unwrap(), ["/usr/share/doc/aeon/cheatsheet.txt"]);
    }

    #[test]
    fn source_urls() {
        let mut fs = new_fs();
        assert_eq!(set_source(&mut fs, "http://10.0.2.2:8080").unwrap(), "http://10.0.2.2:8080/");
        assert_eq!(get_source(&fs).unwrap(), "http://10.0.2.2:8080/");
        assert!(set_source(&mut fs, "https://example.org/repo/").unwrap_err().contains("TLS"));
        assert!(set_source(&mut fs, "ftp://x").is_err());
        assert!(set_source(&mut fs, "garbage").is_err());
        assert_eq!(get_source(&fs).unwrap(), "http://10.0.2.2:8080/", "a refused address changes nothing");
    }

    #[test]
    fn reinstall_refuses_uninstalled_and_replaces_installed() {
        let (mut fs, mut f) = updated();
        let (r, _, _) = run(|say, persist| reinstall(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist));
        assert!(r.unwrap_err().contains("not installed"));

        run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist)).0.unwrap();
        assert!(fs.read("/usr/share/greet/README").is_some());

        let (r, said, _) = run(|say, persist| reinstall(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist));
        r.unwrap();
        assert!(said.iter().any(|s| s.contains("Reinstalled greeter 1.0")));
        assert!(fs.read("/usr/share/greet/README").is_some());
        let st = load_status(&fs);
        assert!(!st.iter().find(|p| p.name == "greeter").unwrap().auto);
    }

    #[test]
    fn upgrade_when_up_to_date_says_up_to_date() {
        let (mut fs, mut f) = updated();
        run(|say, persist| install(&mut fs, &mut f, &[real_test_key()], &["greeter"], say, persist)).0.unwrap();
        let (r, said, _) = run(|say, persist| upgrade(&mut fs, &mut f, &[real_test_key()], false, say, persist));
        assert_eq!(r.unwrap(), 0);
        assert!(said.iter().any(|s| s.contains("up to date")));
    }
}
