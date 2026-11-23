//! xHCI USB host controller driver (polling, no interrupts) for boot-protocol keyboards.
//!
//! `init` finds the controller on the PCI bus, takes it over from the firmware, resets and starts it,
//! then looks at every connected port: reset the port, enable a device slot, give the device an address,
//! read its descriptors, and when it is a boot keyboard switch it to the boot protocol and start polling
//! its interrupt endpoint. `poll_key` hands the keys to the shell. Register decoding (`xhcireg`),
//! rings (`xhcitrb`), contexts (`xhcictx`), descriptors (`usbdesc`) and key translation (`hidkbd`) are pure
//! and host-tested; this file only does the hardware access.
//!
//! Every wait has a timeout, and any failure leaves the controller unused: the PS/2 keyboard path does
//! not depend on this driver. Not handled: hubs, hot plug (devices must be present at boot), more than
//! one keyboard.
//!
//! Global state: `XHCI` follows rule A in `docs/design/globals.md`: only boot code (`init`) and the shell
//! task touch it, never an interrupt handler.

use core::ptr::read_volatile;

use crate::bcache::BlockCache;
use crate::block::{check_request, BlockDev, BlockError, BlockWrite, SECTOR_SIZE};
use crate::global::Global;
use crate::hidkbd::HidKeyboard;
use crate::pointer::{self, MouseMotion};
use crate::usbnet;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use crate::keymap::Key;
use crate::mmio::{dma_frames, dma_page, rd32, wait_until, wr32, wr64, PAGE_BYTES};
use crate::usbdesc;
use crate::usbmsd::{self, Cbw, Csw, CswStatus};
use crate::xhcictx::{self, Endpoint};
use crate::xhcireg::{self as reg, Caps};
use crate::xhcitrb::{EventRing, ProducerRing, Trb, CC_SHORT_PACKET, CC_SUCCESS, TRB_COMMAND_COMPLETION, TRB_TRANSFER_EVENT};
use crate::{arch, log, paging, pci};

/// TRBs per ring: one 4 KiB frame holds 256 of 16 bytes.
const RING_TRBS: usize = 256;
/// Most device slots this driver asks the controller for.
const MAX_SLOTS: u8 = 32;
/// Longest configuration descriptor that is read (it must fit the one-page buffer).
const MAX_CONFIG_DESC: u16 = 1024;
/// Bounce buffer size in 4 KiB frames for bulk transfers (64 KiB = 128 sectors).
const BOUNCE_FRAMES: usize = 16;
const MAX_SECTORS: usize = BOUNCE_FRAMES * PAGE_BYTES / SECTOR_SIZE;

// ---- register and memory access (shared helpers are in `mmio.rs`) ----

/// The bytes of a DMA page.
///
/// # Safety
/// `page` must come from `dma_page`, and the controller must not be writing it while the slice is used
/// (true for input contexts and descriptor buffers between commands).
unsafe fn page_bytes(page: u64) -> &'static mut [u8] {
    // SAFETY: the caller's contract: a live, never-freed 4 KiB frame.
    unsafe { core::slice::from_raw_parts_mut(page as *mut u8, PAGE_BYTES) }
}

/// A transfer or command ring in a fresh DMA page.
fn new_ring(ac64: bool) -> Result<(u64, ProducerRing<'static>), &'static str> {
    let phys = dma_page(ac64)?;
    // SAFETY: a zeroed frame nobody else uses; it is never freed, so the slice is valid for the rest of the run.
    let mem: &'static mut [Trb] = unsafe { core::slice::from_raw_parts_mut(phys as *mut Trb, RING_TRBS) };
    let ring = ProducerRing::new(mem, phys).ok_or("ring too small")?;
    Ok((phys, ring))
}

// ---- the controller ----

/// Keys waiting for the shell (a report can carry several presses).
struct KeyQueue {
    items: [Option<Key>; 16],
    head: usize,
    len: usize,
}

impl KeyQueue {
    const fn new() -> Self {
        Self { items: [None; 16], head: 0, len: 0 }
    }

    fn push(&mut self, key: Key) {
        if self.len < self.items.len() {
            let at = (self.head + self.len) % self.items.len();
            self.items[at] = Some(key);
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<Key> {
        if self.len == 0 {
            return None;
        }
        let key = self.items[self.head].take();
        self.head = (self.head + 1) % self.items.len();
        self.len -= 1;
        key
    }
}

struct Keyboard {
    port: u8,
    slot: u8,
    dci: u8,
    ring: ProducerRing<'static>,
    /// Physical address of the 8-byte report buffer.
    report: u64,
    hid: HidKeyboard,
    queue: KeyQueue,
    errors: u8,
}

/// A USB network function (RNDIS or CDC-ECM): bulk IN is kept armed, frames wait in `rx` for the IP stack.
struct Net {
    port: u8,
    slot: u8,
    kind: usbnet::Kind,
    dci_in: u8,
    dci_out: u8,
    in_ring: ProducerRing<'static>,
    out_ring: ProducerRing<'static>,
    in_buf: u64,
    out_buf: u64,
    max_packet_out: usize,
    mac: [u8; 6],
    rx: VecDeque<Vec<u8>>,
    errors: u8,
}

/// Size of the bulk IN buffer: an RNDIS transfer may bundle several frames (we asked for up to 16 KiB).
const NET_IN_BUF: u32 = 16 * 1024;
const NET_OUT_BUF: usize = 4096;
/// A USB network function with this many errors in a row (or a send that never finished) is not used any more.
const NET_MAX_ERRORS: u8 = 16;

struct Mouse {
    port: u8,
    slot: u8,
    dci: u8,
    ring: ProducerRing<'static>,
    /// Physical address of the report buffer.
    report: u64,
    /// Movement since the last `poll_mouse`.
    motion: MouseMotion,
    dirty: bool,
    errors: u8,
}

/// A device that has a slot and an address.
struct Device {
    slot: u8,
    ep0: ProducerRing<'static>,
    /// Physical address of the page used for descriptors.
    buf: u64,
}

struct Storage {
    port: u8,
    slot: u8,
    interface: u8,
    dci_in: u8,
    dci_out: u8,
    ep_in_addr: u8,
    ep_out_addr: u8,
    ep0: ProducerRing<'static>,
    ep0_buf: u64,
    in_ring: ProducerRing<'static>,
    out_ring: ProducerRing<'static>,
    cbw_buf: u64,
    bounce: u64,
    sectors: u64,
    tag: u32,
    vendor: [u8; 8],
    product: [u8; 16],
}

pub struct StorageInfo {
    pub slot: u8,
    pub sectors: u64,
    pub vendor: [u8; 8],
    pub product: [u8; 16],
}

impl StorageInfo {
    pub fn vendor_str(&self) -> &str {
        core::str::from_utf8(&self.vendor).unwrap_or("?").trim()
    }

    pub fn product_str(&self) -> &str {
        core::str::from_utf8(&self.product).unwrap_or("?").trim()
    }
}

pub struct XhciDisk;

impl BlockDev for XhciDisk {
    fn sectors(&self) -> u64 {
        // SAFETY: rule A: only shell task touches XHCI.
        let x = unsafe { XHCI.get() }.as_ref();
        x.and_then(|x| x.storage.as_ref()).map_or(0, |s| s.sectors)
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let sectors = self.sectors();
        check_request(sectors, lba, buf.len())?;
        let mut cur_lba = lba;
        let mut offset = 0;
        while offset < buf.len() {
            let rem_sectors = ((buf.len() - offset) / SECTOR_SIZE) as u64;
            let chunk_sectors = rem_sectors.min(MAX_SECTORS as u64) as u16;
            let chunk_bytes = usize::from(chunk_sectors) * SECTOR_SIZE;

            let lba32 = u32::try_from(cur_lba).map_err(|_| BlockError::BadRequest)?;
            // SAFETY: rule A.
            let x = unsafe { XHCI.get() }.as_mut().ok_or(BlockError::NoDevice)?;
            let tag = x.next_storage_tag()?;
            let cbw = usbmsd::scsi_read_10(tag, 0, lba32, chunk_sectors, SECTOR_SIZE as u32)
                .map_err(|_| BlockError::BadRequest)?;
            let bounce = x.storage.as_ref().ok_or(BlockError::NoDevice)?.bounce;
            x.storage_transfer(&cbw, bounce)?;

            // SAFETY: `bounce` holds `chunk_bytes` freshly read from the device; `buf` has at least `offset + chunk_bytes`.
            unsafe {
                core::ptr::copy_nonoverlapping(bounce as *const u8, buf.as_mut_ptr().add(offset), chunk_bytes);
            }
            cur_lba += u64::from(chunk_sectors);
            offset += chunk_bytes;
        }
        Ok(())
    }
}

impl BlockWrite for XhciDisk {
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        let sectors = self.sectors();
        check_request(sectors, lba, buf.len())?;
        let mut cur_lba = lba;
        let mut offset = 0;
        while offset < buf.len() {
            let rem_sectors = ((buf.len() - offset) / SECTOR_SIZE) as u64;
            let chunk_sectors = rem_sectors.min(MAX_SECTORS as u64) as u16;
            let chunk_bytes = usize::from(chunk_sectors) * SECTOR_SIZE;

            let lba32 = u32::try_from(cur_lba).map_err(|_| BlockError::BadRequest)?;
            // SAFETY: rule A.
            let x = unsafe { XHCI.get() }.as_mut().ok_or(BlockError::NoDevice)?;
            let tag = x.next_storage_tag()?;
            let cbw = usbmsd::scsi_write_10(tag, 0, lba32, chunk_sectors, SECTOR_SIZE as u32)
                .map_err(|_| BlockError::BadRequest)?;
            let bounce = x.storage.as_ref().ok_or(BlockError::NoDevice)?.bounce;

            // SAFETY: `buf` has at least `offset + chunk_bytes`; `bounce` has at least `chunk_bytes` capacity.
            unsafe {
                core::ptr::copy_nonoverlapping(buf.as_ptr().add(offset), bounce as *mut u8, chunk_bytes);
            }
            x.storage_transfer(&cbw, bounce)?;

            cur_lba += u64::from(chunk_sectors);
            offset += chunk_bytes;
        }
        Ok(())
    }
}

pub struct Xhci {
    vendor: u16,
    device: u16,
    caps: Caps,
    slots: u8,
    /// Operational registers, doorbells and runtime registers.
    op: u64,
    db: u64,
    rt: u64,
    dcbaa: u64,
    handoff: &'static str,
    cmd: ProducerRing<'static>,
    events: EventRing,
    event_mem: u64,
    keyboard: Option<Keyboard>,
    mouse: Option<Mouse>,
    net: Option<Net>,
    storage: Option<Storage>,
}

static XHCI: Global<Option<Xhci>> = Global::new(None);
static USB_CACHE: Global<Option<BlockCache<XhciDisk, 64>>> = Global::new(None);

/// Asks the firmware to hand the controller over (USB legacy support capability).
fn take_over_from_firmware(base: u64, caps: &Caps) -> &'static str {
    let mut at = u64::from(caps.xecp_dwords) * 4;
    if at == 0 {
        return "no extended capabilities";
    }
    for _ in 0..64 {
        if at >= 0x1_0000 {
            break; // outside the window that was mapped
        }
        let header = rd32(base + at);
        let (id, next) = reg::xecp_header(header);
        if id == reg::LEGACY_CAP_ID {
            if header & reg::LEGACY_BIOS_OWNED == 0 {
                return "firmware does not own it";
            }
            wr32(base + at, header | reg::LEGACY_OS_OWNED);
            let released = wait_until(1000, || rd32(base + at) & reg::LEGACY_BIOS_OWNED == 0);
            return if released { "taken over from the firmware" } else { "firmware did not let go, continuing" };
        }
        if next == 0 {
            break;
        }
        at += u64::from(next) * 4;
    }
    "no legacy support capability"
}

fn bring_up(d: &pci::Device) -> Result<Xhci, &'static str> {
    // Memory space and bus mastering on.
    let command = pci::config_read(d.bus, d.dev, d.func, 4);
    pci::config_write(d.bus, d.dev, d.func, 4, (command & 0xFFFF) | 0x6);
    let base = pci::memory_bar(d, 0).ok_or("BAR 0 is not a memory BAR")?;
    if !(paging::map_mmio_uc(base) && paging::map_mmio_uc(base + 0xFFFF)) {
        return Err("cannot map the registers");
    }
    let caps = Caps::decode(rd32(base), rd32(base + 4), rd32(base + 8), rd32(base + 0x10), rd32(base + 0x14), rd32(base + 0x18));
    if caps.cap_length < 0x20 || caps.max_slots == 0 || caps.max_ports == 0 || caps.max_ports > 127 {
        return Err("capability registers look wrong");
    }
    let op = base + u64::from(caps.cap_length);
    let rt = base + u64::from(caps.runtime_offset);
    let db = base + u64::from(caps.doorbell_offset);
    let handoff = take_over_from_firmware(base, &caps);

    // Stop the controller if it runs, then reset it.
    if rd32(op) & 1 != 0 {
        wr32(op, rd32(op) & !1);
        if !wait_until(100, || rd32(op + 4) & 1 != 0) {
            return Err("controller would not stop");
        }
    }
    wr32(op, rd32(op) | 2);
    if !wait_until(1000, || rd32(op) & 2 == 0 && rd32(op + 4) & (1 << 11) == 0) {
        return Err("controller reset did not finish");
    }

    let slots = caps.max_slots.min(MAX_SLOTS);
    wr32(op + 0x38, u32::from(slots));

    // Device context base address array, with the scratchpad buffers the controller asked for.
    let dcbaa = dma_page(caps.ac64)?;
    if caps.scratchpads > 0 {
        let n = usize::from(caps.scratchpads);
        if n > PAGE_BYTES / 8 {
            return Err("too many scratchpad buffers");
        }
        let array = dma_page(caps.ac64)?;
        for i in 0..n {
            wr64(array + 8 * i as u64, dma_page(caps.ac64)?);
        }
        wr64(dcbaa, array);
    }
    wr64(op + 0x30, dcbaa);

    // Command ring (ring cycle state 1).
    let (cmd_phys, cmd) = new_ring(caps.ac64)?;
    wr64(op + 0x18, cmd_phys | 1);

    // Event ring with a one-entry segment table on interrupter 0.
    let event_mem = dma_page(caps.ac64)?;
    let table = dma_page(caps.ac64)?;
    wr64(table, event_mem);
    wr32(table + 8, RING_TRBS as u32);
    wr32(rt + 0x28, 1); // ERSTSZ
    wr64(rt + 0x38, event_mem); // ERDP
    wr64(rt + 0x30, table); // ERSTBA, written last: it starts the ring
    let events = EventRing::new(RING_TRBS, event_mem).ok_or("event ring too small")?;

    // Run.
    wr32(op, rd32(op) | 1);
    if !wait_until(100, || rd32(op + 4) & 1 == 0) {
        return Err("controller did not start");
    }
    if rd32(op + 4) & (1 << 2) != 0 {
        return Err("host system error after start");
    }
    Ok(Xhci { vendor: d.header.vendor, device: d.header.device, caps, slots, op, db, rt, dcbaa, handoff, cmd, events, event_mem, keyboard: None, mouse: None, net: None, storage: None })
}

impl Xhci {
    fn port_reg(&self, port: u8) -> u64 {
        self.op + 0x400 + 0x10 * u64::from(port - 1)
    }

    /// The next event the controller has posted, if any; tells the controller how far we have read.
    fn try_event(&mut self) -> Option<Trb> {
        // SAFETY: `event_mem` is the event ring frame from `bring_up`, never freed. The controller writes
        // it by DMA; `EventRing::next` reads it with volatile loads.
        let mem = unsafe { core::slice::from_raw_parts(self.event_mem as *const Trb, RING_TRBS) };
        let event = self.events.next(mem)?;
        // Bit 3 clears the "event handler busy" flag.
        wr64(self.rt + 0x38, self.events.dequeue_pointer() | 1 << 3);
        Some(event)
    }

    /// Waits for a command completion (`kind` 33) or a transfer event (`kind` 32) of `slot` and endpoint
    /// `ep`. Transfer events of the keyboard that arrive meanwhile are handled, others are dropped.
    fn wait_for(&mut self, kind: u8, slot: u8, ep: u8, ms: u64) -> Option<Trb> {
        let t0 = arch::rdtsc();
        let ticks = log::tsc_hz() / 1000 * ms;
        loop {
            while let Some(event) = self.try_event() {
                if event.kind() == kind && (kind != TRB_TRANSFER_EVENT || (event.slot_id() == slot && event.endpoint_id() == ep)) {
                    return Some(event);
                }
                if event.kind() == TRB_TRANSFER_EVENT {
                    self.on_hid_event(&event);
                }
            }
            if arch::rdtsc().wrapping_sub(t0) >= ticks {
                return None;
            }
            arch::pause();
        }
    }

    /// Runs one command and returns its completion event.
    fn command(&mut self, trb: Trb) -> Result<Trb, &'static str> {
        self.cmd.enqueue(trb).ok_or("command ring full")?;
        wr32(self.db, 0);
        let event = self.wait_for(TRB_COMMAND_COMPLETION, 0, 0, 1000).ok_or("command timed out")?;
        self.cmd.retire();
        if event.completion_code() != CC_SUCCESS {
            warn!("xhci", "command type {} failed with completion code {}", trb.kind(), event.completion_code());
            return Err("command failed");
        }
        Ok(event)
    }

    /// Resetting the controller drops every link. USB 3 devices (an external hard disk, say) need a few hundred
    /// milliseconds to train their link again and USB 2 devices are debounced, so scanning the ports right away
    /// finds nothing and the drive is never set up. This waits until the set of connected ports has stopped
    /// changing for 300 ms (at most 2 s; at most 1 s if nothing has shown up at all).
    fn settle_ports(&self) {
        let connected = |x: &Self| (1..=x.caps.max_ports).filter(|&p| rd32(x.port_reg(p)) & reg::PORT_CONNECTED != 0).fold(0u128, |m, p| m | 1u128 << (p - 1).min(127));
        let t0 = crate::log::uptime_us();
        let mut last = connected(self);
        let mut changed_at = t0;
        loop {
            wait_until(20, || false);
            let now = crate::log::uptime_us();
            let m = connected(self);
            if m != last {
                last = m;
                changed_at = now;
            }
            let total = now.saturating_sub(t0);
            if (m != 0 && now.saturating_sub(changed_at) >= 300_000) || (m == 0 && total >= 1_000_000) || total >= 2_000_000 {
                info!("xhci", "ports settled after {} ms, {} device(s) connected", total / 1000, m.count_ones());
                return;
            }
        }
    }

    /// Resets a connected port and returns its status once the device is enabled.
    fn reset_port(&self, port: u8) -> Option<u32> {
        let at = self.port_reg(port);
        let mut v = rd32(at);
        if v & reg::PORT_POWER == 0 {
            wr32(at, reg::port_neutral(v) | reg::PORT_POWER);
            wait_until(20, || false);
            v = rd32(at);
        }
        if v & reg::PORT_ENABLED == 0 {
            wr32(at, reg::port_reset_command(v));
            if !wait_until(500, || rd32(at) & reg::PORT_RESET_CHANGE != 0) {
                return None;
            }
            wr32(at, reg::port_ack_reset(rd32(at)));
        }
        // Acknowledge the connect change so it does not look new later.
        wr32(at, reg::port_neutral(rd32(at)) | reg::PORT_CONNECT_CHANGE);
        let v = rd32(at);
        (v & reg::PORT_ENABLED != 0 && v & reg::PORT_CONNECTED != 0).then_some(v)
    }

    /// One control transfer on endpoint 0 of `dev`. `data` is the length and direction (true = IN) of the
    /// data stage, which uses the device's descriptor buffer.
    fn control(&mut self, dev: &mut Device, packet: [u8; 8], data: Option<(u16, bool)>) -> Result<(), &'static str> {
        let in_data = data.map(|d| d.1);
        let mut trbs = 0;
        dev.ep0.enqueue(Trb::setup_stage(packet, in_data)).ok_or("control ring full")?;
        trbs += 1;
        if let Some((len, dir_in)) = data {
            dev.ep0.enqueue(Trb::data_stage(dev.buf, len, dir_in)).ok_or("control ring full")?;
            trbs += 1;
        }
        // The status stage runs the other way from the data stage (IN when there is no data stage).
        dev.ep0.enqueue(Trb::status_stage(in_data != Some(true))).ok_or("control ring full")?;
        trbs += 1;
        wr32(self.db + 4 * u64::from(dev.slot), 1);
        let event = self.wait_for(TRB_TRANSFER_EVENT, dev.slot, 1, 1000);
        for _ in 0..trbs {
            dev.ep0.retire();
        }
        match event.map(|e| e.completion_code()) {
            Some(CC_SUCCESS | CC_SHORT_PACKET) => Ok(()),
            Some(code) => {
                warn!("xhci", "control transfer on slot {} failed with completion code {}", dev.slot, code);
                Err("control transfer failed")
            }
            None => Err("control transfer timed out"),
        }
    }

    /// Switches a boot-protocol HID interface to the boot protocol, registers its interrupt IN endpoint with
    /// the controller and queues the first report read. Returns the endpoint's DCI, its ring and the physical
    /// address of the 8-byte report buffer.
    fn start_boot_hid(
        &mut self,
        dev: &mut Device,
        slot: u8,
        speed: u8,
        out_ctx: u64,
        in_ctx: u64,
        hid: &usbdesc::BootKeyboard,
    ) -> Result<(u8, ProducerRing<'static>, u64), &'static str> {
        let ac64 = self.caps.ac64;
        let csz = self.caps.context_size();
        // Configuration, boot protocol, and no idle repeats from the device.
        self.control(dev, [0x00, 9, hid.config, 0, 0, 0, 0, 0], None)?;
        self.control(dev, [0x21, 0x0B, 0, 0, hid.interface, 0, 0, 0], None)?;
        let _ = self.control(dev, [0x21, 0x0A, 0, 0, hid.interface, 0, 0, 0], None); // stalls on some devices: fine

        // Tell the controller about the interrupt IN endpoint.
        let dci = xhcictx::dci_in(hid.endpoint);
        let (ring_phys, mut ring) = new_ring(ac64)?;
        // SAFETY: `in_ctx` and `out_ctx` are pages from `dma_page`; no command is running.
        let (ctx, out) = unsafe { (page_bytes(in_ctx), page_bytes(out_ctx)) };
        ctx.fill(0);
        let ep = Endpoint {
            dci,
            ep_type: xhcictx::EP_INTERRUPT_IN,
            max_packet: hid.max_packet,
            interval: reg::interrupt_interval_field(speed, hid.interval),
            dequeue: ring_phys,
            avg_trb_len: 8,
        };
        if !(xhcictx::input_control(ctx, 0, 1 | 1 << dci) && xhcictx::copy_slot_from_output(out, ctx, csz) && xhcictx::set_slot_entries(ctx, csz, dci) && xhcictx::endpoint(ctx, csz, &ep)) {
            return Err("input context too small");
        }
        self.command(Trb::configure_endpoint(in_ctx, slot))?;

        let report = dma_page(ac64)?;
        ring.enqueue(Trb::normal(report, 8)).ok_or("interrupt ring full")?;
        wr32(self.db + 4 * u64::from(slot), u32::from(dci));
        Ok((dci, ring, report))
    }

    /// Brings up the device on `port`; when it is a boot keyboard or mouse, starts reading its reports.
    fn setup_device(&mut self, port: u8) -> Result<(), &'static str> {
        let portsc = self.reset_port(port).ok_or("port did not enable after reset")?;
        let speed = reg::port_speed(portsc);
        let slot = self.command(Trb::enable_slot())?.slot_id();
        if slot == 0 || slot > self.slots {
            return Err("controller returned a bad slot number");
        }
        let ac64 = self.caps.ac64;
        let csz = self.caps.context_size();
        let out_ctx = dma_page(ac64)?;
        let in_ctx = dma_page(ac64)?;
        let buf = dma_page(ac64)?;
        let (ep0_phys, ep0) = new_ring(ac64)?;
        wr64(self.dcbaa + 8 * u64::from(slot), out_ctx);
        let mut dev = Device { slot, ep0, buf };

        // Address the device with the default control endpoint.
        let assumed = reg::default_ep0_packet(speed);
        // SAFETY: `in_ctx` is a page from `dma_page`; the controller does not write it.
        let ctx = unsafe { page_bytes(in_ctx) };
        let ep0_ctx = Endpoint { dci: xhcictx::DCI_EP0, ep_type: xhcictx::EP_CONTROL, max_packet: assumed, interval: 0, dequeue: ep0_phys, avg_trb_len: 8 };
        if !(xhcictx::input_control(ctx, 0, 0b11) && xhcictx::slot(ctx, csz, speed, port, 1) && xhcictx::endpoint(ctx, csz, &ep0_ctx)) {
            return Err("input context too small");
        }
        self.command(Trb::address_device(in_ctx, slot, false))?;

        // The first 8 bytes of the device descriptor give the real packet size of endpoint 0.
        self.control(&mut dev, [0x80, 6, 0, 1, 0, 0, 8, 0], Some((8, true)))?;
        // SAFETY: `buf` is a page from `dma_page`; the transfer has finished.
        let b = unsafe { page_bytes(buf) };
        let real = if speed >= 4 { 1u16 << b[7].min(15) } else { u16::from(b[7]) };
        if real >= 8 && real != assumed {
            xhcictx::input_control(ctx, 0, 1 << 1);
            xhcictx::set_max_packet(ctx, csz, xhcictx::DCI_EP0, real);
            self.command(Trb::evaluate_context(in_ctx, slot))?;
        }

        // Configuration descriptor: its header tells the full length.
        self.control(&mut dev, [0x80, 6, 0, 2, 0, 0, 9, 0], Some((9, true)))?;
        // SAFETY: as above.
        let b = unsafe { page_bytes(buf) };
        let total = u16::from_le_bytes([b[2], b[3]]).clamp(9, MAX_CONFIG_DESC);
        let [lo, hi] = total.to_le_bytes();
        self.control(&mut dev, [0x80, 6, 0, 2, 0, 0, lo, hi], Some((total, true)))?;
        // SAFETY: as above.
        let b = unsafe { page_bytes(buf) };
        let desc_slice = &b[..usize::from(total)];
        if let Some(kb) = usbdesc::find_boot_keyboard(desc_slice) {
            if self.keyboard.is_some() {
                info!("xhci", "port {}: a second keyboard is ignored", port);
                return Ok(());
            }

            let (dci, ring, report) = self.start_boot_hid(&mut dev, slot, speed, out_ctx, in_ctx, &kb)?;
            self.keyboard = Some(Keyboard { port, slot, dci, ring, report, hid: HidKeyboard::new(), queue: KeyQueue::new(), errors: 0 });
            info!("xhci", "port {}: USB keyboard ready (slot {}, speed {})", port, slot, reg::speed_name(speed));
            Ok(())
        } else if let Some(mo) = usbdesc::find_boot_mouse(desc_slice) {
            if self.mouse.is_some() {
                info!("xhci", "port {}: a second mouse is ignored", port);
                return Ok(());
            }
            let (dci, ring, report) = self.start_boot_hid(&mut dev, slot, speed, out_ctx, in_ctx, &mo)?;
            self.mouse = Some(Mouse { port, slot, dci, ring, report, motion: MouseMotion::default(), dirty: false, errors: 0 });
            info!("xhci", "port {}: USB mouse ready (slot {}, speed {})", port, slot, reg::speed_name(speed));
            Ok(())
        } else if let Some(ms) = usbdesc::find_mass_storage(desc_slice) {
            if self.storage.is_some() {
                info!("xhci", "port {}: a second storage device is ignored", port);
                return Ok(());
            }

            // Set configuration
            self.control(&mut dev, [0x00, 9, ms.config, 0, 0, 0, 0, 0], None)?;

            // Configure bulk IN and bulk OUT endpoints
            let dci_out = xhcictx::dci_out(ms.ep_out & 0x0F);
            let dci_in = xhcictx::dci_in(ms.ep_in & 0x0F);
            let (out_ring_phys, out_ring) = new_ring(ac64)?;
            let (in_ring_phys, in_ring) = new_ring(ac64)?;

            // SAFETY: `in_ctx` and `out_ctx` are pages from `dma_page`; no command is running.
            let (ctx, out) = unsafe { (page_bytes(in_ctx), page_bytes(out_ctx)) };
            ctx.fill(0);
            let ep_out = Endpoint {
                dci: dci_out,
                ep_type: xhcictx::EP_BULK_OUT,
                max_packet: ms.max_packet_out,
                interval: 0,
                dequeue: out_ring_phys,
                avg_trb_len: 512,
            };
            let ep_in = Endpoint {
                dci: dci_in,
                ep_type: xhcictx::EP_BULK_IN,
                max_packet: ms.max_packet_in,
                interval: 0,
                dequeue: in_ring_phys,
                avg_trb_len: 512,
            };
            let max_dci = dci_out.max(dci_in);
            let add_flags = 1 | (1 << dci_out) | (1 << dci_in);
            if !(xhcictx::input_control(ctx, 0, add_flags)
                && xhcictx::copy_slot_from_output(out, ctx, csz)
                && xhcictx::set_slot_entries(ctx, csz, max_dci)
                && xhcictx::endpoint(ctx, csz, &ep_out)
                && xhcictx::endpoint(ctx, csz, &ep_in))
            {
                return Err("input context too small");
            }
            self.command(Trb::configure_endpoint(in_ctx, slot))?;

            let cbw_buf = dma_page(ac64)?;
            let bounce = dma_frames(BOUNCE_FRAMES, ac64)?;

            let storage = Storage {
                port,
                slot,
                interface: ms.interface,
                dci_in,
                dci_out,
                ep_in_addr: ms.ep_in,
                ep_out_addr: ms.ep_out,
                ep0: dev.ep0,
                ep0_buf: dev.buf,
                in_ring,
                out_ring,
                cbw_buf,
                bounce,
                sectors: 0,
                tag: 0,
                vendor: [b' '; 8],
                product: [b' '; 16],
            };
            self.storage = Some(storage);

            if let Err(why) = self.init_storage(port, slot, speed) {
                self.storage = None;
                return Err(why);
            }
            Ok(())
        } else if let Some(un) = usbnet::find_usb_net(desc_slice) {
            if self.net.is_some() {
                info!("xhci", "port {}: a second USB network device is ignored", port);
                return Ok(());
            }
            self.setup_net(port, slot, speed, &mut dev, out_ctx, in_ctx, un)
        } else {
            info!("xhci", "port {}: device is not supported, ignored", port);
            Ok(())
        }
    }

    /// Control transfer to the storage device on endpoint 0.
    fn storage_control(&mut self, packet: [u8; 8], data: Option<(u16, bool)>) -> Result<(), &'static str> {
        let s = self.storage.as_mut().ok_or("no storage device")?;
        let slot = s.slot;
        let buf = s.ep0_buf;
        let in_data = data.map(|d| d.1);
        let mut trbs = 0;
        s.ep0.enqueue(Trb::setup_stage(packet, in_data)).ok_or("control ring full")?;
        trbs += 1;
        if let Some((len, dir_in)) = data {
            s.ep0.enqueue(Trb::data_stage(buf, len, dir_in)).ok_or("control ring full")?;
            trbs += 1;
        }
        s.ep0.enqueue(Trb::status_stage(in_data != Some(true))).ok_or("control ring full")?;
        trbs += 1;
        wr32(self.db + 4 * u64::from(slot), 1);
        let event = self.wait_for(TRB_TRANSFER_EVENT, slot, 1, 1000);
        let s = self.storage.as_mut().ok_or("no storage device")?;
        for _ in 0..trbs {
            s.ep0.retire();
        }
        match event.map(|e| e.completion_code()) {
            Some(CC_SUCCESS | CC_SHORT_PACKET) => Ok(()),
            _ => Err("storage control transfer failed"),
        }
    }

    /// Clears a stall on a bulk endpoint: resets the endpoint in xHCI, sets the dequeue pointer,
    /// and sends ClearFeature(ENDPOINT_HALT) to the device.
    fn clear_storage_stall(&mut self, dci: u8, is_in: bool) {
        let Some(s) = self.storage.as_mut() else { return };
        let slot = s.slot;
        let ep_addr = if is_in { s.ep_in_addr } else { s.ep_out_addr };
        let base = if is_in { s.in_ring.base() } else { s.out_ring.base() };
        if is_in {
            s.in_ring.reset();
        } else {
            s.out_ring.reset();
        }
        let _ = self.command(Trb::reset_endpoint(slot, dci));
        let _ = self.command(Trb::set_tr_dequeue_pointer(base, slot, dci, true));
        let _ = self.storage_control([0x02, 0x01, 0, 0, ep_addr, 0, 0, 0], None);
    }

    /// Bulk-Only Transport reset recovery: sends class-specific reset then clears halt on both endpoints.
    fn storage_reset_recovery(&mut self) {
        let Some(s) = self.storage.as_ref() else { return };
        let iface = s.interface;
        let dci_in = s.dci_in;
        let dci_out = s.dci_out;
        let _ = self.storage_control([0x21, 0xFF, 0, 0, iface, 0, 0, 0], None);
        self.clear_storage_stall(dci_in, true);
        self.clear_storage_stall(dci_out, false);
    }

    fn next_storage_tag(&mut self) -> Result<u32, BlockError> {
        let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
        s.tag = s.tag.wrapping_add(1);
        Ok(s.tag)
    }

    /// Executes one SCSI command via USB Bulk-Only Transport.
    fn storage_transfer(&mut self, cbw: &Cbw, data_phys: u64) -> Result<u32, BlockError> {
        let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
        let tag = cbw.tag;
        let slot = s.slot;
        let dci_out = s.dci_out;
        let dci_in = s.dci_in;
        let cbw_buf = s.cbw_buf;
        let csw_buf = s.cbw_buf + 64;

        // 1. Send CBW
        let cbw_encoded = cbw.encode();
        // SAFETY: `cbw_buf` is a live DMA page owned by `storage`.
        unsafe {
            core::ptr::copy_nonoverlapping(cbw_encoded.as_ptr(), cbw_buf as *mut u8, 31);
        }
        s.out_ring.enqueue(Trb::normal(cbw_buf, 31)).ok_or(BlockError::Io)?;
        wr32(self.db + 4 * u64::from(slot), u32::from(dci_out));
        let event = match self.wait_for(TRB_TRANSFER_EVENT, slot, dci_out, 2000) {
            Some(ev) => ev,
            None => {
                self.storage_reset_recovery();
                return Err(BlockError::Timeout);
            }
        };
        let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
        s.out_ring.retire();
        let code = event.completion_code();
        if code != CC_SUCCESS && code != CC_SHORT_PACKET {
            self.clear_storage_stall(dci_out, false);
            return Err(BlockError::Io);
        }

        // 2. Data stage (if any)
        if cbw.data_len > 0 {
            if cbw.direction == usbmsd::Direction::In {
                let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
                s.in_ring.enqueue(Trb::normal(data_phys, cbw.data_len)).ok_or(BlockError::Io)?;
                wr32(self.db + 4 * u64::from(slot), u32::from(dci_in));
                let event = match self.wait_for(TRB_TRANSFER_EVENT, slot, dci_in, 5000) {
                    Some(ev) => ev,
                    None => {
                        self.storage_reset_recovery();
                        return Err(BlockError::Timeout);
                    }
                };
                let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
                s.in_ring.retire();
                let code = event.completion_code();
                if code != CC_SUCCESS && code != CC_SHORT_PACKET {
                    self.clear_storage_stall(dci_in, true);
                }
            } else {
                let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
                s.out_ring.enqueue(Trb::normal(data_phys, cbw.data_len)).ok_or(BlockError::Io)?;
                wr32(self.db + 4 * u64::from(slot), u32::from(dci_out));
                let event = match self.wait_for(TRB_TRANSFER_EVENT, slot, dci_out, 5000) {
                    Some(ev) => ev,
                    None => {
                        self.storage_reset_recovery();
                        return Err(BlockError::Timeout);
                    }
                };
                let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
                s.out_ring.retire();
                let code = event.completion_code();
                if code != CC_SUCCESS && code != CC_SHORT_PACKET {
                    self.clear_storage_stall(dci_out, false);
                    return Err(BlockError::Io);
                }
            }
        }

        // 3. Status stage (CSW)
        let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
        // SAFETY: zeroing CSW buffer prior to reception.
        unsafe {
            core::ptr::write_bytes(csw_buf as *mut u8, 0, 13);
        }
        s.in_ring.enqueue(Trb::normal(csw_buf, 13)).ok_or(BlockError::Io)?;
        wr32(self.db + 4 * u64::from(slot), u32::from(dci_in));
        let event = match self.wait_for(TRB_TRANSFER_EVENT, slot, dci_in, 2000) {
            Some(ev) => ev,
            None => {
                self.storage_reset_recovery();
                return Err(BlockError::Timeout);
            }
        };
        let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
        s.in_ring.retire();
        let code = event.completion_code();
        if code != CC_SUCCESS && code != CC_SHORT_PACKET {
            self.clear_storage_stall(dci_in, true);
            let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
            s.in_ring.enqueue(Trb::normal(csw_buf, 13)).ok_or(BlockError::Io)?;
            wr32(self.db + 4 * u64::from(slot), u32::from(dci_in));
            let event = match self.wait_for(TRB_TRANSFER_EVENT, slot, dci_in, 2000) {
                Some(ev) => ev,
                None => {
                    self.storage_reset_recovery();
                    return Err(BlockError::Timeout);
                }
            };
            let s = self.storage.as_mut().ok_or(BlockError::NoDevice)?;
            s.in_ring.retire();
            let code = event.completion_code();
            if code != CC_SUCCESS && code != CC_SHORT_PACKET {
                return Err(BlockError::Io);
            }
        }

        // 4. Validate CSW
        // SAFETY: reading back the 13-byte status wrapper filled by DMA.
        let csw_slice = unsafe { core::slice::from_raw_parts(csw_buf as *const u8, 13) };
        let csw = Csw::decode(csw_slice, tag).map_err(|_| BlockError::Io)?;
        match csw.status {
            CswStatus::Passed => Ok(cbw.data_len.saturating_sub(csw.data_residue)),
            CswStatus::Failed => Err(BlockError::Io),
            CswStatus::PhaseError => {
                self.storage_reset_recovery();
                Err(BlockError::Io)
            }
        }
    }

    /// Queries device parameters via SCSI (TEST UNIT READY, INQUIRY, READ CAPACITY).
    fn init_storage(&mut self, port: u8, slot: u8, speed: u8) -> Result<(), &'static str> {
        for _ in 0..5 {
            let tag = self.next_storage_tag().map_err(|_| "no device")?;
            let cbw = usbmsd::scsi_test_unit_ready(tag, 0);
            if self.storage_transfer(&cbw, 0).is_ok() {
                break;
            }
            let rs_tag = self.next_storage_tag().map_err(|_| "no device")?;
            let rs_cbw = usbmsd::scsi_request_sense(rs_tag, 0, 18);
            let rs_buf = self.storage.as_ref().ok_or("no storage")?.cbw_buf + 128;
            let _ = self.storage_transfer(&rs_cbw, rs_buf);
            wait_until(50, || false);
        }

        let inq_tag = self.next_storage_tag().map_err(|_| "no device")?;
        let inq_cbw = usbmsd::scsi_inquiry(inq_tag, 0, 36);
        let inq_buf = self.storage.as_ref().ok_or("no storage")?.cbw_buf + 128;
        if self.storage_transfer(&inq_cbw, inq_buf).is_err() {
            return Err("SCSI INQUIRY failed");
        }
        // SAFETY: `inq_buf` holds the 36-byte response from the device.
        let inq_slice = unsafe { core::slice::from_raw_parts(inq_buf as *const u8, 36) };
        let inq = usbmsd::InquiryData::decode(inq_slice).map_err(|_| "bad INQUIRY response")?;
        if let Some(s) = self.storage.as_mut() {
            s.vendor = inq.vendor;
            s.product = inq.product;
        }

        let cap_tag = self.next_storage_tag().map_err(|_| "no device")?;
        let cap_cbw = usbmsd::scsi_read_capacity_10(cap_tag, 0);
        let cap_buf = self.storage.as_ref().ok_or("no storage")?.cbw_buf + 128;
        if self.storage_transfer(&cap_cbw, cap_buf).is_err() {
            return Err("SCSI READ CAPACITY failed");
        }
        // SAFETY: `cap_buf` holds the 8-byte response from the device.
        let cap_slice = unsafe { core::slice::from_raw_parts(cap_buf as *const u8, 8) };
        let cap = usbmsd::Capacity::decode(cap_slice).map_err(|_| "bad capacity response")?;
        if cap.block_size != 512 || cap.total_sectors() == 0 {
            return Err("unsupported block size or 0 sectors");
        }
        if let Some(s) = self.storage.as_mut() {
            s.sectors = cap.total_sectors();
        }

        let s = self.storage.as_ref().ok_or("no storage")?;
        info!(
            "xhci",
            "port {}: USB mass storage ready (slot {}, speed {}): {} {}, {} sectors ({} MiB)",
            port,
            slot,
            reg::speed_name(speed),
            core::str::from_utf8(&s.vendor).unwrap_or("?").trim(),
            core::str::from_utf8(&s.product).unwrap_or("?").trim(),
            s.sectors,
            s.sectors / 2048,
        );

        // SAFETY: rule A: boot code, before shell task starts.
        unsafe {
            *USB_CACHE.get() = Some(BlockCache::new(XhciDisk));
        }

        Ok(())
    }

    /// Looks at every port with something connected until a keyboard and storage are set up.
    fn enumerate(&mut self) {
        for port in 1..=self.caps.max_ports {
            if self.keyboard.is_some() && self.storage.is_some() && self.mouse.is_some() && self.net.is_some() {
                break;
            }
            if rd32(self.port_reg(port)) & reg::PORT_CONNECTED == 0 {
                continue;
            }
            if let Err(why) = self.setup_device(port) {
                warn!("xhci", "port {}: device not used: {}", port, why);
            }
        }
    }

    /// Hands a transfer event to whichever HID device it belongs to.
    fn on_hid_event(&mut self, event: &Trb) {
        self.on_keyboard_event(event);
        self.on_mouse_event(event);
        self.on_net_event(event);
    }

    /// Bulk IN finished on the network function: keep its frames, arm the next read.
    fn on_net_event(&mut self, event: &Trb) {
        let db = self.db;
        let Some(n) = self.net.as_mut() else { return };
        if event.slot_id() != n.slot || event.endpoint_id() != n.dci_in {
            return;
        }
        n.in_ring.retire();
        match event.completion_code() {
            CC_SUCCESS | CC_SHORT_PACKET => {
                let got = (NET_IN_BUF - event.residual().min(NET_IN_BUF)) as usize;
                // SAFETY: `in_buf` is a DMA allocation of NET_IN_BUF bytes owned by `net`; the transfer has completed.
                let data = unsafe { core::slice::from_raw_parts(n.in_buf as *const u8, got) };
                let mut push = |f: &[u8]| {
                    if n.rx.len() >= 64 {
                        n.rx.pop_front();
                    }
                    n.rx.push_back(f.to_vec());
                };
                match n.kind {
                    usbnet::Kind::Rndis => usbnet::unwrap_packets(data).into_iter().for_each(&mut push),
                    usbnet::Kind::Ecm if got >= 14 => push(data),
                    usbnet::Kind::Ecm => {}
                }
            }
            _ => n.errors = n.errors.saturating_add(1),
        }
        if n.errors < NET_MAX_ERRORS && n.in_ring.enqueue(Trb::normal(n.in_buf, NET_IN_BUF)).is_some() {
            wr32(db + 4 * u64::from(n.slot), u32::from(n.dci_in));
        }
    }

    /// Class request on the default pipe of a device being set up (RNDIS and CDC control).
    fn class_out(&mut self, dev: &mut Device, request: u8, value: u16, index: u16, data: &[u8]) -> Result<(), &'static str> {
        if !data.is_empty() {
            // SAFETY: `dev.buf` is a DMA page from `dma_page`; no transfer is running on it.
            let b = unsafe { page_bytes(dev.buf) };
            b[..data.len()].copy_from_slice(data);
        }
        let [vl, vh] = value.to_le_bytes();
        let [il, ih] = index.to_le_bytes();
        let [ll, lh] = (data.len() as u16).to_le_bytes();
        let d = if data.is_empty() { None } else { Some((data.len() as u16, false)) };
        self.control(dev, [0x21, request, vl, vh, il, ih, ll, lh], d)
    }

    /// RNDIS: sends one control message and waits for its completion.
    fn rndis_command(&mut self, dev: &mut Device, iface: u16, msg: &[u8], want: u32, req: u32) -> Result<Vec<u8>, &'static str> {
        self.class_out(dev, 0x00, 0, iface, msg).map_err(|_| "RNDIS SEND_ENCAPSULATED_COMMAND failed")?;
        for _ in 0..50 {
            // SAFETY: as above.
            let z = unsafe { page_bytes(dev.buf) };
            z[..64].fill(0);
            let [il, ih] = iface.to_le_bytes();
            if self.control(dev, [0xA1, 0x01, 0, 0, il, ih, 0x00, 0x04], Some((1024, true))).is_ok() {
                // SAFETY: the transfer has finished.
                let b = unsafe { page_bytes(dev.buf) };
                let len = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
                // Every completion carries its status at offset 12, so anything shorter than 16 bytes is malformed.
                if (16..=1024).contains(&len) && u32::from_le_bytes([b[0], b[1], b[2], b[3]]) == want && u32::from_le_bytes([b[8], b[9], b[10], b[11]]) == req {
                    return Ok(b[..len].to_vec());
                }
            }
            wait_until(20, || false);
        }
        Err("the RNDIS device did not answer")
    }

    /// Brings up an RNDIS or CDC-ECM network function on `port` and keeps it ready for the IP stack.
    #[allow(clippy::too_many_arguments)]
    fn setup_net(&mut self, port: u8, slot: u8, speed: u8, dev: &mut Device, out_ctx: u64, in_ctx: u64, un: usbnet::UsbNet) -> Result<(), &'static str> {
        let ac64 = self.caps.ac64;
        let csz = self.caps.context_size();
        info!(
            "xhci",
            "port {}: USB network function found ({:?}), config {}, control interface {}, data interface {} alt {}, bulk in {} out {}",
            port, un.kind, un.config, un.ctrl_interface, un.data_interface, un.data_alt, un.ep_in, un.ep_out
        );

        self.control(dev, [0x00, 9, un.config, 0, 0, 0, 0, 0], None).map_err(|_| "SET_CONFIGURATION failed")?;
        if un.data_alt != 0 {
            self.control(dev, [0x01, 0x0B, un.data_alt, 0, un.data_interface, 0, 0, 0], None).map_err(|_| "SET_INTERFACE (data alternate) failed")?;
        }

        // Endpoint 0 is the control pipe: its device context index would overwrite the slot's own entries.
        if !(1..=15).contains(&un.ep_in) || !(1..=15).contains(&un.ep_out) || un.max_packet_in == 0 || un.max_packet_out == 0 {
            return Err("the descriptor gives unusable bulk endpoints");
        }
        let dci_out = xhcictx::dci_out(un.ep_out);
        let dci_in = xhcictx::dci_in(un.ep_in);
        let (out_phys, out_ring) = new_ring(ac64)?;
        let (in_phys, in_ring) = new_ring(ac64)?;
        // SAFETY: `in_ctx` and `out_ctx` are pages from `dma_page`; no command is running.
        let (ctx, out) = unsafe { (page_bytes(in_ctx), page_bytes(out_ctx)) };
        ctx.fill(0);
        let ep_out = Endpoint { dci: dci_out, ep_type: xhcictx::EP_BULK_OUT, max_packet: un.max_packet_out, interval: 0, dequeue: out_phys, avg_trb_len: 512 };
        let ep_in = Endpoint { dci: dci_in, ep_type: xhcictx::EP_BULK_IN, max_packet: un.max_packet_in, interval: 0, dequeue: in_phys, avg_trb_len: 512 };
        let max_dci = dci_out.max(dci_in);
        if !(xhcictx::input_control(ctx, 0, 1 | (1 << dci_out) | (1 << dci_in))
            && xhcictx::copy_slot_from_output(out, ctx, csz)
            && xhcictx::set_slot_entries(ctx, csz, max_dci)
            && xhcictx::endpoint(ctx, csz, &ep_out)
            && xhcictx::endpoint(ctx, csz, &ep_in))
        {
            return Err("input context too small");
        }
        self.command(Trb::configure_endpoint(in_ctx, slot))?;

        let mac = match un.kind {
            usbnet::Kind::Rndis => {
                let iface = u16::from(un.ctrl_interface);
                let r = self.rndis_command(dev, iface, &usbnet::initialize(1), usbnet::MSG_INITIALIZE | usbnet::COMPLETE, 1)?;
                match r.get(12..16) {
                    Some(s) if u32::from_le_bytes([s[0], s[1], s[2], s[3]]) == 0 => {}
                    Some(_) => return Err("RNDIS initialisation was refused"),
                    None => return Err("RNDIS initialisation answer too short"),
                }
                let r = self.rndis_command(dev, iface, &usbnet::query(2, usbnet::OID_PERMANENT_ADDRESS), usbnet::MSG_QUERY | usbnet::COMPLETE, 2)?;
                let c = usbnet::parse_completion(&r).ok_or("bad RNDIS query answer")?;
                if c.status != 0 || c.info.len() < 6 {
                    return Err("the RNDIS device would not tell its MAC address");
                }
                let mac: [u8; 6] = c.info[..6].try_into().unwrap();
                let r = self.rndis_command(dev, iface, &usbnet::set(3, usbnet::OID_PACKET_FILTER, usbnet::FILTER_NORMAL), usbnet::MSG_SET | usbnet::COMPLETE, 3)?;
                if usbnet::parse_completion(&r).map(|c| c.status) != Some(0) {
                    return Err("the RNDIS device refused the packet filter");
                }
                mac
            }
            usbnet::Kind::Ecm => {
                let idx = un.mac_string.ok_or("no MAC address string in the descriptor")?;
                self.control(dev, [0x80, 6, idx, 3, 0x09, 0x04, 32, 0], Some((32, true))).map_err(|_| "reading the MAC address string failed")?;
                // SAFETY: the transfer has finished.
                let mac = usbnet::parse_mac_string(&unsafe { page_bytes(dev.buf) }[..32]).ok_or("unreadable MAC address string")?;
                // SET_ETHERNET_PACKET_FILTER: directed, broadcast, all multicast.
                self.class_out(dev, 0x43, 0x000E, u16::from(un.ctrl_interface), &[]).map_err(|_| "SET_ETHERNET_PACKET_FILTER failed")?;
                mac
            }
        };

        let in_buf = dma_frames((NET_IN_BUF as usize).div_ceil(PAGE_BYTES), ac64)?;
        let out_buf = dma_frames(NET_OUT_BUF.div_ceil(PAGE_BYTES), ac64)?;
        let mut net = Net {
            port,
            slot,
            kind: un.kind,
            dci_in,
            dci_out,
            in_ring,
            out_ring,
            in_buf,
            out_buf,
            max_packet_out: usize::from(un.max_packet_out),
            mac,
            rx: VecDeque::new(),
            errors: 0,
        };
        net.in_ring.enqueue(Trb::normal(in_buf, NET_IN_BUF)).ok_or("bulk ring full")?;
        wr32(self.db + 4 * u64::from(slot), u32::from(dci_in));
        self.net = Some(net);
        info!(
            "xhci",
            "port {}: USB network ready ({:?}, slot {}, speed {}), MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            port, un.kind, slot, reg::speed_name(speed), mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );
        Ok(())
    }

    /// A transfer event for the mouse: add its movement to the pending motion, ask for the next report.
    fn on_mouse_event(&mut self, event: &Trb) {
        let db = self.db;
        let Some(m) = self.mouse.as_mut() else { return };
        if event.slot_id() != m.slot || event.endpoint_id() != m.dci {
            return;
        }
        m.ring.retire();
        match event.completion_code() {
            CC_SUCCESS | CC_SHORT_PACKET => {
                // SAFETY: `report` is a page from `dma_page`; the transfer that wrote it has completed.
                let report = unsafe { read_volatile(m.report as *const [u8; 8]) };
                if let Some(r) = pointer::parse_report(&report) {
                    m.motion.add(r);
                    m.dirty = true;
                }
            }
            _ => m.errors = m.errors.saturating_add(1),
        }
        if m.errors < 8 && m.ring.enqueue(Trb::normal(m.report, 8)).is_some() {
            wr32(db + 4 * u64::from(m.slot), u32::from(m.dci));
        }
    }

    /// A transfer event for the keyboard: read the report, queue the keys, ask for the next report.
    fn on_keyboard_event(&mut self, event: &Trb) {
        let db = self.db;
        let Some(k) = self.keyboard.as_mut() else { return };
        if event.slot_id() != k.slot || event.endpoint_id() != k.dci {
            return;
        }
        k.ring.retire();
        match event.completion_code() {
            CC_SUCCESS | CC_SHORT_PACKET => {
                // SAFETY: `report` is a page from `dma_page`; the transfer that wrote it has completed.
                let report = unsafe { read_volatile(k.report as *const [u8; 8]) };
                let queue = &mut k.queue;
                k.hid.feed(&report, &mut |key| queue.push(key));
            }
            _ => k.errors = k.errors.saturating_add(1),
        }
        if k.errors < 8 && k.ring.enqueue(Trb::normal(k.report, 8)).is_some() {
            wr32(db + 4 * u64::from(k.slot), u32::from(k.dci));
        }
    }
}

/// Finds an xHCI controller (PCI class 0C, subclass 03, interface 30), starts it and sets up a USB
/// keyboard if one is plugged in. Called once at boot, after the PCI scan.
pub fn init() {
    let mut found = None;
    pci::for_each(|d| {
        let h = &d.header;
        if found.is_none() && h.class == 0x0C && h.subclass == 0x03 && h.prog_if == 0x30 {
            found = Some(*d);
        }
    });
    let Some(d) = found else {
        info!("xhci", "no xHCI controller on the PCI bus");
        return;
    };
    match bring_up(&d) {
        Ok(mut x) => {
            info!(
                "xhci",
                "controller {:04x}:{:04x} version {:x}.{:02x}: {} ports, {} slots, {} scratchpad pages, {}",
                x.vendor,
                x.device,
                x.caps.version >> 8,
                x.caps.version & 0xFF,
                x.caps.max_ports,
                x.slots,
                x.caps.scratchpads,
                x.handoff
            );
            x.settle_ports();
            for port in 1..=x.caps.max_ports {
                let portsc = rd32(x.port_reg(port));
                if portsc & reg::PORT_CONNECTED != 0 {
                    info!("xhci", "port {}: device connected, speed {}", port, reg::speed_name(reg::port_speed(portsc)));
                }
            }
            x.enumerate();
            // SAFETY: rule A: boot code, before the shell task exists; no interrupt handler touches XHCI.
            *unsafe { XHCI.get() } = Some(x);
        }
        Err(why) => warn!("xhci", "controller at {:02x}:{:02x}.{} not used: {}", d.bus, d.dev, d.func, why),
    }
}

/// The next key from a USB keyboard, if one was pressed. Called from the shell loop; cheap when there is
/// no controller.
pub fn poll_key() -> Option<Key> {
    // SAFETY: rule A: only the shell task and boot code reach XHCI.
    let x = unsafe { XHCI.get() }.as_mut()?;
    while let Some(event) = x.try_event() {
        if event.kind() == TRB_TRANSFER_EVENT {
            x.on_hid_event(&event);
        }
    }
    x.keyboard.as_mut()?.queue.pop()
}

/// Mouse movement and buttons since the last call, or `None` if nothing happened (or no mouse is set up).
pub fn poll_mouse() -> Option<MouseMotion> {
    // SAFETY: rule A: only the shell task and boot code reach XHCI.
    let x = unsafe { XHCI.get() }.as_mut()?;
    while let Some(event) = x.try_event() {
        if event.kind() == TRB_TRANSFER_EVENT {
            x.on_hid_event(&event);
        }
    }
    let m = x.mouse.as_mut()?;
    if !m.dirty {
        return None;
    }
    m.dirty = false;
    Some(core::mem::take(&mut m.motion))
}

/// What the `usb` command shows about the controller.
pub struct Summary {
    pub vendor: u16,
    pub device: u16,
    pub version: u16,
    pub ports: u8,
    pub slots: u8,
    pub scratchpads: u16,
    pub handoff: &'static str,
    pub running: bool,
    /// Port of the USB keyboard in use, if any.
    pub keyboard_port: Option<u8>,
    /// Port of the USB mouse in use, if any.
    pub mouse_port: Option<u8>,
    /// Port of the USB mass storage device in use, if any.
    pub storage_port: Option<u8>,
}

pub fn summary() -> Option<Summary> {
    // SAFETY: rule A: only the shell task and boot code reach XHCI.
    let x = unsafe { XHCI.get() }.as_ref()?;
    Some(Summary {
        vendor: x.vendor,
        device: x.device,
        version: x.caps.version,
        ports: x.caps.max_ports,
        slots: x.slots,
        scratchpads: x.caps.scratchpads,
        handoff: x.handoff,
        running: rd32(x.op + 4) & 1 == 0,
        keyboard_port: x.keyboard.as_ref().map(|k| k.port),
        mouse_port: x.mouse.as_ref().map(|m| m.port),
        storage_port: x.storage.as_ref().map(|s| s.port),
    })
}

/// Calls `f(port number, PORTSC)` for every root hub port (numbers start at 1).
pub fn for_each_port(mut f: impl FnMut(u8, u32)) {
    // SAFETY: rule A, as in `summary`.
    let Some(x) = (unsafe { XHCI.get() }).as_ref() else { return };
    for p in 1..=x.caps.max_ports {
        f(p, rd32(x.port_reg(p)));
    }
}

/// Runs `f` on the USB block cache (read path).
pub fn with_disk<R>(f: impl FnOnce(&mut dyn BlockDev) -> R) -> Option<R> {
    // SAFETY: rule A: only the shell task and boot code reach USB_CACHE.
    let cache = unsafe { USB_CACHE.get() }.as_mut()?;
    Some(f(cache))
}

/// Runs `f` on the USB block cache (write path).
pub fn with_usb_write<R>(f: impl FnOnce(&mut dyn BlockWrite) -> R) -> Option<R> {
    // SAFETY: rule A: only the shell task and boot code reach USB_CACHE.
    let cache = unsafe { USB_CACHE.get() }.as_mut()?;
    Some(f(cache))
}

/// Flushes all cached writes to disk.
pub fn sync() -> Result<(), BlockError> {
    // SAFETY: rule A: only the shell task and boot code reach USB_CACHE.
    let cache = unsafe { USB_CACHE.get() }.as_mut().ok_or(BlockError::NoDevice)?;
    cache.sync()
}

/// Returns true if a mass storage drive was detected.
pub fn has_storage() -> bool {
    // SAFETY: rule A.
    let x = unsafe { XHCI.get() }.as_ref();
    x.and_then(|x| x.storage.as_ref()).is_some()
}

/// Information about the connected USB mass storage device.
pub fn disk_info() -> Option<StorageInfo> {
    // SAFETY: rule A.
    let x = unsafe { XHCI.get() }.as_ref()?;
    let s = x.storage.as_ref()?;
    Some(StorageInfo {
        slot: s.slot,
        sectors: s.sectors,
        vendor: s.vendor,
        product: s.product,
    })
}

// --- USB network function: what the IP stack uses ---------------------------------------------------------------

/// Is a USB network function (RNDIS or CDC-ECM) up?
pub fn net_present() -> bool {
    // SAFETY: rule A: only the shell task and boot code reach XHCI.
    unsafe { XHCI.get() }.as_ref().is_some_and(|x| x.net.as_ref().is_some_and(|n| n.errors < NET_MAX_ERRORS))
}

pub fn net_mac() -> [u8; 6] {
    // SAFETY: as above.
    unsafe { XHCI.get() }.as_ref().and_then(|x| x.net.as_ref()).map_or([0; 6], |n| n.mac)
}

pub fn net_name() -> &'static str {
    // SAFETY: as above.
    match unsafe { XHCI.get() }.as_ref().and_then(|x| x.net.as_ref()).map(|n| n.kind) {
        Some(usbnet::Kind::Rndis) => "USB RNDIS (phone tethering)",
        Some(usbnet::Kind::Ecm) => "USB CDC-ECM Ethernet",
        None => "USB network (none)",
    }
}

/// Sends one Ethernet frame through the USB network function.
pub fn net_send(frame: &[u8]) -> Result<(), crate::netdev::NetError> {
    use crate::netdev::NetError;
    // SAFETY: as above.
    let x = unsafe { XHCI.get() }.as_mut().ok_or(NetError::DeviceNotFound)?;
    let (slot, dci, out_buf, kind, maxp) = {
        let n = x.net.as_ref().ok_or(NetError::DeviceNotFound)?;
        // A failed device (too many errors, or a send that never finished) is not touched again: its ring
        // and buffer may still belong to the controller.
        if n.errors >= NET_MAX_ERRORS {
            return Err(NetError::HardwareError);
        }
        (n.slot, n.dci_out, n.out_buf, n.kind, n.max_packet_out)
    };
    if frame.len() > 1514 {
        return Err(NetError::PacketTooLarge);
    }
    let bytes: Vec<u8> = match kind {
        usbnet::Kind::Rndis => usbnet::wrap_packet(frame, maxp),
        usbnet::Kind::Ecm => {
            let mut v = frame.to_vec();
            if maxp > 0 && v.len() % maxp == 0 {
                v.push(0);
            }
            v
        }
    };
    // SAFETY: `out_buf` is a DMA allocation of NET_OUT_BUF bytes owned by `net`, and the previous send finished.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf as *mut u8, bytes.len()) };
    let n = x.net.as_mut().ok_or(NetError::DeviceNotFound)?;
    n.out_ring.enqueue(Trb::normal(out_buf, bytes.len() as u32)).ok_or(NetError::TxFull)?;
    wr32(x.db + 4 * u64::from(slot), u32::from(dci));
    let ev = x.wait_for(TRB_TRANSFER_EVENT, slot, dci, 1000);
    if let Some(n) = x.net.as_mut() {
        if ev.is_some() {
            n.out_ring.retire();
        } else {
            // The TRB may still be owned by the controller: never reuse the ring slot or `out_buf`.
            n.errors = u8::MAX;
            warn!("xhci", "USB network send timed out; the device is switched off");
        }
    }
    match ev.map(|e| e.completion_code()) {
        Some(CC_SUCCESS | CC_SHORT_PACKET) => Ok(()),
        Some(_) => Err(NetError::HardwareError),
        None => Err(NetError::Timeout),
    }
}

/// The next received Ethernet frame, if any.
pub fn net_receive(out: &mut [u8]) -> Result<usize, crate::netdev::NetError> {
    use crate::netdev::NetError;
    // SAFETY: as above.
    let x = unsafe { XHCI.get() }.as_mut().ok_or(NetError::DeviceNotFound)?;
    while let Some(event) = x.try_event() {
        if event.kind() == TRB_TRANSFER_EVENT {
            x.on_hid_event(&event);
        }
    }
    let n = x.net.as_mut().ok_or(NetError::DeviceNotFound)?;
    match n.rx.pop_front() {
        Some(f) if f.len() <= out.len() => {
            out[..f.len()].copy_from_slice(&f);
            Ok(f.len())
        }
        Some(_) => Err(NetError::NoBuffer),
        None => Err(NetError::RxEmpty),
    }
}

/// Looks again at every port that is not already in use: for a phone plugged in after boot.
pub fn rescan() {
    // SAFETY: as above.
    let Some(x) = (unsafe { XHCI.get() }).as_mut() else { return };
    let used: Vec<u8> = [x.keyboard.as_ref().map(|k| k.port), x.mouse.as_ref().map(|m| m.port), x.storage.as_ref().map(|s| s.port), x.net.as_ref().map(|n| n.port)].into_iter().flatten().collect();
    for port in 1..=x.caps.max_ports {
        // Only a port with a fresh connect event: `reset_port` acknowledges the event after every attempt, so a
        // device that was already tried and refused is not addressed a second time (which the controller rejects).
        let sc = rd32(x.port_reg(port));
        if used.contains(&port) || sc & reg::PORT_CONNECTED == 0 || sc & reg::PORT_CONNECT_CHANGE == 0 {
            continue;
        }
        if let Err(why) = x.setup_device(port) {
            warn!("xhci", "port {}: device not used: {}", port, why);
        }
    }
}
