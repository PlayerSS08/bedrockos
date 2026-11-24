#![no_std]
#![no_main]
use bedrockrt::{entry, getpid, print, print_num};

fn main(args: bedrockrt::Args) -> i32 {
    print("hello from /bin/hello, pid ");
    print_num(getpid() as u64);
    print(", ");
    print_num(args.len() as u64);
    print(" argument(s)\n");
    0
}
entry!(main);
