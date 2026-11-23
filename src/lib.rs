//! BedrockOS: boot stage of a small educational x86_64 UEFI micro-OS.
//!
//! The crate is split so that everything that can be tested without hardware (formatting, memory-map
//! and ACPI parsing, CPUID decoding, pixel math) compiles on the host and has unit tests. The
//! hardware-facing modules only exist for the UEFI target.

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

extern crate alloc;

pub mod acpi;
pub mod anim;
pub mod apicmath;
pub mod banker;
pub mod bcache;
pub mod block;
pub mod ahcifis;
pub mod aes;
pub mod sha1;
pub mod wpa;
pub mod apt;
pub mod ed25519;
pub mod editbuf;
pub mod elf;
pub mod menulogic;
pub mod palette;
pub mod textcmd;
pub mod bootcount;
pub mod bootinfo;
pub mod bootopts;
pub mod bootscene;
pub mod desktop;
pub mod apps;
pub mod apps2;
pub mod battery;
pub mod calc;
pub mod tz;
pub mod vmdisplay;
pub mod wm;
pub mod cmdhelp;
pub mod cpuid;
pub mod dns;
pub mod dwi2c;
pub mod entropy;
pub mod fat;
pub mod fatname;
pub mod fatw;
pub mod eventlog;
pub mod font;
pub mod fontdata;
pub mod gfont;
pub mod gopmode;
pub mod gui;
pub mod frames;
pub mod heapalloc;
pub mod http;
pub mod hidkbd;
pub mod initfs;
pub mod hidparse;
pub mod i2chid;
pub mod ieee80211;
pub mod keymap;
pub mod lineedit;
pub mod loader;
pub mod logfmt;
pub mod memmap;
pub mod netdev;
pub mod netstack;
pub mod persist;
pub mod pci;
pub mod pipe;
pub mod pixel;
pub mod pkg;
pub mod pkgkey;
pub mod pointer;
pub mod procargs;
pub mod readyq;
pub mod rtc;
pub mod rtl8168;
pub mod rtw89;
pub mod rtw89_regs;
pub mod ramfs;
pub mod schedpolicy;
pub mod schedtrace;
pub mod sha256;
pub mod shparse;
pub mod sha512;
pub mod splash;
pub mod spin;
pub mod syscall;
pub mod tcp;
pub mod tcg2;
pub mod touchpad;
pub mod usbdesc;
pub mod wifi;
pub mod wifihost;
pub mod usbfs;
pub mod usbmsd;
pub mod usbnet;
pub mod usercopy;
pub mod virtio;
pub mod vmspace;
pub mod waitq;
pub mod xhcictx;
pub mod xhcireg;
pub mod xhcitrb;

// `log` defines the logging macros, so it must come before the modules that use them.
#[cfg(target_os = "uefi")]
pub mod global;
#[cfg(target_os = "uefi")]
#[macro_use]
pub mod log;
#[cfg(target_os = "uefi")]
pub mod bootfx;
#[cfg(target_os = "uefi")]
pub mod ahci;
#[cfg(target_os = "uefi")]
pub mod disk;
#[cfg(target_os = "uefi")]
pub mod e1000;
#[cfg(target_os = "uefi")]
pub mod virtionet;
#[cfg(target_os = "uefi")]
pub mod apic;
#[cfg(target_os = "uefi")]
pub mod arch;
#[cfg(target_os = "uefi")]
pub mod fb;
#[cfg(target_os = "uefi")]
pub mod boot;
#[cfg(target_os = "uefi")]
pub mod mm;
#[cfg(target_os = "uefi")]
pub mod mmio;
#[cfg(target_os = "uefi")]
pub mod panicrec;
#[cfg(target_os = "uefi")]
pub mod paging;
#[cfg(target_os = "uefi")]
pub mod power;
#[cfg(target_os = "uefi")]
pub mod sched;
#[cfg(target_os = "uefi")]
pub mod shell;
#[cfg(target_os = "uefi")]
pub mod uefi;
#[cfg(target_os = "uefi")]
pub mod usermode;
#[cfg(target_os = "uefi")]
pub mod vblk;
#[cfg(target_os = "uefi")]
pub mod xhci;
#[cfg(target_os = "uefi")]
pub mod touchpad_hw;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Set `AEON_GIT_HASH` in the build environment to embed a revision in the boot banner.
pub const GIT_HASH: &str = match option_env!("AEON_GIT_HASH") {
    Some(h) => h,
    None => "unknown",
};
