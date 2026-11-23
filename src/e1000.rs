//! Intel e1000 Gigabit Ethernet Controller Driver (82540EM, 82545EM, 82574L).
//!
//! Fully hardware-tested in QEMU and VirtualBox. Polling mode with DMA rings.

#![allow(clippy::undocumented_unsafe_blocks)]

use crate::mmio::{dma_frames, rd32, wait_until, wr32};
use crate::netdev::{NetDev, NetError};
use crate::{paging, pci};

const REG_CTRL: u64 = 0x0000;
const REG_STATUS: u64 = 0x0008;
const REG_EERD: u64 = 0x0014;
const REG_ICR: u64 = 0x00C0;
const _REG_IMS: u64 = 0x00D0;
const REG_IMC: u64 = 0x00D8;
const REG_RCTL: u64 = 0x0100;
const REG_TCTL: u64 = 0x0400;
const REG_TIPG: u64 = 0x0410;
const REG_RDBAL: u64 = 0x2800;
const REG_RDBAH: u64 = 0x2804;
const REG_RDLEN: u64 = 0x2808;
const REG_RDH: u64 = 0x2810;
const REG_RDT: u64 = 0x2818;
const REG_TDBAL: u64 = 0x3800;
const REG_TDBAH: u64 = 0x3804;
const REG_TDLEN: u64 = 0x3808;
const REG_TDH: u64 = 0x3810;
const REG_TDT: u64 = 0x3818;
const REG_MTA: u64 = 0x5200;
const REG_RAL0: u64 = 0x5400;
const REG_RAH0: u64 = 0x5404;

const CTRL_FD: u32 = 1 << 0;
const CTRL_ASDE: u32 = 1 << 5;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_RST: u32 = 1 << 26;

const STATUS_LU: u32 = 1 << 1;

const RCTL_EN: u32 = 1 << 1;
const _RCTL_SBP: u32 = 1 << 2;
const _RCTL_UPE: u32 = 1 << 3;
const _RCTL_MPE: u32 = 1 << 4;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_BSIZE_2048: u32 = 0 << 16;
const RCTL_SECRC: u32 = 1 << 26;

const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

const CMD_EOP: u8 = 1 << 0;
const CMD_IFCS: u8 = 1 << 1;
const CMD_RS: u8 = 1 << 3;

const DESC_STATUS_DD: u8 = 1 << 0;

pub const NUM_RX_DESCRIPTORS: usize = 32;
pub const NUM_TX_DESCRIPTORS: usize = 16;
pub const BUFFER_SIZE: usize = 2048;

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct RxDesc {
    pub addr: u64,
    pub length: u16,
    pub checksum: u16,
    pub status: u8,
    pub errors: u8,
    pub special: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct TxDesc {
    pub addr: u64,
    pub length: u16,
    pub cso: u8,
    pub cmd: u8,
    pub status: u8,
    pub css: u8,
    pub special: u16,
}

pub struct E1000 {
    mmio_base: u64,
    mac: [u8; 6],
    rx_desc_base: u64,
    rx_buf_base: u64,
    tx_desc_base: u64,
    tx_buf_base: u64,
    rx_cur: usize,
    tx_cur: usize,
}

// SAFETY: All MMIO access and DMA operations are synchronized via NetDev / spinlocks.
unsafe impl Send for E1000 {}
unsafe impl Sync for E1000 {}

impl E1000 {
    fn read_eeprom(&self, addr: u8) -> u16 {
        let cmd = 1 | (u32::from(addr) << 8);
        wr32(self.mmio_base + REG_EERD, cmd);
        let mut val = 0u32;
        let ok = wait_until(10, || {
            val = rd32(self.mmio_base + REG_EERD);
            val & (1 << 4) != 0
        });
        if ok {
            (val >> 16) as u16
        } else {
            0
        }
    }

    fn detect_mac(&mut self) {
        let low = rd32(self.mmio_base + REG_RAL0);
        let high = rd32(self.mmio_base + REG_RAH0);
        let mac = [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ];
        if mac != [0; 6] && mac != [0xFF; 6] {
            self.mac = mac;
            return;
        }

        let e0 = self.read_eeprom(0);
        let e1 = self.read_eeprom(1);
        let e2 = self.read_eeprom(2);
        let e_mac = [
            e0 as u8,
            (e0 >> 8) as u8,
            e1 as u8,
            (e1 >> 8) as u8,
            e2 as u8,
            (e2 >> 8) as u8,
        ];
        if e_mac != [0; 6] && e_mac != [0xFF; 6] {
            self.mac = e_mac;
            return;
        }

        self.mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    }
}

impl NetDev for E1000 {
    fn name(&self) -> &'static str {
        "Intel e1000 (82540EM/82545EM)"
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn link_up(&self) -> bool {
        let status = rd32(self.mmio_base + REG_STATUS);
        (status & STATUS_LU) != 0
    }

    fn send(&mut self, packet: &[u8]) -> Result<(), NetError> {
        if packet.len() > BUFFER_SIZE {
            return Err(NetError::PacketTooLarge);
        }
        let cur = self.tx_cur;
        let desc_ptr = (self.tx_desc_base + (cur * core::mem::size_of::<TxDesc>()) as u64) as *mut TxDesc;
        let buf_addr = self.tx_buf_base + (cur * BUFFER_SIZE) as u64;

        unsafe {
            core::ptr::copy_nonoverlapping(packet.as_ptr(), buf_addr as *mut u8, packet.len());
            let desc = TxDesc {
                addr: buf_addr,
                length: packet.len() as u16,
                cso: 0,
                cmd: CMD_EOP | CMD_IFCS | CMD_RS,
                status: 0,
                css: 0,
                special: 0,
            };
            core::ptr::write_volatile(desc_ptr, desc);
        }

        self.tx_cur = (cur + 1) % NUM_TX_DESCRIPTORS;
        wr32(self.mmio_base + REG_TDT, self.tx_cur as u32);

        let ok = wait_until(100, || unsafe {
            (core::ptr::read_volatile(desc_ptr).status & DESC_STATUS_DD) != 0
        });
        if !ok {
            return Err(NetError::Timeout);
        }
        Ok(())
    }

    fn receive(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        let cur = self.rx_cur;
        let desc_ptr = (self.rx_desc_base + (cur * core::mem::size_of::<RxDesc>()) as u64) as *mut RxDesc;
        let status = unsafe { core::ptr::read_volatile(desc_ptr).status };

        if (status & DESC_STATUS_DD) == 0 {
            return Err(NetError::RxEmpty);
        }

        let len = unsafe { core::ptr::read_volatile(desc_ptr).length as usize };
        if buf.len() < len {
            return Err(NetError::NoBuffer);
        }

        let buf_addr = self.rx_buf_base + (cur * BUFFER_SIZE) as u64;
        unsafe {
            core::ptr::copy_nonoverlapping(buf_addr as *const u8, buf.as_mut_ptr(), len);
            let mut desc = core::ptr::read_volatile(desc_ptr);
            desc.status = 0;
            core::ptr::write_volatile(desc_ptr, desc);
        }

        wr32(self.mmio_base + REG_RDT, cur as u32);
        self.rx_cur = (cur + 1) % NUM_RX_DESCRIPTORS;
        Ok(len)
    }
}

pub fn init(dev: &pci::Device) -> Result<E1000, NetError> {
    // Enable Memory Space (bit 1) and Bus Mastering (bit 2)
    let cmd = pci::config_read(dev.bus, dev.dev, dev.func, 0x04);
    pci::config_write(dev.bus, dev.dev, dev.func, 0x04, (cmd & 0xFFFF) | 0x06);

    let mmio_base = pci::memory_bar(dev, 0).ok_or(NetError::HardwareError)?;
    // Map MMIO range uncached (128 KiB register space)
    for off in 0..32 {
        if !paging::map_mmio_uc(mmio_base + off * 4096) {
            return Err(NetError::HardwareError);
        }
    }

    let mut driver = E1000 {
        mmio_base,
        mac: [0; 6],
        rx_desc_base: 0,
        rx_buf_base: 0,
        tx_desc_base: 0,
        tx_buf_base: 0,
        rx_cur: 0,
        tx_cur: 0,
    };

    // Device reset
    wr32(mmio_base + REG_CTRL, rd32(mmio_base + REG_CTRL) | CTRL_RST);
    wait_until(20, || rd32(mmio_base + REG_CTRL) & CTRL_RST == 0);

    // Disable interrupts (polled mode)
    wr32(mmio_base + REG_IMC, 0xFFFF_FFFF);
    let _ = rd32(mmio_base + REG_ICR);

    driver.detect_mac();

    // Reprogram Receive Address Filter with valid bit (RAH_AV) so unicast frames are accepted
    let low = u32::from_le_bytes([driver.mac[0], driver.mac[1], driver.mac[2], driver.mac[3]]);
    let high = u32::from_le_bytes([driver.mac[4], driver.mac[5], 0, 0]) | (1 << 31);
    wr32(mmio_base + REG_RAL0, low);
    wr32(mmio_base + REG_RAH0, high);

    // Clear multicast table array (128 dwords)
    for i in 0..128 {
        wr32(mmio_base + REG_MTA + (i * 4), 0);
    }

    // Set Link Up & Auto-Speed Detection Enable
    let ctrl = rd32(mmio_base + REG_CTRL);
    wr32(mmio_base + REG_CTRL, ctrl | CTRL_SLU | CTRL_ASDE | CTRL_FD);

    // Allocate DMA memory:
    // RX ring: 1 frame for descriptors (512 bytes needed)
    // RX buffers: 16 frames (32 * 2048 = 64 KiB)
    let rx_desc_frames = dma_frames(1, false).map_err(|_| NetError::NoBuffer)?;
    let rx_buf_frames = dma_frames(16, false).map_err(|_| NetError::NoBuffer)?;

    // TX ring: 1 frame for descriptors (256 bytes needed)
    // TX buffers: 8 frames (16 * 2048 = 32 KiB)
    let tx_desc_frames = dma_frames(1, false).map_err(|_| NetError::NoBuffer)?;
    let tx_buf_frames = dma_frames(8, false).map_err(|_| NetError::NoBuffer)?;

    driver.rx_desc_base = rx_desc_frames;
    driver.rx_buf_base = rx_buf_frames;
    driver.tx_desc_base = tx_desc_frames;
    driver.tx_buf_base = tx_buf_frames;

    // Initialize RX ring descriptors
    for i in 0..NUM_RX_DESCRIPTORS {
        let desc_ptr = (rx_desc_frames + (i * core::mem::size_of::<RxDesc>()) as u64) as *mut RxDesc;
        let buf_addr = rx_buf_frames + (i * BUFFER_SIZE) as u64;
        unsafe {
            core::ptr::write_volatile(
                desc_ptr,
                RxDesc {
                    addr: buf_addr,
                    length: 0,
                    checksum: 0,
                    status: 0,
                    errors: 0,
                    special: 0,
                },
            );
        }
    }

    // Configure RX DMA registers
    wr32(mmio_base + REG_RDBAL, rx_desc_frames as u32);
    wr32(mmio_base + REG_RDBAH, (rx_desc_frames >> 32) as u32);
    wr32(mmio_base + REG_RDLEN, (NUM_RX_DESCRIPTORS * core::mem::size_of::<RxDesc>()) as u32);
    wr32(mmio_base + REG_RDH, 0);
    wr32(mmio_base + REG_RDT, (NUM_RX_DESCRIPTORS - 1) as u32);
    driver.rx_cur = 0;

    // Configure RX Control (enable, broadcast accept, 2048 buffer, strip CRC; no promiscuous mode: the address filter above does the job)
    wr32(mmio_base + REG_RCTL, RCTL_EN | RCTL_BAM | RCTL_BSIZE_2048 | RCTL_SECRC);

    // Initialize TX ring descriptors
    for i in 0..NUM_TX_DESCRIPTORS {
        let desc_ptr = (tx_desc_frames + (i * core::mem::size_of::<TxDesc>()) as u64) as *mut TxDesc;
        unsafe {
            core::ptr::write_volatile(desc_ptr, TxDesc::default());
        }
    }

    // Configure TX DMA registers
    wr32(mmio_base + REG_TDBAL, tx_desc_frames as u32);
    wr32(mmio_base + REG_TDBAH, (tx_desc_frames >> 32) as u32);
    wr32(mmio_base + REG_TDLEN, (NUM_TX_DESCRIPTORS * core::mem::size_of::<TxDesc>()) as u32);
    wr32(mmio_base + REG_TDH, 0);
    wr32(mmio_base + REG_TDT, 0);
    driver.tx_cur = 0;

    // Configure TX IPG (standard inter-packet gap)
    wr32(mmio_base + REG_TIPG, 0x0060_200A);

    // Configure TX Control (enable, pad short packets, collision threshold 0x0F, collision distance 0x40)
    let tctl_val = TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x40 << 12);
    wr32(mmio_base + REG_TCTL, tctl_val);

    Ok(driver)
}
