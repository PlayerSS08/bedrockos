#![no_std]
#![no_main]
use aeonrt::{close, entry, eprint, getdents, open, print, write_all, DT_DIR, O_DIRECTORY, O_RDONLY};

fn main(args: aeonrt::Args) -> i32 {
    let name = args.get(1).unwrap_or(b".");
    let mut path = [0u8; 129];
    if name.len() > 128 {
        eprint("ls: name too long\n");
        return 1;
    }
    path[..name.len()].copy_from_slice(name);
    let fd = open(&path[..name.len() + 1], O_RDONLY | O_DIRECTORY);
    if fd < 0 {
        eprint("ls: not a folder\n");
        return 1;
    }
    let mut buf = [0u8; 1024];
    loop {
        let n = getdents(fd as usize, &mut buf);
        if n < 0 {
            eprint("ls: read error\n");
            close(fd as usize);
            return 1;
        }
        if n == 0 {
            break;
        }
        let mut at = 0usize;
        let n = n as usize;
        while at + 19 <= n {
            let len = u16::from_le_bytes([buf[at + 16], buf[at + 17]]) as usize;
            if len < 20 || at + len > n {
                break;
            }
            let kind = buf[at + 18];
            let mut end = at + 19;
            while end < at + len && buf[end] != 0 {
                end += 1;
            }
            write_all(1, &buf[at + 19..end]);
            if kind == DT_DIR {
                print("/");
            }
            print("\n");
            at += len;
        }
    }
    close(fd as usize);
    0
}
entry!(main);
