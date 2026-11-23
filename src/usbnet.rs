//! USB network devices, pure and host-testable: finding an RNDIS or CDC-ECM function in a configuration
//! descriptor, and the RNDIS control/data message formats. RNDIS is what Android's "USB tethering" speaks;
//! CDC-ECM is what Linux gadgets, many Ethernet dongles and Android's "Ethernet tethering" speak.
//!
//! Not covered: CDC-NCM, vendor-specific USB Ethernet chips (ASIX, Realtek USB), RNDIS keep-alives from the
//! device and interrupt notifications (the driver polls instead).

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Rndis,
    Ecm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbNet {
    pub kind: Kind,
    /// Value for Set Configuration.
    pub config: u8,
    pub ctrl_interface: u8,
    pub data_interface: u8,
    /// Alternate setting of the data interface that has the bulk endpoints (0 for RNDIS, usually 1 for ECM).
    pub data_alt: u8,
    pub ep_in: u8,
    pub max_packet_in: u16,
    pub ep_out: u8,
    pub max_packet_out: u16,
    /// ECM only: index of the string descriptor that holds the MAC address as 12 hex digits.
    pub mac_string: Option<u8>,
}

const DESC_CONFIG: u8 = 2;
const DESC_INTERFACE: u8 = 4;
const DESC_ENDPOINT: u8 = 5;
const DESC_CS_INTERFACE: u8 = 0x24;
const CS_ETHERNET: u8 = 0x0F;
const CLASS_CDC_DATA: u8 = 0x0A;

fn rndis_control(class: u8, sub: u8, proto: u8) -> bool {
    matches!((class, sub, proto), (0xE0, 1, 3) | (0xEF, 4, 1) | (0x02, 0x02, 0xFF))
}

fn ecm_control(class: u8, sub: u8, proto: u8) -> bool {
    (class, sub, proto) == (0x02, 0x06, 0x00)
}

/// A data interface (number, alternate setting) with its bulk IN and OUT endpoints (number, max packet).
type Candidate = (u8, u8, (u8, u16), (u8, u16));

/// Walks the whole configuration descriptor and returns the first RNDIS or ECM function found. The malformed
/// cases (zero lengths, entries past the end) give `None`.
pub fn find_usb_net(d: &[u8]) -> Option<UsbNet> {
    let mut config = 0u8;
    let mut ctrl: Option<(Kind, u8, Option<u8>)> = None; // kind, interface number, MAC string
    // The interface/alternate setting being read and the bulk endpoints seen in it.
    let mut cur: Option<(u8, u8, u8)> = None; // number, alt, class
    let mut bulk_in: Option<(u8, u16)> = None;
    let mut bulk_out: Option<(u8, u16)> = None;
    let mut best: Option<Candidate> = None;

    let mut i = 0usize;
    while i < d.len() {
        let len = usize::from(d[i]);
        let kind = *d.get(i + 1)?;
        if len < 2 {
            return None;
        }
        let e = d.get(i..i + len)?;
        match kind {
            DESC_CONFIG if len >= 9 => config = e[5],
            DESC_INTERFACE if len >= 9 => {
                // Close the previous interface: a data interface with both bulk endpoints is a candidate.
                if let (Some((n, alt, CLASS_CDC_DATA)), Some(bi), Some(bo), None) = (cur, bulk_in, bulk_out, best) {
                    best = Some((n, alt, bi, bo));
                }
                bulk_in = None;
                bulk_out = None;
                let (num, alt, class, sub, proto) = (e[2], e[3], e[5], e[6], e[7]);
                cur = Some((num, alt, class));
                if alt == 0 && ctrl.is_none() {
                    if rndis_control(class, sub, proto) {
                        ctrl = Some((Kind::Rndis, num, None));
                    } else if ecm_control(class, sub, proto) {
                        ctrl = Some((Kind::Ecm, num, None));
                    }
                }
            }
            DESC_CS_INTERFACE if len >= 4 && e[2] == CS_ETHERNET && len >= 5 => {
                if let Some((Kind::Ecm, num, _)) = ctrl {
                    if cur.map(|c| c.0) == Some(num) {
                        ctrl = Some((Kind::Ecm, num, Some(e[3])));
                    }
                }
            }
            // Endpoint 0 is the control pipe and a zero packet size cannot carry anything: a descriptor that
            // claims either as a bulk endpoint is lying (or hostile), so such an entry is ignored.
            DESC_ENDPOINT if len >= 7 && e[3] & 0b11 == 0b10 && e[2] & 0x0F != 0 && u16::from_le_bytes([e[4], e[5]]) & 0x07FF != 0 => {
                let mp = u16::from_le_bytes([e[4], e[5]]) & 0x07FF;
                if e[2] & 0x80 != 0 {
                    bulk_in = Some((e[2] & 0x0F, mp));
                } else {
                    bulk_out = Some((e[2] & 0x0F, mp));
                }
            }
            _ => {}
        }
        i += len;
    }
    if let (Some((n, alt, CLASS_CDC_DATA)), Some(bi), Some(bo), None) = (cur, bulk_in, bulk_out, best) {
        best = Some((n, alt, bi, bo));
    }
    let (kind, ctrl_interface, mac_string) = ctrl?;
    let (data_interface, data_alt, (ep_in, max_packet_in), (ep_out, max_packet_out)) = best?;
    // ECM keeps its MAC address in a string; RNDIS asks the device with a query message.
    Some(UsbNet { kind, config, ctrl_interface, data_interface, data_alt, ep_in, max_packet_in, ep_out, max_packet_out, mac_string: if kind == Kind::Ecm { mac_string } else { None } })
}

/// The MAC address in an ECM string descriptor: UTF-16LE text of 12 hexadecimal digits.
pub fn parse_mac_string(desc: &[u8]) -> Option<[u8; 6]> {
    if desc.len() < 2 + 24 || desc[1] != 3 {
        return None;
    }
    let mut digits = [0u8; 12];
    for (i, dg) in digits.iter_mut().enumerate() {
        let (lo, hi) = (desc[2 + i * 2], desc[3 + i * 2]);
        if hi != 0 {
            return None;
        }
        *dg = (lo as char).to_digit(16)? as u8;
    }
    let mut mac = [0u8; 6];
    for (i, m) in mac.iter_mut().enumerate() {
        *m = digits[i * 2] << 4 | digits[i * 2 + 1];
    }
    (mac != [0; 6]).then_some(mac)
}

// --- RNDIS ---------------------------------------------------------------------------------------------------

pub const MSG_PACKET: u32 = 0x0000_0001;
pub const MSG_INITIALIZE: u32 = 0x0000_0002;
pub const MSG_QUERY: u32 = 0x0000_0004;
pub const MSG_SET: u32 = 0x0000_0005;
pub const MSG_KEEPALIVE: u32 = 0x0000_0008;
pub const COMPLETE: u32 = 0x8000_0000;
pub const OID_PERMANENT_ADDRESS: u32 = 0x0101_0101;
pub const OID_PACKET_FILTER: u32 = 0x0001_010E;
/// Directed + multicast + broadcast, what a host NIC needs.
pub const FILTER_NORMAL: u32 = 0x0000_000B;
pub const MAX_TRANSFER: u32 = 0x4000;
pub const PACKET_HEADER: usize = 44;

fn put(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn get(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

pub fn initialize(req: u32) -> Vec<u8> {
    let mut m = Vec::with_capacity(24);
    for x in [MSG_INITIALIZE, 24, req, 1, 0, MAX_TRANSFER] {
        put(&mut m, x);
    }
    m
}

pub fn query(req: u32, oid: u32) -> Vec<u8> {
    let mut m = Vec::with_capacity(32);
    // length, request id, OID, information length 0, information offset 20 (from the request id), reserved.
    // Four bytes of padding follow: QEMU's RNDIS device refuses a query whose information offset reaches the
    // very end of the message, and a padded message is valid for real devices too.
    for x in [MSG_QUERY, 32, req, oid, 0, 20, 0, 0] {
        put(&mut m, x);
    }
    m
}

pub fn set(req: u32, oid: u32, value: u32) -> Vec<u8> {
    let mut m = Vec::with_capacity(32);
    for x in [MSG_SET, 32, req, oid, 4, 20, 0, value] {
        put(&mut m, x);
    }
    m
}

pub fn keepalive_reply(req: u32) -> Vec<u8> {
    let mut m = Vec::with_capacity(16);
    for x in [MSG_KEEPALIVE | COMPLETE, 16, req, 0] {
        put(&mut m, x);
    }
    m
}

#[derive(Debug, PartialEq, Eq)]
pub struct Completion<'a> {
    pub kind: u32,
    pub req: u32,
    pub status: u32,
    /// Information buffer of a query completion (empty otherwise).
    pub info: &'a [u8],
}

/// Reads a completion (or keep-alive request) message.
pub fn parse_completion(b: &[u8]) -> Option<Completion<'_>> {
    let (kind, len, req) = (get(b, 0)?, get(b, 4)? as usize, get(b, 8)?);
    if len < 12 || len > b.len() {
        return None;
    }
    if kind == MSG_KEEPALIVE {
        return Some(Completion { kind, req, status: 0, info: &[] });
    }
    let status = get(b, 12)?;
    let mut info: &[u8] = &[];
    if kind == MSG_QUERY | COMPLETE {
        let (ilen, ioff) = (get(b, 16)? as usize, get(b, 20)? as usize);
        // The offset counts from the request id field, which is 8 bytes into the message.
        let start = 8usize.checked_add(ioff)?;
        info = b.get(start..start.checked_add(ilen)?).filter(|_| start + ilen <= len)?;
    }
    Some(Completion { kind, req, status, info })
}

/// Wraps an Ethernet frame for the bulk OUT endpoint. If the transfer would be an exact multiple of the packet
/// size, one padding byte is added so the device sees the end of the transfer (the Linux driver does the same).
pub fn wrap_packet(frame: &[u8], max_packet: usize) -> Vec<u8> {
    let mut m = Vec::with_capacity(PACKET_HEADER + frame.len() + 1);
    // type, length, data offset (from byte 8), data length, then seven zero fields (OOB and per-packet info)
    for x in [MSG_PACKET, (PACKET_HEADER + frame.len()) as u32, 36, frame.len() as u32, 0, 0, 0, 0, 0, 0, 0] {
        put(&mut m, x);
    }
    m.extend_from_slice(frame);
    if max_packet > 0 && m.len() % max_packet == 0 {
        m.push(0);
    }
    m
}

/// Splits a bulk IN transfer into the Ethernet frames it carries (a transfer may hold several). Stops at the
/// first malformed message; frames already read are kept.
pub fn unwrap_packets(mut b: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while b.len() >= PACKET_HEADER {
        let (kind, len, off, dlen) = match (get(b, 0), get(b, 4), get(b, 8), get(b, 12)) {
            (Some(k), Some(l), Some(o), Some(d)) => (k, l as usize, o as usize, d as usize),
            _ => break,
        };
        if kind != MSG_PACKET || len < PACKET_HEADER || len > b.len() {
            break;
        }
        let start = 8 + off;
        match start.checked_add(dlen) {
            Some(end) if start >= PACKET_HEADER - 36 && end <= len && dlen >= 14 => out.push(&b[start..end]),
            _ => break,
        }
        b = &b[len..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Android/QEMU style RNDIS function: control interface 0 (E0/01/03, interrupt IN), data interface 1 (class 0A, bulk).
    fn rndis_desc() -> Vec<u8> {
        let mut d = alloc::vec![0x09, 0x02, 0, 0, 0x02, 0x02, 0x00, 0x80, 0xFA];
        d.extend_from_slice(&[0x09, 0x04, 0, 0, 1, 0xE0, 0x01, 0x03, 0]);
        d.extend_from_slice(&[0x05, 0x24, 0x00, 0x10, 0x01, 0x05, 0x24, 0x01, 0x00, 0x01]); // CDC header, call management
        d.extend_from_slice(&[0x07, 0x05, 0x81, 0x03, 0x08, 0x00, 0x0A]); // interrupt IN
        d.extend_from_slice(&[0x09, 0x04, 1, 0, 2, 0x0A, 0x00, 0x00, 0]);
        d.extend_from_slice(&[0x07, 0x05, 0x82, 0x02, 0x00, 0x02, 0x00]); // bulk IN 2, 512
        d.extend_from_slice(&[0x07, 0x05, 0x03, 0x02, 0x00, 0x02, 0x00]); // bulk OUT 3, 512
        let n = d.len() as u16;
        d[2..4].copy_from_slice(&n.to_le_bytes());
        d
    }

    /// CDC-ECM function with the Ethernet Networking functional descriptor (MAC in string 5) and data alt 1.
    fn ecm_desc() -> Vec<u8> {
        let mut d = alloc::vec![0x09, 0x02, 0, 0, 0x02, 0x01, 0x00, 0x80, 0xFA];
        d.extend_from_slice(&[0x09, 0x04, 0, 0, 1, 0x02, 0x06, 0x00, 0]);
        d.extend_from_slice(&[0x05, 0x24, 0x00, 0x10, 0x01]);
        d.extend_from_slice(&[0x0D, 0x24, 0x0F, 0x05, 0, 0, 0, 0, 0xEA, 0x05, 0, 0, 0]); // Ethernet Networking, iMACAddress = 5
        d.extend_from_slice(&[0x07, 0x05, 0x83, 0x03, 0x10, 0x00, 0x10]);
        d.extend_from_slice(&[0x09, 0x04, 1, 0, 0, 0x0A, 0x00, 0x00, 0]); // data, alt 0, no endpoints
        d.extend_from_slice(&[0x09, 0x04, 1, 1, 2, 0x0A, 0x00, 0x00, 0]); // data, alt 1
        d.extend_from_slice(&[0x07, 0x05, 0x81, 0x02, 0x00, 0x02, 0x00]);
        d.extend_from_slice(&[0x07, 0x05, 0x02, 0x02, 0x00, 0x02, 0x00]);
        let n = d.len() as u16;
        d[2..4].copy_from_slice(&n.to_le_bytes());
        d
    }

    #[test]
    fn rndis_function_is_found() {
        let n = find_usb_net(&rndis_desc()).unwrap();
        assert_eq!((n.kind, n.config, n.ctrl_interface, n.data_interface, n.data_alt), (Kind::Rndis, 2, 0, 1, 0));
        assert_eq!((n.ep_in, n.ep_out, n.max_packet_in, n.max_packet_out, n.mac_string), (2, 3, 512, 512, None));
    }

    #[test]
    fn ecm_function_is_found_with_its_alternate_setting_and_mac_string() {
        let n = find_usb_net(&ecm_desc()).unwrap();
        assert_eq!((n.kind, n.config, n.ctrl_interface, n.data_interface, n.data_alt), (Kind::Ecm, 1, 0, 1, 1));
        assert_eq!((n.ep_in, n.ep_out, n.mac_string), (1, 2, Some(5)));
    }

    #[test]
    fn other_devices_are_not_network_functions() {
        // A keyboard, and a network control interface without any data interface.
        let kb = [0x09, 0x02, 0x22, 0, 1, 1, 0, 0xA0, 0x32, 0x09, 0x04, 0, 0, 1, 3, 1, 1, 0, 0x09, 0x21, 0x11, 1, 0, 1, 0x22, 0x3F, 0, 0x07, 0x05, 0x81, 0x03, 8, 0, 10];
        assert!(find_usb_net(&kb).is_none());
        let mut d = rndis_desc();
        d.truncate(9 + 9 + 10 + 7);
        assert!(find_usb_net(&d).is_none());
        for n in 0..rndis_desc().len() {
            let _ = find_usb_net(&rndis_desc()[..n]);
            let _ = find_usb_net(&ecm_desc()[..n]);
        }
    }

    #[test]
    fn bulk_endpoints_that_are_endpoint_zero_or_zero_sized_are_ignored() {
        // The bulk IN endpoint of the data interface claims number 0 (the control pipe's index in a context).
        let mut d = rndis_desc();
        let at = d.len() - 14; // the bulk IN descriptor
        d[at + 2] = 0x80;
        assert!(find_usb_net(&d).is_none());
        // The bulk OUT endpoint has a packet size of zero.
        let mut d = rndis_desc();
        let n = d.len();
        d[n - 3] = 0;
        d[n - 2] = 0;
        assert!(find_usb_net(&d).is_none());
    }

    #[test]
    fn mac_string_parsing() {
        let mut s = alloc::vec![26u8, 3];
        for c in "0a1B2c3D4e5F".chars() {
            s.extend_from_slice(&[c as u8, 0]);
        }
        assert_eq!(parse_mac_string(&s), Some([0x0A, 0x1B, 0x2C, 0x3D, 0x4E, 0x5F]));
        assert_eq!(parse_mac_string(&s[..20]), None);
        s[4] = b'z';
        assert_eq!(parse_mac_string(&s), None);
        let zero = {
            let mut z = alloc::vec![26u8, 3];
            for _ in 0..12 {
                z.extend_from_slice(&[b'0', 0]);
            }
            z
        };
        assert_eq!(parse_mac_string(&zero), None, "an all-zero address is not a real one");
    }

    #[test]
    fn control_messages_have_the_documented_layout() {
        assert_eq!(initialize(7), [2, 0, 0, 0, 24, 0, 0, 0, 7, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0, 0]);
        let q = query(9, OID_PERMANENT_ADDRESS);
        assert_eq!((q.len(), get(&q, 0), get(&q, 4), get(&q, 8), get(&q, 12), get(&q, 20)), (32, Some(4), Some(32), Some(9), Some(0x0101_0101), Some(20)));
        let s = set(3, OID_PACKET_FILTER, FILTER_NORMAL);
        assert_eq!((s.len(), get(&s, 12), get(&s, 16), get(&s, 28)), (32, Some(0x0001_010E), Some(4), Some(0x0B)));
        assert_eq!(keepalive_reply(5), [8, 0, 0, 0x80, 16, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0]);
    }

    fn complete(kind: u32, req: u32, status: u32, info: &[u8]) -> Vec<u8> {
        let mut m = Vec::new();
        let total = if kind == MSG_QUERY | COMPLETE { 24 + info.len() } else { 16 };
        for x in [kind, total as u32, req, status] {
            put(&mut m, x);
        }
        if kind == MSG_QUERY | COMPLETE {
            put(&mut m, info.len() as u32);
            put(&mut m, 16); // offset from the request id field
            m.extend_from_slice(info);
        }
        m
    }

    #[test]
    fn completions_are_parsed_and_checked() {
        let c = complete(MSG_QUERY | COMPLETE, 4, 0, &[1, 2, 3, 4, 5, 6]);
        let p = parse_completion(&c).unwrap();
        assert_eq!((p.kind, p.req, p.status, p.info), (MSG_QUERY | COMPLETE, 4, 0, &[1, 2, 3, 4, 5, 6][..]));
        let s = complete(MSG_SET | COMPLETE, 5, 0xC000_0001, &[]);
        assert_eq!(parse_completion(&s).unwrap().status, 0xC000_0001);
        let k = {
            let mut m = Vec::new();
            for x in [MSG_KEEPALIVE, 12, 77] {
                put(&mut m, x);
            }
            m
        };
        assert_eq!(parse_completion(&k).unwrap().kind, MSG_KEEPALIVE);
        // An information buffer that points outside the message is refused.
        let mut bad = c.clone();
        bad[20] = 200;
        assert!(parse_completion(&bad).is_none());
        for n in 0..c.len() {
            let _ = parse_completion(&c[..n]);
        }
    }

    #[test]
    fn packets_round_trip_through_the_rndis_framing() {
        let frame: Vec<u8> = (0..60u8).collect();
        let w = wrap_packet(&frame, 512);
        assert_eq!((get(&w, 0), get(&w, 4), get(&w, 8), get(&w, 12)), (Some(1), Some(104), Some(36), Some(60)));
        assert_eq!(unwrap_packets(&w), [&frame[..]]);
        // A transfer that would be a multiple of the packet size gets one pad byte, which is ignored.
        let f2 = alloc::vec![9u8; 512 - PACKET_HEADER];
        let w2 = wrap_packet(&f2, 512);
        assert_eq!(w2.len(), 513);
        assert_eq!(unwrap_packets(&w2), [&f2[..]]);
        // Several packets in one transfer.
        let mut both = wrap_packet(&frame, 0);
        both.extend_from_slice(&wrap_packet(&f2, 0));
        let got = unwrap_packets(&both);
        assert_eq!((got.len(), got[0], got[1]), (2, &frame[..], &f2[..]));
    }

    #[test]
    fn hostile_packet_streams_never_panic() {
        let good = wrap_packet(&[7u8; 40], 0);
        for n in 0..good.len() {
            let _ = unwrap_packets(&good[..n]);
        }
        let mut evil = good.clone();
        evil[12] = 0xFF; // data length larger than the message
        assert!(unwrap_packets(&evil).is_empty());
        let mut evil = good.clone();
        evil[8] = 0xFF; // data offset beyond the message
        assert!(unwrap_packets(&evil).is_empty());
        let mut seed = 11u32;
        for len in 0..400usize {
            let junk: Vec<u8> = (0..len).map(|_| { seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345); (seed >> 16) as u8 }).collect();
            let _ = unwrap_packets(&junk);
            let _ = parse_completion(&junk);
        }
    }
}
