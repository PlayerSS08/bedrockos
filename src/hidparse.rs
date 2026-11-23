//! HID report descriptor parser. Pure and host-tested.
//!
//! Turns the byte stream a HID device publishes (what each bit of its input reports means) into a list of
//! `Field`s, then finds the two layouts BedrockOS can use for pointing: a relative mouse report, and a Windows
//! Precision Touchpad report (a few finger blocks with tip switch, contact id and absolute X and Y).
//! Everything here treats the descriptor as hostile input: counts and sizes are capped and nothing indexes
//! past a slice.

use alloc::vec::Vec;

pub const PAGE_GENERIC_DESKTOP: u16 = 0x01;
pub const PAGE_BUTTON: u16 = 0x09;
pub const PAGE_DIGITIZER: u16 = 0x0D;
pub const USAGE_X: u16 = 0x30;
pub const USAGE_Y: u16 = 0x31;
pub const USAGE_WHEEL: u16 = 0x38;
pub const USAGE_FINGER: u16 = 0x22;
pub const USAGE_TIP: u16 = 0x42;
pub const USAGE_CONFIDENCE: u16 = 0x47;
pub const USAGE_CONTACT_ID: u16 = 0x51;
pub const USAGE_CONTACT_COUNT: u16 = 0x54;

const MAX_FIELDS: usize = 512;
const MAX_REPORT_COUNT: u32 = 256;

/// One value in an input report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub report_id: u8,
    /// Position of the first bit in the report data (after the report id byte, if the device uses ids).
    pub bit_offset: u32,
    pub bit_size: u8,
    pub usage_page: u16,
    pub usage: u16,
    pub logical_min: i32,
    pub logical_max: i32,
    pub relative: bool,
    /// Which finger block (`Finger` collection, counted from 0) the field is in.
    pub group: Option<u8>,
}

impl Field {
    /// The value in `data` (a report without its id byte), sign-extended when the logical minimum is negative.
    pub fn value(&self, data: &[u8]) -> Option<i32> {
        let raw = get_bits(data, self.bit_offset, self.bit_size)?;
        Some(if self.logical_min < 0 && self.bit_size < 32 && raw & (1 << (self.bit_size - 1)) != 0 {
            (raw | (u32::MAX << self.bit_size)) as i32
        } else {
            raw as i32
        })
    }

    pub fn range(&self) -> i32 {
        self.logical_max.saturating_sub(self.logical_min).max(1)
    }
}

/// `size` bits (at most 32) starting at bit `offset`, least significant bit first. `None` past the end.
pub fn get_bits(data: &[u8], offset: u32, size: u8) -> Option<u32> {
    if size == 0 || size > 32 {
        return None;
    }
    let end = offset.checked_add(u32::from(size))?;
    if end.div_ceil(8) as usize > data.len() {
        return None;
    }
    let mut v = 0u32;
    for i in 0..u32::from(size) {
        let bit = offset + i;
        if data[(bit / 8) as usize] >> (bit % 8) & 1 != 0 {
            v |= 1 << i;
        }
    }
    Some(v)
}

#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub fields: Vec<Field>,
    /// Any `Report ID` item seen: every report then starts with the id byte.
    pub uses_report_ids: bool,
}

#[derive(Clone, Copy)]
struct Globals {
    page: u16,
    lmin: i32,
    lmax: i32,
    size: u32,
    count: u32,
    id: u8,
}

fn sign_extend(v: u32, bytes: usize) -> i32 {
    match bytes {
        1 => i32::from(v as u8 as i8),
        2 => i32::from(v as u16 as i16),
        _ => v as i32,
    }
}

/// Parses a report descriptor. `None` when it is empty or malformed in a way that makes the fields meaningless.
pub fn parse(desc: &[u8]) -> Option<Parsed> {
    let mut out = Parsed::default();
    let mut g = Globals { page: 0, lmin: 0, lmax: 0, size: 0, count: 0, id: 0 };
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new();
    let (mut umin, mut umax): (Option<u32>, Option<u32>) = (None, None);
    // (usage page, usage, finger block) per open collection.
    let mut colls: Vec<(u16, u16, Option<u8>)> = Vec::new();
    let mut fingers = 0u8;
    let mut offsets: Vec<(u8, u32)> = Vec::new();

    let mut i = 0usize;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Long item: length byte, tag byte, data. Nothing in HID defines one, so skip it.
            let n = usize::from(*desc.get(i + 1)?);
            i = i.checked_add(3 + n)?;
            continue;
        }
        let bytes = match prefix & 3 {
            3 => 4,
            n => usize::from(n),
        };
        let data = desc.get(i + 1..i + 1 + bytes)?;
        let mut u = 0u32;
        for (k, b) in data.iter().enumerate() {
            u |= u32::from(*b) << (8 * k);
        }
        i += 1 + bytes;
        let (kind, tag) = ((prefix >> 2) & 3, prefix >> 4);
        match (kind, tag) {
            // Global items.
            (1, 0x0) => g.page = u as u16,
            (1, 0x1) => g.lmin = sign_extend(u, bytes),
            (1, 0x2) => {
                // A maximum that does not fit as signed (0xFF with a minimum of 0) is a large positive number.
                g.lmax = if g.lmin < 0 { sign_extend(u, bytes) } else { u as i32 };
            }
            (1, 0x7) => g.size = u,
            (1, 0x8) => {
                g.id = u as u8;
                out.uses_report_ids = true;
            }
            (1, 0x9) => g.count = u,
            (1, 0xA) => {
                if stack.len() < 16 {
                    stack.push(g);
                }
            }
            (1, 0xB) => {
                if let Some(p) = stack.pop() {
                    g = p;
                }
            }
            // Local items.
            (2, 0x0) => {
                if usages.len() < 1024 {
                    usages.push(if bytes == 4 { u } else { u & 0xFFFF });
                }
            }
            (2, 0x1) => umin = Some(u),
            (2, 0x2) => umax = Some(u),
            // Main items.
            (0, 0xA) => {
                let first = usages.first().copied().or(umin);
                let (page, usage) = first.map_or((g.page, 0), |f| if f >> 16 != 0 { ((f >> 16) as u16, f as u16) } else { (g.page, f as u16) });
                let parent = colls.iter().rev().find_map(|c| c.2);
                let group = if (page, usage) == (PAGE_DIGITIZER, USAGE_FINGER) {
                    let n = fingers;
                    fingers = fingers.saturating_add(1);
                    Some(n)
                } else {
                    parent
                };
                if colls.len() < 32 {
                    colls.push((page, usage, group));
                }
                usages.clear();
                umin = None;
                umax = None;
            }
            (0, 0xC) => {
                colls.pop();
                usages.clear();
                umin = None;
                umax = None;
            }
            (0, 0x8) => {
                if g.count > MAX_REPORT_COUNT || g.size > 64 {
                    return None;
                }
                let pos = match offsets.iter_mut().find(|o| o.0 == g.id) {
                    Some(o) => o,
                    None => {
                        offsets.push((g.id, 0));
                        offsets.last_mut()?
                    }
                };
                let start = pos.1;
                pos.1 = pos.1.checked_add(g.size * g.count)?;
                // Constant (padding) and array items occupy bits but carry no named values.
                let variable = u & 0x02 != 0 && u & 0x01 == 0;
                if variable && (1..=32).contains(&g.size) {
                    let group = colls.iter().rev().find_map(|c| c.2);
                    for k in 0..g.count {
                        let raw = if let (Some(lo), Some(hi)) = (umin, umax) {
                            let v = lo.saturating_add(k);
                            v.min(hi)
                        } else {
                            usages.get(k as usize).or(usages.last()).copied().unwrap_or(0)
                        };
                        let (page, usage) = if raw >> 16 != 0 { ((raw >> 16) as u16, raw as u16) } else { (g.page, raw as u16) };
                        if out.fields.len() >= MAX_FIELDS {
                            return None;
                        }
                        out.fields.push(Field {
                            report_id: g.id,
                            bit_offset: start + k * g.size,
                            bit_size: g.size as u8,
                            usage_page: page,
                            usage,
                            logical_min: g.lmin,
                            logical_max: g.lmax,
                            relative: u & 0x04 != 0,
                            group,
                        });
                    }
                }
                usages.clear();
                umin = None;
                umax = None;
            }
            // Output, Feature: not needed, but they end the local state like any main item.
            (0, 0x9) | (0, 0xB) => {
                usages.clear();
                umin = None;
                umax = None;
            }
            _ => {}
        }
    }
    (!out.fields.is_empty()).then_some(out)
}

/// A relative pointer report: buttons, X, Y and an optional wheel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MouseLayout {
    pub report_id: u8,
    pub buttons: Vec<Field>,
    pub x: Field,
    pub y: Field,
    pub wheel: Option<Field>,
}

/// One finger block of a touchpad report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contact {
    pub tip: Field,
    pub id: Option<Field>,
    pub x: Field,
    pub y: Field,
    pub confidence: Option<Field>,
}

/// A Windows Precision Touchpad report: up to a handful of contacts and the click button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TouchLayout {
    pub report_id: u8,
    pub contacts: Vec<Contact>,
    pub button: Option<Field>,
    pub count: Option<Field>,
}

pub fn find_mouse(p: &Parsed) -> Option<MouseLayout> {
    let x = p.fields.iter().find(|f| f.usage_page == PAGE_GENERIC_DESKTOP && f.usage == USAGE_X && f.relative && f.group.is_none())?;
    let same = |f: &&Field| f.report_id == x.report_id && f.group.is_none();
    let y = p.fields.iter().filter(same).find(|f| f.usage_page == PAGE_GENERIC_DESKTOP && f.usage == USAGE_Y && f.relative)?;
    let wheel = p.fields.iter().filter(same).find(|f| f.usage_page == PAGE_GENERIC_DESKTOP && f.usage == USAGE_WHEEL && f.relative).copied();
    let buttons: Vec<Field> = p.fields.iter().filter(same).filter(|f| f.usage_page == PAGE_BUTTON && (1..=3).contains(&f.usage)).copied().collect();
    Some(MouseLayout { report_id: x.report_id, buttons, x: *x, y: *y, wheel })
}

pub fn find_touch(p: &Parsed) -> Option<TouchLayout> {
    let first = p.fields.iter().find(|f| f.usage_page == PAGE_DIGITIZER && f.usage == USAGE_TIP && f.group.is_some())?;
    let rid = first.report_id;
    let in_report = |f: &&Field| f.report_id == rid;
    let mut contacts = Vec::new();
    for tip in p.fields.iter().filter(in_report).filter(|f| f.usage_page == PAGE_DIGITIZER && f.usage == USAGE_TIP && f.group.is_some()) {
        let g = tip.group;
        let mine = |f: &&Field| f.report_id == rid && f.group == g;
        let find = |page: u16, usage: u16| p.fields.iter().filter(mine).find(|f| f.usage_page == page && f.usage == usage).copied();
        let (Some(x), Some(y)) = (find(PAGE_GENERIC_DESKTOP, USAGE_X), find(PAGE_GENERIC_DESKTOP, USAGE_Y)) else { continue };
        contacts.push(Contact { tip: *tip, id: find(PAGE_DIGITIZER, USAGE_CONTACT_ID), x, y, confidence: find(PAGE_DIGITIZER, USAGE_CONFIDENCE) });
    }
    if contacts.is_empty() {
        return None;
    }
    let button = p.fields.iter().filter(in_report).find(|f| f.usage_page == PAGE_BUTTON && f.usage == 1 && f.group.is_none()).copied();
    let count = p.fields.iter().filter(in_report).find(|f| f.usage_page == PAGE_DIGITIZER && f.usage == USAGE_CONTACT_COUNT).copied();
    Some(TouchLayout { report_id: rid, contacts, button, count })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Report id 1: three buttons, X, Y and wheel (relative, 8 bit).
    pub const MOUSE: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81,
        0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03, 0x81, 0x06, 0xC0, 0xC0,
    ];

    /// Report id 4: two finger blocks (tip, confidence, pad, contact id, X 16 bit, Y 16 bit), the click button and the contact count.
    pub fn touch_desc() -> Vec<u8> {
        let mut d = alloc::vec![0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x04];
        for _ in 0..2 {
            d.extend_from_slice(&[0x09, 0x22, 0xA1, 0x02]);
            d.extend_from_slice(&[0x15, 0x00, 0x25, 0x01, 0x09, 0x42, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02]); // tip
            d.extend_from_slice(&[0x09, 0x47, 0x81, 0x02]); // confidence
            d.extend_from_slice(&[0x75, 0x06, 0x81, 0x03]); // padding
            d.extend_from_slice(&[0x75, 0x08, 0x09, 0x51, 0x81, 0x02]); // contact id
            d.extend_from_slice(&[0x05, 0x01, 0x26, 0xFF, 0x0F, 0x75, 0x10, 0x09, 0x30, 0x81, 0x02]); // X 0..4095
            d.extend_from_slice(&[0x26, 0xFF, 0x07, 0x09, 0x31, 0x81, 0x02]); // Y 0..2047
            d.extend_from_slice(&[0x05, 0x0D, 0xC0]);
        }
        d.extend_from_slice(&[0x05, 0x09, 0x09, 0x01, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x75, 0x07, 0x81, 0x03]); // button + pad
        d.extend_from_slice(&[0x05, 0x0D, 0x09, 0x54, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02]); // contact count
        d.push(0xC0);
        d
    }

    #[test]
    fn bits_are_read_least_significant_first() {
        assert_eq!(get_bits(&[0b1010_0101], 0, 4), Some(0b0101));
        assert_eq!(get_bits(&[0b1010_0101], 4, 4), Some(0b1010));
        assert_eq!(get_bits(&[0xE8, 0x03], 0, 16), Some(1000));
        assert_eq!(get_bits(&[0xFF, 0x01], 7, 3), Some(0b011));
        assert_eq!(get_bits(&[0xFF], 4, 8), None);
        assert_eq!(get_bits(&[0xFF], 0, 0), None);
        assert_eq!(get_bits(&[0xFF; 8], u32::MAX, 8), None);
    }

    #[test]
    fn mouse_layout_is_found() {
        let p = parse(MOUSE).unwrap();
        assert!(p.uses_report_ids);
        let m = find_mouse(&p).unwrap();
        assert_eq!((m.report_id, m.buttons.len()), (1, 3));
        assert_eq!((m.x.bit_offset, m.y.bit_offset, m.wheel.map(|w| w.bit_offset)), (8, 16, Some(24)));
        assert_eq!((m.x.logical_min, m.x.logical_max), (-127, 127));
        // 0xFB is -5 as a signed byte.
        assert_eq!(m.x.value(&[0, 0xFB, 0, 0]), Some(-5));
        assert!(find_touch(&p).is_none());
    }

    #[test]
    fn touchpad_layout_is_found() {
        let p = parse(&touch_desc()).unwrap();
        let t = find_touch(&p).unwrap();
        assert_eq!(t.report_id, 4);
        assert_eq!(t.contacts.len(), 2);
        let c0 = t.contacts[0];
        assert_eq!((c0.tip.bit_offset, c0.confidence.unwrap().bit_offset, c0.id.unwrap().bit_offset, c0.x.bit_offset, c0.y.bit_offset), (0, 1, 8, 16, 32));
        let c1 = t.contacts[1];
        assert_eq!((c1.tip.bit_offset, c1.id.unwrap().bit_offset, c1.x.bit_offset, c1.y.bit_offset), (48, 56, 64, 80));
        assert_eq!((c0.x.logical_max, c0.y.logical_max), (4095, 2047));
        assert_eq!((t.button.unwrap().bit_offset, t.count.unwrap().bit_offset), (96, 104));
        assert!(find_mouse(&p).is_none());
    }

    #[test]
    fn a_device_with_both_gives_both() {
        let mut d = MOUSE.to_vec();
        d.extend_from_slice(&touch_desc());
        let p = parse(&d).unwrap();
        assert!(find_mouse(&p).is_some() && find_touch(&p).is_some());
    }

    #[test]
    fn hostile_descriptors_never_panic() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0x05]).is_none()); // item cut short
        assert!(parse(&[0xFE, 0xFF, 0x00]).is_none()); // long item past the end
        // Huge report count and size are refused.
        assert!(parse(&[0x75, 0x20, 0x96, 0xFF, 0xFF, 0x81, 0x02]).is_none());
        // Every prefix and every single-byte corruption of a real descriptor is survivable.
        let d = touch_desc();
        for n in 0..d.len() {
            let _ = parse(&d[..n]);
            let mut c = d.clone();
            c[n] ^= 0xFF;
            let _ = parse(&c).map(|p| (find_mouse(&p), find_touch(&p)));
        }
    }

    #[test]
    fn usage_ranges_and_repeated_last_usage() {
        // Report size 8, count 3, usages X then Y (the last one repeats): three one-byte relative fields.
        let d = [0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03, 0x81, 0x06];
        let p = parse(&d).unwrap();
        let u: Vec<u16> = p.fields.iter().map(|f| f.usage).collect();
        assert_eq!(u, [0x30, 0x31, 0x31]);
        assert!(!p.uses_report_ids);
    }
}
