//! Time zone: the clock in the top bar shows local time. The hardware clock runs on UTC; the offset comes
//! from the boot option `tz=+05:30` (see `config/bedrock.cfg`) or the `tz` command (saved in /config/tz).

use core::sync::atomic::{AtomicI32, Ordering::Relaxed};

/// Known zones: (short name used by `tz`, offset in minutes, abbreviation, region).
pub const ZONES: &[(&str, i32, &str, &str)] = &[
    ("utc", 0, "UTC", "Coordinated Universal Time"),
    ("london", 0, "GMT", "United Kingdom (London)"),
    ("berlin", 60, "CET", "Central Europe (Berlin, Paris)"),
    ("dubai", 240, "GST", "Gulf (Dubai)"),
    ("india", 330, "IST", "India (Chennai, Kolkata, Mumbai, New Delhi)"),
    ("singapore", 480, "SGT", "Singapore"),
    ("tokyo", 540, "JST", "Japan (Tokyo)"),
    ("sydney", 600, "AEST", "Australia (Sydney)"),
    ("new-york", -300, "EST", "US Eastern (New York)"),
    ("chicago", -360, "CST", "US Central (Chicago)"),
    ("los-angeles", -480, "PST", "US Pacific (Los Angeles)"),
];

static OFFSET: AtomicI32 = AtomicI32::new(0);

/// The offset from UTC in minutes.
pub fn offset() -> i32 {
    OFFSET.load(Relaxed)
}

pub fn set(minutes: i32) {
    OFFSET.store(minutes.clamp(-14 * 60, 14 * 60), Relaxed);
}

/// `+05:30`, `-4`, `+9`, `5:45` or a zone name from [`ZONES`] to minutes.
pub fn parse(s: &str) -> Option<i32> {
    let s = s.trim();
    if let Some(z) = ZONES.iter().find(|z| z.0.eq_ignore_ascii_case(s) || z.2.eq_ignore_ascii_case(s)) {
        return Some(z.1);
    }
    let (sign, rest) = match s.as_bytes().first()? {
        b'-' => (-1, &s[1..]),
        b'+' => (1, &s[1..]),
        _ => (1, s),
    };
    let (h, m) = rest.split_once(':').unwrap_or((rest, "0"));
    let (h, m) = (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?);
    (h <= 14 && (0..60).contains(&m) && h >= 0).then_some(sign * (h * 60 + m))
}

/// `UTC+05:30` style text for an offset.
pub fn utc_text(minutes: i32) -> alloc::string::String {
    let a = minutes.abs();
    alloc::format!("UTC{}{:02}:{:02}", if minutes < 0 { '-' } else { '+' }, a / 60, a % 60)
}

/// (abbreviation, region) for an offset; an unnamed offset is shown as `UTC+hh:mm`.
pub fn names(minutes: i32) -> (alloc::string::String, alloc::string::String) {
    // UTC and London share 0: prefer UTC.
    match ZONES.iter().find(|z| z.1 == minutes) {
        Some(z) => (alloc::string::String::from(z.2), alloc::string::String::from(z.3)),
        None => (utc_text(minutes), alloc::string::String::from("custom offset")),
    }
}

/// Local time now, or `None` when the clock cannot be read.
#[cfg(target_os = "uefi")]
pub fn local_now() -> Option<crate::rtc::DateTime> {
    crate::rtc::now().map(|t| crate::rtc::from_unix(t.unix_seconds() + i64::from(offset()) * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_offsets_and_names() {
        assert_eq!(parse("+05:30"), Some(330));
        assert_eq!(parse("5:30"), Some(330));
        assert_eq!(parse("-4"), Some(-240));
        assert_eq!(parse("India"), Some(330));
        assert_eq!(parse("ist"), Some(330));
        assert_eq!(parse("+15"), None);
        assert_eq!(parse("+5:75"), None);
        assert_eq!(parse("mars"), None);
        assert_eq!(utc_text(330), "UTC+05:30");
        assert_eq!(utc_text(-210), "UTC-03:30");
        assert_eq!(names(330).0, "IST");
        assert_eq!(names(0).0, "UTC");
        assert_eq!(names(345).0, "UTC+05:45");
    }
}
