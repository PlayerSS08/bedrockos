//! User address spaces: the 4-level page tables of one process. Pure logic over a [`PhysMem`], so it is
//! host-tested with ordinary heap pages standing in for physical frames.
//!
//! Layout: the kernel is identity-mapped in PML4 slots 0 and 1 (the first 1 TiB). Those two entries are
//! copied into every process's PML4, so the kernel keeps running whichever process is active, and they
//! never carry the user bit. A process owns PML4 slot 2 only: virtual `USER_BASE..USER_END`. Every table
//! and page below that slot belongs to the process and is freed with it.
//!
//! The kernel never dereferences a user pointer. `copy_in`/`copy_out` walk the tables, check the user and
//! write bits, and touch the page through its physical address, so a bad pointer is an error value and
//! never a page fault in ring 0.

use alloc::vec::Vec;

pub const PAGE: u64 = 4096;
/// First user address: the start of PML4 slot 2 (1 TiB).
pub const USER_BASE: u64 = 0x100_0000_0000;
/// One past the last user address: the end of PML4 slot 2.
pub const USER_END: u64 = 0x180_0000_0000;
const USER_SLOT: usize = 2;
const KERNEL_SLOTS: usize = 2;

const P: u64 = 1;
const RW: u64 = 1 << 1;
const US: u64 = 1 << 2;
const NX: u64 = 1 << 63;
const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// Physical memory as the page-table code sees it.
pub trait PhysMem {
    /// One zeroed 4 KiB frame, or `None` when memory is exhausted.
    fn alloc_frame(&mut self) -> Option<u64>;
    fn free_frame(&mut self, phys: u64);
    /// The frame at `phys` viewed as a page table.
    fn table(&mut self, phys: u64) -> &mut [u64; 512];
    /// The frame at `phys` viewed as bytes.
    fn page(&mut self, phys: u64) -> &mut [u8; 4096];
    fn copy_frame(&mut self, from: u64, to: u64);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// Not page-aligned or outside `USER_BASE..USER_END`.
    BadAddress,
    AlreadyMapped,
    NoMemory,
    /// The process would own more pages than its limit allows.
    Quota,
    /// Writable and executable at once is never allowed.
    WritableAndExecutable,
}

#[derive(Debug)]
pub struct AddressSpace {
    pml4: u64,
    /// Leaf pages mapped for the process (tables are not counted).
    pages: usize,
    max_pages: usize,
    /// NX bit to put on non-executable leaves (0 when the CPU has no NX).
    nx: u64,
}

fn index(vaddr: u64, level: u32) -> usize {
    ((vaddr >> (12 + 9 * level)) & 0x1FF) as usize
}

/// True when `[addr, addr + len)` lies inside the user range (an empty range must still start inside it).
pub fn in_user_range(addr: u64, len: u64) -> bool {
    match addr.checked_add(len) {
        Some(end) => addr >= USER_BASE && end <= USER_END,
        None => false,
    }
}

impl AddressSpace {
    /// A new, empty address space that shares the kernel's two PML4 entries.
    pub fn new(mem: &mut dyn PhysMem, kernel: [u64; KERNEL_SLOTS], nx: bool, max_pages: usize) -> Option<Self> {
        let pml4 = mem.alloc_frame()?;
        let t = mem.table(pml4);
        for (i, e) in kernel.iter().enumerate() {
            // The kernel half is reachable from every process but never from ring 3.
            t[i] = *e & !US;
        }
        Some(Self { pml4, pages: 0, max_pages, nx: if nx { NX } else { 0 } })
    }

    /// Physical address of the PML4: the value for CR3.
    pub fn root(&self) -> u64 {
        self.pml4
    }

    pub fn pages(&self) -> usize {
        self.pages
    }

    /// The page table holding the leaf for `vaddr`, creating the tables above it when `create` is set.
    fn leaf_table(&self, mem: &mut dyn PhysMem, vaddr: u64, create: bool) -> Result<u64, MapError> {
        if !in_user_range(vaddr, 1) {
            return Err(MapError::BadAddress);
        }
        debug_assert_eq!(index(vaddr, 3), USER_SLOT);
        let mut table = self.pml4;
        for level in [3u32, 2, 1] {
            let i = index(vaddr, level);
            let e = mem.table(table)[i];
            table = if e & P != 0 {
                e & ADDR
            } else if create {
                let next = mem.alloc_frame().ok_or(MapError::NoMemory)?;
                // Upper levels are permissive; the leaf decides what ring 3 may do.
                mem.table(table)[i] = next | P | RW | US;
                next
            } else {
                return Err(MapError::BadAddress);
            };
        }
        Ok(table)
    }

    fn leaf_bits(&self, writable: bool, exec: bool) -> u64 {
        P | US | if writable { RW } else { 0 } | if exec { 0 } else { self.nx }
    }

    /// Maps one fresh zeroed page at `vaddr` and returns its physical address.
    pub fn map(&mut self, mem: &mut dyn PhysMem, vaddr: u64, writable: bool, exec: bool) -> Result<u64, MapError> {
        if vaddr % PAGE != 0 || !in_user_range(vaddr, PAGE) {
            return Err(MapError::BadAddress);
        }
        if writable && exec {
            return Err(MapError::WritableAndExecutable);
        }
        if self.pages >= self.max_pages {
            return Err(MapError::Quota);
        }
        let pt = self.leaf_table(mem, vaddr, true)?;
        let i = index(vaddr, 0);
        if mem.table(pt)[i] & P != 0 {
            return Err(MapError::AlreadyMapped);
        }
        let frame = mem.alloc_frame().ok_or(MapError::NoMemory)?;
        mem.table(pt)[i] = frame | self.leaf_bits(writable, exec);
        self.pages += 1;
        Ok(frame)
    }

    /// Changes the rights of a mapped page. Returns false if nothing is mapped there.
    pub fn protect(&mut self, mem: &mut dyn PhysMem, vaddr: u64, writable: bool, exec: bool) -> bool {
        if writable && exec {
            return false;
        }
        let Ok(pt) = self.leaf_table(mem, vaddr, false) else { return false };
        let i = index(vaddr, 0);
        let e = mem.table(pt)[i];
        if e & P == 0 {
            return false;
        }
        mem.table(pt)[i] = (e & ADDR) | self.leaf_bits(writable, exec);
        true
    }

    /// Removes the page at `vaddr` and frees its frame. Returns false if nothing is mapped there.
    pub fn unmap(&mut self, mem: &mut dyn PhysMem, vaddr: u64) -> bool {
        let Ok(pt) = self.leaf_table(mem, vaddr & !(PAGE - 1), false) else { return false };
        let i = index(vaddr, 0);
        let e = mem.table(pt)[i];
        if e & P == 0 {
            return false;
        }
        mem.table(pt)[i] = 0;
        mem.free_frame(e & ADDR);
        self.pages -= 1;
        true
    }

    /// Physical address of the byte at `vaddr` and whether ring 3 may write it.
    pub fn translate(&self, mem: &mut dyn PhysMem, vaddr: u64) -> Option<(u64, bool)> {
        let pt = self.leaf_table(mem, vaddr, false).ok()?;
        let e = mem.table(pt)[index(vaddr, 0)];
        if e & P == 0 || e & US == 0 {
            return None;
        }
        Some(((e & ADDR) + (vaddr & (PAGE - 1)), e & RW != 0))
    }

    /// Copies user memory at `vaddr` into `out`. False (and `out` partly filled) if any byte is unmapped.
    pub fn copy_in(&self, mem: &mut dyn PhysMem, vaddr: u64, out: &mut [u8]) -> bool {
        if !in_user_range(vaddr, out.len() as u64) {
            return false;
        }
        let mut done = 0usize;
        while done < out.len() {
            let va = vaddr + done as u64;
            let Some((phys, _)) = self.translate(mem, va) else { return false };
            let off = (phys & (PAGE - 1)) as usize;
            let n = (PAGE as usize - off).min(out.len() - done);
            out[done..done + n].copy_from_slice(&mem.page(phys & !(PAGE - 1))[off..off + n]);
            done += n;
        }
        true
    }

    /// Copies `data` into user memory at `vaddr`. Every page must be mapped writable for ring 3, so the
    /// kernel can never be tricked into changing a program's read-only or executable pages. Checked for
    /// the whole range before the first byte is written.
    pub fn copy_out(&self, mem: &mut dyn PhysMem, vaddr: u64, data: &[u8]) -> bool {
        if !in_user_range(vaddr, data.len() as u64) {
            return false;
        }
        let mut va = vaddr & !(PAGE - 1);
        while va < vaddr + data.len() as u64 {
            match self.translate(mem, va) {
                Some((_, true)) => {}
                _ => return false,
            }
            va += PAGE;
        }
        self.write_phys(mem, vaddr, data)
    }

    /// Writes through the tables without asking for the write bit: for the loader, which fills pages
    /// (code included) before the program first runs.
    pub fn write_phys(&self, mem: &mut dyn PhysMem, vaddr: u64, data: &[u8]) -> bool {
        if !in_user_range(vaddr, data.len() as u64) {
            return false;
        }
        let mut done = 0usize;
        while done < data.len() {
            let va = vaddr + done as u64;
            let Some((phys, _)) = self.translate(mem, va) else { return false };
            let off = (phys & (PAGE - 1)) as usize;
            let n = (PAGE as usize - off).min(data.len() - done);
            mem.page(phys & !(PAGE - 1))[off..off + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
        true
    }

    /// Reads a NUL-terminated string of at most `max` bytes (the NUL not counted). `None` if it runs
    /// into unmapped memory or has no terminator within `max` bytes.
    pub fn read_cstr(&self, mem: &mut dyn PhysMem, vaddr: u64, max: usize) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut va = vaddr;
        loop {
            let (phys, _) = self.translate(mem, va)?;
            let off = (phys & (PAGE - 1)) as usize;
            let page = mem.page(phys & !(PAGE - 1));
            for &b in &page[off..] {
                if b == 0 {
                    return Some(out);
                }
                if out.len() >= max {
                    return None;
                }
                out.push(b);
            }
            va = (va & !(PAGE - 1)).checked_add(PAGE)?;
        }
    }

    /// A full copy: new tables, new frames, the same bytes and rights. This is `fork`.
    pub fn duplicate(&self, mem: &mut dyn PhysMem) -> Option<Self> {
        let k = {
            let t = mem.table(self.pml4);
            [t[0], t[1]]
        };
        let mut child = Self::new(mem, k, self.nx != 0, self.max_pages)?;
        if self.copy_tree(mem, &mut child).is_none() {
            child.destroy(mem);
            return None;
        }
        Some(child)
    }

    fn copy_tree(&self, mem: &mut dyn PhysMem, child: &mut Self) -> Option<()> {
        let l4 = mem.table(self.pml4)[USER_SLOT];
        if l4 & P == 0 {
            return Some(());
        }
        for i3 in 0..512usize {
            let e3 = mem.table(l4 & ADDR)[i3];
            if e3 & P == 0 {
                continue;
            }
            for i2 in 0..512usize {
                let e2 = mem.table(e3 & ADDR)[i2];
                if e2 & P == 0 {
                    continue;
                }
                for i1 in 0..512usize {
                    let e1 = mem.table(e2 & ADDR)[i1];
                    if e1 & P == 0 {
                        continue;
                    }
                    let vaddr = USER_BASE + ((i3 as u64) << 30) + ((i2 as u64) << 21) + ((i1 as u64) << 12);
                    // Map read-only first (the rights are set right after), so W^X never blocks the copy.
                    let frame = child.map(mem, vaddr, false, false).ok()?;
                    mem.copy_frame(e1 & ADDR, frame);
                    let pt = child.leaf_table(mem, vaddr, false).ok()?;
                    mem.table(pt)[i1] = frame | (e1 & !ADDR);
                }
            }
        }
        Some(())
    }

    /// Frees every page and table of the process, then the PML4. The kernel's tables are not touched.
    pub fn destroy(self, mem: &mut dyn PhysMem) {
        let l4 = mem.table(self.pml4)[USER_SLOT];
        if l4 & P != 0 {
            for i3 in 0..512usize {
                let e3 = mem.table(l4 & ADDR)[i3];
                if e3 & P == 0 {
                    continue;
                }
                for i2 in 0..512usize {
                    let e2 = mem.table(e3 & ADDR)[i2];
                    if e2 & P == 0 {
                        continue;
                    }
                    for i1 in 0..512usize {
                        let e1 = mem.table(e2 & ADDR)[i1];
                        if e1 & P != 0 {
                            // Wipe before the frame can be handed to anyone else.
                            mem.page(e1 & ADDR).fill(0);
                            mem.free_frame(e1 & ADDR);
                        }
                    }
                    mem.free_frame(e2 & ADDR);
                }
                mem.free_frame(e3 & ADDR);
            }
            mem.free_frame(l4 & ADDR);
        }
        mem.free_frame(self.pml4);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Heap pages standing in for physical frames. Frame numbers start high so a stray identity access shows.
    pub struct FakeMem {
        pub frames: BTreeMap<u64, Box<[u64; 512]>>,
        next: u64,
        pub limit: usize,
    }

    impl FakeMem {
        pub fn new(limit: usize) -> Self {
            Self { frames: BTreeMap::new(), next: 0x10_0000, limit }
        }
    }

    impl PhysMem for FakeMem {
        fn alloc_frame(&mut self) -> Option<u64> {
            if self.frames.len() >= self.limit {
                return None;
            }
            let p = self.next;
            self.next += PAGE;
            self.frames.insert(p, Box::new([0; 512]));
            Some(p)
        }
        fn free_frame(&mut self, phys: u64) {
            assert!(self.frames.remove(&phys).is_some(), "double free or unknown frame {phys:#x}");
        }
        fn table(&mut self, phys: u64) -> &mut [u64; 512] {
            self.frames.get_mut(&phys).expect("table frame exists")
        }
        fn page(&mut self, phys: u64) -> &mut [u8; 4096] {
            let p = self.frames.get_mut(&phys).expect("page frame exists");
            // SAFETY: 512 u64 are exactly 4096 bytes, bytes have no alignment need, and the borrow of
            // the box is carried by the returned reference.
            unsafe { &mut *(p.as_mut_ptr() as *mut [u8; 4096]) }
        }
        fn copy_frame(&mut self, from: u64, to: u64) {
            let src = *self.frames[&from];
            **self.frames.get_mut(&to).unwrap() = src;
        }
    }

    const K: [u64; 2] = [0xAAAA_0000 | P | RW | US, 0xBBBB_0000 | P | RW];

    #[test]
    fn new_space_shares_the_kernel_half_without_the_user_bit() {
        let mut m = FakeMem::new(64);
        let s = AddressSpace::new(&mut m, K, true, 16).unwrap();
        let t = *m.table(s.root());
        assert_eq!(t[0], 0xAAAA_0000 | P | RW);
        assert_eq!(t[1], 0xBBBB_0000 | P | RW);
        assert!(t[2..].iter().all(|e| *e == 0));
    }

    #[test]
    fn map_translate_and_rights() {
        let mut m = FakeMem::new(64);
        let mut s = AddressSpace::new(&mut m, K, true, 16).unwrap();
        let code = s.map(&mut m, USER_BASE, false, true).unwrap();
        let data = s.map(&mut m, USER_BASE + PAGE, true, false).unwrap();
        assert_eq!(s.translate(&mut m, USER_BASE + 5), Some((code + 5, false)));
        assert_eq!(s.translate(&mut m, USER_BASE + PAGE + 7), Some((data + 7, true)));
        assert_eq!(s.translate(&mut m, USER_BASE + 2 * PAGE), None);
        assert_eq!(s.pages(), 2);
        // The leaf of the code page is user, read-only, executable; the data page is NX and writable.
        let pt = s.leaf_table(&mut m, USER_BASE, false).unwrap();
        let (c, d) = (m.table(pt)[0], m.table(pt)[1]);
        assert_eq!(c & (P | US | RW | NX), P | US);
        assert_eq!(d & (P | US | RW | NX), P | US | RW | NX);
    }

    #[test]
    fn map_refuses_bad_requests() {
        let mut m = FakeMem::new(64);
        let mut s = AddressSpace::new(&mut m, K, true, 2).unwrap();
        assert_eq!(s.map(&mut m, USER_BASE + 1, true, false), Err(MapError::BadAddress));
        assert_eq!(s.map(&mut m, 0x1000, true, false), Err(MapError::BadAddress));
        assert_eq!(s.map(&mut m, USER_END, true, false), Err(MapError::BadAddress));
        assert_eq!(s.map(&mut m, USER_BASE - PAGE, true, false), Err(MapError::BadAddress));
        assert_eq!(s.map(&mut m, USER_BASE, true, true), Err(MapError::WritableAndExecutable));
        s.map(&mut m, USER_BASE, true, false).unwrap();
        assert_eq!(s.map(&mut m, USER_BASE, true, false), Err(MapError::AlreadyMapped));
        s.map(&mut m, USER_BASE + PAGE, true, false).unwrap();
        assert_eq!(s.map(&mut m, USER_BASE + 2 * PAGE, true, false), Err(MapError::Quota));
    }

    #[test]
    fn out_of_memory_is_an_error_and_destroy_returns_everything() {
        let mut m = FakeMem::new(6);
        let mut s = AddressSpace::new(&mut m, K, true, 100).unwrap();
        // PML4 + PDPT + PD + PT + 2 pages = 6 frames; the third page cannot be had.
        s.map(&mut m, USER_BASE, true, false).unwrap();
        s.map(&mut m, USER_BASE + PAGE, true, false).unwrap();
        assert_eq!(s.map(&mut m, USER_BASE + 2 * PAGE, true, false), Err(MapError::NoMemory));
        s.destroy(&mut m);
        assert!(m.frames.is_empty());
    }

    #[test]
    fn copy_in_and_out_cross_pages_and_respect_rights() {
        let mut m = FakeMem::new(64);
        let mut s = AddressSpace::new(&mut m, K, true, 16).unwrap();
        s.map(&mut m, USER_BASE, true, false).unwrap();
        s.map(&mut m, USER_BASE + PAGE, true, false).unwrap();
        s.map(&mut m, USER_BASE + 2 * PAGE, false, true).unwrap();
        let msg: Vec<u8> = (0..200u8).collect();
        let at = USER_BASE + PAGE - 100;
        assert!(s.copy_out(&mut m, at, &msg));
        let mut back = [0u8; 200];
        assert!(s.copy_in(&mut m, at, &mut back));
        assert_eq!(&back[..], &msg[..]);
        // Into the read-only code page: refused, and nothing before it is written either.
        let at2 = USER_BASE + 2 * PAGE - 4;
        assert!(!s.copy_out(&mut m, at2, &[9u8; 8]));
        let mut probe = [1u8; 4];
        assert!(s.copy_in(&mut m, at2, &mut probe));
        assert_eq!(probe, [0; 4]);
        // The loader may fill it.
        assert!(s.write_phys(&mut m, USER_BASE + 2 * PAGE, b"code"));
        // Unmapped, kernel and wrapping ranges.
        assert!(!s.copy_in(&mut m, USER_BASE + 3 * PAGE, &mut probe));
        assert!(!s.copy_in(&mut m, 0x10_0000, &mut probe));
        assert!(!s.copy_out(&mut m, 0x10_0000, &probe));
        assert!(!s.copy_in(&mut m, u64::MAX - 1, &mut probe));
        assert!(!s.copy_in(&mut m, USER_END - 2, &mut probe));
    }

    #[test]
    fn read_cstr_stops_at_nul_limit_and_holes() {
        let mut m = FakeMem::new(64);
        let mut s = AddressSpace::new(&mut m, K, true, 16).unwrap();
        s.map(&mut m, USER_BASE, true, false).unwrap();
        s.map(&mut m, USER_BASE + PAGE, true, false).unwrap();
        assert!(s.copy_out(&mut m, USER_BASE + PAGE - 3, b"hello\0"));
        assert_eq!(s.read_cstr(&mut m, USER_BASE + PAGE - 3, 64).as_deref(), Some(&b"hello"[..]));
        assert_eq!(s.read_cstr(&mut m, USER_BASE + PAGE - 3, 5).as_deref(), Some(&b"hello"[..]));
        assert_eq!(s.read_cstr(&mut m, USER_BASE + PAGE - 3, 4), None);
        // A string that runs off the end of mapped memory without a NUL.
        assert!(s.copy_out(&mut m, USER_BASE + 2 * PAGE - 2, b"ab"));
        assert_eq!(s.read_cstr(&mut m, USER_BASE + 2 * PAGE - 2, 64), None);
        assert_eq!(s.read_cstr(&mut m, 0x1000, 64), None);
    }

    #[test]
    fn unmap_and_protect() {
        let mut m = FakeMem::new(64);
        let mut s = AddressSpace::new(&mut m, K, true, 16).unwrap();
        s.map(&mut m, USER_BASE, true, false).unwrap();
        assert!(s.protect(&mut m, USER_BASE, false, true));
        assert_eq!(s.translate(&mut m, USER_BASE).map(|t| t.1), Some(false));
        assert!(!s.protect(&mut m, USER_BASE, true, true));
        assert!(!s.protect(&mut m, USER_BASE + PAGE, true, false));
        let before = m.frames.len();
        assert!(s.unmap(&mut m, USER_BASE));
        assert!(!s.unmap(&mut m, USER_BASE));
        assert_eq!(m.frames.len(), before - 1);
        assert_eq!(s.pages(), 0);
    }

    #[test]
    fn duplicate_copies_bytes_and_rights_into_separate_frames() {
        let mut m = FakeMem::new(256);
        let mut s = AddressSpace::new(&mut m, K, true, 64).unwrap();
        s.map(&mut m, USER_BASE, false, true).unwrap();
        s.map(&mut m, USER_BASE + PAGE, true, false).unwrap();
        // A page far away, under different upper tables.
        let far = USER_END - PAGE;
        s.map(&mut m, far, true, false).unwrap();
        assert!(s.write_phys(&mut m, USER_BASE, b"text"));
        assert!(s.copy_out(&mut m, USER_BASE + PAGE, b"data"));
        assert!(s.copy_out(&mut m, far, b"stack"));
        let c = s.duplicate(&mut m).unwrap();
        assert_eq!(c.pages(), 3);
        let mut b = [0u8; 5];
        assert!(c.copy_in(&mut m, far, &mut b));
        assert_eq!(&b, b"stack");
        assert_eq!(c.translate(&mut m, USER_BASE).map(|t| t.1), Some(false));
        assert_eq!(c.translate(&mut m, USER_BASE + PAGE).map(|t| t.1), Some(true));
        assert_ne!(c.translate(&mut m, far).unwrap().0, s.translate(&mut m, far).unwrap().0);
        // Writing in the child leaves the parent alone.
        assert!(c.copy_out(&mut m, far, b"CHILD"));
        assert!(s.copy_in(&mut m, far, &mut b));
        assert_eq!(&b, b"stack");
        c.destroy(&mut m);
        s.destroy(&mut m);
        assert!(m.frames.is_empty());
    }

    #[test]
    fn duplicate_that_runs_out_of_memory_frees_what_it_took() {
        let mut m = FakeMem::new(64);
        let mut s = AddressSpace::new(&mut m, K, true, 64).unwrap();
        for i in 0..8 {
            s.map(&mut m, USER_BASE + i * PAGE, true, false).unwrap();
        }
        let used = m.frames.len();
        m.limit = used + 6;
        assert!(s.duplicate(&mut m).is_none());
        assert_eq!(m.frames.len(), used);
    }
}
