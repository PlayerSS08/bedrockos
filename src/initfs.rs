//! The programs that ship inside the kernel image (`user/initfs.bin`, made by `tools/build-user.sh`), packed
//! with a zero-run encoding, and the code that unpacks them. Pure and host-tested; the shell installs the
//! result into `/bin` at start-up.

use alloc::string::String;
use alloc::vec::Vec;

/// The embedded archive.
pub static ARCHIVE: &[u8] = include_bytes!("../user/initfs.bin");

/// Decodes zero-run packing; `expect` is the length the result must have.
pub fn rle_decode(src: &[u8], expect: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(expect);
    let mut i = 0;
    while i < src.len() {
        if src[i] == 0 {
            let n = u16::from_le_bytes([*src.get(i + 1)?, *src.get(i + 2)?]) as usize;
            if n == 0 || out.len() + n > expect {
                return None;
            }
            out.resize(out.len() + n, 0);
            i += 3;
        } else {
            if out.len() >= expect {
                return None;
            }
            out.push(src[i]);
            i += 1;
        }
    }
    (out.len() == expect).then_some(out)
}

/// Splits the archive into `(name, bytes)`. `None` if it is damaged in any way.
pub fn unpack(blob: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    if blob.get(0..4)? != b"AINI" {
        return None;
    }
    let count = u16::from_le_bytes([*blob.get(4)?, *blob.get(5)?]) as usize;
    let mut at = 6;
    let mut out = Vec::new();
    for _ in 0..count {
        let nlen = *blob.get(at)? as usize;
        let name = core::str::from_utf8(blob.get(at + 1..at + 1 + nlen)?).ok()?;
        at += 1 + nlen;
        let len = u32::from_le_bytes(blob.get(at..at + 4)?.try_into().ok()?) as usize;
        let plen = u32::from_le_bytes(blob.get(at + 4..at + 8)?.try_into().ok()?) as usize;
        at += 8;
        if len > 1 << 20 || !crate::ramfs::name_ok(name) {
            return None;
        }
        let data = rle_decode(blob.get(at..at.checked_add(plen)?)?, len)?;
        at += plen;
        out.push((String::from(name), data));
    }
    (at == blob.len()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vmspace::USER_BASE;

    #[test]
    fn rle_roundtrips_and_refuses_damage() {
        let data: Vec<u8> = [&[1u8, 2, 0, 0, 0, 3][..], &[0u8; 70_000][..], &[9][..]].concat();
        let mut packed = Vec::new();
        let mut i = 0;
        while i < data.len() {
            if data[i] == 0 {
                let mut j = i;
                while j < data.len() && data[j] == 0 && j - i < 0xFFFF {
                    j += 1;
                }
                packed.push(0);
                packed.extend_from_slice(&((j - i) as u16).to_le_bytes());
                i = j;
            } else {
                packed.push(data[i]);
                i += 1;
            }
        }
        assert_eq!(rle_decode(&packed, data.len()).as_deref(), Some(&data[..]));
        assert_eq!(rle_decode(&packed, data.len() - 1), None);
        assert_eq!(rle_decode(&packed, data.len() + 1), None);
        assert_eq!(rle_decode(&[0, 5], 5), None);
        assert_eq!(rle_decode(&[0, 0, 0], 0), None);
    }

    #[test]
    fn the_shipped_archive_unpacks_and_every_program_is_loadable() {
        let progs = unpack(ARCHIVE).expect("archive is intact");
        let names: Vec<&str> = progs.iter().map(|(n, _)| n.as_str()).collect();
        for want in ["cat", "echo", "forktest", "hello", "ls", "wc"] {
            assert!(names.contains(&want), "{want} missing from {names:?}");
        }
        for (name, data) in &progs {
            // BedrockOS's own loader checks must accept what the build produced.
            let img = crate::elf::parse(data, USER_BASE, crate::loader::HEAP_BASE, crate::loader::MAX_IMAGE).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(img.entry >= USER_BASE);
            assert!(img.iter().all(|s| !(s.writable() && s.executable())), "{name}");
        }
    }

    #[test]
    fn damaged_archives_are_refused() {
        assert!(unpack(b"").is_none());
        assert!(unpack(b"XXXX\0\0").is_none());
        let mut bad = ARCHIVE.to_vec();
        bad.pop();
        assert!(unpack(&bad).is_none());
        let mut extra = ARCHIVE.to_vec();
        extra.push(0);
        assert!(unpack(&extra).is_none());
        let mut flipped = ARCHIVE.to_vec();
        flipped[0] = b'B';
        assert!(unpack(&flipped).is_none());
    }
}
