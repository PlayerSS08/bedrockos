#![no_std]
#![no_main]
use aeonrt::{close, entry, eprint, open, read, write_all, O_RDONLY};

fn copy(fd: usize) -> bool {
    let mut buf = [0u8; 2048];
    loop {
        let n = read(fd, &mut buf);
        if n < 0 {
            return false;
        }
        if n == 0 {
            return true;
        }
        if !write_all(1, &buf[..n as usize]) {
            return false;
        }
    }
}

fn main(args: aeonrt::Args) -> i32 {
    if args.len() < 2 {
        return if copy(0) { 0 } else { 1 };
    }
    let mut status = 0;
    for i in 1..args.len() {
        let name = args.get(i).unwrap_or(b"");
        let mut path = [0u8; 129];
        if name.len() > 128 {
            eprint("cat: name too long\n");
            status = 1;
            continue;
        }
        path[..name.len()].copy_from_slice(name);
        let fd = open(&path[..name.len() + 1], O_RDONLY);
        if fd < 0 {
            eprint("cat: cannot open a file\n");
            status = 1;
            continue;
        }
        if !copy(fd as usize) {
            eprint("cat: read error\n");
            status = 1;
        }
        close(fd as usize);
    }
    status
}
entry!(main);
