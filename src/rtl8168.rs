//! Realtek RTL8168 / RTL8111 PCIe Gigabit Ethernet (PCI 10EC:8168): the wired port of the HP OMEN 16.
//!
//! STATUS: written from the public Linux `r8169` and iPXE `r8169` init sequences. The descriptor and register
//! logic is unit-tested on the host; the hardware part has NOT been run on a real RTL8168 (VirtualBox and QEMU
//! do not emulate this chip). The boot log prints the chip revision (`xid`) so a photo of the screen shows what
//! the laptop really is. Newer variants (8168g/h) normally also want chip-specific tuning writes that are not
//! included: expect link and basic traffic to be the first thing to verify, not full speed.
//!
//! Polled mode, one TX and one RX ring, 2 KiB buffers, no VLAN, no checksum offload, no interrupts.

// --- Register map (byte offsets in the BAR) --------------------------------------------------------------
pub const REG_IDR0: u64 = 0x00;
pub const REG_MAR0: u64 = 0x08;
pub const REG_TNPDS: u64 = 0x20;
pub const REG_CHIPCMD: u64 = 0x37;
pub const REG_TXPOLL: u64 = 0x38;
pub const REG_IMR: u64 = 0x3C;
pub const REG_ISR: u64 = 0x3E;
pub const REG_TXCONFIG: u64 = 0x40;
pub const REG_RXCONFIG: u64 = 0x44;
pub const REG_CFG9346: u64 = 0x50;
pub const REG_PHYAR: u64 = 0x60;
pub const REG_PHYSTATUS: u64 = 0x6C;
pub const REG_RXMAXSIZE: u64 = 0xDA;
pub const REG_CPLUSCMD: u64 = 0xE0;
pub const REG_RDSAR: u64 = 0xE4;
pub const REG_MAXTXPKT: u64 = 0xEC;

pub const CMD_RESET: u8 = 1 << 4;
pub const CMD_RX_ENABLE: u8 = 1 << 3;
pub const CMD_TX_ENABLE: u8 = 1 << 2;
pub const TXPOLL_NORMAL: u8 = 1 << 6;
pub const CFG9346_UNLOCK: u8 = 0xC0;
pub const CFG9346_LOCK: u8 = 0x00;
pub const PHYSTATUS_LINK: u8 = 1 << 1;

pub const RXCFG_ACCEPT_ALL_PHYS: u32 = 1 << 0;
pub const RXCFG_ACCEPT_MY_PHYS: u32 = 1 << 1;
pub const RXCFG_ACCEPT_MULTICAST: u32 = 1 << 2;
pub const RXCFG_ACCEPT_BROADCAST: u32 = 1 << 3;
pub const RXCFG_FIFO_NO_LIMIT: u32 = 7 << 13;
pub const RXCFG_DMA_UNLIMITED: u32 = 7 << 8;
pub const TXCFG_DMA_UNLIMITED: u32 = 7 << 8;
pub const TXCFG_IFG_NORMAL: u32 = 3 << 24;

/// What we accept: our MAC, broadcast and multicast (needed for ARP replies to broadcast and for DHCP).
pub const RX_CONFIG: u32 =
    RXCFG_ACCEPT_MY_PHYS | RXCFG_ACCEPT_BROADCAST | RXCFG_ACCEPT_MULTICAST | RXCFG_FIFO_NO_LIMIT | RXCFG_DMA_UNLIMITED;
pub const TX_CONFIG: u32 = TXCFG_DMA_UNLIMITED | TXCFG_IFG_NORMAL;

// --- Descriptors -----------------------------------------------------------------------------------------
pub const DESC_OWN: u32 = 1 << 31;
pub const DESC_RING_END: u32 = 1 << 30;
pub const DESC_FIRST: u32 = 1 << 29;
pub const DESC_LAST: u32 = 1 << 28;
pub const RX_ERROR: u32 = 1 << 21;
pub const LEN_MASK: u32 = 0x3FFF;

pub const RX_RING: usize = 32;
pub const TX_RING: usize = 16;
pub const BUF_SIZE: usize = 2048;
pub const CRC_LEN: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Desc {
    pub opts1: u32,
    pub opts2: u32,
    pub addr: u64,
}

/// `opts1` for an RX descriptor handed to the hardware.
pub const fn rx_opts1(index: usize) -> u32 {
    DESC_OWN | if index + 1 == RX_RING { DESC_RING_END } else { 0 } | BUF_SIZE as u32
}

/// `opts1` for a single-fragment TX descriptor of `len` bytes.
pub const fn tx_opts1(index: usize, len: usize) -> u32 {
    DESC_OWN | DESC_FIRST | DESC_LAST | if index + 1 == TX_RING { DESC_RING_END } else { 0 } | (len as u32 & LEN_MASK)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxResult {
    /// Still owned by the hardware: nothing received.
    Empty,
    /// A good frame of this many bytes (CRC removed).
    Frame(usize),
    /// A frame the hardware flagged as bad, or one too short to hold a header: drop it.
    Bad,
}

pub fn classify_rx(opts1: u32) -> RxResult {
    if opts1 & DESC_OWN != 0 {
        return RxResult::Empty;
    }
    let total = (opts1 & LEN_MASK) as usize;
    if opts1 & RX_ERROR != 0 || !(14 + CRC_LEN..=BUF_SIZE).contains(&total) {
        return RxResult::Bad;
    }
    RxResult::Frame(total - CRC_LEN)
}

// --- PHY access ------------------------------------------------------------------------------------------
pub const PHYAR_FLAG: u32 = 1 << 31;

pub const fn phyar_write(reg: u8, value: u16) -> u32 {
    PHYAR_FLAG | ((reg as u32 & 0x1F) << 16) | value as u32
}

pub const fn phyar_read(reg: u8) -> u32 {
    (reg as u32 & 0x1F) << 16
}

pub const MII_BMCR: u8 = 0;
pub const MII_BMSR: u8 = 1;
pub const MII_ADVERTISE: u8 = 4;
pub const MII_CTRL1000: u8 = 9;
/// BMCR: auto-negotiation enable + restart.
pub const BMCR_ANRESTART: u16 = 0x1200;
/// BMCR: auto-negotiation enable alone (also clears the power-down bit, which firmware may have left set).
pub const BMCR_ANENABLE: u16 = 0x1000;
/// BMCR: PHY reset; the bit clears itself when the reset is done.
pub const BMCR_RESET: u16 = 0x8000;
/// Realtek PHYs have register pages; page 0 holds the standard registers above.
pub const MII_PAGE: u8 = 0x1F;
pub const MII_LPA: u8 = 5;
pub const MII_STAT1000: u8 = 10;
/// Advertise 10/100 half and full, pause.
pub const ADV_ALL_10_100: u16 = 0x01E1;
/// Advertise 1000BASE-T full and half.
pub const ADV_1000: u16 = 0x0300;

// --- Chip identification ---------------------------------------------------------------------------------
/// The chip revision field of TxConfig, as the Linux driver reads it.
pub const fn xid(txconfig: u32) -> u32 {
    (txconfig >> 20) & 0xFCF
}

/// A name for the revisions the Linux r8169 table lists for the 8168 family; `None` for anything else. The
/// result is for the boot log only: the driver does not branch on it.
pub fn chip_name(xid: u32) -> Option<&'static str> {
    Some(match xid & 0x7CF {
        0x540..=0x541 => "RTL8168h/8111h",
        0x54A => "RTL8168h/8111h (rev 45)",
        0x5C8 | 0x509 | 0x4C1 | 0x4C0 => "RTL8168g/8111g",
        0x500..=0x502 => "RTL8168ep/8111ep",
        0x480..=0x481 => "RTL8168f/8111f",
        0x2C8 | 0x2C1 | 0x2C0 => "RTL8168e/8111e",
        _ => return None,
    })
}

pub const fn is_realtek_gbe(vendor: u16, device: u16) -> bool {
    vendor == 0x10EC && device == 0x8168
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_layout_is_16_bytes() {
        assert_eq!(core::mem::size_of::<Desc>(), 16);
        assert_eq!(core::mem::align_of::<Desc>(), 8);
    }

    #[test]
    fn rx_descriptors_own_and_end_flags() {
        assert_eq!(rx_opts1(0), 0x8000_0800);
        assert_eq!(rx_opts1(RX_RING - 1), 0xC000_0800);
        assert_ne!(rx_opts1(5) & DESC_OWN, 0);
    }

    #[test]
    fn tx_descriptors_carry_length_and_single_fragment_flags() {
        assert_eq!(tx_opts1(0, 60), DESC_OWN | DESC_FIRST | DESC_LAST | 60);
        assert_ne!(tx_opts1(TX_RING - 1, 60) & DESC_RING_END, 0);
        assert_eq!(tx_opts1(0, 1514) & LEN_MASK, 1514);
    }

    #[test]
    fn rx_classification() {
        assert_eq!(classify_rx(DESC_OWN | 100), RxResult::Empty);
        assert_eq!(classify_rx(64 + 4), RxResult::Frame(64));
        assert_eq!(classify_rx(60 | RX_ERROR), RxResult::Bad);
        assert_eq!(classify_rx(10), RxResult::Bad);
        assert_eq!(classify_rx(BUF_SIZE as u32 + 1), RxResult::Bad);
        // Ring-end and first/last flags do not disturb the length.
        assert_eq!(classify_rx(DESC_RING_END | DESC_FIRST | DESC_LAST | 1518), RxResult::Frame(1514));
    }

    #[test]
    fn phy_access_words() {
        assert_eq!(phyar_write(MII_BMCR, BMCR_ANRESTART), 0x8000_1200);
        assert_eq!(phyar_write(MII_CTRL1000, ADV_1000), 0x8009_0300);
        assert_eq!(phyar_read(MII_BMSR), 0x0001_0000);
    }

    #[test]
    fn chip_identification() {
        // TxConfig of an 8168h seen in Linux logs: XID 0x541 sits in bits 30:20.
        assert_eq!(xid(0x5410_0700), 0x541);
        assert_eq!(chip_name(0x541), Some("RTL8168h/8111h"));
        assert_eq!(chip_name(0x123), None);
        assert!(is_realtek_gbe(0x10EC, 0x8168));
        assert!(!is_realtek_gbe(0x10EC, 0xB520)); // the Wi-Fi chip is not this driver's
    }
}

#[cfg(target_os = "uefi")]
pub use hw::{init, Rtl8168};

#[cfg(target_os = "uefi")]
mod hw {
    #![allow(clippy::undocumented_unsafe_blocks)]

    use core::ptr::{read_volatile, write_volatile};
    use core::sync::atomic::{fence, Ordering};

    use super::*;
    use crate::mmio::{dma_frames, rd32, wait_until, wr32};
    use crate::netdev::{NetDev, NetError};
    use crate::{paging, pci};

    pub struct Rtl8168 {
        base: u64,
        mac: [u8; 6],
        rx_ring: u64,
        rx_bufs: u64,
        tx_ring: u64,
        tx_bufs: u64,
        rx_cur: usize,
        tx_cur: usize,
        xid: u32,
    }

    // SAFETY: all access goes through the NetDev lock in `netdev`.
    unsafe impl Send for Rtl8168 {}
    unsafe impl Sync for Rtl8168 {}

    fn rd8(addr: u64) -> u8 {
        unsafe { read_volatile(addr as *const u8) }
    }
    fn wr8(addr: u64, v: u8) {
        unsafe { write_volatile(addr as *mut u8, v) }
    }
    fn rd16(addr: u64) -> u16 {
        unsafe { read_volatile(addr as *const u16) }
    }
    fn wr16(addr: u64, v: u16) {
        unsafe { write_volatile(addr as *mut u16, v) }
    }

    impl Rtl8168 {
        pub fn xid(&self) -> u32 {
            self.xid
        }

        fn phy_write(&self, reg: u8, value: u16) {
            wr32(self.base + REG_PHYAR, phyar_write(reg, value));
            // The flag clears when the write has been done.
            wait_until(20, || rd32(self.base + REG_PHYAR) & PHYAR_FLAG == 0);
        }

        fn phy_read(&self, reg: u8) -> u16 {
            wr32(self.base + REG_PHYAR, phyar_read(reg));
            let mut v = 0;
            wait_until(20, || {
                v = rd32(self.base + REG_PHYAR);
                v & PHYAR_FLAG != 0
            });
            v as u16
        }

        fn rx_desc(&self, i: usize) -> *mut Desc {
            (self.rx_ring + (i * core::mem::size_of::<Desc>()) as u64) as *mut Desc
        }
        fn tx_desc(&self, i: usize) -> *mut Desc {
            (self.tx_ring + (i * core::mem::size_of::<Desc>()) as u64) as *mut Desc
        }
    }

    impl NetDev for Rtl8168 {
        fn name(&self) -> &'static str {
            "Realtek RTL8168/8111 Gigabit"
        }

        fn mac_address(&self) -> [u8; 6] {
            self.mac
        }

        fn link_up(&self) -> bool {
            rd8(self.base + REG_PHYSTATUS) & PHYSTATUS_LINK != 0
        }

        fn send(&mut self, packet: &[u8]) -> Result<(), NetError> {
            if packet.len() > BUF_SIZE {
                return Err(NetError::PacketTooLarge);
            }
            let cur = self.tx_cur;
            let desc = self.tx_desc(cur);
            // The slot is free once the hardware cleared OWN (it was sent). Give a previous frame a moment.
            let free = wait_until(50, || unsafe { read_volatile(desc).opts1 } & DESC_OWN == 0);
            if !free {
                return Err(NetError::TxFull);
            }
            let buf = self.tx_bufs + (cur * BUF_SIZE) as u64;
            unsafe {
                core::ptr::copy_nonoverlapping(packet.as_ptr(), buf as *mut u8, packet.len());
                write_volatile(desc, Desc { opts1: 0, opts2: 0, addr: buf });
            }
            fence(Ordering::SeqCst);
            // OWN is set last, in its own write, so the hardware never sees a half-built descriptor.
            unsafe {
                write_volatile(&mut (*desc).opts1, tx_opts1(cur, packet.len()));
            }
            fence(Ordering::SeqCst);
            wr8(self.base + REG_TXPOLL, TXPOLL_NORMAL);
            self.tx_cur = (cur + 1) % TX_RING;
            Ok(())
        }

        fn receive(&mut self, out: &mut [u8]) -> Result<usize, NetError> {
            let cur = self.rx_cur;
            let desc = self.rx_desc(cur);
            let opts1 = unsafe { read_volatile(desc).opts1 };
            let result = classify_rx(opts1);
            if result == RxResult::Empty {
                return Err(NetError::RxEmpty);
            }
            fence(Ordering::SeqCst);
            let buf = self.rx_bufs + (cur * BUF_SIZE) as u64;
            let outcome = match result {
                RxResult::Frame(n) if n <= out.len() => {
                    unsafe { core::ptr::copy_nonoverlapping(buf as *const u8, out.as_mut_ptr(), n) };
                    Ok(n)
                }
                RxResult::Frame(_) => Err(NetError::NoBuffer),
                _ => Err(NetError::HardwareError),
            };
            // Hand the buffer back to the hardware whatever happened.
            unsafe { write_volatile(&mut (*desc).opts1, rx_opts1(cur)) };
            fence(Ordering::SeqCst);
            self.rx_cur = (cur + 1) % RX_RING;
            outcome
        }
    }

    pub fn init(dev: &pci::Device) -> Result<Rtl8168, NetError> {
        // Memory space + bus master.
        let cmd = pci::config_read(dev.bus, dev.dev, dev.func, 0x04);
        pci::config_write(dev.bus, dev.dev, dev.func, 0x04, (cmd & 0xFFFF) | 0x06);

        // PCIe 8168 puts its registers in BAR2 (BAR0 is the I/O alias); older ones use BAR1 or BAR0.
        let base = [2u8, 4, 1, 0]
            .iter()
            .find_map(|&b| pci::memory_bar(dev, b))
            .ok_or(NetError::HardwareError)?;
        if !paging::map_mmio_uc(base) {
            return Err(NetError::HardwareError);
        }

        let xid = xid(rd32(base + REG_TXCONFIG));

        // Software reset: the bit clears when it is done.
        wr8(base + REG_CHIPCMD, CMD_RESET);
        if !wait_until(100, || rd8(base + REG_CHIPCMD) & CMD_RESET == 0) {
            return Err(NetError::Timeout);
        }

        let mut mac = [0u8; 6];
        for (i, m) in mac.iter_mut().enumerate() {
            *m = rd8(base + REG_IDR0 + i as u64);
        }
        if mac == [0; 6] || mac == [0xFF; 6] {
            return Err(NetError::HardwareError);
        }

        let rx_ring = dma_frames(1, false).map_err(|_| NetError::NoBuffer)?;
        let tx_ring = dma_frames(1, false).map_err(|_| NetError::NoBuffer)?;
        let rx_bufs = dma_frames(RX_RING * BUF_SIZE / 4096, false).map_err(|_| NetError::NoBuffer)?;
        let tx_bufs = dma_frames(TX_RING * BUF_SIZE / 4096, false).map_err(|_| NetError::NoBuffer)?;

        let d = Rtl8168 { base, mac, rx_ring, rx_bufs, tx_ring, tx_bufs, rx_cur: 0, tx_cur: 0, xid };
        for i in 0..RX_RING {
            let buf = rx_bufs + (i * BUF_SIZE) as u64;
            unsafe { write_volatile(d.rx_desc(i), Desc { opts1: rx_opts1(i), opts2: 0, addr: buf }) };
        }
        for i in 0..TX_RING {
            unsafe { write_volatile(d.tx_desc(i), Desc { opts1: if i + 1 == TX_RING { DESC_RING_END } else { 0 }, opts2: 0, addr: 0 }) };
        }
        fence(Ordering::SeqCst);

        wr8(base + REG_CFG9346, CFG9346_UNLOCK);
        wr16(base + REG_RXMAXSIZE, (BUF_SIZE - 1) as u16);
        // C+ command: no VLAN stripping, no RX checksum offload.
        let cp = rd16(base + REG_CPLUSCMD) & !((1 << 6) | (1 << 5));
        wr16(base + REG_CPLUSCMD, cp);
        // Descriptor rings (high word first, as Linux does).
        wr32(base + REG_TNPDS + 4, (tx_ring >> 32) as u32);
        wr32(base + REG_TNPDS, tx_ring as u32);
        wr32(base + REG_RDSAR + 4, (rx_ring >> 32) as u32);
        wr32(base + REG_RDSAR, rx_ring as u32);
        wr8(base + REG_MAXTXPKT, 0x3F);
        wr8(base + REG_CHIPCMD, CMD_TX_ENABLE | CMD_RX_ENABLE);
        wr32(base + REG_TXCONFIG, TX_CONFIG);
        wr32(base + REG_RXCONFIG, RX_CONFIG);
        // Accept all multicast hashes (DHCP and ARP need broadcast; multicast is harmless).
        wr32(base + REG_MAR0, 0xFFFF_FFFF);
        wr32(base + REG_MAR0 + 4, 0xFFFF_FFFF);
        wr16(base + REG_IMR, 0); // polled
        wr16(base + REG_ISR, 0xFFFF);
        wr8(base + REG_CFG9346, CFG9346_LOCK);

        // The PHY: firmware may have left it powered down or on another register page, and a link that never
        // comes up is the classic failure of a first driver. So: page 0, power up, reset and wait, then negotiate.
        d.phy_write(MII_PAGE, 0);
        d.phy_write(MII_BMCR, BMCR_ANENABLE);
        d.phy_write(MII_BMCR, BMCR_RESET | BMCR_ANENABLE);
        wait_until(500, || d.phy_read(MII_BMCR) & BMCR_RESET == 0);
        d.phy_write(MII_ADVERTISE, ADV_ALL_10_100);
        d.phy_write(MII_CTRL1000, ADV_1000);
        d.phy_write(MII_BMCR, BMCR_ANRESTART);
        let _ = d.phy_read(MII_BMSR);
        Ok(d)
    }

    impl Rtl8168 {
        /// Registers that explain a link that does not come up, for the `report` command.
        pub fn diag(&self) -> alloc::string::String {
            use alloc::format;
            let b = self.base;
            format!(
                "RTL8168 xid {:#010x} mac {:02x?} base {:#x}\n  CHIPCMD {:02x} PHYSTATUS {:02x} (bit1 = link) ISR {:04x} TXCONFIG {:08x} RXCONFIG {:08x}\n  PHY BMCR {:04x} BMSR {:04x} ANAR {:04x} LPA {:04x} CTRL1000 {:04x} STAT1000 {:04x}\n",
                self.xid,
                self.mac,
                b,
                rd8(b + REG_CHIPCMD),
                rd8(b + REG_PHYSTATUS),
                rd16(b + REG_ISR),
                rd32(b + REG_TXCONFIG),
                rd32(b + REG_RXCONFIG),
                self.phy_read(MII_BMCR),
                self.phy_read(MII_BMSR),
                self.phy_read(MII_ADVERTISE),
                self.phy_read(MII_LPA),
                self.phy_read(MII_CTRL1000),
                self.phy_read(MII_STAT1000),
            )
        }
    }
}
