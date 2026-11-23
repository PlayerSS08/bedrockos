//! Follows the size of the VirtualBox window. VirtualBox's guest device (VMMDev, PCI 80EE:CAFE) tells a guest
//! that says it can change its screen size which size the window would like ("auto-resize guest display").
//! BedrockOS reports itself, polls for that size, and switches the screen mode with the Bochs VBE registers
//! (I/O ports 0x1CE/0x1CF), which VirtualBox's graphics device provides. The desktop then rebuilds itself
//! (`bootfx`). Request layouts are pure and host-tested; the port I/O only runs in a UEFI build.

/// VMMDev request header version and the interface version BedrockOS speaks.
pub const HEADER_VERSION: u32 = 0x0001_0001;
pub const VMMDEV_VERSION: u32 = 0x0001_0004;
pub const REQ_REPORT_GUEST_INFO: u32 = 50;
pub const REQ_GET_DISPLAY_CHANGE2: u32 = 54;
pub const REQ_SET_GUEST_CAPS: u32 = 55;
/// Guest capability: it can change its screen size on request.
pub const CAP_GRAPHICS: u32 = 1 << 2;
/// Event: the host asks for another screen size.
pub const EVENT_DISPLAY_CHANGE: u32 = 1 << 2;

/// Smallest and largest screen the desktop handles.
pub const MIN_SIZE: (usize, usize) = (800, 600);

/// A request: the 24-byte header (size, version, type, result, two reserved words) and `body` words.
pub fn request(kind: u32, body: &[u32], out: &mut [u32; 16]) -> usize {
    let n = 6 + body.len();
    out.fill(0);
    out[0] = (n * 4) as u32;
    out[1] = HEADER_VERSION;
    out[2] = kind;
    out[6..n].copy_from_slice(body);
    n
}

/// The size to switch to for a host request of `w` x `h`: the width rounded down to a multiple of 8 (the
/// graphics device wants whole 8-pixel groups), both kept within [`MIN_SIZE`] and `max`. `None` for "no
/// request" (zero) or a size too small to use.
pub fn fit(w: u32, h: u32, max: (usize, usize)) -> Option<(usize, usize)> {
    let (w, h) = (w as usize & !7, h as usize);
    if w < MIN_SIZE.0 || h < MIN_SIZE.1 {
        return None;
    }
    Some((w.min(max.0 & !7), h.min(max.1)))
}

#[cfg(target_os = "uefi")]
mod hw {
    use super::*;
    use crate::arch::{inw, outl, outw};
    use core::sync::atomic::{AtomicU16, AtomicU64, Ordering::Relaxed};

    static PORT: AtomicU16 = AtomicU16::new(0);
    /// Physical address of the page that requests are built in (below 4 GiB, identity mapped).
    static PAGE: AtomicU64 = AtomicU64::new(0);

    /// Sends a request; returns its result code (0 = success) and leaves the reply in the page.
    fn send(kind: u32, body: &[u32]) -> Option<(i32, [u32; 16])> {
        let (port, page) = (PORT.load(Relaxed), PAGE.load(Relaxed));
        if port == 0 || page == 0 {
            return None;
        }
        let mut req = [0u32; 16];
        let n = request(kind, body, &mut req);
        let p = page as *mut u32;
        // SAFETY: `page` is a 4 KiB frame from `dma_page`, identity mapped and only used here (callers run with
        // interrupts off, one at a time). The device reads and writes it during the `outl`.
        unsafe {
            for (i, w) in req.iter().take(n).enumerate() {
                core::ptr::write_volatile(p.add(i), *w);
            }
            outl(port, page as u32);
            let mut out = [0u32; 16];
            for (i, w) in out.iter_mut().take(n).enumerate() {
                *w = core::ptr::read_volatile(p.add(i));
            }
            Some((out[3] as i32, out))
        }
    }

    /// Finds VMMDev, reports BedrockOS and says it can change its screen size. False outside VirtualBox.
    pub fn init() -> bool {
        let Some(d) = crate::pci::find(0x80EE, 0xCAFE) else { return false };
        let Some(port) = crate::pci::io_bar(&d, 0) else { return false };
        let Ok(page) = crate::mmio::dma_page(false) else { return false };
        PORT.store(port, Relaxed);
        PAGE.store(page, Relaxed);
        let info = send(REQ_REPORT_GUEST_INFO, &[VMMDEV_VERSION, 0]).map_or(-1, |r| r.0);
        let caps = send(REQ_SET_GUEST_CAPS, &[CAP_GRAPHICS]).map_or(-1, |r| r.0);
        crate::info!("vmdisplay", "VirtualBox guest device at port {:#x}: guest info {}, graphics capability {}", port, info, caps);
        if dispi_read(0) < 0xB0C0 {
            crate::info!("vmdisplay", "no VBE mode registers: the screen keeps its size");
            PORT.store(0, Relaxed);
            return false;
        }
        caps == 0
    }

    /// The size the VirtualBox window asks for, if any.
    pub fn wanted() -> Option<(u32, u32)> {
        let (rc, r) = send(REQ_GET_DISPLAY_CHANGE2, &[0, 0, 0, EVENT_DISPLAY_CHANGE, 0])?;
        (rc == 0 && r[6] != 0 && r[7] != 0).then_some((r[6], r[7]))
    }

    fn dispi_read(index: u16) -> u16 {
        // SAFETY: 0x1CE/0x1CF are the Bochs VBE index/data ports of the graphics device; reads have no effect.
        unsafe {
            outw(0x1CE, index);
            inw(0x1CF)
        }
    }

    fn dispi_write(index: u16, v: u16) {
        // SAFETY: as above; the mode registers only change the picture.
        unsafe {
            outw(0x1CE, index);
            outw(0x1CF, v);
        }
    }

    /// Switches to a `w` x `h` 32-bit mode (the frame buffer stays where it is). Returns the new pixels per
    /// row, or `None` when the device did not take the mode.
    pub fn set_mode(w: usize, h: usize) -> Option<usize> {
        dispi_write(4, 0); // disable
        dispi_write(1, w as u16);
        dispi_write(2, h as u16);
        dispi_write(3, 32);
        dispi_write(4, 0x41); // enable, linear frame buffer
        let (gw, gh) = (usize::from(dispi_read(1)), usize::from(dispi_read(2)));
        let stride = usize::from(dispi_read(6)).max(w);
        ((gw, gh) == (w, h)).then_some(stride)
    }
}

#[cfg(target_os = "uefi")]
pub use hw::{init, set_mode, wanted};

/// Host builds have no VirtualBox.
#[cfg(not(target_os = "uefi"))]
pub fn init() -> bool {
    false
}

#[cfg(not(target_os = "uefi"))]
pub fn wanted() -> Option<(u32, u32)> {
    None
}

#[cfg(not(target_os = "uefi"))]
pub fn set_mode(_w: usize, _h: usize) -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_layout() {
        let mut r = [0u32; 16];
        let n = request(REQ_GET_DISPLAY_CHANGE2, &[0, 0, 0, EVENT_DISPLAY_CHANGE, 0], &mut r);
        assert_eq!(n, 11);
        assert_eq!(&r[..11], &[44, HEADER_VERSION, 54, 0, 0, 0, 0, 0, 0, 4, 0]);
        assert_eq!(request(REQ_SET_GUEST_CAPS, &[CAP_GRAPHICS], &mut r), 7);
        assert_eq!(r[0], 28);
    }

    #[test]
    fn fitting_sizes() {
        assert_eq!(fit(1533, 851, (2560, 1600)), Some((1528, 851)));
        assert_eq!(fit(0, 0, (2560, 1600)), None);
        assert_eq!(fit(640, 480, (2560, 1600)), None);
        assert_eq!(fit(4000, 3000, (2560, 1600)), Some((2560, 1600)));
    }
}
