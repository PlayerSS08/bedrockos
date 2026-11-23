//! The laptop's battery, as VirtualBox reports it. VirtualBox's ACPI device mirrors the host's battery and
//! mains state through two I/O ports (an index register at 0x4040 and a data register at 0x4044), which its
//! own ACPI tables read for the `_BST`/`_BIF`/`_STA`/`_PSR` methods. BedrockOS has no AML interpreter, so it
//! reads the same registers directly, and only when it runs in VirtualBox (a PCI device from VirtualBox's
//! vendor, 0x80EE, is present). On other machines the battery is reported as unknown.

#[cfg(target_os = "uefi")]
const BAT_INDEX: u16 = 0x4040;
#[cfg(target_os = "uefi")]
const BAT_DATA: u16 = 0x4044;

#[cfg(target_os = "uefi")]
const STATE: u32 = 0x00;
#[cfg(target_os = "uefi")]
const REMAINING: u32 = 0x02;
#[cfg(target_os = "uefi")]
const INFO_UNITS: u32 = 0x04;
#[cfg(target_os = "uefi")]
const LAST_FULL: u32 = 0x06;
#[cfg(target_os = "uefi")]
const DEVICE_STATUS: u32 = 0x0D;
#[cfg(target_os = "uefi")]
const POWER_SOURCE: u32 = 0x0E;

/// `_STA` bit: a battery is present.
const STA_PRESENT: u32 = 0x10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    /// Charge in percent (0 to 100), when a battery is present.
    pub percent: Option<u8>,
    pub charging: bool,
    /// Running on mains power.
    pub on_ac: bool,
}

/// Turns the raw register values into a [`Battery`]. Pure, for tests.
pub fn decode(state: u32, remaining: u32, last_full: u32, status: u32, power: u32) -> Battery {
    let present = status & STA_PRESENT != 0 && last_full != 0 && last_full != u32::MAX;
    let percent = present.then(|| (u64::from(remaining.min(last_full)) * 100 / u64::from(last_full)) as u8);
    Battery { percent, charging: present && state & 2 != 0, on_ac: power == 1 }
}

#[cfg(target_os = "uefi")]
static IN_VBOX: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// True when running in VirtualBox (checked once).
#[cfg(target_os = "uefi")]
fn in_virtualbox() -> bool {
    use core::sync::atomic::Ordering::Relaxed;
    match IN_VBOX.load(Relaxed) {
        0 => {
            let mut found = false;
            crate::pci::for_each(|d| found |= d.header.vendor == 0x80EE);
            IN_VBOX.store(if found { 1 } else { 2 }, Relaxed);
            found
        }
        v => v == 1,
    }
}

#[cfg(target_os = "uefi")]
fn reg(i: u32) -> u32 {
    // SAFETY: only called inside VirtualBox, where 0x4040/0x4044 are its ACPI battery registers; reading
    // them has no side effects.
    unsafe {
        crate::arch::outl(BAT_INDEX, i);
        crate::arch::inl(BAT_DATA)
    }
}

/// The battery now, or `None` when it cannot be known (not running in VirtualBox).
#[cfg(target_os = "uefi")]
pub fn read() -> Option<Battery> {
    if !in_virtualbox() {
        return None;
    }
    Some(crate::arch::without_interrupts(|| {
        // Selecting register 0 makes VirtualBox fetch the battery status, register 4 the battery information
        // (capacities); without the second, the full-charge capacity stays 0.
        let state = reg(STATE);
        let remaining = reg(REMAINING);
        let _ = reg(INFO_UNITS);
        decode(state, remaining, reg(LAST_FULL), reg(DEVICE_STATUS), reg(POWER_SOURCE))
    }))
}

/// The raw battery registers 0 to 15 (for `battery -v`), when running in VirtualBox.
#[cfg(target_os = "uefi")]
pub fn raw() -> Option<[u32; 16]> {
    if !in_virtualbox() {
        return None;
    }
    let mut v = [0u32; 16];
    crate::arch::without_interrupts(|| {
        for (i, x) in v.iter_mut().enumerate() {
            *x = reg(i as u32);
        }
    });
    Some(v)
}

#[cfg(not(target_os = "uefi"))]
pub fn raw() -> Option<[u32; 16]> {
    None
}

/// Host builds have no hardware.
#[cfg(not(target_os = "uefi"))]
pub fn read() -> Option<Battery> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_registers() {
        assert_eq!(decode(2, 5000, 10000, 0x1F, 1), Battery { percent: Some(50), charging: true, on_ac: true });
        assert_eq!(decode(1, 10000, 10000, 0x1F, 0), Battery { percent: Some(100), charging: false, on_ac: false });
        assert_eq!(decode(0, 0, 0, 0x0F, 1), Battery { percent: None, charging: false, on_ac: true }, "no battery");
        assert_eq!(decode(0, 120, 100, 0x1F, 1).percent, Some(100), "capped at 100");
    }
}
