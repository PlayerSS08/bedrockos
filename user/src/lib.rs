//! Runtime for Aeon programs: system calls, program start-up and a few helpers. No heap, no std.
#![no_std]

use core::arch::{asm, global_asm};

pub const O_RDONLY: usize = 0;
pub const O_WRONLY: usize = 1;
pub const O_CREAT: usize = 0x40;
pub const O_TRUNC: usize = 0x200;
pub const O_DIRECTORY: usize = 0x1_0000;
pub const DT_DIR: u8 = 4;

const SYS_READ: usize = 0;
const SYS_WRITE: usize = 1;
const SYS_OPEN: usize = 2;
const SYS_CLOSE: usize = 3;
const SYS_GETPID: usize = 39;
const SYS_FORK: usize = 57;
const SYS_EXIT: usize = 60;
const SYS_WAIT4: usize = 61;
const SYS_GETDENTS64: usize = 217;

/// A raw system call. Returns the kernel's value: a negative number is `-errno`.
///
/// # Safety
/// The arguments must be valid for the call number (pointers to memory the call may use).
#[inline]
pub unsafe fn syscall(n: usize, a: usize, b: usize, c: usize) -> isize {
    let ret: isize;
    // SAFETY: the Linux-style syscall ABI; only rax, rcx and r11 change.
    unsafe { asm!("syscall", inlateout("rax") n as isize => ret, in("rdi") a, in("rsi") b, in("rdx") c, lateout("rcx") _, lateout("r11") _, options(nostack)) };
    ret
}

pub fn exit(code: i32) -> ! {
    // SAFETY: exit takes only a code.
    unsafe { syscall(SYS_EXIT, code as usize, 0, 0) };
    loop {}
}

pub fn write(fd: usize, buf: &[u8]) -> isize {
    // SAFETY: `buf` is valid for its length.
    unsafe { syscall(SYS_WRITE, fd, buf.as_ptr() as usize, buf.len()) }
}

/// Writes everything, retrying after partial writes. False on an error.
pub fn write_all(fd: usize, mut buf: &[u8]) -> bool {
    while !buf.is_empty() {
        let n = write(fd, buf);
        if n <= 0 {
            return false;
        }
        buf = &buf[n as usize..];
    }
    true
}

pub fn print(s: &str) {
    write_all(1, s.as_bytes());
}

pub fn eprint(s: &str) {
    write_all(2, s.as_bytes());
}

pub fn read(fd: usize, buf: &mut [u8]) -> isize {
    // SAFETY: `buf` is valid for writes of its length.
    unsafe { syscall(SYS_READ, fd, buf.as_mut_ptr() as usize, buf.len()) }
}

/// `path` must end with a NUL byte.
pub fn open(path: &[u8], flags: usize) -> isize {
    // SAFETY: `path` is valid and NUL-terminated by the caller's contract.
    unsafe { syscall(SYS_OPEN, path.as_ptr() as usize, flags, 0o644) }
}

pub fn close(fd: usize) -> isize {
    // SAFETY: close takes only a descriptor.
    unsafe { syscall(SYS_CLOSE, fd, 0, 0) }
}

pub fn getpid() -> isize {
    // SAFETY: no arguments.
    unsafe { syscall(SYS_GETPID, 0, 0, 0) }
}

pub fn fork() -> isize {
    // SAFETY: no arguments.
    unsafe { syscall(SYS_FORK, 0, 0, 0) }
}

/// Waits for any child; returns its pid and exit status (the low 8 bits of its exit code).
pub fn wait() -> (isize, i32) {
    let mut status = 0u32;
    // SAFETY: `status` is valid for a 4-byte write.
    let pid = unsafe { syscall(SYS_WAIT4, usize::MAX, &mut status as *mut u32 as usize, 0) };
    (pid, ((status >> 8) & 0xFF) as i32)
}

pub fn getdents(fd: usize, buf: &mut [u8]) -> isize {
    // SAFETY: `buf` is valid for writes of its length.
    unsafe { syscall(SYS_GETDENTS64, fd, buf.as_mut_ptr() as usize, buf.len()) }
}

/// Formats a number into `buf` and returns the text.
pub fn utoa(mut v: u64, buf: &mut [u8; 20]) -> &str {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

pub fn print_num(v: u64) {
    let mut b = [0u8; 20];
    print(utoa(v, &mut b));
}

/// The program's arguments, as given on its start-up stack.
pub struct Args {
    argc: usize,
    argv: *const *const u8,
}

impl Args {
    pub fn len(&self) -> usize {
        self.argc
    }
    pub fn is_empty(&self) -> bool {
        self.argc == 0
    }
    /// Argument `i` without its NUL.
    pub fn get(&self, i: usize) -> Option<&'static [u8]> {
        if i >= self.argc {
            return None;
        }
        // SAFETY: the kernel put `argc` valid NUL-terminated strings on the start-up stack.
        unsafe {
            let p = *self.argv.add(i);
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            Some(core::slice::from_raw_parts(p, n))
        }
    }
}

global_asm!(
    ".globl _start",
    "_start:",
    "mov rdi, rsp",
    "and rsp, -16",
    "call aeon_start",
    "ud2",
);

unsafe extern "C" {
    fn aeon_main(argc: usize, argv: *const *const u8) -> i32;
}

/// # Safety
/// Called only by `_start` with the start-up stack pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aeon_start(sp: *const u64) -> ! {
    // SAFETY: the kernel laid out argc, then the argv pointers, at the initial stack pointer.
    let code = unsafe { aeon_main(*sp as usize, sp.add(1) as *const *const u8) };
    exit(code)
}

/// Declares the program's entry: `entry!(main)` with `fn main(args: Args) -> i32`.
#[macro_export]
macro_rules! entry {
    ($f:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn aeon_main(argc: usize, argv: *const *const u8) -> i32 {
            let f: fn($crate::Args) -> i32 = $f;
            f(unsafe { $crate::Args::from_raw(argc, argv) })
        }
    };
}

impl Args {
    /// # Safety
    /// `argv` must point at `argc` valid NUL-terminated strings.
    pub unsafe fn from_raw(argc: usize, argv: *const *const u8) -> Self {
        Args { argc, argv }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    eprint("program panicked\n");
    exit(101)
}
