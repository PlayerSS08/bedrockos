//! System call numbers, flags and error codes. Pure, host-testable.
//!
//! The calling convention and the numbers are the x86-64 Linux ones: number in `rax`, arguments in `rdi`,
//! `rsi`, `rdx`, `r10`, `r8`, `r9`, result in `rax` (a small negative number is `-errno`), issued with the
//! `syscall` instruction. Only `rax`, `rcx` and `r11` change across a call.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Number {
    /// `read(fd, buf, count)`
    Read = 0,
    /// `write(fd, buf, count)`
    Write = 1,
    /// `open(path, flags, mode)`
    Open = 2,
    /// `close(fd)`
    Close = 3,
    /// `lseek(fd, offset, whence)`
    Lseek = 8,
    /// `mmap(addr, len, prot, flags, fd, offset)`: anonymous private memory only
    Mmap = 9,
    /// `munmap(addr, len)`
    Munmap = 11,
    /// `brk(addr)`
    Brk = 12,
    /// `pipe(fds[2])`
    Pipe = 22,
    /// `sched_yield()`
    Yield = 24,
    /// `dup2(old, new)`
    Dup2 = 33,
    /// `nanosleep(req, rem)`
    Nanosleep = 35,
    /// `getpid()`
    GetPid = 39,
    /// `fork()`
    Fork = 57,
    /// `execve(path, argv, envp)`
    Execve = 59,
    /// `exit(code)`
    Exit = 60,
    /// `wait4(pid, status, options, rusage)`
    Wait4 = 61,
    /// `kill(pid, sig)`: every signal ends the target
    Kill = 62,
    /// `uname(buf)`
    Uname = 63,
    /// `getcwd(buf, size)`
    GetCwd = 79,
    /// `chdir(path)`
    Chdir = 80,
    /// `mkdir(path, mode)`
    Mkdir = 83,
    /// `unlink(path)`
    Unlink = 87,
    /// `getppid()`
    GetPpid = 110,
    /// `getdents64(fd, buf, count)`
    GetDents64 = 217,
    /// `clock_gettime(clk_id, tp)`: 0 = clock chip time, 1 = time since boot
    ClockGetTime = 228,
    /// `exit_group(code)`: the same as `exit` (one thread per process)
    ExitGroup = 231,
}

/// Every call, in the order of their allow-mask bits.
pub const ALL: [Number; 27] = [
    Number::Read,
    Number::Write,
    Number::Open,
    Number::Close,
    Number::Lseek,
    Number::Mmap,
    Number::Munmap,
    Number::Brk,
    Number::Pipe,
    Number::Yield,
    Number::Dup2,
    Number::Nanosleep,
    Number::GetPid,
    Number::Fork,
    Number::Execve,
    Number::Exit,
    Number::Wait4,
    Number::Kill,
    Number::Uname,
    Number::GetCwd,
    Number::Chdir,
    Number::Mkdir,
    Number::Unlink,
    Number::GetPpid,
    Number::GetDents64,
    Number::ClockGetTime,
    Number::ExitGroup,
];

/// Most bytes one `write` to the screen takes; the rest is reported as not written.
pub const MAX_WRITE: u64 = 4096;
/// Most bytes one file `read` or `write` moves.
pub const MAX_IO: u64 = 1024 * 1024;
/// What a refused call returns: `-EPERM`.
pub const ERR: u64 = u64::MAX;

pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_ACCMODE: u32 = 3;
pub const O_CREAT: u32 = 0x40;
pub const O_TRUNC: u32 = 0x200;
pub const O_APPEND: u32 = 0x400;
pub const O_DIRECTORY: u32 = 0x1_0000;

pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;
pub const MAP_PRIVATE: u64 = 2;
pub const MAP_FIXED: u64 = 0x10;
pub const MAP_ANONYMOUS: u64 = 0x20;

/// `wait4` option: return 0 at once when no child has ended yet.
pub const WNOHANG: u64 = 1;

pub const CLOCK_REALTIME: u64 = 0;
pub const CLOCK_MONOTONIC: u64 = 1;

/// `d_type` values in a `getdents64` record.
pub const DT_DIR: u8 = 4;
pub const DT_REG: u8 = 8;

pub const EPERM: u64 = 1;
pub const ENOENT: u64 = 2;
pub const ESRCH: u64 = 3;
pub const E2BIG: u64 = 7;
pub const ENOEXEC: u64 = 8;
pub const EBADF: u64 = 9;
pub const ECHILD: u64 = 10;
pub const EAGAIN: u64 = 11;
pub const ENOMEM: u64 = 12;
pub const EACCES: u64 = 13;
pub const EFAULT: u64 = 14;
pub const EEXIST: u64 = 17;
pub const ENOTDIR: u64 = 20;
pub const EISDIR: u64 = 21;
pub const EINVAL: u64 = 22;
pub const EMFILE: u64 = 24;
pub const ENOSPC: u64 = 28;
pub const ESPIPE: u64 = 29;
pub const EPIPE: u64 = 32;
pub const ERANGE: u64 = 34;
pub const ENAMETOOLONG: u64 = 36;
pub const ENOSYS: u64 = 38;
pub const ENOTEMPTY: u64 = 39;

pub const fn errno(code: u64) -> u64 {
    (0u64).wrapping_sub(code)
}

/// Makes bytes from a user program safe to print: everything except printable ASCII, newline and tab
/// becomes `?`, so a program cannot forge log lines with carriage returns or send terminal escapes.
pub fn sanitize_output(buf: &mut [u8]) {
    for b in buf {
        if !(matches!(*b, b'\n' | b'\t') || (0x20..0x7F).contains(b)) {
            *b = b'?';
        }
    }
}

impl Number {
    /// This call's bit in an allow-mask.
    pub const fn bit(self) -> u64 {
        let mut i = 0;
        while i < ALL.len() {
            if ALL[i] as u64 == self as u64 {
                return 1 << i;
            }
            i += 1;
        }
        0
    }
}

/// Calls every program may make: without `exit` a program could never end cleanly.
pub const ALWAYS: u64 = Number::Exit.bit() | Number::ExitGroup.bit();

/// What a program loaded from a file may do: every call. Files and the screen are the same for all
/// programs (BedrockOS has one user), so there is nothing to withhold from one that the shell itself has.
pub const STANDARD: u64 = (1 << ALL.len()) - 1;

/// The calls a built-in demo program is allowed to make (least privilege: `hello` may print, `denied`
/// may only exit). Unknown names get `exit` only.
pub fn allow_mask(program: &str) -> u64 {
    ALWAYS
        | match program {
            "hello" | "badcall" => Number::Write.bit(),
            "spin" => Number::Yield.bit(),
            "linux" => STANDARD,
            _ => 0,
        }
}

/// True if `mask` lets the program make call `n`.
pub const fn permits(mask: u64, n: Number) -> bool {
    mask & n.bit() != 0
}

pub fn decode(n: u64) -> Option<Number> {
    ALL.iter().copied().find(|c| *c as u64 == n)
}

/// Writes one `linux_dirent64` record for `name` into `out` and returns its length, or `None` when it
/// does not fit. Records are padded to 8 bytes; `next_off` is the position to continue from.
pub fn write_dirent(out: &mut [u8], ino: u64, next_off: u64, d_type: u8, name: &[u8]) -> Option<usize> {
    let len = (19 + name.len() + 1).next_multiple_of(8);
    if len > out.len() || len > u16::MAX as usize {
        return None;
    }
    out[..len].fill(0);
    out[0..8].copy_from_slice(&ino.to_le_bytes());
    out[8..16].copy_from_slice(&next_off.to_le_bytes());
    out[16..18].copy_from_slice(&(len as u16).to_le_bytes());
    out[18] = d_type;
    out[19..19 + name.len()].copy_from_slice(name);
    Some(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_roundtrip_and_unknown() {
        for n in ALL {
            assert_eq!(decode(n as u64), Some(n));
        }
        for n in [4, 5, 6, 7, 10, 13, 23, 25, 38, 40, 41, 42, 44, 45, 49, 50, 58, 64, 227, 229, 232, u64::MAX] {
            assert_eq!(decode(n), None, "{n}");
        }
    }

    #[test]
    fn number_bits_are_distinct_and_nonzero() {
        let mut seen = 0u64;
        for n in ALL {
            assert_ne!(n.bit(), 0, "{n:?}");
            assert_eq!(seen & n.bit(), 0, "duplicate bit for {n:?}");
            seen |= n.bit();
        }
        assert_eq!(seen, STANDARD);
    }

    #[test]
    fn allow_masks_are_least_privilege() {
        // Every program can exit, and the restricted demos can do nothing outside their list.
        for p in ["hello", "fault", "gp", "spin", "badcall", "denied", "whatever", ""] {
            assert!(permits(allow_mask(p), Number::Exit), "{p} must be able to exit");
            assert!(permits(allow_mask(p), Number::ExitGroup));
            for n in [Number::Open, Number::Fork, Number::Execve, Number::Kill, Number::Unlink, Number::GetPid, Number::ClockGetTime] {
                assert!(!permits(allow_mask(p), n), "{p} must not be allowed {n:?}");
            }
        }
        assert!(permits(allow_mask("hello"), Number::Write));
        assert!(!permits(allow_mask("hello"), Number::Yield));
        assert!(permits(allow_mask("spin"), Number::Yield));
        assert!(!permits(allow_mask("spin"), Number::Write));
        for p in ["denied", "fault", "gp", "whatever"] {
            for n in ALL {
                assert_eq!(permits(allow_mask(p), n), matches!(n, Number::Exit | Number::ExitGroup), "{p} {n:?}");
            }
        }
        for n in ALL {
            assert!(permits(allow_mask("linux"), n));
            assert!(permits(STANDARD, n));
        }
    }

    #[test]
    fn sanitize_keeps_text_and_replaces_controls() {
        let mut b = *b"ok\tline\n";
        sanitize_output(&mut b);
        assert_eq!(&b, b"ok\tline\n");
        let mut c = *b"a\rb\x1b[2Jc\x00\x7f\xffd";
        sanitize_output(&mut c);
        assert_eq!(&c, b"a?b?[2Jc???d");
    }

    #[test]
    fn errno_values() {
        assert_eq!(errno(EPERM), ERR);
        assert_eq!(errno(ENOENT) as i64, -2);
        assert_eq!(errno(ENOSYS) as i64, -38);
    }

    #[test]
    fn dirent_records() {
        let mut buf = [0xFFu8; 64];
        let n = write_dirent(&mut buf, 7, 3, DT_REG, b"hello.txt").unwrap();
        assert_eq!(n, 32);
        assert_eq!(u64::from_le_bytes(buf[0..8].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(buf[8..16].try_into().unwrap()), 3);
        assert_eq!(u16::from_le_bytes(buf[16..18].try_into().unwrap()), 32);
        assert_eq!(buf[18], DT_REG);
        assert_eq!(&buf[19..29], b"hello.txt\0");
        assert!(buf[29..32].iter().all(|b| *b == 0));
        assert_eq!(buf[32], 0xFF);
        // Exactly fitting and one byte short.
        assert_eq!(write_dirent(&mut [0u8; 24], 1, 1, DT_DIR, b"abcd"), Some(24));
        assert_eq!(write_dirent(&mut [0u8; 23], 1, 1, DT_DIR, b"abcd"), None);
    }
}
