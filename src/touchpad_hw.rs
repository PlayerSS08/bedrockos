//! Laptop touchpad on real hardware: Intel's LPSS "Serial IO" DesignWare I2C controllers on PCI, with a HID
//! over I2C touchpad behind one of them. Hardware only (UEFI build); the protocol, the controller logic and
//! the report decoding are in `i2chid`, `dwi2c`, `hidparse` and `touchpad` and are tested on the host.
//!
//! No ACPI interpreter is available, so the usual answers (which controller, which address, which register)
//! are searched for among the values laptop touchpads are known to use. Everything is logged at boot and
//! `touchpad` prints it again, because this path can only be debugged from what the machine says.
//! Input is polled: the touchpad's interrupt line is a GPIO pin that is not driven here.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::dwi2c::{DwI2c, Regs, COMP_TYPE_DW_I2C, IC_COMP_TYPE};
use crate::global::Global;
use crate::i2chid::{self, I2cBus};
use crate::mmio::{rd32, wr32};
use crate::pci;
use crate::pointer::MouseMotion;
use crate::touchpad::Touchpad;
use crate::{arch, log, paging};

/// Intel serial-bus controller class (0x0C, subclass 0x80) covers the I2C, SPI and UART-less blocks of LPSS.
const CLASS_SERIAL_OTHER: (u8, u8) = (0x0C, 0x80);
/// The I2C controller the OMEN 16 keeps its touchpad on (ACPI TPD0 under I2C0), tried first.
const PREFERRED_DEVICE: u16 = 0x7A4C;
/// LPSS private registers sit 0x200 into the BAR; the reset register releases the function and its DMA engine.
const LPSS_PRIV_RESETS: u64 = 0x204;
const LPSS_RESETS_RELEASE: u32 = 0b111;
/// Time between touchpad reads.
const POLL_INTERVAL_US: u64 = 6_000;
/// Consecutive failed reads after which the touchpad is given up on (a gone or wedged device).
const MAX_ERRORS: u32 = 200;

struct Mmio(u64);

impl Regs for Mmio {
    fn read(&mut self, offset: u32) -> u32 {
        rd32(self.0 + u64::from(offset))
    }

    fn write(&mut self, offset: u32, value: u32) {
        wr32(self.0 + u64::from(offset), value);
    }

    fn now_us(&mut self) -> u64 {
        log::uptime_us()
    }

    fn delay_us(&mut self, us: u32) {
        let t0 = log::uptime_us();
        while log::uptime_us().saturating_sub(t0) < u64::from(us) {
            arch::pause();
        }
    }
}

struct Active {
    bus: DwI2c<Mmio>,
    dev: i2chid::Device,
    pad: Touchpad,
    controller: String,
    last_poll_us: u64,
    errors: u32,
    failed: bool,
    reports: u32,
}

struct State {
    active: Option<Active>,
    /// What `init` did, line by line, for the `touchpad` command.
    notes: Vec<String>,
}

static STATE: Global<State> = Global::new(State { active: None, notes: Vec::new() });

fn note(s: &mut State, line: String) {
    info!("touchpad", "{}", line);
    s.notes.push(line);
}

/// Puts a PCI function in power state D0 if its power-management capability says it is asleep.
fn wake(d: &pci::Device) -> &'static str {
    let (b, dv, f) = (d.bus, d.dev, d.func);
    if pci::config_read(b, dv, f, 4) >> 16 & 0x10 == 0 {
        return "no capability list";
    }
    let mut ptr = (pci::config_read(b, dv, f, 0x34) & 0xFC) as u8;
    for _ in 0..48 {
        if ptr < 0x40 {
            break;
        }
        let head = pci::config_read(b, dv, f, ptr);
        if head & 0xFF == 0x01 {
            let at = ptr + 4;
            let pmcsr = pci::config_read(b, dv, f, at);
            if pmcsr & 3 == 0 {
                return "already in D0";
            }
            pci::config_write(b, dv, f, at, pmcsr & !3);
            let mut m = Mmio(0);
            m.delay_us(12_000);
            return "woken from a low-power state";
        }
        ptr = ((head >> 8) & 0xFC) as u8;
    }
    "no power-management capability"
}

fn try_controller(s: &mut State, d: &pci::Device) -> bool {
    let name = format!("{:02x}:{:02x}.{} {:04x}:{:04x}", d.bus, d.dev, d.func, d.header.vendor, d.header.device);
    let power = wake(d);
    let command = pci::config_read(d.bus, d.dev, d.func, 4);
    pci::config_write(d.bus, d.dev, d.func, 4, (command & 0xFFFF) | 0x6);
    let Some(base) = pci::memory_bar(d, 0) else {
        note(s, format!("i2c {name}: BAR 0 is not assigned (the firmware hides this controller)"));
        return false;
    };
    if !(paging::map_mmio_uc(base) && paging::map_mmio_uc(base + 0xFFF)) {
        note(s, format!("i2c {name}: cannot map the registers at {base:#x}"));
        return false;
    }
    wr32(base + LPSS_PRIV_RESETS, LPSS_RESETS_RELEASE);
    Mmio(base).delay_us(1000);
    let comp = rd32(base + u64::from(IC_COMP_TYPE));
    if comp != COMP_TYPE_DW_I2C {
        note(s, format!("i2c {name}: registers at {base:#x} are not a DesignWare I2C block (type {comp:#010x})"));
        return false;
    }
    let mut bus = match DwI2c::new(Mmio(base)) {
        Ok(b) => b,
        Err(why) => {
            note(s, format!("i2c {name}: {why}"));
            return false;
        }
    };
    let (tx, rx) = bus.fifo_depths();
    note(s, format!("i2c {name}: DesignWare I2C at {base:#x} ({power}), FIFOs {tx}/{rx}, SCL counts {}/{}", bus.timing.0, bus.timing.1));
    let Some((addr, reg, desc)) = i2chid::find_device(&mut bus) else {
        note(s, format!("i2c {name}: no HID device answered at {:02x?} (registers {:02x?})", i2chid::TOUCHPAD_ADDRESSES, i2chid::DESCRIPTOR_REGISTERS));
        return false;
    };
    note(s, format!("i2c {name}: HID device at {addr:#04x}, descriptor register {reg:#06x}, vendor {:04x} product {:04x}, report descriptor {} bytes, reports up to {} bytes", desc.vendor, desc.product, desc.report_desc_len, desc.max_input));
    let dev = match i2chid::open(&mut bus, addr, desc) {
        Ok(d) => d,
        Err(why) => {
            note(s, format!("i2c {name}: could not start the device: {why}"));
            return false;
        }
    };
    note(s, format!("i2c {name}: device started (reset {})", if dev.reset_acknowledged { "acknowledged" } else { "not acknowledged, continuing" }));
    let Some(pad) = Touchpad::new(&dev.report_descriptor) else {
        note(s, format!("i2c {name}: the report descriptor has neither a mouse nor a touchpad layout (`touchpad dump` shows it)"));
        s.active = None;
        return false;
    };
    note(s, format!("i2c {name}: using {}", pad.describe()));
    s.active = Some(Active { bus, dev, pad, controller: name, last_poll_us: 0, errors: 0, failed: false, reports: 0 });
    true
}

/// Looks for a touchpad on every LPSS I2C controller. Safe to call again (the `touchpad rescan` command).
pub fn init() {
    // SAFETY: only boot code and the shell task reach STATE (rule 1 in `global`), and not at the same time.
    let s = unsafe { STATE.get() };
    s.active = None;
    s.notes.clear();
    let mut controllers: Vec<pci::Device> = Vec::new();
    pci::for_each(|d| {
        if d.header.vendor == 0x8086 && (d.header.class, d.header.subclass) == CLASS_SERIAL_OTHER {
            controllers.push(*d);
        }
    });
    controllers.sort_by_key(|d| d.header.device != PREFERRED_DEVICE);
    if controllers.is_empty() {
        note(s, String::from("no Intel serial-bus controllers on PCI (`pci` lists what is there)"));
        return;
    }
    for d in &controllers {
        if try_controller(s, d) {
            return;
        }
    }
    warn!("touchpad", "no touchpad found on {} controller(s)", controllers.len());
}

/// The next pointer movement from the touchpad, if its next report brought one. Cheap when there is no
/// touchpad or it is not time to look yet.
pub fn poll() -> Option<MouseMotion> {
    // SAFETY: as in `init`.
    let a = unsafe { STATE.get() }.active.as_mut()?;
    if a.failed {
        return None;
    }
    let now = log::uptime_us();
    if now.saturating_sub(a.last_poll_us) < POLL_INTERVAL_US {
        return None;
    }
    a.last_poll_us = now;
    let now_ms = now / 1000;
    match a.dev.poll(&mut a.bus) {
        Ok(Some(report)) => {
            a.errors = 0;
            a.reports = a.reports.wrapping_add(1);
            a.pad.feed(report, now_ms)
        }
        Ok(None) => {
            a.errors = 0;
            a.pad.idle(now_ms)
        }
        Err(_) => {
            a.errors += 1;
            if a.errors >= MAX_ERRORS {
                a.failed = true;
                warn!("touchpad", "{} keeps failing; the touchpad is switched off", a.controller);
            }
            None
        }
    }
}

pub fn present() -> bool {
    // SAFETY: as in `init`.
    unsafe { STATE.get() }.active.as_ref().is_some_and(|a| !a.failed)
}

/// What the probe found, for the `touchpad` command, plus the live counters.
pub fn report() -> Vec<String> {
    // SAFETY: as in `init`.
    let s = unsafe { STATE.get() };
    let mut v = s.notes.clone();
    if let Some(a) = &s.active {
        v.push(format!("{} reports read, {} decoded, {} consecutive errors{}", a.reports, a.pad.reports, a.errors, if a.failed { ", switched off" } else { "" }));
    }
    v
}

/// The device's report descriptor as hex lines, for a bug report.
pub fn dump_report_descriptor() -> Vec<String> {
    // SAFETY: as in `init`.
    let Some(a) = (unsafe { STATE.get() }).active.as_ref() else { return Vec::new() };
    a.dev.report_descriptor.chunks(16).enumerate().map(|(i, c)| format!("{:04x}: {}", i * 16, c.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "))).collect()
}

/// Scans every plausible address on the first working controller and lists who answers (`touchpad scan`).
/// A read of one byte per address: it does not write anything the device could keep.
pub fn scan_bus() -> Vec<String> {
    let mut out = Vec::new();
    let mut controllers: Vec<pci::Device> = Vec::new();
    pci::for_each(|d| {
        if d.header.vendor == 0x8086 && (d.header.class, d.header.subclass) == CLASS_SERIAL_OTHER {
            controllers.push(*d);
        }
    });
    for d in &controllers {
        let Some(base) = pci::memory_bar(d, 0) else { continue };
        if !(paging::map_mmio_uc(base) && paging::map_mmio_uc(base + 0xFFF)) || rd32(base + u64::from(IC_COMP_TYPE)) != COMP_TYPE_DW_I2C {
            continue;
        }
        let Ok(mut bus) = DwI2c::new(Mmio(base)) else { continue };
        let mut found = Vec::new();
        for addr in 0x08..0x78u8 {
            let mut b = [0u8; 1];
            if bus.read(addr, &mut b).is_ok() {
                found.push(format!("{addr:#04x}"));
            }
        }
        out.push(format!("{:02x}:{:02x}.{} {:04x}:{:04x}: {}", d.bus, d.dev, d.func, d.header.vendor, d.header.device, if found.is_empty() { String::from("nothing answered") } else { found.join(" ") }));
    }
    if out.is_empty() {
        out.push(String::from("no usable I2C controller"));
    }
    out
}
