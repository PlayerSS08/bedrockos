//! Realtek RTL8852B-family Wi-Fi 6 chip (the OMEN's RTL8852BE-VT, PCI `10EC:B520`): driver, written in stages.
//! Ported from the Linux `rtw89` driver (drivers/net/wireless/realtek/rtw89, SPDX GPL-2.0 OR BSD-3-Clause,
//! Copyright Realtek Corporation), which is the only public description of this chip.
//!
//! Stage 1 (this file): find the chip, enable it on the PCI bus, map its registers (BAR 2: the old `wifi diag`
//! looked at BAR 0, which is an I/O window on this chip), power the MAC on exactly as Linux does
//! (`rtw8852bt_pwr_on_func`), read what the chip says about itself, and power it off again.
//!
//! NOT YET WRITTEN, in this order: firmware download (needs the DMA rings and DMAC/DLE/HFC set-up), BB/RF
//! tables and calibration, scan/join through firmware commands, data path. Nothing here has run on real
//! hardware yet: `wifi probe` exists so the first real run produces the log the next stage is built from.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

pub use crate::rtw89_regs::*;

pub const VENDOR_REALTEK: u16 = 0x10EC;
/// RTL8852BE-VT, the variant Linux drives with `rtw8852bt`.
pub const DEVICE_8852BT: u16 = 0xB520;
/// Linux lists this exact laptop (HP, subsystem 103C:88E9) with a thermal-protection quirk.
pub const OMEN_SUBSYSTEM: (u16, u16) = (0x103C, 0x88E9);
/// Registers live in PCI BAR 2 (BAR 0 is an I/O window).
pub const MMIO_BAR: u8 = 2;
/// `R_AX_WLLPS_CTRL` value written when powering off.
pub const SW_LPS_OPTION: u32 = 0x0001_A0B2;

/// Position of the lowest set bit of a mask (the shift of a register field).
pub const fn shift(mask: u32) -> u32 {
    mask.trailing_zeros()
}

/// Reads the field `mask` out of `value`.
pub const fn get_field(value: u32, mask: u32) -> u32 {
    (value & mask) >> shift(mask)
}

/// Returns `value` with the field `mask` set to `field`.
pub const fn set_field(value: u32, mask: u32, field: u32) -> u32 {
    (value & !mask) | ((field << shift(mask)) & mask)
}

/// What `R_AX_IC_PWR_STATE` says about the MAC.
pub fn describe_power_state(v: u32) -> &'static str {
    match get_field(v, B_AX_WLMAC_PWR_STE_MASK) {
        0 => "off",
        1 => "on",
        2 => "in low power",
        _ => "unknown",
    }
}

/// The value written to the analog-die (xtal SI) control register for one write.
pub const fn xtal_si_write_cmd(offset: u32, val: u32, mask: u32) -> u32 {
    set_field(0, B_AX_WL_XTAL_SI_ADDR_MASK, offset)
        | set_field(0, B_AX_WL_XTAL_SI_DATA_MASK, val)
        | set_field(0, B_AX_WL_XTAL_SI_BITMASK_MASK, mask)
        | set_field(0, B_AX_WL_XTAL_SI_MODE_MASK, XTAL_SI_NORMAL_WRITE)
        | B_AX_WL_XTAL_SI_CMD_POLL
}

pub const fn xtal_si_read_cmd(offset: u32) -> u32 {
    set_field(0, B_AX_WL_XTAL_SI_ADDR_MASK, offset) | set_field(0, B_AX_WL_XTAL_SI_MODE_MASK, XTAL_SI_NORMAL_READ) | B_AX_WL_XTAL_SI_CMD_POLL
}

/// The chip's identity as far as stage 1 can tell, for the report and for the next stage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Identity {
    pub cut_version: u32,
    pub xtal_chip_id: Option<u16>,
}

/// One step of a power sequence: a register write that is a no-op on the host, so the sequence can be
/// checked against Linux's by a test that replays it on a fake register file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Set32(u32, u32),
    Clr32(u32, u32),
    Mask32(u32, u32, u32),
    Set8(u32, u32),
    Clr8(u32, u32),
    Write32(u32, u32),
    /// Wait until `(reg & mask) == want` (up to 20 ms in Linux).
    Poll32 { reg: u32, mask: u32, want: u32 },
    Xtal { offset: u32, val: u32, mask: u32 },
    Delay1ms,
}

/// The register-level part of `rtw8852bt_pwr_on_func`, in Linux's order. `efuse_power_k` is false because
/// stage 1 does not read the efuse (Linux then skips the two voltage trims too).
pub fn power_on_sequence() -> Vec<Op> {
    use Op::*;
    let mut v = alloc::vec![
        Set32(R_AX_LDO_AON_CTRL0, B_AX_PD_REGU_L),
        Clr32(R_AX_SYS_PW_CTRL, B_AX_AFSM_WLSUS_EN | B_AX_AFSM_PCIE_SUS_EN),
        Set32(R_AX_SYS_PW_CTRL, B_AX_DIS_WLBT_PDNSUSEN_SOPC),
        Set32(R_AX_WLLPS_CTRL, B_AX_DIS_WLBT_LPSEN_LOPC),
        Clr32(R_AX_SYS_PW_CTRL, B_AX_APDM_HPDN),
        Clr32(R_AX_SYS_PW_CTRL, B_AX_APFM_SWLPS),
        Mask32(R_AX_SPS_DIG_ON_CTRL0, B_AX_OCP_L1_MASK, 7),
        Poll32 { reg: R_AX_SYS_PW_CTRL, mask: B_AX_RDY_SYSPWR, want: B_AX_RDY_SYSPWR },
        Set32(R_AX_SYS_PW_CTRL, B_AX_EN_WLON),
        Set32(R_AX_SYS_PW_CTRL, B_AX_APFN_ONMAC),
        Poll32 { reg: R_AX_SYS_PW_CTRL, mask: B_AX_APFN_ONMAC, want: 0 },
    ];
    for _ in 0..2 {
        v.push(Set8(R_AX_PLATFORM_ENABLE, B_AX_PLATFORM_EN));
        v.push(Clr8(R_AX_PLATFORM_ENABLE, B_AX_PLATFORM_EN));
    }
    v.push(Set8(R_AX_PLATFORM_ENABLE, B_AX_PLATFORM_EN));
    v.push(Clr32(R_AX_SYS_SDIO_CTRL, B_AX_PCIE_CALIB_EN_V1));
    v.push(Set32(R_AX_SYS_ADIE_PAD_PWR_CTRL, B_AX_SYM_PADPDN_WL_PTA_1P3));
    v.push(Xtal { offset: XTAL_SI_ANAPAR_WL, val: XTAL_SI_GND_SHDN_WL, mask: XTAL_SI_GND_SHDN_WL });
    v.push(Set32(R_AX_SYS_ADIE_PAD_PWR_CTRL, B_AX_SYM_PADPDN_WL_RFC_1P3));
    v.extend([
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: XTAL_SI_SHDN_WL, mask: XTAL_SI_SHDN_WL },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: XTAL_SI_OFF_WEI, mask: XTAL_SI_OFF_WEI },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: XTAL_SI_OFF_EI, mask: XTAL_SI_OFF_EI },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: 0, mask: XTAL_SI_RFC2RF },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: XTAL_SI_PON_WEI, mask: XTAL_SI_PON_WEI },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: XTAL_SI_PON_EI, mask: XTAL_SI_PON_EI },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: 0, mask: XTAL_SI_SRAM2RFC },
        Xtal { offset: XTAL_SI_SRAM_CTRL, val: 0, mask: XTAL_SI_SRAM_DIS },
        Xtal { offset: XTAL_SI_XTAL_XMD_2, val: 0, mask: XTAL_SI_LDO_LPS },
        Xtal { offset: XTAL_SI_XTAL_XMD_4, val: 0, mask: XTAL_SI_LPS_CAP },
        Set32(R_AX_PMC_DBG_CTRL2, B_AX_SYSON_DIS_PMCR_AX_WRMSK),
        Set32(R_AX_SYS_ISO_CTRL, B_AX_ISO_EB2CORE),
        Clr32(R_AX_SYS_ISO_CTRL, B_AX_PWC_EV2EF_B15),
        Delay1ms,
        Clr32(R_AX_SYS_ISO_CTRL, B_AX_PWC_EV2EF_B14),
        Clr32(R_AX_PMC_DBG_CTRL2, B_AX_SYSON_DIS_PMCR_AX_WRMSK),
        Set32(
            R_AX_DMAC_FUNC_EN,
            B_AX_MAC_FUNC_EN
                | B_AX_DMAC_FUNC_EN
                | B_AX_MPDU_PROC_EN
                | B_AX_WD_RLS_EN
                | B_AX_DLE_WDE_EN
                | B_AX_TXPKT_CTRL_EN
                | B_AX_STA_SCH_EN
                | B_AX_DLE_PLE_EN
                | B_AX_PKT_BUF_EN
                | B_AX_DMAC_TBL_EN
                | B_AX_PKT_IN_EN
                | B_AX_DLE_CPUIO_EN
                | B_AX_DISPATCHER_EN
                | B_AX_BBRPT_EN
                | B_AX_MAC_SEC_EN
                | B_AX_DMACREG_GCKEN,
        ),
        Set32(
            R_AX_CMAC_FUNC_EN,
            B_AX_CMAC_EN
                | B_AX_CMAC_TXEN
                | B_AX_CMAC_RXEN
                | B_AX_FORCE_CMACREG_GCKEN
                | B_AX_PHYINTF_EN
                | B_AX_CMAC_DMA_EN
                | B_AX_PTCLTOP_EN
                | B_AX_SCHEDULER_EN
                | B_AX_TMAC_EN
                | B_AX_RMAC_EN,
        ),
        Mask32(R_AX_EECS_EESK_FUNC_SEL, B_AX_PINMUX_EESK_FUNC_SEL_MASK, 1),
    ]);
    v
}

/// The register-level part of `rtw8852bt_pwr_off_func`, in Linux's order.
pub fn power_off_sequence() -> Vec<Op> {
    use Op::*;
    alloc::vec![
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: XTAL_SI_RFC2RF, mask: XTAL_SI_RFC2RF },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: 0, mask: XTAL_SI_OFF_EI },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: 0, mask: XTAL_SI_OFF_WEI },
        Xtal { offset: XTAL_SI_WL_RFC_S0, val: 0, mask: XTAL_SI_RF00 },
        Xtal { offset: XTAL_SI_WL_RFC_S1, val: 0, mask: XTAL_SI_RF10 },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: XTAL_SI_SRAM2RFC, mask: XTAL_SI_SRAM2RFC },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: 0, mask: XTAL_SI_PON_EI },
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: 0, mask: XTAL_SI_PON_WEI },
        Set32(R_AX_SYS_PW_CTRL, B_AX_EN_WLON),
        Clr32(R_AX_WLRF_CTRL, B_AX_AFC_AFEDIG),
        Clr8(R_AX_SYS_FUNC_EN, B_AX_FEN_BB_GLB_RSTN | B_AX_FEN_BBRSTB),
        Clr32(R_AX_SYS_ADIE_PAD_PWR_CTRL, B_AX_SYM_PADPDN_WL_RFC_1P3),
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: 0, mask: XTAL_SI_SHDN_WL },
        Clr32(R_AX_SYS_ADIE_PAD_PWR_CTRL, B_AX_SYM_PADPDN_WL_PTA_1P3),
        Xtal { offset: XTAL_SI_ANAPAR_WL, val: 0, mask: XTAL_SI_GND_SHDN_WL },
        Set32(R_AX_SYS_PW_CTRL, B_AX_APFM_OFFMAC),
        Poll32 { reg: R_AX_SYS_PW_CTRL, mask: B_AX_APFM_OFFMAC, want: 0 },
        Write32(R_AX_WLLPS_CTRL, SW_LPS_OPTION),
        Set32(R_AX_SYS_SWR_CTRL1, B_AX_SYM_CTRL_SPS_PWMFREQ),
        Mask32(R_AX_SPS_DIG_ON_CTRL0, B_AX_REG_ZCDC_H_MASK, 3),
        Set32(R_AX_SYS_PW_CTRL, B_AX_APFM_SWLPS),
    ]
}

/// A register file the sequences can run against: the real chip, or a fake in tests.
pub trait Regs {
    fn r32(&mut self, off: u32) -> u32;
    fn w32(&mut self, off: u32, v: u32);
    fn r8(&mut self, off: u32) -> u8;
    fn w8(&mut self, off: u32, v: u8);
    fn delay_us(&mut self, us: u64);
    /// Polls `cond` against register reads for up to `ms`; true if it held.
    fn poll(&mut self, ms: u64, f: &mut dyn FnMut(&mut Self) -> bool) -> bool;
}

fn xtal_write<R: Regs>(r: &mut R, offset: u32, val: u32, mask: u32) -> Result<(), String> {
    r.w32(R_AX_WLAN_XTAL_SI_CTRL, xtal_si_write_cmd(offset, val, mask));
    if !r.poll(50, &mut |r| r.r32(R_AX_WLAN_XTAL_SI_CTRL) & B_AX_WL_XTAL_SI_CMD_POLL == 0) {
        return Err(alloc::format!("analog die not ready (write offset {offset:#x})"));
    }
    Ok(())
}

pub fn xtal_read<R: Regs>(r: &mut R, offset: u32) -> Result<u8, String> {
    r.w32(R_AX_WLAN_XTAL_SI_CTRL, xtal_si_read_cmd(offset));
    let mut last = 0;
    if !r.poll(50, &mut |r| {
        last = r.r32(R_AX_WLAN_XTAL_SI_CTRL);
        last & B_AX_WL_XTAL_SI_CMD_POLL == 0
    }) {
        return Err(alloc::format!("analog die not ready (read offset {offset:#x})"));
    }
    Ok(get_field(last, B_AX_WL_XTAL_SI_DATA_MASK) as u8)
}

/// Runs a sequence. Stops at the first failure and says which step failed.
pub fn run<R: Regs>(r: &mut R, seq: &[Op]) -> Result<(), String> {
    for (i, op) in seq.iter().enumerate() {
        match *op {
            Op::Set32(reg, bits) => {
                let v = r.r32(reg);
                r.w32(reg, v | bits);
            }
            Op::Clr32(reg, bits) => {
                let v = r.r32(reg);
                r.w32(reg, v & !bits);
            }
            Op::Mask32(reg, mask, field) => {
                let v = r.r32(reg);
                r.w32(reg, set_field(v, mask, field));
            }
            Op::Set8(reg, bits) => {
                let v = r.r8(reg);
                r.w8(reg, v | bits as u8);
            }
            Op::Clr8(reg, bits) => {
                let v = r.r8(reg);
                r.w8(reg, v & !(bits as u8));
            }
            Op::Write32(reg, v) => r.w32(reg, v),
            Op::Poll32 { reg, mask, want } => {
                if !r.poll(20, &mut |r| r.r32(reg) & mask == want) {
                    return Err(alloc::format!("step {i}: register {reg:#06x} never reached {want:#x} under mask {mask:#x} (now {:#x})", r.r32(reg)));
                }
            }
            Op::Xtal { offset, val, mask } => xtal_write(r, offset, val, mask).map_err(|e| alloc::format!("step {i}: {e}"))?,
            Op::Delay1ms => r.delay_us(1000),
        }
    }
    Ok(())
}

/// Powers the MAC on as Linux's `rtw89_mac_pwr_on` does: if it is already on, power off first and retry.
pub fn power_on<R: Regs>(r: &mut R, log: &mut String) -> Result<(), String> {
    let state = r.r32(R_AX_IC_PWR_STATE);
    let _ = writeln!(log, "  power state before: {:#010x} ({})", state, describe_power_state(state));
    if get_field(state, B_AX_WLMAC_PWR_STE_MASK) == MAC_AX_MAC_ON {
        let _ = writeln!(log, "  MAC is already on (firmware left it on): powering off first, as Linux does");
        run(r, &power_off_sequence())?;
    }
    run(r, &power_on_sequence())?;
    let after = r.r32(R_AX_IC_PWR_STATE);
    let _ = writeln!(log, "  power state after power-on: {:#010x} ({})", after, describe_power_state(after));
    Ok(())
}

pub fn identity<R: Regs>(r: &mut R) -> Identity {
    let cut_version = get_field(r.r32(R_AX_SYS_CFG1), B_AX_CHIP_VER_MASK);
    let id = match (xtal_read(r, XTAL_SI_CHIP_ID_L), xtal_read(r, XTAL_SI_CHIP_ID_H)) {
        (Ok(l), Ok(h)) => Some(u16::from(l) | u16::from(h) << 8),
        _ => None,
    };
    Identity { cut_version, xtal_chip_id: id }
}

#[cfg(target_os = "uefi")]
mod hw {
    use super::*;
    use crate::{arch, log as klog, mmio, pci};
    use core::ptr::{read_volatile, write_volatile};

    pub struct Mmio {
        pub base: u64,
    }

    impl Regs for Mmio {
        fn r32(&mut self, off: u32) -> u32 {
            mmio::rd32(self.base + u64::from(off))
        }
        fn w32(&mut self, off: u32, v: u32) {
            mmio::wr32(self.base + u64::from(off), v);
        }
        fn r8(&mut self, off: u32) -> u8 {
            // SAFETY: a register inside the mapped BAR; byte reads are what Linux does for this register.
            unsafe { read_volatile((self.base + u64::from(off)) as *const u8) }
        }
        fn w8(&mut self, off: u32, v: u8) {
            // SAFETY: as in `r8`.
            unsafe { write_volatile((self.base + u64::from(off)) as *mut u8, v) };
        }
        fn delay_us(&mut self, us: u64) {
            let t0 = arch::rdtsc();
            let ticks = klog::tsc_hz() / 1_000_000 * us;
            while arch::rdtsc().wrapping_sub(t0) < ticks {
                arch::pause();
            }
        }
        fn poll(&mut self, ms: u64, f: &mut dyn FnMut(&mut Self) -> bool) -> bool {
            let t0 = arch::rdtsc();
            let ticks = klog::tsc_hz() / 1000 * ms;
            loop {
                if f(self) {
                    return true;
                }
                if arch::rdtsc().wrapping_sub(t0) >= ticks {
                    return false;
                }
                arch::pause();
            }
        }
    }

    /// `wifi probe`: stage 1 on the real chip. Returns the text to show and to save in the report.
    pub fn probe(power_cycle: bool) -> String {
        let mut s = String::new();
        let Some(dev) = pci::find(VENDOR_REALTEK, DEVICE_8852BT) else {
            let mut other = String::new();
            pci::for_each(|d| {
                if d.header.vendor == VENDOR_REALTEK {
                    let _ = write!(other, " {:04x}:{:04x}", d.header.vendor, d.header.device);
                }
            });
            let _ = writeln!(s, "wifi probe: no RTL8852BE-VT (10ec:b520) on the PCI bus. Realtek devices seen:{}", if other.is_empty() { " none" } else { &other });
            return s;
        };
        let (bus, d, f) = (dev.bus, dev.dev, dev.func);
        let sub = pci::config_read(bus, d, f, 0x2C);
        let (sv, sd) = ((sub & 0xFFFF) as u16, (sub >> 16) as u16);
        let _ = writeln!(s, "RTL8852BE-VT at {bus:02x}:{d:02x}.{f}, subsystem {sv:04x}:{sd:04x}{}", if (sv, sd) == OMEN_SUBSYSTEM { " (the HP OMEN Linux lists)" } else { "" });
        let Some(base) = pci::memory_bar(&dev, MMIO_BAR) else {
            let _ = writeln!(s, "  BAR 2 is not a memory BAR or the firmware did not assign it; BAR 0 = {:?}, BAR 1 = {:?}", pci::memory_bar(&dev, 0), pci::memory_bar(&dev, 1));
            return s;
        };
        // Memory space + bus master, what every PCI driver does first.
        let cmd = pci::config_read(bus, d, f, 4);
        pci::config_write(bus, d, f, 4, (cmd & 0xFFFF) | 0x6);
        let _ = writeln!(s, "  BAR 2 at {base:#x}, PCI command {:#06x} -> {:#06x}", cmd & 0xFFFF, (cmd & 0xFFFF) | 0x6);
        // The register window is below 1 MiB; two 2 MiB pages cover it wherever the BAR sits.
        if !(crate::paging::map_mmio_uc(base) && crate::paging::map_mmio_uc(base + 0xFFFFF)) {
            let _ = writeln!(s, "  cannot map the register window");
            return s;
        }
        let mut r = Mmio { base };
        let _ = writeln!(s, "before power-on:");
        let _ = writeln!(s, "  SYS_CFG1 {:#010x}  SYS_STATUS1 {:#010x}  IC_PWR_STATE {:#010x}", r.r32(R_AX_SYS_CFG1), r.r32(R_AX_SYS_STATUS1), r.r32(R_AX_IC_PWR_STATE));
        if r.r32(R_AX_SYS_CFG1) == u32::MAX {
            let _ = writeln!(s, "  all ones: the chip is not answering (asleep, wrong BAR or no link)");
            return s;
        }
        if !power_cycle {
            let _ = writeln!(s, "(read-only; `wifi probe on` also powers the chip on and off)");
            return s;
        }
        let _ = writeln!(s, "power on:");
        match power_on(&mut r, &mut s) {
            Ok(()) => {
                let id = identity(&mut r);
                let _ = writeln!(s, "  cut version {}  analog-die chip id {}", id.cut_version, id.xtal_chip_id.map_or(String::from("unreadable"), |v| alloc::format!("{v:#06x}")));
                let _ = writeln!(
                    s,
                    "  DMAC_FUNC_EN {:#010x}  CMAC_FUNC_EN {:#010x}  SYS_STATUS1 {:#010x}",
                    r.r32(R_AX_DMAC_FUNC_EN),
                    r.r32(R_AX_CMAC_FUNC_EN),
                    r.r32(R_AX_SYS_STATUS1)
                );
                let _ = writeln!(s, "  WCPU_FW_CTRL {:#010x}  BOOT_REASON {:#06x}  UDM0..3 {:08x} {:08x} {:08x} {:08x}", r.r32(R_AX_WCPU_FW_CTRL), r.r8(R_AX_BOOT_REASON), r.r32(R_AX_UDM0), r.r32(R_AX_UDM1), r.r32(R_AX_UDM2), r.r32(R_AX_UDM3));
            }
            Err(e) => {
                let _ = writeln!(s, "  POWER-ON FAILED: {e}");
            }
        }
        let _ = writeln!(s, "power off:");
        match run(&mut r, &power_off_sequence()) {
            Ok(()) => {
                let st = r.r32(R_AX_IC_PWR_STATE);
                let _ = writeln!(s, "  power state: {:#010x} ({})", st, describe_power_state(st));
            }
            Err(e) => {
                let _ = writeln!(s, "  POWER-OFF FAILED: {e}");
            }
        }
        s
    }
}

#[cfg(target_os = "uefi")]
pub use hw::probe;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A register file that remembers writes and plays the chip's "ready" answers.
    #[derive(Default)]
    struct Fake {
        regs: HashMap<u32, u32>,
        log: Vec<String>,
        xtal: HashMap<u32, u32>,
        fail_poll_on: Option<u32>,
    }

    impl Fake {
        fn new() -> Self {
            let mut f = Fake::default();
            f.regs.insert(R_AX_SYS_PW_CTRL, B_AX_RDY_SYSPWR);
            f
        }
    }

    impl Regs for Fake {
        fn r32(&mut self, off: u32) -> u32 {
            let v = *self.regs.get(&off).unwrap_or(&0);
            if off == R_AX_SYS_PW_CTRL {
                // The chip raises "system power ready" on its own and clears the on-MAC request when done.
                return (v | B_AX_RDY_SYSPWR) & !(B_AX_APFN_ONMAC | B_AX_APFM_OFFMAC) | if self.fail_poll_on == Some(off) { B_AX_APFN_ONMAC } else { 0 };
            }
            if off == R_AX_WLAN_XTAL_SI_CTRL {
                return v & !B_AX_WL_XTAL_SI_CMD_POLL;
            }
            v
        }
        fn w32(&mut self, off: u32, v: u32) {
            self.log.push(alloc::format!("w32 {off:#x} {v:#x}"));
            if off == R_AX_WLAN_XTAL_SI_CTRL {
                let a = get_field(v, B_AX_WL_XTAL_SI_ADDR_MASK);
                let d = get_field(v, B_AX_WL_XTAL_SI_DATA_MASK);
                let m = get_field(v, B_AX_WL_XTAL_SI_BITMASK_MASK);
                if get_field(v, B_AX_WL_XTAL_SI_MODE_MASK) == XTAL_SI_NORMAL_WRITE {
                    let old = *self.xtal.get(&a).unwrap_or(&0);
                    self.xtal.insert(a, (old & !m) | (d & m));
                } else {
                    let val = *self.xtal.get(&a).unwrap_or(&0);
                    self.regs.insert(off, set_field(v, B_AX_WL_XTAL_SI_DATA_MASK, val));
                    return;
                }
            }
            self.regs.insert(off, v);
        }
        fn r8(&mut self, off: u32) -> u8 {
            *self.regs.get(&off).unwrap_or(&0) as u8
        }
        fn w8(&mut self, off: u32, v: u8) {
            self.log.push(alloc::format!("w8 {off:#x} {v:#x}"));
            self.regs.insert(off, u32::from(v));
        }
        fn delay_us(&mut self, _: u64) {}
        fn poll(&mut self, _: u64, f: &mut dyn FnMut(&mut Self) -> bool) -> bool {
            f(self)
        }
    }

    #[test]
    fn field_helpers() {
        assert_eq!(shift(0xF000), 12);
        assert_eq!(get_field(0xABCD, 0xF000), 0xA);
        assert_eq!(set_field(0xABCD, 0x00F0, 0x3), 0xAB3D);
        assert_eq!(set_field(0, B_AX_OCP_L1_MASK, 7), 7 << 13);
        // A value wider than the field cannot spill out of it.
        assert_eq!(set_field(0, 0xF0, 0x1F), 0xF0);
    }

    #[test]
    fn constants_match_the_linux_headers() {
        assert_eq!(R_AX_SYS_PW_CTRL, 0x04);
        assert_eq!(R_AX_WLAN_XTAL_SI_CTRL, 0x270);
        assert_eq!(R_AX_DMAC_FUNC_EN, 0x8400);
        assert_eq!(R_AX_CMAC_FUNC_EN, 0xC000);
        assert_eq!(B_AX_RDY_SYSPWR, 1 << 17);
        assert_eq!(B_AX_WL_XTAL_SI_CMD_POLL, 1 << 31);
        assert_eq!(B_AX_WL_XTAL_SI_ADDR_MASK, 0xFF);
        assert_eq!(B_AX_WL_XTAL_SI_DATA_MASK, 0xFF00);
        assert_eq!(B_AX_WL_XTAL_SI_BITMASK_MASK, 0xFF_0000);
        assert_eq!(B_AX_WL_XTAL_SI_MODE_MASK, 0x300_0000);
        assert_eq!(XTAL_SI_ANAPAR_WL, 0x90);
        assert_eq!(SW_LPS_OPTION, 0x0001_A0B2);
    }

    #[test]
    fn xtal_commands_pack_like_linux() {
        let c = xtal_si_write_cmd(0x90, 0x40, 0x40);
        assert_eq!(c, 0x90 | 0x40 << 8 | 0x40 << 16 | 1 << 31);
        let r = xtal_si_read_cmd(0xFD);
        assert_eq!(r, 0xFD | 1 << 24 | 1 << 31);
    }

    #[test]
    fn power_on_runs_to_the_end_and_enables_the_mac_blocks() {
        let mut f = Fake::new();
        let mut log = String::new();
        power_on(&mut f, &mut log).expect("power on");
        assert_eq!(f.regs[&R_AX_DMAC_FUNC_EN] & B_AX_DMAC_FUNC_EN, B_AX_DMAC_FUNC_EN);
        assert_eq!(f.regs[&R_AX_CMAC_FUNC_EN] & B_AX_CMAC_EN, B_AX_CMAC_EN);
        // The analog-die writes landed with the right masks: PON bits set, SRAM2RFC cleared.
        assert_eq!(f.xtal[&XTAL_SI_ANAPAR_WL] & XTAL_SI_PON_WEI, XTAL_SI_PON_WEI);
        assert_eq!(f.xtal[&XTAL_SI_ANAPAR_WL] & XTAL_SI_SRAM2RFC, 0);
        // The PLATFORM_ENABLE toggle sequence is set, clear, set, clear, set.
        let seq: Vec<&String> = f.log.iter().filter(|l| l.starts_with("w8 0x88")).collect();
        assert_eq!(seq.len(), 5);
        assert!(seq[0].ends_with(" 0x1") && seq[1].ends_with(" 0x0") && seq[4].ends_with(" 0x1"));
    }

    #[test]
    fn power_on_keeps_linux_order() {
        let ops = power_on_sequence();
        let pos = |want: Op| ops.iter().position(|o| *o == want).unwrap_or_else(|| panic!("missing {want:?}"));
        let ldo = pos(Op::Set32(R_AX_LDO_AON_CTRL0, B_AX_PD_REGU_L));
        let en = pos(Op::Set32(R_AX_SYS_PW_CTRL, B_AX_EN_WLON));
        let onmac = pos(Op::Set32(R_AX_SYS_PW_CTRL, B_AX_APFN_ONMAC));
        let funcs = ops.iter().position(|o| matches!(o, Op::Set32(R_AX_DMAC_FUNC_EN, _))).unwrap();
        assert!(ldo < en && en < onmac && onmac < funcs);
        assert!(matches!(ops.last(), Some(Op::Mask32(R_AX_EECS_EESK_FUNC_SEL, _, 1))));
        assert_eq!(ops.iter().filter(|o| matches!(o, Op::Xtal { .. })).count(), 11);
    }

    #[test]
    fn an_already_on_mac_is_powered_off_first() {
        let mut f = Fake::new();
        f.regs.insert(R_AX_IC_PWR_STATE, 1 << 8);
        let mut log = String::new();
        power_on(&mut f, &mut log).expect("power on after off");
        assert!(log.contains("already on"), "{log}");
        // The off sequence ends by writing SW_LPS_OPTION, which must come before the on sequence's first write.
        let off_at = f.log.iter().position(|l| l == &alloc::format!("w32 {R_AX_WLLPS_CTRL:#x} {SW_LPS_OPTION:#x}")).expect("off ran");
        let on_at = f.log.iter().position(|l| l.starts_with(&alloc::format!("w32 {R_AX_LDO_AON_CTRL0:#x}"))).expect("on ran");
        assert!(off_at < on_at);
    }

    #[test]
    fn a_stuck_step_reports_which_step() {
        let mut f = Fake::new();
        f.fail_poll_on = Some(R_AX_SYS_PW_CTRL);
        let mut log = String::new();
        let e = power_on(&mut f, &mut log).unwrap_err();
        assert!(e.contains("never reached"), "{e}");
        assert!(e.starts_with("step "), "{e}");
    }

    #[test]
    fn xtal_read_returns_the_data_field() {
        let mut f = Fake::new();
        f.xtal.insert(XTAL_SI_CHIP_ID_L, 0x52);
        f.xtal.insert(XTAL_SI_CHIP_ID_H, 0x85);
        assert_eq!(identity(&mut f).xtal_chip_id, Some(0x8552));
    }

    #[test]
    fn power_state_names() {
        assert_eq!(describe_power_state(0), "off");
        assert_eq!(describe_power_state(1 << 8), "on");
        assert_eq!(describe_power_state(2 << 8), "in low power");
        assert_eq!(describe_power_state(3 << 8), "unknown");
    }
}
