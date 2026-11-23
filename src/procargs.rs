//! The stack a new program starts with (System V x86-64 layout). Pure, host-tested.
//!
//! From the stack pointer upwards: `argc`, the `argv` pointers, a null, the (empty) environment's null,
//! an `AT_NULL` auxiliary entry, then the argument strings. The stack pointer is 16-byte aligned, as the
//! ABI requires at process entry.

use alloc::vec::Vec;

/// Most arguments a program can be started with.
pub const MAX_ARGS: usize = 32;
/// Most bytes the whole start block may take (strings, pointers and padding).
pub const MAX_BYTES: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub struct StartStack {
    /// Stack pointer at entry; `bytes` is to be written at exactly this address.
    pub rsp: u64,
    pub bytes: Vec<u8>,
}

/// Lays out `args` so that the block ends at `top` (the first address above the stack). `None` when
/// there are too many arguments, they are too long, one contains a NUL, or `top` is not 16-aligned.
pub fn build(args: &[&[u8]], top: u64) -> Option<StartStack> {
    if args.len() > MAX_ARGS || top % 16 != 0 || args.iter().any(|a| a.contains(&0)) {
        return None;
    }
    let strings: usize = args.iter().map(|a| a.len() + 1).sum();
    // argc + argv[n] + null + envp null + auxv (two words).
    let words = 1 + args.len() + 1 + 1 + 2;
    let total = (words * 8 + strings).next_multiple_of(16);
    if total > MAX_BYTES || (total as u64) > top {
        return None;
    }
    let rsp = top - total as u64;
    let mut bytes = alloc::vec![0u8; total];
    let mut str_at = words * 8;
    bytes[0..8].copy_from_slice(&(args.len() as u64).to_le_bytes());
    for (i, a) in args.iter().enumerate() {
        let ptr = rsp + str_at as u64;
        bytes[8 + i * 8..16 + i * 8].copy_from_slice(&ptr.to_le_bytes());
        bytes[str_at..str_at + a.len()].copy_from_slice(a);
        str_at += a.len() + 1;
    }
    Some(StartStack { rsp, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(b: &[u8], i: usize) -> u64 {
        u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap())
    }

    #[test]
    fn layout_matches_the_abi() {
        let top = 0x7000_0000u64;
        let s = build(&[b"cat", b"/etc/hostname"], top).unwrap();
        assert_eq!(s.rsp % 16, 0);
        assert_eq!(s.rsp + s.bytes.len() as u64, top);
        assert_eq!(word(&s.bytes, 0), 2);
        let a0 = (word(&s.bytes, 1) - s.rsp) as usize;
        let a1 = (word(&s.bytes, 2) - s.rsp) as usize;
        assert_eq!(&s.bytes[a0..a0 + 4], b"cat\0");
        assert_eq!(&s.bytes[a1..a1 + 14], b"/etc/hostname\0");
        // argv null, envp null, AT_NULL pair.
        assert_eq!([word(&s.bytes, 3), word(&s.bytes, 4), word(&s.bytes, 5), word(&s.bytes, 6)], [0; 4]);
    }

    #[test]
    fn no_arguments_is_valid() {
        let s = build(&[], 0x1000).unwrap();
        assert_eq!(word(&s.bytes, 0), 0);
        assert_eq!(s.bytes.len(), 48);
        assert_eq!(s.rsp, 0x1000 - 48);
    }

    #[test]
    fn refuses_what_cannot_be_laid_out() {
        let many: Vec<&[u8]> = (0..MAX_ARGS + 1).map(|_| &b"x"[..]).collect();
        assert_eq!(build(&many, 0x10_0000), None);
        let long = vec![b'a'; MAX_BYTES];
        assert_eq!(build(&[&long], 0x10_0000), None);
        assert_eq!(build(&[b"a\0b"], 0x10_0000), None);
        assert_eq!(build(&[b"a"], 0x10_0008), None);
        assert_eq!(build(&[b"a"], 16), None);
    }

    #[test]
    fn the_largest_allowed_block_fits() {
        let args: Vec<&[u8]> = (0..MAX_ARGS).map(|_| &b"0123456789"[..]).collect();
        let s = build(&args, 0x10_0000).unwrap();
        assert!(s.bytes.len() <= MAX_BYTES);
        assert_eq!(word(&s.bytes, 0), MAX_ARGS as u64);
        assert_eq!(word(&s.bytes, 1 + MAX_ARGS), 0);
    }
}
