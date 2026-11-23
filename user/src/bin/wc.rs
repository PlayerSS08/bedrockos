#![no_std]
#![no_main]
use aeonrt::{close, entry, eprint, open, print, print_num, read, O_RDONLY};

fn count(fd: usize) -> Option<(u64, u64, u64)> {
    let mut buf = [0u8; 2048];
    let (mut lines, mut words, mut bytes) = (0u64, 0u64, 0u64);
    let mut in_word = false;
    loop {
        let n = read(fd, &mut buf);
        if n < 0 {
            return None;
        }
        if n == 0 {
            return Some((lines, words, bytes));
        }
        for &b in &buf[..n as usize] {
            bytes += 1;
            if b == b'\n' {
                lines += 1;
            }
            let space = matches!(b, b' ' | b'\n' | b'\t' | b'\r');
            if space {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }
    }
}

fn show(c: (u64, u64, u64)) {
    print_num(c.0);
    print(" ");
    print_num(c.1);
    print(" ");
    print_num(c.2);
}

fn main(args: aeonrt::Args) -> i32 {
    if args.len() < 2 {
        return match count(0) {
            Some(c) => {
                show(c);
                print("\n");
                0
            }
            None => 1,
        };
    }
    let mut status = 0;
    for i in 1..args.len() {
        let name = args.get(i).unwrap_or(b"");
        let mut path = [0u8; 129];
        if name.len() > 128 {
            status = 1;
            continue;
        }
        path[..name.len()].copy_from_slice(name);
        let fd = open(&path[..name.len() + 1], O_RDONLY);
        if fd < 0 {
            eprint("wc: cannot open a file\n");
            status = 1;
            continue;
        }
        match count(fd as usize) {
            Some(c) => {
                show(c);
                print(" ");
                print(core::str::from_utf8(name).unwrap_or("?"));
                print("\n");
            }
            None => status = 1,
        }
        close(fd as usize);
    }
    status
}
entry!(main);
