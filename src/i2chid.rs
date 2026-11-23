//! HID over I2C (Microsoft's protocol, used by nearly every laptop touchpad since 2013). Pure and host-tested:
//! the bus is the `I2cBus` trait, so a fake device stands in for the hardware in the tests.
//!
//! How it works: the device has a 30-byte HID descriptor at a 16-bit register (almost always 0x0001, told by
//! ACPI) that names the other registers. After `SET_POWER on` and `RESET` the host reads the report
//! descriptor, and from then on reads input reports straight from the bus: two length bytes, then the report.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum I2cError {
    /// Nobody acknowledged the address or a data byte.
    Nack,
    Timeout,
    /// The controller aborted the transfer (arbitration lost, bad state, ...), with its abort source bits.
    Abort(u32),
    /// A request the controller cannot do (empty, too long).
    Invalid,
}

/// A master on an I2C bus. Addresses are 7-bit.
pub trait I2cBus {
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<(), I2cError>;
    /// A write followed by a repeated start and a read.
    fn write_read(&mut self, addr: u8, out: &[u8], inp: &mut [u8]) -> Result<(), I2cError>;
    fn read(&mut self, addr: u8, inp: &mut [u8]) -> Result<(), I2cError>;
    fn delay_ms(&mut self, ms: u32);
}

pub const DESCRIPTOR_BYTES: usize = 30;
const BCD_VERSION: u16 = 0x0100;
const MAX_REPORT_DESCRIPTOR: u16 = 8192;
const MAX_INPUT: u16 = 4096;

/// Where a device's registers are and how big its reports are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HidDescriptor {
    pub report_desc_len: u16,
    pub report_desc_reg: u16,
    pub input_reg: u16,
    pub max_input: u16,
    pub output_reg: u16,
    pub max_output: u16,
    pub command_reg: u16,
    pub data_reg: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

/// Checks and decodes the 30 bytes read from the descriptor register. A device that answers with anything
/// else (an EEPROM, a sensor, noise) is not a HID device.
pub fn parse_descriptor(b: &[u8]) -> Option<HidDescriptor> {
    if b.len() < DESCRIPTOR_BYTES || le16(b, 0) as usize != DESCRIPTOR_BYTES || le16(b, 2) != BCD_VERSION {
        return None;
    }
    let d = HidDescriptor {
        report_desc_len: le16(b, 4),
        report_desc_reg: le16(b, 6),
        input_reg: le16(b, 8),
        max_input: le16(b, 10),
        output_reg: le16(b, 12),
        max_output: le16(b, 14),
        command_reg: le16(b, 16),
        data_reg: le16(b, 18),
        vendor: le16(b, 20),
        product: le16(b, 22),
        version: le16(b, 24),
    };
    // An input report needs its two length bytes and at least one byte of report.
    (d.report_desc_len != 0 && d.report_desc_len <= MAX_REPORT_DESCRIPTOR && (3..=MAX_INPUT).contains(&d.max_input)).then_some(d)
}

/// Reads and checks the HID descriptor of the device at `addr` from register `reg`.
pub fn fetch_descriptor<B: I2cBus>(bus: &mut B, addr: u8, reg: u16) -> Result<HidDescriptor, &'static str> {
    let mut b = [0u8; DESCRIPTOR_BYTES];
    bus.write_read(addr, &reg.to_le_bytes(), &mut b).map_err(|_| "no answer from the device")?;
    parse_descriptor(&b).ok_or("the answer is not a HID descriptor")
}

/// Addresses and descriptor registers a laptop touchpad is known to use. Only these are tried: writing to
/// an unknown device on a shared bus is not something to do blindly.
pub const TOUCHPAD_ADDRESSES: [u8; 6] = [0x15, 0x2C, 0x10, 0x20, 0x38, 0x2A];
pub const DESCRIPTOR_REGISTERS: [u16; 2] = [0x0001, 0x0020];

/// The first known address that answers with a valid HID descriptor.
pub fn find_device<B: I2cBus>(bus: &mut B) -> Option<(u8, u16, HidDescriptor)> {
    for &addr in &TOUCHPAD_ADDRESSES {
        for &reg in &DESCRIPTOR_REGISTERS {
            if let Ok(d) = fetch_descriptor(bus, addr, reg) {
                return Some((addr, reg, d));
            }
        }
    }
    None
}

/// A HID device that has been powered, reset and read.
#[derive(Debug)]
pub struct Device {
    pub addr: u8,
    pub desc: HidDescriptor,
    pub report_descriptor: Vec<u8>,
    /// Set when the device answered the reset with its "reset done" message.
    pub reset_acknowledged: bool,
    buf: Vec<u8>,
}

const OP_RESET: u8 = 0x01;
const OP_SET_POWER: u8 = 0x08;

fn command(desc: &HidDescriptor, low: u8, opcode: u8) -> [u8; 4] {
    let [l, h] = desc.command_reg.to_le_bytes();
    [l, h, low, opcode]
}

/// Powers the device on, resets it and reads its report descriptor.
pub fn open<B: I2cBus>(bus: &mut B, addr: u8, desc: HidDescriptor) -> Result<Device, &'static str> {
    bus.write(addr, &command(&desc, 0x00, OP_SET_POWER)).map_err(|_| "SET_POWER failed")?;
    bus.delay_ms(5);
    bus.write(addr, &command(&desc, 0x00, OP_RESET)).map_err(|_| "RESET failed")?;
    let mut dev = Device { addr, desc, report_descriptor: Vec::new(), reset_acknowledged: false, buf: vec![0; usize::from(desc.max_input)] };
    // The device answers a reset with an input report of length zero. Not every device does, so waiting is
    // bounded and a missing answer is only noted.
    for _ in 0..30 {
        bus.delay_ms(10);
        if bus.read(addr, &mut dev.buf).is_ok() && le16(&dev.buf, 0) == 0 {
            dev.reset_acknowledged = true;
            break;
        }
    }
    let mut rd = vec![0u8; usize::from(desc.report_desc_len)];
    bus.write_read(addr, &desc.report_desc_reg.to_le_bytes(), &mut rd).map_err(|_| "reading the report descriptor failed")?;
    dev.report_descriptor = rd;
    Ok(dev)
}

impl Device {
    /// Reads one input report if the device has one. The slice is the report itself (report id first when the
    /// device uses ids), valid until the next call. A device with nothing to say answers with length zero.
    pub fn poll<B: I2cBus>(&mut self, bus: &mut B) -> Result<Option<&[u8]>, I2cError> {
        bus.read(self.addr, &mut self.buf)?;
        let len = usize::from(le16(&self.buf, 0));
        if len == 0 {
            return Ok(None);
        }
        // The length counts its own two bytes. Anything impossible is noise, not a report.
        if len < 3 || len > self.buf.len() {
            return Err(I2cError::Invalid);
        }
        Ok(Some(&self.buf[2..len]))
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use alloc::collections::VecDeque;

    /// A touchpad at one address with the registers a descriptor names.
    pub struct FakeBus {
        pub addr: u8,
        pub desc_reg: u16,
        pub desc: [u8; DESCRIPTOR_BYTES],
        pub report_desc: Vec<u8>,
        pub reports: VecDeque<Vec<u8>>,
        pub commands: Vec<[u8; 4]>,
        pub reset_pending: bool,
        pub acks_reset: bool,
        pub delayed_ms: u32,
    }

    pub fn descriptor_bytes(report_desc_len: u16) -> [u8; DESCRIPTOR_BYTES] {
        let mut b = [0u8; DESCRIPTOR_BYTES];
        let fields: [(usize, u16); 10] = [(0, 30), (2, 0x0100), (4, report_desc_len), (6, 0x0002), (8, 0x0003), (10, 0x0040), (12, 0x0004), (14, 0x0040), (16, 0x0005), (18, 0x0006)];
        for (at, v) in fields {
            b[at..at + 2].copy_from_slice(&v.to_le_bytes());
        }
        b[20..22].copy_from_slice(&0x04F3u16.to_le_bytes()); // ELAN
        b[22..24].copy_from_slice(&0x3200u16.to_le_bytes());
        b
    }

    impl FakeBus {
        pub fn new(addr: u8, report_desc: Vec<u8>) -> Self {
            Self {
                addr,
                desc_reg: 0x0001,
                desc: descriptor_bytes(report_desc.len() as u16),
                report_desc,
                reports: VecDeque::new(),
                commands: Vec::new(),
                reset_pending: false,
                acks_reset: true,
                delayed_ms: 0,
            }
        }
    }

    impl I2cBus for FakeBus {
        fn write(&mut self, addr: u8, data: &[u8]) -> Result<(), I2cError> {
            if addr != self.addr {
                return Err(I2cError::Nack);
            }
            let c: [u8; 4] = data.try_into().map_err(|_| I2cError::Invalid)?;
            if c[0..2] != 0x0005u16.to_le_bytes() {
                return Err(I2cError::Nack);
            }
            if c[3] == OP_RESET {
                self.reset_pending = self.acks_reset;
            }
            self.commands.push(c);
            Ok(())
        }

        fn write_read(&mut self, addr: u8, out: &[u8], inp: &mut [u8]) -> Result<(), I2cError> {
            if addr != self.addr || out.len() != 2 {
                return Err(I2cError::Nack);
            }
            let reg = u16::from_le_bytes([out[0], out[1]]);
            let src: &[u8] = if reg == self.desc_reg {
                &self.desc
            } else if reg == 0x0002 {
                &self.report_desc
            } else {
                return Err(I2cError::Nack);
            };
            let n = inp.len().min(src.len());
            inp[..n].copy_from_slice(&src[..n]);
            Ok(())
        }

        fn read(&mut self, addr: u8, inp: &mut [u8]) -> Result<(), I2cError> {
            if addr != self.addr {
                return Err(I2cError::Nack);
            }
            inp.fill(0);
            if self.reset_pending {
                self.reset_pending = false;
                return Ok(()); // length 0: reset done
            }
            if !self.acks_reset && self.reports.is_empty() {
                return Err(I2cError::Nack); // a device that says nothing at all
            }
            if let Some(r) = self.reports.pop_front() {
                let len = (r.len() + 2) as u16; // the header may claim more than the read can hold
                inp[..2].copy_from_slice(&len.to_le_bytes());
                let n = r.len().min(inp.len() - 2);
                inp[2..2 + n].copy_from_slice(&r[..n]);
            }
            Ok(())
        }

        fn delay_ms(&mut self, ms: u32) {
            self.delayed_ms += ms;
        }
    }

    #[test]
    fn a_good_descriptor_parses_and_bad_ones_do_not() {
        let d = parse_descriptor(&descriptor_bytes(100)).unwrap();
        assert_eq!((d.report_desc_len, d.report_desc_reg, d.max_input, d.command_reg, d.vendor, d.product), (100, 2, 0x40, 5, 0x04F3, 0x3200));
        let mut b = descriptor_bytes(100);
        b[0] = 29; // wrong length
        assert!(parse_descriptor(&b).is_none());
        let mut b = descriptor_bytes(100);
        b[3] = 0x02; // wrong version
        assert!(parse_descriptor(&b).is_none());
        assert!(parse_descriptor(&descriptor_bytes(0)).is_none());
        assert!(parse_descriptor(&descriptor_bytes(9000)).is_none());
        let mut b = descriptor_bytes(100);
        b[10..12].copy_from_slice(&1u16.to_le_bytes()); // an input report could not even hold a length
        assert!(parse_descriptor(&b).is_none());
        assert!(parse_descriptor(&[0u8; 30]).is_none());
        assert!(parse_descriptor(&[0xFFu8; 30]).is_none());
        assert!(parse_descriptor(&[1, 2, 3]).is_none());
    }

    #[test]
    fn the_device_is_found_at_a_known_address_and_register() {
        let mut bus = FakeBus::new(0x15, alloc::vec![1, 2, 3]);
        let (addr, reg, d) = find_device(&mut bus).unwrap();
        assert_eq!((addr, reg, d.report_desc_len), (0x15, 0x0001, 3));
        let mut bus = FakeBus::new(0x2C, alloc::vec![1]);
        bus.desc_reg = 0x0020;
        assert_eq!(find_device(&mut bus).map(|x| (x.0, x.1)), Some((0x2C, 0x0020)));
        let mut bus = FakeBus::new(0x50, alloc::vec![1]); // not an address anyone tries
        assert!(find_device(&mut bus).is_none());
    }

    #[test]
    fn opening_powers_resets_and_reads_the_report_descriptor() {
        let rd = crate::hidparse::tests::touch_desc();
        let mut bus = FakeBus::new(0x15, rd.clone());
        let (addr, _, d) = find_device(&mut bus).unwrap();
        let dev = open(&mut bus, addr, d).unwrap();
        // SET_POWER (on) then RESET, each to the command register 0x0005.
        assert_eq!(bus.commands, [[0x05, 0x00, 0x00, 0x08], [0x05, 0x00, 0x00, 0x01]]);
        assert!(dev.reset_acknowledged);
        assert_eq!(dev.report_descriptor, rd);
    }

    #[test]
    fn a_device_that_never_acknowledges_the_reset_is_still_opened() {
        let mut bus = FakeBus::new(0x15, alloc::vec![0x05, 0x01]);
        bus.acks_reset = false;
        let (addr, _, d) = find_device(&mut bus).unwrap();
        let dev = open(&mut bus, addr, d).unwrap();
        assert!(!dev.reset_acknowledged);
        assert!(bus.delayed_ms >= 300, "waited {} ms", bus.delayed_ms); // bounded wait, not forever
    }

    #[test]
    fn input_reports_come_out_without_their_length_bytes() {
        let mut bus = FakeBus::new(0x15, alloc::vec![0x05, 0x01]);
        let (addr, _, d) = find_device(&mut bus).unwrap();
        let mut dev = open(&mut bus, addr, d).unwrap();
        assert_eq!(dev.poll(&mut bus).unwrap(), None); // nothing yet
        bus.reports.push_back(alloc::vec![4, 9, 8, 7]);
        assert_eq!(dev.poll(&mut bus).unwrap(), Some(&[4u8, 9, 8, 7][..]));
        assert_eq!(dev.poll(&mut bus).unwrap(), None);
        // A length that cannot be right is noise.
        bus.reports.push_back(alloc::vec![0u8; 100]); // 102 bytes do not fit a 64-byte read
        assert_eq!(dev.poll(&mut bus), Err(I2cError::Invalid));
        // A dead device is an error, not a panic.
        bus.addr = 0x77;
        assert_eq!(dev.poll(&mut bus), Err(I2cError::Nack));
    }

    #[test]
    fn a_touchpad_report_goes_all_the_way_to_pointer_movement() {
        let mut bus = FakeBus::new(0x15, crate::hidparse::tests::touch_desc());
        let (addr, _, d) = find_device(&mut bus).unwrap();
        let mut dev = open(&mut bus, addr, d).unwrap();
        let mut tp = crate::touchpad::Touchpad::new(&dev.report_descriptor).unwrap();
        let frame = |x: u16| {
            let mut r = [0u8; 15];
            r[0] = 4;
            r[1] = 0b11;
            r[2] = 1;
            r[3..5].copy_from_slice(&x.to_le_bytes());
            r[5..7].copy_from_slice(&500u16.to_le_bytes());
            r[14] = 1;
            r.to_vec()
        };
        bus.reports.push_back(frame(1000));
        bus.reports.push_back(frame(1400));
        let first = dev.poll(&mut bus).unwrap().map(|r| r.to_vec()).unwrap();
        assert_eq!(tp.feed(&first, 0), None);
        let second = dev.poll(&mut bus).unwrap().map(|r| r.to_vec()).unwrap();
        let m = tp.feed(&second, 10).unwrap();
        assert!(m.dx > 80 && m.dy == 0, "dx {}", m.dx);
    }
}
