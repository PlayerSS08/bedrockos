#![no_std]
#![no_main]
use aeonrt::{entry, exit, fork, getpid, print, print_num, wait};

fn main(_: aeonrt::Args) -> i32 {
    let pid = fork();
    if pid < 0 {
        print("forktest: fork failed\n");
        return 1;
    }
    if pid == 0 {
        print("child  pid ");
        print_num(getpid() as u64);
        print(" running\n");
        exit(7);
    }
    let (done, status) = wait();
    print("parent pid ");
    print_num(getpid() as u64);
    print(": child ");
    print_num(done as u64);
    print(" exited with ");
    print_num(status as u64);
    print("\n");
    if done == pid && status == 7 { 0 } else { 1 }
}
entry!(main);
