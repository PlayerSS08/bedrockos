#![no_std]
#![no_main]
use aeonrt::{entry, write_all};

fn main(args: aeonrt::Args) -> i32 {
    for i in 1..args.len() {
        if i > 1 {
            write_all(1, b" ");
        }
        write_all(1, args.get(i).unwrap_or(b""));
    }
    write_all(1, b"\n");
    0
}
entry!(main);
