//! Builds a new program image inside an [`AddressSpace`]: the ELF segments (or a flat code blob), the
//! stack with `argc/argv`, and the addresses where the heap and `mmap` areas start. Pure over
//! [`PhysMem`], so the whole path is host-tested with a fake memory.

use crate::elf::{self, ElfError};
use crate::procargs;
use crate::vmspace::{AddressSpace, MapError, PhysMem, PAGE, USER_BASE, USER_END};

/// `brk` heap starts here (64 GiB into the user range), far above any image.
pub const HEAP_BASE: u64 = USER_BASE + 0x10_0000_0000;
/// Anonymous `mmap` memory is handed out from here.
pub const MMAP_BASE: u64 = USER_BASE + 0x40_0000_0000;
pub const STACK_PAGES: u64 = 64;
/// Top of the stack; the page above it stays unmapped as a guard.
pub const STACK_TOP: u64 = USER_END - PAGE;
/// Most memory an executable's segments may ask for.
pub const MAX_IMAGE: u64 = 8 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    Elf(ElfError),
    NoMemory,
    /// Too many or too long arguments.
    BadArgs,
    /// The flat image does not fit its page budget.
    TooBig,
}

impl From<MapError> for LoadError {
    fn from(_: MapError) -> Self {
        LoadError::NoMemory
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Loaded {
    pub entry: u64,
    pub rsp: u64,
}

fn build_stack(a: &mut AddressSpace, mem: &mut dyn PhysMem, args: &[&[u8]]) -> Result<u64, LoadError> {
    let base = STACK_TOP - STACK_PAGES * PAGE;
    for i in 0..STACK_PAGES {
        a.map(mem, base + i * PAGE, true, false)?;
    }
    let st = procargs::build(args, STACK_TOP).ok_or(LoadError::BadArgs)?;
    if !a.write_phys(mem, st.rsp, &st.bytes) {
        return Err(LoadError::NoMemory);
    }
    Ok(st.rsp)
}

/// Loads a static ELF executable.
pub fn load_elf(a: &mut AddressSpace, mem: &mut dyn PhysMem, file: &[u8], args: &[&[u8]]) -> Result<Loaded, LoadError> {
    let img = elf::parse(file, USER_BASE, HEAP_BASE, MAX_IMAGE).map_err(LoadError::Elf)?;
    for seg in img.iter() {
        let (first, count) = seg.pages();
        for i in 0..count {
            a.map(mem, first + i * PAGE, seg.writable(), seg.executable())?;
        }
        let data = &file[seg.file_off..seg.file_off + seg.file_size];
        if !a.write_phys(mem, seg.vaddr, data) {
            return Err(LoadError::NoMemory);
        }
    }
    let rsp = build_stack(a, mem, args)?;
    Ok(Loaded { entry: img.entry, rsp })
}

/// Loads a flat, position-independent code blob at `USER_BASE` (the built-in demo programs).
pub fn load_flat(a: &mut AddressSpace, mem: &mut dyn PhysMem, code: &[u8], args: &[&[u8]]) -> Result<Loaded, LoadError> {
    if code.is_empty() || code.len() > 16 * PAGE as usize {
        return Err(LoadError::TooBig);
    }
    for i in 0..(code.len() as u64).div_ceil(PAGE) {
        a.map(mem, USER_BASE + i * PAGE, false, true)?;
    }
    if !a.write_phys(mem, USER_BASE, code) {
        return Err(LoadError::NoMemory);
    }
    let rsp = build_stack(a, mem, args)?;
    Ok(Loaded { entry: USER_BASE, rsp })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vmspace::tests::FakeMem;

    /// A minimal ET_EXEC file: one RX segment (text) and one RW segment (data + bss).
    fn sample_elf(entry: u64) -> Vec<u8> {
        let mut f = vec![0u8; 0x3000];
        f[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        f[4] = 2;
        f[5] = 1;
        f[6] = 1;
        f[16..18].copy_from_slice(&2u16.to_le_bytes());
        f[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        f[24..32].copy_from_slice(&entry.to_le_bytes());
        f[32..40].copy_from_slice(&64u64.to_le_bytes());
        f[52..54].copy_from_slice(&64u16.to_le_bytes());
        f[54..56].copy_from_slice(&56u16.to_le_bytes());
        f[56..58].copy_from_slice(&2u16.to_le_bytes());
        let mut ph = |i: usize, flags: u32, off: u64, vaddr: u64, filesz: u64, memsz: u64| {
            let o = 64 + i * 56;
            f[o..o + 4].copy_from_slice(&1u32.to_le_bytes());
            f[o + 4..o + 8].copy_from_slice(&flags.to_le_bytes());
            f[o + 8..o + 16].copy_from_slice(&off.to_le_bytes());
            f[o + 16..o + 24].copy_from_slice(&vaddr.to_le_bytes());
            f[o + 32..o + 40].copy_from_slice(&filesz.to_le_bytes());
            f[o + 40..o + 48].copy_from_slice(&memsz.to_le_bytes());
        };
        ph(0, 5, 0x1000, USER_BASE + 0x1000, 8, 8);
        ph(1, 6, 0x2000, USER_BASE + 0x2000, 4, 0x1800);
        f[0x1000..0x1008].copy_from_slice(b"TEXTTEXT");
        f[0x2000..0x2004].copy_from_slice(b"DATA");
        f
    }

    fn space(m: &mut FakeMem) -> AddressSpace {
        AddressSpace::new(m, [0xA000 | 1, 0xB000 | 1], true, 4096).unwrap()
    }

    #[test]
    fn elf_segments_stack_and_arguments_land_where_they_should() {
        let mut m = FakeMem::new(512);
        let mut a = space(&mut m);
        let l = load_elf(&mut a, &mut m, &sample_elf(USER_BASE + 0x1000), &[b"prog", b"arg1"]).unwrap();
        assert_eq!(l.entry, USER_BASE + 0x1000);
        let mut t = [0u8; 8];
        assert!(a.copy_in(&mut m, USER_BASE + 0x1000, &mut t));
        assert_eq!(&t, b"TEXTTEXT");
        // Text is not writable by the kernel on a program's behalf, data is, and bss is zero.
        assert_eq!(a.translate(&mut m, USER_BASE + 0x1000).map(|x| x.1), Some(false));
        assert_eq!(a.translate(&mut m, USER_BASE + 0x2000).map(|x| x.1), Some(true));
        let mut d = [9u8; 6];
        assert!(a.copy_in(&mut m, USER_BASE + 0x2000, &mut d));
        assert_eq!(&d, b"DATA\0\0");
        assert!(a.copy_in(&mut m, USER_BASE + 0x3000, &mut d));
        assert_eq!(d, [0; 6]);
        // Stack: rsp aligned, argc = 2, argv[1] points at "arg1".
        assert_eq!(l.rsp % 16, 0);
        let mut w = [0u8; 8];
        assert!(a.copy_in(&mut m, l.rsp, &mut w));
        assert_eq!(u64::from_le_bytes(w), 2);
        assert!(a.copy_in(&mut m, l.rsp + 16, &mut w));
        let p = u64::from_le_bytes(w);
        let s = a.read_cstr(&mut m, p, 16).unwrap();
        assert_eq!(s, b"arg1");
        // The page just under the stack and the one above it are unmapped (guards).
        assert!(a.translate(&mut m, STACK_TOP - STACK_PAGES * PAGE - 1).is_none());
        assert!(a.translate(&mut m, STACK_TOP).is_none());
        a.destroy(&mut m);
        assert!(m.frames.is_empty());
    }

    #[test]
    fn bad_files_are_refused_and_leave_nothing_behind_after_destroy() {
        let mut m = FakeMem::new(512);
        let mut a = space(&mut m);
        assert_eq!(load_elf(&mut a, &mut m, b"not an elf", &[]), Err(LoadError::Elf(ElfError::TooShort)));
        let mut wx = sample_elf(USER_BASE + 0x1000);
        wx[64 + 4] = 7; // text segment RWX
        assert_eq!(load_elf(&mut a, &mut m, &wx, &[]), Err(LoadError::Elf(ElfError::WritableAndExecutable)));
        let low = sample_elf(0x40_0000);
        assert!(matches!(load_elf(&mut a, &mut m, &low, &[]), Err(LoadError::Elf(_))));
        let many: Vec<&[u8]> = (0..40).map(|_| &b"x"[..]).collect();
        assert_eq!(load_elf(&mut a, &mut m, &sample_elf(USER_BASE + 0x1000), &many), Err(LoadError::BadArgs));
        a.destroy(&mut m);
        assert!(m.frames.is_empty());
    }

    #[test]
    fn running_out_of_memory_is_reported() {
        let mut m = FakeMem::new(20);
        let mut a = space(&mut m);
        assert_eq!(load_elf(&mut a, &mut m, &sample_elf(USER_BASE + 0x1000), &[]), Err(LoadError::NoMemory));
        a.destroy(&mut m);
        assert!(m.frames.is_empty());
    }

    #[test]
    fn flat_programs_load_at_the_base() {
        let mut m = FakeMem::new(512);
        let mut a = space(&mut m);
        let l = load_flat(&mut a, &mut m, &[0x90, 0xC3], &[b"hello"]).unwrap();
        assert_eq!(l.entry, USER_BASE);
        let mut b = [0u8; 2];
        assert!(a.copy_in(&mut m, USER_BASE, &mut b));
        assert_eq!(b, [0x90, 0xC3]);
        assert_eq!(a.translate(&mut m, USER_BASE).map(|x| x.1), Some(false));
        assert_eq!(load_flat(&mut a, &mut m, &[], &[]), Err(LoadError::TooBig));
        assert_eq!(load_flat(&mut a, &mut m, &vec![0u8; 17 * 4096], &[]), Err(LoadError::TooBig));
        a.destroy(&mut m);
    }
}
