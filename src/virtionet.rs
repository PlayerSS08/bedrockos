//! VirtIO-Net Network Driver (virtio 1.0 modern and legacy PCI).
//!
//! Provides bare-metal polling network packet transmission and reception
//! over VirtIO network virtqueues.

#![allow(clippy::undocumented_unsafe_blocks)]

use core::ptr::{read_volatile, write_bytes, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::mmio::{dma_frames, wait_until};
use crate::netdev::{NetDev, NetError};
use crate::{paging, pci, virtio};

// VirtIO Status Bits
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

// Common config register offsets (modern MMIO)
const REG_DEVICE_STATUS: u64 = 20;
const REG_QUEUE_SELECT: u64 = 22;
const REG_QUEUE_SIZE: u64 = 24;
const REG_QUEUE_ENABLE: u64 = 28;
const REG_QUEUE_NOTIFY_OFF: u64 = 30;
const REG_QUEUE_DESC: u64 = 32;
const REG_QUEUE_DRIVER: u64 = 40;
const REG_QUEUE_DEVICE: u64 = 48;

const _DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

const HDR_SIZE: usize = 10;
const BUFFER_SIZE: usize = 2048;
const QUEUE_SIZE: u16 = 8;

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
}

#[repr(C)]
struct VqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

pub struct VirtioNet {
    _common: u64,
    notify: u64,
    notify_mult: u32,
    rx_notify_off: u16,
    tx_notify_off: u16,
    mac: [u8; 6],

    // RX Queue (Queue 0)
    rx_ring: u64,
    rx_layout: virtio::RingLayout,
    rx_bufs: u64,
    rx_last_used: u16,

    // TX Queue (Queue 1)
    tx_ring: u64,
    tx_layout: virtio::RingLayout,
    tx_bufs: u64,
    tx_avail_idx: u16,
    tx_last_used: u16,
}

// SAFETY: VirtioNet operations are protected by the active device spinlock.
unsafe impl Send for VirtioNet {}
unsafe impl Sync for VirtioNet {}

fn rd8(a: u64) -> u8 {
    unsafe { read_volatile(a as *const u8) }
}
fn rd16(a: u64) -> u16 {
    unsafe { read_volatile(a as *const u16) }
}
fn wr8(a: u64, v: u8) {
    unsafe { write_volatile(a as *mut u8, v) }
}
fn wr16(a: u64, v: u16) {
    unsafe { write_volatile(a as *mut u16, v) }
}
fn wr32(a: u64, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) }
}
fn wr64(a: u64, v: u64) {
    wr32(a, v as u32);
    wr32(a + 4, (v >> 32) as u32);
}

impl VirtioNet {
    fn notify_queue(&self, notify_off: u16, qidx: u16) {
        let addr = self.notify + (u64::from(notify_off) * u64::from(self.notify_mult));
        wr16(addr, qidx);
    }
}

impl NetDev for VirtioNet {
    fn name(&self) -> &'static str {
        "VirtIO-Net (PCI)"
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn link_up(&self) -> bool {
        true
    }

    fn send(&mut self, packet: &[u8]) -> Result<(), NetError> {
        if packet.len() > BUFFER_SIZE - HDR_SIZE {
            return Err(NetError::PacketTooLarge);
        }

        let desc_table = self.tx_ring as *mut VqDesc;
        let avail_ring = (self.tx_ring + self.tx_layout.avail as u64) as *mut u16;
        let used_ring = (self.tx_ring + self.tx_layout.used as u64) as *const u16;

        let tx_buf = self.tx_bufs;
        unsafe {
            // Write zeroed virtio header at buffer start
            write_bytes(tx_buf as *mut u8, 0, HDR_SIZE);
            core::ptr::copy_nonoverlapping(packet.as_ptr(), (tx_buf + HDR_SIZE as u64) as *mut u8, packet.len());

            // Populate descriptor 0
            let total_len = (HDR_SIZE + packet.len()) as u32;
            let desc = VqDesc {
                addr: tx_buf,
                len: total_len,
                flags: 0,
                next: 0,
            };
            write_volatile(desc_table, desc);

            // Publish descriptor to available ring
            let idx = self.tx_avail_idx;
            let slot = 2 + (idx % QUEUE_SIZE) as usize;
            write_volatile(avail_ring.add(slot), 0);
            fence(Ordering::SeqCst);
            self.tx_avail_idx = idx.wrapping_add(1);
            write_volatile(avail_ring.add(1), self.tx_avail_idx);
            fence(Ordering::SeqCst);
        }

        self.notify_queue(self.tx_notify_off, 1);

        // Poll used ring for completion
        let ok = wait_until(100, || unsafe {
            let used_idx = read_volatile(used_ring.add(1));
            used_idx != self.tx_last_used
        });

        if !ok {
            return Err(NetError::Timeout);
        }

        self.tx_last_used = self.tx_last_used.wrapping_add(1);
        Ok(())
    }

    fn receive(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        let used_ring = (self.rx_ring + self.rx_layout.used as u64) as *const u16;
        let used_idx = unsafe { read_volatile(used_ring.add(1)) };

        if used_idx == self.rx_last_used {
            return Err(NetError::RxEmpty);
        }

        let slot = (self.rx_last_used % QUEUE_SIZE) as usize;
        let elem_ptr = unsafe { (used_ring as *const u8).add(4 + slot * 8) as *const u32 };
        let _desc_idx = unsafe { read_volatile(elem_ptr) };
        let len = unsafe { read_volatile(elem_ptr.add(1)) } as usize;

        if len <= HDR_SIZE {
            self.rx_last_used = self.rx_last_used.wrapping_add(1);
            return Err(NetError::HardwareError);
        }

        let payload_len = len - HDR_SIZE;
        if buf.len() < payload_len {
            return Err(NetError::NoBuffer);
        }

        let rx_buf = self.rx_bufs + (slot * BUFFER_SIZE) as u64;
        unsafe {
            core::ptr::copy_nonoverlapping((rx_buf + HDR_SIZE as u64) as *const u8, buf.as_mut_ptr(), payload_len);
        }

        // Re-publish descriptor to available ring
        let avail_ring = (self.rx_ring + self.rx_layout.avail as u64) as *mut u16;
        unsafe {
            let cur_idx = read_volatile(avail_ring.add(1));
            let ring_slot = 2 + (cur_idx % QUEUE_SIZE) as usize;
            write_volatile(avail_ring.add(ring_slot), slot as u16);
            fence(Ordering::SeqCst);
            write_volatile(avail_ring.add(1), cur_idx.wrapping_add(1));
            fence(Ordering::SeqCst);
        }
        self.notify_queue(self.rx_notify_off, 0);

        self.rx_last_used = self.rx_last_used.wrapping_add(1);
        Ok(payload_len)
    }
}

pub fn init(dev: &pci::Device) -> Result<VirtioNet, NetError> {
    // Enable Memory Space and Bus Mastering
    let cmd = pci::config_read(dev.bus, dev.dev, dev.func, 0x04);
    pci::config_write(dev.bus, dev.dev, dev.func, 0x04, (cmd & 0xFFFF) | 0x06);

    let caps = virtio::find_caps(|off| pci::config_read(dev.bus, dev.dev, dev.func, off));
    let common_loc = caps.common.ok_or(NetError::HardwareError)?;
    let notify_loc = caps.notify.ok_or(NetError::HardwareError)?;
    let dev_loc = caps.device.ok_or(NetError::HardwareError)?;

    let common_bar = pci::memory_bar(dev, common_loc.bar).ok_or(NetError::HardwareError)?;
    let notify_bar = pci::memory_bar(dev, notify_loc.bar).ok_or(NetError::HardwareError)?;
    let dev_bar = pci::memory_bar(dev, dev_loc.bar).ok_or(NetError::HardwareError)?;

    for b in [common_bar, notify_bar, dev_bar] {
        if !paging::map_mmio_uc(b) {
            return Err(NetError::HardwareError);
        }
    }

    let common = common_bar + u64::from(common_loc.offset);
    let notify = notify_bar + u64::from(notify_loc.offset);
    let dev_cfg = dev_bar + u64::from(dev_loc.offset);

    // Read MAC address from device config
    let mut mac = [0u8; 6];
    for (i, slot) in mac.iter_mut().enumerate() {
        *slot = rd8(dev_cfg + i as u64);
    }
    if mac == [0; 6] || mac == [0xFF; 6] {
        mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x57];
    }

    // VirtIO Reset Sequence
    wr8(common + REG_DEVICE_STATUS, 0);
    wait_until(10, || rd8(common + REG_DEVICE_STATUS) == 0);

    wr8(common + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

    wr8(common + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);
    if rd8(common + REG_DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
        wr8(common + REG_DEVICE_STATUS, STATUS_FAILED);
        return Err(NetError::HardwareError);
    }

    // Configure RX Queue (0)
    wr16(common + REG_QUEUE_SELECT, 0);
    let rx_max = rd16(common + REG_QUEUE_SIZE);
    if rx_max == 0 {
        return Err(NetError::HardwareError);
    }
    wr16(common + REG_QUEUE_SIZE, QUEUE_SIZE);
    let rx_notify_off = rd16(common + REG_QUEUE_NOTIFY_OFF);

    let rx_layout = virtio::ring_layout(QUEUE_SIZE);
    let rx_ring = dma_frames(1, false).map_err(|_| NetError::NoBuffer)?;
    let rx_bufs = dma_frames(4, false).map_err(|_| NetError::NoBuffer)?;

    wr64(common + REG_QUEUE_DESC, rx_ring);
    wr64(common + REG_QUEUE_DRIVER, rx_ring + rx_layout.avail as u64);
    wr64(common + REG_QUEUE_DEVICE, rx_ring + rx_layout.used as u64);
    wr16(common + REG_QUEUE_ENABLE, 1);

    // Populate initial RX descriptors
    let rx_desc_table = rx_ring as *mut VqDesc;
    let rx_avail_ring = (rx_ring + rx_layout.avail as u64) as *mut u16;
    for i in 0..QUEUE_SIZE {
        let desc = VqDesc {
            addr: rx_bufs + (u64::from(i) * BUFFER_SIZE as u64),
            len: BUFFER_SIZE as u32,
            flags: DESC_F_WRITE,
            next: 0,
        };
        unsafe {
            write_volatile(rx_desc_table.add(usize::from(i)), desc);
            write_volatile(rx_avail_ring.add(2 + usize::from(i)), i);
        }
    }
    unsafe {
        write_volatile(rx_avail_ring.add(1), QUEUE_SIZE);
    }

    // Configure TX Queue (1)
    wr16(common + REG_QUEUE_SELECT, 1);
    let tx_max = rd16(common + REG_QUEUE_SIZE);
    if tx_max == 0 {
        return Err(NetError::HardwareError);
    }
    wr16(common + REG_QUEUE_SIZE, QUEUE_SIZE);
    let tx_notify_off = rd16(common + REG_QUEUE_NOTIFY_OFF);

    let tx_layout = virtio::ring_layout(QUEUE_SIZE);
    let tx_ring = dma_frames(1, false).map_err(|_| NetError::NoBuffer)?;
    let tx_bufs = dma_frames(4, false).map_err(|_| NetError::NoBuffer)?;

    wr64(common + REG_QUEUE_DESC, tx_ring);
    wr64(common + REG_QUEUE_DRIVER, tx_ring + tx_layout.avail as u64);
    wr64(common + REG_QUEUE_DEVICE, tx_ring + tx_layout.used as u64);
    wr16(common + REG_QUEUE_ENABLE, 1);

    // Set DRIVER_OK
    let st = rd8(common + REG_DEVICE_STATUS);
    wr8(common + REG_DEVICE_STATUS, st | STATUS_DRIVER_OK);

    let driver = VirtioNet {
        _common: common,
        notify,
        notify_mult: caps.notify_mult,
        rx_notify_off,
        tx_notify_off,
        mac,
        rx_ring,
        rx_layout,
        rx_bufs,
        rx_last_used: 0,
        tx_ring,
        tx_layout,
        tx_bufs,
        tx_avail_idx: 0,
        tx_last_used: 0,
    };

    // Kick RX queue to start listening
    driver.notify_queue(rx_notify_off, 0);

    Ok(driver)
}
