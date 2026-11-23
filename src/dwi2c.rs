//! Synopsys DesignWare I2C master (the controller inside Intel's LPSS "Serial IO" on every recent laptop
//! chipset, and many other chips). Pure logic over the `Regs` trait, so it is tested on the host against a
//! fake register model; `touchpad_hw` supplies the real memory-mapped registers.
//!
//! Master use only: the controller is disabled, pointed at a target address, enabled, fed command words
//! (one per byte, with READ/STOP/RESTART flags) through a small FIFO, and drained as read data arrives.
//! Interrupts stay masked; everything is polled with timeouts.

use crate::i2chid::{I2cBus, I2cError};

pub const IC_CON: u32 = 0x00;
pub const IC_TAR: u32 = 0x04;
pub const IC_DATA_CMD: u32 = 0x10;
pub const IC_FS_SCL_HCNT: u32 = 0x1C;
pub const IC_FS_SCL_LCNT: u32 = 0x20;
pub const IC_INTR_MASK: u32 = 0x30;
pub const IC_RAW_INTR_STAT: u32 = 0x34;
pub const IC_CLR_INTR: u32 = 0x40;
pub const IC_CLR_TX_ABRT: u32 = 0x54;
pub const IC_ENABLE: u32 = 0x6C;
pub const IC_STATUS: u32 = 0x70;
pub const IC_TXFLR: u32 = 0x74;
pub const IC_RXFLR: u32 = 0x78;
pub const IC_TX_ABRT_SOURCE: u32 = 0x80;
pub const IC_ENABLE_STATUS: u32 = 0x9C;
pub const IC_COMP_PARAM_1: u32 = 0xF4;
pub const IC_COMP_TYPE: u32 = 0xFC;

/// What `IC_COMP_TYPE` reads on a DesignWare I2C block ("DW" and the component number).
pub const COMP_TYPE_DW_I2C: u32 = 0x4457_0140;

const CON_MASTER: u32 = 1 << 0;
const CON_SPEED_FAST: u32 = 2 << 1;
const CON_RESTART_EN: u32 = 1 << 5;
const CON_SLAVE_DISABLE: u32 = 1 << 6;

const CMD_READ: u32 = 1 << 8;
const CMD_STOP: u32 = 1 << 9;
const CMD_RESTART: u32 = 1 << 10;

const STATUS_TFE: u32 = 1 << 2;
const STATUS_MST_ACTIVITY: u32 = 1 << 5;
const RAW_TX_ABRT: u32 = 1 << 6;

/// Abort source bits that mean "nobody answered": address NACK (7 and 10 bit forms) and data NACK.
const ABRT_NACK_MASK: u32 = 0b1111;

/// The input clock is not known without ACPI. LPSS I2C blocks run at 133 MHz on recent Intel parts; if the
/// real clock differs the bus runs proportionally slower or faster, which a fast-mode device tolerates.
pub const ASSUMED_CLOCK_HZ: u32 = 133_000_000;
const TARGET_BUS_HZ: u32 = 350_000;

/// Register access and time for one controller.
pub trait Regs {
    fn read(&mut self, offset: u32) -> u32;
    fn write(&mut self, offset: u32, value: u32);
    fn now_us(&mut self) -> u64;
    fn delay_us(&mut self, us: u32);
}

pub struct DwI2c<R: Regs> {
    regs: R,
    tx_depth: u32,
    rx_depth: u32,
    /// Bus timing in use: SCL high and low counts.
    pub timing: (u32, u32),
}

/// SCL high and low counts for `TARGET_BUS_HZ` at `clock_hz`, leaving room for the controller's own delays.
pub fn scl_counts(clock_hz: u32) -> (u32, u32) {
    let period = clock_hz / TARGET_BUS_HZ;
    let high = (period * 2 / 5).saturating_sub(8).max(8);
    let low = (period - period * 2 / 5).saturating_sub(1).max(8);
    (high, low)
}

impl<R: Regs> DwI2c<R> {
    /// Checks the block really is a DesignWare I2C master, reads its FIFO sizes and sets the bus speed
    /// (keeping the counts the firmware left behind when they look sane).
    pub fn new(mut regs: R) -> Result<Self, &'static str> {
        if regs.read(IC_COMP_TYPE) != COMP_TYPE_DW_I2C {
            return Err("not a DesignWare I2C block");
        }
        let param = regs.read(IC_COMP_PARAM_1);
        let rx_depth = ((param >> 8) & 0xFF) + 1;
        let tx_depth = ((param >> 16) & 0xFF) + 1;
        // A FIFO of one entry or a nonsense parameter register: fall back to the smallest real hardware.
        let (tx_depth, rx_depth) = if tx_depth < 2 || rx_depth < 2 { (8, 8) } else { (tx_depth, rx_depth) };
        let (fh, fl) = (regs.read(IC_FS_SCL_HCNT) & 0xFFFF, regs.read(IC_FS_SCL_LCNT) & 0xFFFF);
        let timing = if (16..=0x8000).contains(&fh) && (16..=0x8000).contains(&fl) { (fh, fl) } else { scl_counts(ASSUMED_CLOCK_HZ) };
        let mut d = Self { regs, tx_depth, rx_depth, timing };
        d.disable().map_err(|_| "the controller will not switch off")?;
        d.regs.write(IC_CON, CON_MASTER | CON_SPEED_FAST | CON_RESTART_EN | CON_SLAVE_DISABLE);
        d.regs.write(IC_FS_SCL_HCNT, timing.0);
        d.regs.write(IC_FS_SCL_LCNT, timing.1);
        d.regs.write(IC_INTR_MASK, 0);
        Ok(d)
    }

    pub fn fifo_depths(&self) -> (u32, u32) {
        (self.tx_depth, self.rx_depth)
    }

    fn wait(&mut self, ms: u64, mut done: impl FnMut(&mut R) -> bool) -> bool {
        let start = self.regs.now_us();
        loop {
            if done(&mut self.regs) {
                return true;
            }
            if self.regs.now_us().saturating_sub(start) >= ms * 1000 {
                return false;
            }
            self.regs.delay_us(10);
        }
    }

    fn disable(&mut self) -> Result<(), I2cError> {
        self.regs.write(IC_ENABLE, 0);
        if self.wait(100, |r| r.read(IC_ENABLE_STATUS) & 1 == 0) {
            Ok(())
        } else {
            Err(I2cError::Timeout)
        }
    }

    fn enable(&mut self) -> Result<(), I2cError> {
        self.regs.write(IC_ENABLE, 1);
        if self.wait(100, |r| r.read(IC_ENABLE_STATUS) & 1 != 0) {
            Ok(())
        } else {
            Err(I2cError::Timeout)
        }
    }

    /// Writes `out` (if any) to the device at `addr`, then reads `inp.len()` bytes after a repeated start (if
    /// any). Either part may be empty, not both.
    pub fn transfer(&mut self, addr: u8, out: &[u8], inp: &mut [u8]) -> Result<(), I2cError> {
        let total = out.len() + inp.len();
        if total == 0 || out.len() > 0xFFFF || inp.len() > 0xFFFF || addr > 0x7F {
            return Err(I2cError::Invalid);
        }
        self.disable()?;
        // The target address may only change while the controller is disabled.
        self.regs.write(IC_TAR, u32::from(addr));
        self.regs.read(IC_CLR_INTR);
        self.enable()?;

        let (mut sent, mut received) = (0usize, 0usize);
        let start = self.regs.now_us();
        let result = loop {
            if let Some(e) = self.take_abort() {
                break Err(e);
            }
            // Feed the FIFO without overfilling it, and without asking for more bytes than the receive FIFO holds.
            let mut tx = self.regs.read(IC_TXFLR);
            while sent < total && tx < self.tx_depth {
                let outstanding = (sent.saturating_sub(out.len()) - received) as u32;
                if sent >= out.len() && outstanding >= self.rx_depth {
                    break;
                }
                let last = sent + 1 == total;
                let cmd = if sent < out.len() {
                    u32::from(out[sent]) | if last { CMD_STOP } else { 0 }
                } else {
                    CMD_READ | if sent == out.len() && !out.is_empty() { CMD_RESTART } else { 0 } | if last { CMD_STOP } else { 0 }
                };
                self.regs.write(IC_DATA_CMD, cmd);
                sent += 1;
                tx += 1;
            }
            let mut rx = self.regs.read(IC_RXFLR);
            while received < inp.len() && rx > 0 {
                inp[received] = self.regs.read(IC_DATA_CMD) as u8;
                received += 1;
                rx -= 1;
            }
            if sent == total && received == inp.len() {
                break Ok(());
            }
            if self.regs.now_us().saturating_sub(start) > 200_000 {
                break Err(I2cError::Timeout);
            }
            self.regs.delay_us(5);
        };
        // A write is "done" as soon as its bytes are queued, but a missing device only says so (NACK) once the
        // controller has put them on the wire. So wait for the STOP to finish, then look for an abort again.
        let result = if result.is_ok() {
            self.wait(50, |r| r.read(IC_STATUS) & (STATUS_MST_ACTIVITY | STATUS_TFE) == STATUS_TFE);
            self.take_abort().map_or(Ok(()), Err)
        } else {
            result
        };
        let off = self.disable();
        result.and(off)
    }

    /// If the controller aborted the transfer, clears that and says why.
    fn take_abort(&mut self) -> Option<I2cError> {
        if self.regs.read(IC_RAW_INTR_STAT) & RAW_TX_ABRT == 0 {
            return None;
        }
        let src = self.regs.read(IC_TX_ABRT_SOURCE);
        self.regs.read(IC_CLR_TX_ABRT);
        Some(if src & ABRT_NACK_MASK != 0 { I2cError::Nack } else { I2cError::Abort(src) })
    }
}

impl<R: Regs> I2cBus for DwI2c<R> {
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<(), I2cError> {
        self.transfer(addr, data, &mut [])
    }

    fn write_read(&mut self, addr: u8, out: &[u8], inp: &mut [u8]) -> Result<(), I2cError> {
        self.transfer(addr, out, inp)
    }

    fn read(&mut self, addr: u8, inp: &mut [u8]) -> Result<(), I2cError> {
        self.transfer(addr, &[], inp)
    }

    fn delay_ms(&mut self, ms: u32) {
        self.regs.delay_us(ms.saturating_mul(1000));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    /// A DesignWare block with a device behind it. Commands in the TX FIFO are executed whenever time passes;
    /// breaking a hardware rule (TAR written while enabled, FIFO overflow) is recorded in `violations`.
    struct Fake {
        enabled: bool,
        tar: u32,
        tx: VecDeque<u32>,
        rx: VecDeque<u8>,
        abort_source: u32,
        raw: u32,
        clock: u64,
        // The device behind the bus.
        device_addr: u32,
        device_out: VecDeque<u8>,
        device_in: Vec<u8>,
        restarts: usize,
        stops: usize,
        violations: Vec<&'static str>,
        comp_type: u32,
        param: u32,
        hcnt: u32,
        lcnt: u32,
        written: Vec<(u32, u32)>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                enabled: false,
                tar: 0,
                tx: VecDeque::new(),
                rx: VecDeque::new(),
                abort_source: 0,
                raw: 0,
                clock: 0,
                device_addr: 0x15,
                device_out: VecDeque::new(),
                device_in: Vec::new(),
                restarts: 0,
                stops: 0,
                violations: Vec::new(),
                comp_type: COMP_TYPE_DW_I2C,
                param: (7 << 16) | (7 << 8), // 8-entry FIFOs
                hcnt: 0,
                lcnt: 0,
                written: Vec::new(),
            }
        }

        fn run(&mut self) {
            while let Some(c) = self.tx.pop_front() {
                if self.tar != self.device_addr {
                    self.raw |= RAW_TX_ABRT;
                    self.abort_source = 1;
                    self.tx.clear();
                    return;
                }
                if c & CMD_RESTART != 0 {
                    self.restarts += 1;
                }
                if c & CMD_STOP != 0 {
                    self.stops += 1;
                }
                if c & CMD_READ != 0 {
                    if self.rx.len() >= 8 {
                        self.violations.push("receive FIFO overflow");
                    }
                    self.rx.push_back(self.device_out.pop_front().unwrap_or(0));
                } else {
                    self.device_in.push(c as u8);
                }
            }
        }
    }

    impl Regs for Fake {
        fn read(&mut self, off: u32) -> u32 {
            match off {
                IC_COMP_TYPE => self.comp_type,
                IC_COMP_PARAM_1 => self.param,
                IC_FS_SCL_HCNT => self.hcnt,
                IC_FS_SCL_LCNT => self.lcnt,
                IC_ENABLE_STATUS => u32::from(self.enabled),
                IC_RAW_INTR_STAT => self.raw,
                IC_TX_ABRT_SOURCE => self.abort_source,
                IC_CLR_TX_ABRT => {
                    self.raw &= !RAW_TX_ABRT;
                    self.abort_source = 0;
                    0
                }
                IC_CLR_INTR => {
                    self.raw = 0;
                    0
                }
                IC_TXFLR => self.tx.len() as u32,
                IC_RXFLR => self.rx.len() as u32,
                IC_STATUS => STATUS_TFE,
                IC_DATA_CMD => u32::from(self.rx.pop_front().unwrap_or(0)),
                _ => 0,
            }
        }

        fn write(&mut self, off: u32, v: u32) {
            self.written.push((off, v));
            match off {
                IC_ENABLE => {
                    self.enabled = v & 1 != 0;
                    if !self.enabled {
                        self.tx.clear();
                        self.rx.clear();
                    }
                }
                IC_TAR => {
                    if self.enabled {
                        self.violations.push("TAR written while enabled");
                    }
                    self.tar = v;
                }
                IC_DATA_CMD => {
                    if !self.enabled {
                        self.violations.push("command while disabled");
                    }
                    if self.tx.len() >= 8 {
                        self.violations.push("transmit FIFO overflow");
                    }
                    if self.raw & RAW_TX_ABRT == 0 {
                        self.tx.push_back(v);
                    }
                }
                _ => {}
            }
        }

        fn now_us(&mut self) -> u64 {
            self.clock += 1;
            self.run();
            self.clock
        }

        fn delay_us(&mut self, us: u32) {
            self.clock += u64::from(us);
            self.run();
        }
    }

    fn dw() -> DwI2c<Fake> {
        DwI2c::new(Fake::new()).unwrap()
    }

    #[test]
    fn only_a_designware_block_is_accepted() {
        let mut f = Fake::new();
        f.comp_type = 0;
        assert!(DwI2c::new(f).is_err());
        let d = dw();
        assert_eq!(d.fifo_depths(), (8, 8));
        // Timing: nothing from firmware, so computed for the assumed clock; both counts are plausible.
        assert_eq!(d.timing, scl_counts(ASSUMED_CLOCK_HZ));
        assert!(d.timing.0 > 100 && d.timing.1 > d.timing.0);
        // Firmware's own counts are kept.
        let mut f = Fake::new();
        f.hcnt = 0x90;
        f.lcnt = 0xE0;
        assert_eq!(DwI2c::new(f).unwrap().timing, (0x90, 0xE0));
        // A silly parameter register falls back to small FIFOs.
        let mut f = Fake::new();
        f.param = 0;
        assert_eq!(DwI2c::new(f).unwrap().fifo_depths(), (8, 8));
    }

    #[test]
    fn a_write_sends_every_byte_and_stops_on_the_last() {
        let mut d = dw();
        d.write(0x15, &[0x01, 0x00, 0xAA]).unwrap();
        assert_eq!(d.regs.device_in, [0x01, 0x00, 0xAA]);
        assert_eq!((d.regs.restarts, d.regs.stops), (0, 1));
        assert!(d.regs.violations.is_empty(), "{:?}", d.regs.violations);
        assert!(!d.regs.enabled);
    }

    #[test]
    fn a_register_read_uses_a_repeated_start() {
        let mut d = dw();
        d.regs.device_out.extend([0x1E, 0x00, 0x00, 0x01]);
        let mut b = [0u8; 4];
        d.write_read(0x15, &[0x01, 0x00], &mut b).unwrap();
        assert_eq!(b, [0x1E, 0x00, 0x00, 0x01]);
        assert_eq!(d.regs.device_in, [0x01, 0x00]);
        assert_eq!((d.regs.restarts, d.regs.stops), (1, 1));
        assert!(d.regs.violations.is_empty(), "{:?}", d.regs.violations);
    }

    #[test]
    fn a_long_read_flows_through_small_fifos() {
        let mut d = dw();
        d.regs.device_out.extend((0..200u32).map(|i| i as u8));
        let mut b = [0u8; 200];
        d.read(0x15, &mut b).unwrap();
        assert!(b.iter().enumerate().all(|(i, &v)| v == i as u8));
        assert_eq!((d.regs.restarts, d.regs.stops), (0, 1));
        assert!(d.regs.violations.is_empty(), "{:?}", d.regs.violations);
    }

    #[test]
    fn nobody_home_is_a_nack_and_the_next_transfer_still_works() {
        let mut d = dw();
        assert_eq!(d.write(0x2C, &[1, 2]), Err(I2cError::Nack));
        assert!(!d.regs.enabled, "left enabled after an abort");
        // The abort is cleared: a good address works straight after.
        d.write(0x15, &[7]).unwrap();
        assert_eq!(d.regs.device_in, [7]);
    }

    #[test]
    fn impossible_requests_are_refused_without_touching_the_bus() {
        let mut d = dw();
        let before = d.regs.written.len();
        assert_eq!(d.transfer(0x15, &[], &mut []), Err(I2cError::Invalid));
        assert_eq!(d.transfer(0x80, &[1], &mut []), Err(I2cError::Invalid));
        assert_eq!(d.regs.written.len(), before);
    }

    #[test]
    fn a_stuck_controller_times_out_instead_of_hanging() {
        let mut d = dw();
        // The enable never takes effect.
        struct Stuck(Fake);
        impl Regs for Stuck {
            fn read(&mut self, off: u32) -> u32 {
                if off == IC_ENABLE_STATUS { 1 } else { self.0.read(off) }
            }
            fn write(&mut self, off: u32, v: u32) {
                self.0.write(off, v)
            }
            fn now_us(&mut self) -> u64 {
                self.0.now_us()
            }
            fn delay_us(&mut self, us: u32) {
                self.0.delay_us(us)
            }
        }
        assert!(DwI2c::new(Stuck(Fake::new())).is_err()); // never reports "disabled"
        d.regs.tar = 0x15;
        assert_eq!(scl_counts(100_000_000).0 < scl_counts(133_000_000).0, true);
    }
}
