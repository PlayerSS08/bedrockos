//! The battery-backed clock (the "CMOS RTC", an MC146818-compatible chip every PC has or emulates).
//! Decoding and calendar maths are pure and host-tested; the port access exists only in the kernel build.
//!
//! The chip keeps the time its last operating system set. Windows sets local time, Linux usually UTC,
//! and the chip does not say which, so BedrockOS shows the value as "clock time" and does not guess a zone.

/// The register values as read from the chip, before any decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Raw {
    pub sec: u8,
    pub min: u8,
    pub hour: u8,
    pub day: u8,
    pub month: u8,
    pub year: u8,
    /// Register 0x32 on most machines; 0 when the firmware does not keep one.
    pub century: u8,
    /// Status register B: bit 2 = binary (not BCD), bit 1 = 24-hour mode.
    pub status_b: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
}

fn bcd(v: u8) -> Option<u8> {
    ((v & 0x0F) < 10 && (v >> 4) < 10).then_some((v >> 4) * 10 + (v & 0x0F))
}

pub fn is_leap(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Turns register values into a date, or `None` if they cannot be a real date and time (a chip that is
/// absent reads as 0xFF everywhere; a flat battery gives nonsense).
pub fn decode(r: Raw) -> Option<DateTime> {
    let binary = r.status_b & 0x04 != 0;
    let h24 = r.status_b & 0x02 != 0;
    let conv = |v: u8| if binary { Some(v) } else { bcd(v) };
    let sec = conv(r.sec)?;
    let min = conv(r.min)?;
    let day = conv(r.day)?;
    let month = conv(r.month)?;
    let yy = conv(r.year)?;
    // In 12-hour mode bit 7 of the hour marks PM, and 12 means midnight or noon.
    let pm = !h24 && r.hour & 0x80 != 0;
    let mut hour = conv(r.hour & if h24 { 0xFF } else { 0x7F })?;
    if !h24 {
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = match (hour, pm) {
            (12, false) => 0,
            (12, true) => 12,
            (h, false) => h,
            (h, true) => h + 12,
        };
    }
    // A plausible century register wins; otherwise two-digit years are 2000-2099 (BedrockOS postdates 2000).
    let century = conv(r.century).filter(|c| (19..=22).contains(c)).unwrap_or(20) as u16;
    let year = century * 100 + yy as u16;
    if yy > 99 || sec > 59 || min > 59 || hour > 23 || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some(DateTime { year, month, day, hour, min, sec })
}

impl DateTime {
    /// Days since 1970-01-01 (negative before it). Civil-from-days arithmetic, valid for every year here.
    fn days_since_epoch(&self) -> i64 {
        let y = self.year as i64 - if self.month <= 2 { 1 } else { 0 };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = (self.month as i64 + 9) % 12;
        let doy = (153 * mp + 2) / 5 + self.day as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// Seconds since 1970-01-01 00:00:00, taking the clock value as it stands (no time zone applied).
    pub fn unix_seconds(&self) -> i64 {
        self.days_since_epoch() * 86_400 + self.hour as i64 * 3600 + self.min as i64 * 60 + self.sec as i64
    }

    /// 0 = Monday .. 6 = Sunday.
    pub fn weekday(&self) -> u8 {
        (self.days_since_epoch() + 3).rem_euclid(7) as u8
    }

    pub fn weekday_name(&self) -> &'static str {
        ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][self.weekday() as usize]
    }

    pub fn month_name(&self) -> &'static str {
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(self.month as usize - 1) % 12]
    }

    /// The date and time `secs` seconds after this one (used for "boot time + uptime").
    pub fn plus_seconds(&self, secs: u64) -> DateTime {
        from_unix(self.unix_seconds().saturating_add(secs.min(i64::MAX as u64) as i64))
    }
}

/// The inverse of [`DateTime::unix_seconds`], clamped to the years 1970..=9999.
pub fn from_unix(t: i64) -> DateTime {
    let t = t.clamp(0, 253_402_300_799);
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + if month <= 2 { 1 } else { 0 }) as u16;
    DateTime { year, month, day, hour: (rem / 3600) as u8, min: (rem % 3600 / 60) as u8, sec: (rem % 60) as u8 }
}

#[cfg(target_os = "uefi")]
mod hw {
    use super::{decode, DateTime, Raw};
    use crate::arch;
    use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

    fn reg(index: u8) -> u8 {
        // SAFETY: ports 0x70/0x71 are the CMOS index and data ports on every PC; bit 7 of the index
        // keeps NMI enabled as it was (0 = enabled). Reading a clock register has no side effect.
        unsafe {
            arch::outb(0x70, index & 0x7F);
            arch::inb(0x71)
        }
    }

    fn snapshot() -> Raw {
        Raw { sec: reg(0x00), min: reg(0x02), hour: reg(0x04), day: reg(0x07), month: reg(0x08), year: reg(0x09), century: reg(0x32), status_b: reg(0x0B) }
    }

    /// Reads the clock. The chip updates once a second and the registers are not consistent while it
    /// does, so this waits for "no update in progress" and takes two identical readings in a row.
    pub fn read() -> Option<DateTime> {
        arch::without_interrupts(|| {
            let mut last: Option<Raw> = None;
            for _ in 0..8 {
                let mut spins = 0u32;
                while reg(0x0A) & 0x80 != 0 && spins < 200_000 {
                    spins += 1;
                    arch::pause();
                }
                let now = snapshot();
                if last == Some(now) {
                    return decode(now);
                }
                last = Some(now);
            }
            None
        })
    }

    /// Clock value (as Unix seconds) and uptime at the moment of the first successful read.
    static BASE_UNIX: AtomicI64 = AtomicI64::new(i64::MIN);
    static BASE_UPTIME_US: AtomicU64 = AtomicU64::new(0);

    /// The current time: the clock chip read once, then advanced by the uptime counter, so later calls
    /// cost no port I/O and never go backwards. `None` when the machine has no usable clock.
    pub fn now() -> Option<DateTime> {
        let up = crate::log::uptime_us();
        if BASE_UNIX.load(Ordering::Relaxed) == i64::MIN {
            let t = read()?;
            BASE_UPTIME_US.store(up, Ordering::Relaxed);
            BASE_UNIX.store(t.unix_seconds(), Ordering::Relaxed);
        }
        let base = BASE_UNIX.load(Ordering::Relaxed);
        let elapsed = up.saturating_sub(BASE_UPTIME_US.load(Ordering::Relaxed)) / 1_000_000;
        Some(super::from_unix(base.saturating_add(elapsed as i64)))
    }
}

#[cfg(target_os = "uefi")]
pub use hw::{now, read};

#[cfg(test)]
mod tests {
    use super::*;

    const BCD_24H: u8 = 0x02;
    const BIN_24H: u8 = 0x06;

    fn raw(sec: u8, min: u8, hour: u8, day: u8, month: u8, year: u8, century: u8, status_b: u8) -> Raw {
        Raw { sec, min, hour, day, month, year, century, status_b }
    }

    #[test]
    fn decodes_bcd_24_hour() {
        let t = decode(raw(0x59, 0x07, 0x23, 0x17, 0x11, 0x26, 0x20, BCD_24H)).unwrap();
        assert_eq!(t, DateTime { year: 2026, month: 11, day: 17, hour: 23, min: 7, sec: 59 });
    }

    #[test]
    fn decodes_binary_and_missing_century() {
        let t = decode(raw(5, 6, 7, 1, 2, 30, 0, BIN_24H)).unwrap();
        assert_eq!(t, DateTime { year: 2030, month: 2, day: 1, hour: 7, min: 6, sec: 5 });
        // A nonsense century register (0xFF is not BCD) falls back to 20xx.
        assert_eq!(decode(raw(0, 0, 0, 0x01, 0x01, 0x99, 0xFF, BCD_24H)).unwrap().year, 2099);
        assert_eq!(decode(raw(0, 0, 0, 0x01, 0x01, 0x99, 0x19, BCD_24H)).unwrap().year, 1999);
    }

    #[test]
    fn twelve_hour_mode() {
        let at = |hour: u8| decode(raw(0, 0, hour, 0x01, 0x01, 0x26, 0x20, 0x00)).map(|t| t.hour);
        assert_eq!(at(0x12), Some(0)); // 12 AM
        assert_eq!(at(0x01), Some(1));
        assert_eq!(at(0x11), Some(11));
        assert_eq!(at(0x92), Some(12)); // 12 PM
        assert_eq!(at(0x81), Some(13));
        assert_eq!(at(0x91), Some(23));
        assert_eq!(at(0x00), None);
        assert_eq!(at(0x13), None);
    }

    #[test]
    fn refuses_impossible_values() {
        // An absent chip reads 0xFF everywhere.
        assert_eq!(decode(raw(0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF)), None);
        assert_eq!(decode(raw(0x60, 0, 0, 0x01, 0x01, 0x26, 0x20, BCD_24H)), None);
        assert_eq!(decode(raw(0, 0x60, 0, 0x01, 0x01, 0x26, 0x20, BCD_24H)), None);
        assert_eq!(decode(raw(0, 0, 0x24, 0x01, 0x01, 0x26, 0x20, BCD_24H)), None);
        assert_eq!(decode(raw(0, 0, 0, 0x00, 0x01, 0x26, 0x20, BCD_24H)), None);
        assert_eq!(decode(raw(0, 0, 0, 0x01, 0x13, 0x26, 0x20, BCD_24H)), None);
        assert_eq!(decode(raw(0, 0, 0, 0x30, 0x02, 0x24, 0x20, BCD_24H)), None);
        assert_eq!(decode(raw(0, 0, 0, 0x29, 0x02, 0x26, 0x20, BCD_24H)), None);
        assert!(decode(raw(0, 0, 0, 0x29, 0x02, 0x24, 0x20, BCD_24H)).is_some());
        assert_eq!(decode(raw(0x1A, 0, 0, 0x01, 0x01, 0x26, 0x20, BCD_24H)), None);
        assert_eq!(decode(raw(0, 0, 0, 1, 1, 100, 0, BIN_24H)), None);
    }

    #[test]
    fn unix_time_and_weekdays_match_known_dates() {
        let epoch = DateTime { year: 1970, month: 1, day: 1, hour: 0, min: 0, sec: 0 };
        assert_eq!(epoch.unix_seconds(), 0);
        assert_eq!(epoch.weekday_name(), "Thu");
        let y2k = DateTime { year: 2000, month: 1, day: 1, hour: 0, min: 0, sec: 0 };
        assert_eq!(y2k.unix_seconds(), 946_684_800);
        assert_eq!(y2k.weekday_name(), "Sat");
        let t = DateTime { year: 2026, month: 11, day: 17, hour: 12, min: 30, sec: 15 };
        assert_eq!(t.unix_seconds(), 1_794_918_615);
        assert_eq!(t.weekday_name(), "Tue");
        assert_eq!(t.month_name(), "Nov");
        let leap = DateTime { year: 2024, month: 2, day: 29, hour: 23, min: 59, sec: 59 };
        assert_eq!(leap.unix_seconds(), 1_709_251_199);
        assert_eq!(leap.weekday_name(), "Thu");
    }

    #[test]
    fn from_unix_is_the_inverse() {
        for t in [0i64, 1, 86_399, 86_400, 951_782_400, 946_684_800, 1_709_251_199, 1_794_918_615, 4_102_444_800, 253_402_300_799] {
            assert_eq!(from_unix(t).unix_seconds(), t, "{t}");
        }
        // Every day for 12 years, crossing leap years and a century that is a leap year.
        let mut t = 946_684_800 - 3 * 86_400;
        for _ in 0..(12 * 366) {
            let d = from_unix(t);
            assert_eq!(d.unix_seconds(), t);
            assert!(d.day >= 1 && d.day <= days_in_month(d.year, d.month));
            t += 86_400;
        }
        assert_eq!(from_unix(-5), from_unix(0));
    }

    #[test]
    fn plus_seconds_rolls_over_days_months_and_years() {
        let t = DateTime { year: 2026, month: 12, day: 31, hour: 23, min: 59, sec: 58 };
        assert_eq!(t.plus_seconds(3), DateTime { year: 2027, month: 1, day: 1, hour: 0, min: 0, sec: 1 });
        let f = DateTime { year: 2028, month: 2, day: 28, hour: 12, min: 0, sec: 0 };
        assert_eq!(f.plus_seconds(86_400).day, 29);
        assert_eq!(f.plus_seconds(2 * 86_400), DateTime { year: 2028, month: 3, day: 1, hour: 12, min: 0, sec: 0 });
    }

    #[test]
    fn leap_years() {
        assert!(is_leap(2024) && is_leap(2000) && !is_leap(1900) && !is_leap(2026));
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 13), 0);
    }
}
