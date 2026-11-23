//! Network device hardware abstraction layer.
//!
//! Provides a unified interface for physical and virtual network interface cards:
//! - Intel e1000 family (82540EM, 82545EM, 82574L)
//! - VirtIO-Net (virtio 1.0 modern and legacy PCI)
//! - Realtek RTL8168/8111 (HP OMEN wired port; untested on real hardware)
//! - Software Loopback (for self-contained local testing)

use crate::spin::SpinLock;
#[cfg(target_os = "uefi")]
use crate::{info, pci, warn};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    DeviceNotFound,
    NoBuffer,
    PacketTooLarge,
    RxEmpty,
    TxFull,
    LinkDown,
    HardwareError,
    Timeout,
}

pub trait NetDev {
    fn name(&self) -> &'static str;
    fn mac_address(&self) -> [u8; 6];
    fn link_up(&self) -> bool;
    fn send(&mut self, packet: &[u8]) -> Result<(), NetError>;
    fn receive(&mut self, buf: &mut [u8]) -> Result<usize, NetError>;
}

/// Software loopback device for testing network stack without hardware.
pub const LOOPBACK_MTU: usize = 1514;
const LOOPBACK_QUEUE_LEN: usize = 16;

pub struct LoopbackDev {
    mac: [u8; 6],
    queue: [[u8; LOOPBACK_MTU]; LOOPBACK_QUEUE_LEN],
    lengths: [usize; LOOPBACK_QUEUE_LEN],
    head: usize,
    tail: usize,
    count: usize,
}

impl LoopbackDev {
    pub const fn new() -> Self {
        Self {
            mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            queue: [[0u8; LOOPBACK_MTU]; LOOPBACK_QUEUE_LEN],
            lengths: [0usize; LOOPBACK_QUEUE_LEN],
            head: 0,
            tail: 0,
            count: 0,
        }
    }
}

impl Default for LoopbackDev {
    fn default() -> Self {
        Self::new()
    }
}

impl NetDev for LoopbackDev {
    fn name(&self) -> &'static str {
        "loopback"
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn link_up(&self) -> bool {
        true
    }

    fn send(&mut self, packet: &[u8]) -> Result<(), NetError> {
        if packet.len() > LOOPBACK_MTU {
            return Err(NetError::PacketTooLarge);
        }
        if self.count >= LOOPBACK_QUEUE_LEN {
            return Err(NetError::TxFull);
        }
        self.queue[self.tail][..packet.len()].copy_from_slice(packet);
        self.lengths[self.tail] = packet.len();
        self.tail = (self.tail + 1) % LOOPBACK_QUEUE_LEN;
        self.count += 1;
        Ok(())
    }

    fn receive(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        if self.count == 0 {
            return Err(NetError::RxEmpty);
        }
        let len = self.lengths[self.head];
        if buf.len() < len {
            return Err(NetError::NoBuffer);
        }
        buf[..len].copy_from_slice(&self.queue[self.head][..len]);
        self.head = (self.head + 1) % LOOPBACK_QUEUE_LEN;
        self.count -= 1;
        Ok(len)
    }
}

pub struct NetStats {
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
}

static STATS: SpinLock<NetStats> = SpinLock::new(NetStats {
    rx_packets: 0,
    tx_packets: 0,
    rx_bytes: 0,
    tx_bytes: 0,
    rx_errors: 0,
    tx_errors: 0,
});

#[allow(clippy::large_enum_variant)]
pub enum ActiveNetDev {
    None,
    Loopback(LoopbackDev),
    #[cfg(target_os = "uefi")]
    E1000(crate::e1000::E1000),
    #[cfg(target_os = "uefi")]
    Virtio(crate::virtionet::VirtioNet),
    #[cfg(target_os = "uefi")]
    Rtl8168(crate::rtl8168::Rtl8168),
    #[cfg(target_os = "uefi")]
    UsbNet(UsbNetDev),
}

/// The USB network function (a phone's USB tethering, an RNDIS/CDC-ECM adapter): the xHCI driver owns the
/// hardware, this is only the handle the IP stack talks to.
#[cfg(target_os = "uefi")]
pub struct UsbNetDev;

#[cfg(target_os = "uefi")]
impl NetDev for UsbNetDev {
    fn name(&self) -> &'static str {
        crate::xhci::net_name()
    }
    fn mac_address(&self) -> [u8; 6] {
        crate::xhci::net_mac()
    }
    fn link_up(&self) -> bool {
        crate::xhci::net_present()
    }
    fn send(&mut self, packet: &[u8]) -> Result<(), NetError> {
        crate::xhci::net_send(packet)
    }
    fn receive(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        crate::xhci::net_receive(buf)
    }
}

static ACTIVE_DEV: SpinLock<ActiveNetDev> = SpinLock::new(ActiveNetDev::None);

pub fn is_active() -> bool {
    let dev = ACTIVE_DEV.lock();
    !matches!(*dev, ActiveNetDev::None)
}

pub fn device_name() -> &'static str {
    let dev = ACTIVE_DEV.lock();
    match &*dev {
        ActiveNetDev::None => "none",
        ActiveNetDev::Loopback(d) => d.name(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::E1000(d) => d.name(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Virtio(d) => d.name(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Rtl8168(d) => d.name(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::UsbNet(d) => d.name(),
    }
}

pub fn mac_address() -> [u8; 6] {
    let dev = ACTIVE_DEV.lock();
    match &*dev {
        ActiveNetDev::None => [0; 6],
        ActiveNetDev::Loopback(d) => d.mac_address(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::E1000(d) => d.mac_address(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Virtio(d) => d.mac_address(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Rtl8168(d) => d.mac_address(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::UsbNet(d) => d.mac_address(),
    }
}

pub fn link_up() -> bool {
    let dev = ACTIVE_DEV.lock();
    match &*dev {
        ActiveNetDev::None => false,
        ActiveNetDev::Loopback(d) => d.link_up(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::E1000(d) => d.link_up(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Virtio(d) => d.link_up(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Rtl8168(d) => d.link_up(),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::UsbNet(d) => d.link_up(),
    }
}

/// What the active network device is and, for the Realtek chip, the registers that explain a dead link.
#[cfg(target_os = "uefi")]
pub fn diagnostics() -> alloc::string::String {
    let mac = mac_address();
    let mut s = alloc::format!("network device: {} mac {:02x?} link {}\n", device_name(), mac, if link_up() { "up" } else { "down" });
    if let ActiveNetDev::Rtl8168(d) = &*ACTIVE_DEV.lock() {
        s.push_str(&d.diag());
    }
    s
}

pub fn send_packet(packet: &[u8]) -> Result<(), NetError> {
    let mut dev = ACTIVE_DEV.lock();
    let res = match &mut *dev {
        ActiveNetDev::None => Err(NetError::DeviceNotFound),
        ActiveNetDev::Loopback(d) => d.send(packet),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::E1000(d) => d.send(packet),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Virtio(d) => d.send(packet),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Rtl8168(d) => d.send(packet),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::UsbNet(d) => d.send(packet),
    };
    let mut stats = STATS.lock();
    match res {
        Ok(()) => {
            stats.tx_packets += 1;
            stats.tx_bytes += packet.len() as u64;
        }
        Err(e) if e != NetError::RxEmpty => {
            stats.tx_errors += 1;
        }
        _ => {}
    }
    res
}

pub fn receive_packet(buf: &mut [u8]) -> Result<usize, NetError> {
    let mut dev = ACTIVE_DEV.lock();
    let res = match &mut *dev {
        ActiveNetDev::None => Err(NetError::DeviceNotFound),
        ActiveNetDev::Loopback(d) => d.receive(buf),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::E1000(d) => d.receive(buf),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Virtio(d) => d.receive(buf),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::Rtl8168(d) => d.receive(buf),
        #[cfg(target_os = "uefi")]
        ActiveNetDev::UsbNet(d) => d.receive(buf),
    };
    let mut stats = STATS.lock();
    match res {
        Ok(len) => {
            stats.rx_packets += 1;
            stats.rx_bytes += len as u64;
        }
        Err(e) if e != NetError::RxEmpty => {
            stats.rx_errors += 1;
        }
        _ => {}
    }
    res
}

pub fn stats() -> (u64, u64, u64, u64, u64, u64) {
    let s = STATS.lock();
    (s.rx_packets, s.rx_bytes, s.rx_errors, s.tx_packets, s.tx_bytes, s.tx_errors)
}

/// Probes the PCI bus for network controllers and initializes the active network device.
#[cfg(target_os = "uefi")]
pub fn probe() {
    let mut chosen_e1000: Option<pci::Device> = None;
    let mut chosen_virtio: Option<pci::Device> = None;
    let mut chosen_rtl: Option<pci::Device> = None;

    pci::for_each(|d| {
        let (vendor, device) = (d.header.vendor, d.header.device);
        // Intel e1000 family: 82540EM (0x100E), 82545EM (0x100F), 82543GC (0x1004), 82574L (0x10D3), 82567LM (0x10DE), etc.
        if vendor == 0x8086 && (device == 0x100E || device == 0x100F || device == 0x1004 || device == 0x1010 || device == 0x10D3)
            && chosen_e1000.is_none() {
            chosen_e1000 = Some(*d);
        }
        // Realtek RTL8168/8111 (the OMEN's wired port).
        if crate::rtl8168::is_realtek_gbe(vendor, device) && chosen_rtl.is_none() {
            chosen_rtl = Some(*d);
        }
        // VirtIO network adapter
        if vendor == 0x1AF4 && (device == 0x1000 || device == 0x1041)
            && chosen_virtio.is_none() {
            chosen_virtio = Some(*d);
        }
    });

    if let Some(dev) = chosen_e1000 {
        match crate::e1000::init(&dev) {
            Ok(driver) => {
                let mac = driver.mac_address();
                info!(
                    "net",
                    "bound Intel e1000 ({:04x}:{:04x}) MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    dev.header.vendor,
                    dev.header.device,
                    mac[0],
                    mac[1],
                    mac[2],
                    mac[3],
                    mac[4],
                    mac[5]
                );
                *ACTIVE_DEV.lock() = ActiveNetDev::E1000(driver);
                return;
            }
            Err(e) => {
                warn!("net", "failed to initialize Intel e1000: {:?}", e);
            }
        }
    }

    if let Some(dev) = chosen_rtl {
        match crate::rtl8168::init(&dev) {
            Ok(driver) => {
                let mac = driver.mac_address();
                let xid = driver.xid();
                info!(
                    "net",
                    "bound Realtek RTL8168 ({:04x}:{:04x} rev {:02x}) xid={:03x} ({}) MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}; untested on hardware",
                    dev.header.vendor,
                    dev.header.device,
                    dev.header.revision,
                    xid,
                    crate::rtl8168::chip_name(xid).unwrap_or("unknown variant"),
                    mac[0],
                    mac[1],
                    mac[2],
                    mac[3],
                    mac[4],
                    mac[5]
                );
                *ACTIVE_DEV.lock() = ActiveNetDev::Rtl8168(driver);
                return;
            }
            Err(e) => {
                warn!("net", "failed to initialize Realtek RTL8168: {:?}", e);
            }
        }
    }

    if let Some(dev) = chosen_virtio {
        match crate::virtionet::init(&dev) {
            Ok(driver) => {
                let mac = driver.mac_address();
                info!(
                    "net",
                    "bound VirtIO-Net ({:04x}:{:04x}) MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    dev.header.vendor,
                    dev.header.device,
                    mac[0],
                    mac[1],
                    mac[2],
                    mac[3],
                    mac[4],
                    mac[5]
                );
                *ACTIVE_DEV.lock() = ActiveNetDev::Virtio(driver);
                return;
            }
            Err(e) => {
                warn!("net", "failed to initialize VirtIO-Net: {:?}", e);
            }
        }
    }

    if crate::xhci::net_present() {
        info!("net", "bound {} (USB network function) MAC {:02x?}", crate::xhci::net_name(), crate::xhci::net_mac());
        *ACTIVE_DEV.lock() = ActiveNetDev::UsbNet(UsbNetDev);
        return;
    }

    info!("net", "no hardware NIC detected; falling back to software loopback");
    *ACTIVE_DEV.lock() = ActiveNetDev::Loopback(LoopbackDev::new());
}

#[cfg(not(target_os = "uefi"))]
pub fn probe() {
    *ACTIVE_DEV.lock() = ActiveNetDev::Loopback(LoopbackDev::new());
}

/// Makes sure the best available network device is bound: if only the software loopback is active, looks again
/// for a USB network function (a phone plugged in after boot) and re-probes.
#[cfg(target_os = "uefi")]
pub fn ensure() {
    let only_loopback = matches!(*ACTIVE_DEV.lock(), ActiveNetDev::Loopback(_) | ActiveNetDev::None);
    if only_loopback {
        crate::xhci::rescan();
        probe();
    }
}

#[cfg(not(target_os = "uefi"))]
pub fn ensure() {}
