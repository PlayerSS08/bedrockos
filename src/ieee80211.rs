//! IEEE 802.11 frames for a WPA2-Personal station: beacon/probe parsing, management frame builders, and CCMP
//! protection of data frames (to and from Ethernet). Pure and host-testable.
//!
//! Scope: infrastructure mode, 2.4/5 GHz, open-system authentication, WPA2-PSK with CCMP, non-QoS data frames,
//! no fragmentation. Not verified against a real access point: it is verified against its own simulated AP.

use alloc::vec::Vec;

use crate::aes;

pub const HDR_LEN: usize = 24;
pub const BROADCAST: [u8; 6] = [0xFF; 6];

// Frame control (first byte: version | type | subtype).
pub const FC_ASSOC_REQ: u8 = 0x00;
pub const FC_ASSOC_RESP: u8 = 0x10;
pub const FC_PROBE_REQ: u8 = 0x40;
pub const FC_PROBE_RESP: u8 = 0x50;
pub const FC_BEACON: u8 = 0x80;
pub const FC_DISASSOC: u8 = 0xA0;
pub const FC_AUTH: u8 = 0xB0;
pub const FC_DEAUTH: u8 = 0xC0;
pub const FC_DATA: u8 = 0x08;

const FLAG_TODS: u8 = 0x01;
const FLAG_FROMDS: u8 = 0x02;
const FLAG_PROTECTED: u8 = 0x40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    Truncated,
    NotForUs,
    NotData,
    Unprotected,
    BadMic,
    Replay,
    UnknownLlc,
    NoKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bss {
    pub bssid: [u8; 6],
    pub ssid: Vec<u8>,
    pub channel: u8,
    pub privacy: bool,
    /// The RSN element exactly as the AP sent it (tag, length and body), if it has one.
    pub rsn: Option<Vec<u8>>,
}

impl Bss {
    /// WPA2-Personal with CCMP for both unicast and group traffic: the only mode this station speaks.
    pub fn supports_wpa2_psk_ccmp(&self) -> bool {
        let Some(rsn) = &self.rsn else { return false };
        // tag, len, version(2), group(4), pairwise count(2) + list, akm count(2) + list
        if rsn.len() < 2 + 2 + 4 + 2 || rsn[2..4] != [1, 0] {
            return false;
        }
        if rsn[4..8] != [0x00, 0x0F, 0xAC, 0x04] {
            return false;
        }
        let pc = u16::from_le_bytes([rsn[8], rsn[9]]) as usize;
        let after_pair = 10 + pc * 4;
        if rsn.len() < after_pair + 2 {
            return false;
        }
        let pair_ok = (0..pc).any(|i| rsn[10 + i * 4..14 + i * 4] == [0x00, 0x0F, 0xAC, 0x04]);
        let ac = u16::from_le_bytes([rsn[after_pair], rsn[after_pair + 1]]) as usize;
        if rsn.len() < after_pair + 2 + ac * 4 {
            return false;
        }
        let akm_ok = (0..ac).any(|i| {
            let o = after_pair + 2 + i * 4;
            rsn[o..o + 4] == [0x00, 0x0F, 0xAC, 0x02]
        });
        pair_ok && akm_ok
    }

    pub fn ssid_str(&self) -> alloc::string::String {
        self.ssid.iter().map(|&b| if (32..127).contains(&b) { b as char } else { '?' }).collect()
    }
}

/// Iterates information elements, stopping at the first malformed one.
pub fn elements(mut d: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    core::iter::from_fn(move || {
        if d.len() < 2 {
            return None;
        }
        let (tag, len) = (d[0], d[1] as usize);
        if d.len() < 2 + len {
            return None;
        }
        let body = &d[2..2 + len];
        d = &d[2 + len..];
        Some((tag, body))
    })
}

fn header(fc0: u8, fc1: u8, a1: &[u8; 6], a2: &[u8; 6], a3: &[u8; 6], seq: u16) -> Vec<u8> {
    let mut h = Vec::with_capacity(HDR_LEN);
    h.push(fc0);
    h.push(fc1);
    h.extend_from_slice(&[0, 0]); // duration: filled by the radio
    h.extend_from_slice(a1);
    h.extend_from_slice(a2);
    h.extend_from_slice(a3);
    h.extend_from_slice(&(seq << 4).to_le_bytes());
    h
}

/// Subtype byte of a management or data frame (the first frame-control byte with the version bits kept).
pub fn frame_kind(frame: &[u8]) -> Option<u8> {
    frame.first().map(|b| b & 0xFC)
}

pub fn parse_bss(frame: &[u8]) -> Option<Bss> {
    let kind = frame_kind(frame)?;
    if (kind != FC_BEACON && kind != FC_PROBE_RESP) || frame.len() < HDR_LEN + 12 {
        return None;
    }
    let bssid: [u8; 6] = frame[16..22].try_into().ok()?;
    let capability = u16::from_le_bytes([frame[HDR_LEN + 10], frame[HDR_LEN + 11]]);
    let mut bss = Bss { bssid, ssid: Vec::new(), channel: 0, privacy: capability & 0x0010 != 0, rsn: None };
    for (tag, body) in elements(&frame[HDR_LEN + 12..]) {
        match tag {
            0 => bss.ssid = body.to_vec(),
            3 if !body.is_empty() => bss.channel = body[0],
            48 => {
                let mut whole = alloc::vec![48, body.len() as u8];
                whole.extend_from_slice(body);
                bss.rsn = Some(whole);
            }
            _ => {}
        }
    }
    Some(bss)
}

const RATES: [u8; 8] = [0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24];
const EXT_RATES: [u8; 4] = [0x30, 0x48, 0x60, 0x6C];

pub fn probe_request(sa: &[u8; 6], ssid: &[u8], seq: u16) -> Vec<u8> {
    let mut f = header(FC_PROBE_REQ, 0, &BROADCAST, sa, &BROADCAST, seq);
    f.extend_from_slice(&[0, ssid.len() as u8]);
    f.extend_from_slice(ssid);
    f.extend_from_slice(&[1, RATES.len() as u8]);
    f.extend_from_slice(&RATES);
    f.extend_from_slice(&[50, EXT_RATES.len() as u8]);
    f.extend_from_slice(&EXT_RATES);
    f
}

pub fn auth_request(sa: &[u8; 6], bssid: &[u8; 6], seq: u16) -> Vec<u8> {
    let mut f = header(FC_AUTH, 0, bssid, sa, bssid, seq);
    f.extend_from_slice(&0u16.to_le_bytes()); // open system
    f.extend_from_slice(&1u16.to_le_bytes()); // transaction 1
    f.extend_from_slice(&0u16.to_le_bytes()); // status
    f
}

/// Returns the status code (0 = success) of an authentication response from `bssid`.
pub fn parse_auth_response(frame: &[u8], bssid: &[u8; 6]) -> Option<u16> {
    if frame_kind(frame)? != FC_AUTH || frame.len() < HDR_LEN + 6 || frame[10..16] != bssid[..] {
        return None;
    }
    let seq = u16::from_le_bytes([frame[HDR_LEN + 2], frame[HDR_LEN + 3]]);
    (seq == 2).then(|| u16::from_le_bytes([frame[HDR_LEN + 4], frame[HDR_LEN + 5]]))
}

pub fn assoc_request(sa: &[u8; 6], bss: &Bss, rsn: &[u8], seq: u16) -> Vec<u8> {
    let mut f = header(FC_ASSOC_REQ, 0, &bss.bssid, sa, &bss.bssid, seq);
    f.extend_from_slice(&0x0431u16.to_le_bytes()); // ESS, privacy, short preamble, short slot
    f.extend_from_slice(&10u16.to_le_bytes()); // listen interval
    f.extend_from_slice(&[0, bss.ssid.len() as u8]);
    f.extend_from_slice(&bss.ssid);
    f.extend_from_slice(&[1, RATES.len() as u8]);
    f.extend_from_slice(&RATES);
    f.extend_from_slice(&[50, EXT_RATES.len() as u8]);
    f.extend_from_slice(&EXT_RATES);
    f.extend_from_slice(rsn);
    f
}

/// Returns `(status, association id)` of an association response from `bssid`.
pub fn parse_assoc_response(frame: &[u8], bssid: &[u8; 6]) -> Option<(u16, u16)> {
    if frame_kind(frame)? != FC_ASSOC_RESP || frame.len() < HDR_LEN + 6 || frame[10..16] != bssid[..] {
        return None;
    }
    let status = u16::from_le_bytes([frame[HDR_LEN + 2], frame[HDR_LEN + 3]]);
    let aid = u16::from_le_bytes([frame[HDR_LEN + 4], frame[HDR_LEN + 5]]) & 0x3FFF;
    Some((status, aid))
}

const LLC_SNAP: [u8; 6] = [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00];

/// Data frame to the AP carrying `llc_payload` (LLC/SNAP + payload) unprotected: used for the EAPOL exchange.
pub fn data_plain(sa: &[u8; 6], bssid: &[u8; 6], da: &[u8; 6], ethertype: u16, payload: &[u8], seq: u16) -> Vec<u8> {
    let mut f = header(FC_DATA, FLAG_TODS, bssid, sa, da, seq);
    f.extend_from_slice(&LLC_SNAP);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

/// Splits a from-DS unprotected data frame into `(source, ethertype, payload)`.
pub fn parse_data_plain(frame: &[u8], bssid: &[u8; 6], me: &[u8; 6]) -> Result<([u8; 6], u16, Vec<u8>), FrameError> {
    let (sa, body) = data_header(frame, bssid, me)?;
    if frame[1] & FLAG_PROTECTED != 0 {
        return Err(FrameError::Unprotected);
    }
    let (et, payload) = unsnap(body)?;
    Ok((sa, et, payload.to_vec()))
}

/// Common checks for a from-DS data frame addressed to `me`: returns the original source and the body.
fn data_header<'a>(frame: &'a [u8], bssid: &[u8; 6], me: &[u8; 6]) -> Result<([u8; 6], &'a [u8]), FrameError> {
    if frame.len() < HDR_LEN {
        return Err(FrameError::Truncated);
    }
    if frame[0] & 0x0C != 0x08 || frame[0] & 0xF0 != 0 {
        return Err(FrameError::NotData); // not a plain (non-QoS) data frame
    }
    if frame[1] & 0x03 != FLAG_FROMDS || frame[10..16] != bssid[..] {
        return Err(FrameError::NotForUs);
    }
    let da: [u8; 6] = frame[4..10].try_into().unwrap();
    if da != *me && da[0] & 1 == 0 {
        return Err(FrameError::NotForUs);
    }
    Ok((frame[16..22].try_into().unwrap(), &frame[HDR_LEN..]))
}

fn unsnap(body: &[u8]) -> Result<(u16, &[u8]), FrameError> {
    if body.len() < 8 {
        return Err(FrameError::Truncated);
    }
    if body[..6] != LLC_SNAP {
        return Err(FrameError::UnknownLlc);
    }
    Ok((u16::from_be_bytes([body[6], body[7]]), &body[8..]))
}

fn ccmp_aad(hdr: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(22);
    aad.push(hdr[0] & 0x8F); // subtype bits 4-6 masked for data frames (non-QoS: unchanged)
    aad.push(hdr[1] & !0x38); // retry, power management, more data masked; protected kept
    aad.extend_from_slice(&hdr[4..22]); // A1, A2, A3
    aad.push(hdr[22] & 0x0F); // fragment number kept, sequence number masked
    aad.push(0);
    aad
}

fn ccmp_nonce(a2: &[u8], pn: u64) -> [u8; aes::CCM_NONCE] {
    let mut n = [0u8; aes::CCM_NONCE];
    n[0] = 0; // priority: non-QoS
    n[1..7].copy_from_slice(a2);
    n[7..13].copy_from_slice(&pn.to_be_bytes()[2..8]);
    n
}

/// Encrypts an Ethernet frame (`dst | src | ethertype | payload`) into a protected to-DS data frame.
pub fn data_protected(
    tk: &[u8; 16],
    pn: u64,
    key_id: u8,
    sa: &[u8; 6],
    bssid: &[u8; 6],
    eth: &[u8],
    seq: u16,
) -> Option<Vec<u8>> {
    if eth.len() < 14 {
        return None;
    }
    let da: [u8; 6] = eth[..6].try_into().unwrap();
    let mut f = header(FC_DATA, FLAG_TODS | FLAG_PROTECTED, bssid, sa, &da, seq);
    let mut plain = Vec::with_capacity(8 + eth.len() - 12);
    plain.extend_from_slice(&LLC_SNAP);
    plain.extend_from_slice(&eth[12..14]);
    plain.extend_from_slice(&eth[14..]);
    let sealed = aes::ccm_encrypt(tk, &ccmp_nonce(sa, pn), &ccmp_aad(&f), &plain);
    let p = pn.to_le_bytes();
    f.extend_from_slice(&[p[0], p[1], 0, 0x20 | (key_id << 6), p[2], p[3], p[4], p[5]]);
    f.extend_from_slice(&sealed);
    Some(f)
}

/// Decrypts a from-DS protected data frame into an Ethernet frame. `last_pn` is the replay counter for this
/// key: it must be strictly increasing, and is updated on success.
pub fn data_unprotect(
    key: &[u8; 16],
    last_pn: &mut u64,
    frame: &[u8],
    bssid: &[u8; 6],
    me: &[u8; 6],
) -> Result<Vec<u8>, FrameError> {
    let (sa, body) = data_header(frame, bssid, me)?;
    if frame[1] & FLAG_PROTECTED == 0 {
        return Err(FrameError::Unprotected);
    }
    if body.len() < 8 + 8 {
        return Err(FrameError::Truncated);
    }
    let pn = u64::from_le_bytes([body[0], body[1], body[4], body[5], body[6], body[7], 0, 0]);
    if pn <= *last_pn {
        return Err(FrameError::Replay);
    }
    let a2: &[u8] = &frame[10..16];
    let plain = aes::ccm_decrypt(key, &ccmp_nonce(a2, pn), &ccmp_aad(&frame[..HDR_LEN]), &body[8..])
        .ok_or(FrameError::BadMic)?;
    *last_pn = pn;
    let (et, payload) = unsnap(&plain)?;
    let mut eth = Vec::with_capacity(14 + payload.len());
    eth.extend_from_slice(&frame[4..10]);
    eth.extend_from_slice(&sa);
    eth.extend_from_slice(&et.to_be_bytes());
    eth.extend_from_slice(payload);
    Ok(eth)
}

/// From-DS protected data frame (what an access point sends to a station). Used by the simulated AP in the
/// tests; it is public because a future hotspot mode would need it too.
#[allow(clippy::too_many_arguments)]
pub fn data_from_ap_protected(
    key: &[u8; 16],
    pn: u64,
    key_id: u8,
    bssid: &[u8; 6],
    da: &[u8; 6],
    sa: &[u8; 6],
    ethertype: u16,
    payload: &[u8],
    seq: u16,
) -> Vec<u8> {
    let mut f = header(FC_DATA, FLAG_FROMDS | FLAG_PROTECTED, da, bssid, sa, seq);
    let mut plain = LLC_SNAP.to_vec();
    plain.extend_from_slice(&ethertype.to_be_bytes());
    plain.extend_from_slice(payload);
    let sealed = aes::ccm_encrypt(key, &ccmp_nonce(bssid, pn), &ccmp_aad(&f), &plain);
    let p = pn.to_le_bytes();
    f.extend_from_slice(&[p[0], p[1], 0, 0x20 | (key_id << 6), p[2], p[3], p[4], p[5]]);
    f.extend_from_slice(&sealed);
    f
}

/// From-DS unprotected data frame (the EAPOL messages from the access point).
pub fn data_from_ap_plain(bssid: &[u8; 6], da: &[u8; 6], ethertype: u16, payload: &[u8], seq: u16) -> Vec<u8> {
    let mut f = header(FC_DATA, FLAG_FROMDS, da, bssid, bssid, seq);
    f.extend_from_slice(&LLC_SNAP);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

/// Management frames an access point sends (shared by the tests' simulated AP).
pub fn mgmt_from_ap(kind: u8, bssid: &[u8; 6], da: &[u8; 6], body: &[u8], seq: u16) -> Vec<u8> {
    let mut f = header(kind, 0, da, bssid, bssid, seq);
    f.extend_from_slice(body);
    f
}

/// The decrypt direction for to-DS protected frames, used by the simulated AP.
pub fn data_unprotect_to_ap(key: &[u8; 16], frame: &[u8], bssid: &[u8; 6]) -> Option<Vec<u8>> {
    if frame.len() < HDR_LEN + 16 || frame[1] & 0x43 != FLAG_TODS | FLAG_PROTECTED || frame[4..10] != bssid[..] {
        return None;
    }
    let body = &frame[HDR_LEN..];
    let pn = u64::from_le_bytes([body[0], body[1], body[4], body[5], body[6], body[7], 0, 0]);
    let plain = aes::ccm_decrypt(key, &ccmp_nonce(&frame[10..16], pn), &ccmp_aad(&frame[..HDR_LEN]), &body[8..])?;
    let (et, payload) = unsnap(&plain).ok()?;
    let mut eth = Vec::new();
    eth.extend_from_slice(&frame[16..22]); // DA
    eth.extend_from_slice(&frame[10..16]); // SA
    eth.extend_from_slice(&et.to_be_bytes());
    eth.extend_from_slice(payload);
    Some(eth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wpa::RSN_WPA2_PSK_CCMP;

    const AP: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    const ME: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];

    pub fn beacon(ssid: &[u8], channel: u8, rsn: Option<&[u8]>) -> Vec<u8> {
        let mut f = header(FC_BEACON, 0, &BROADCAST, &AP, &AP, 7);
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&100u16.to_le_bytes());
        f.extend_from_slice(&0x0431u16.to_le_bytes());
        f.extend_from_slice(&[0, ssid.len() as u8]);
        f.extend_from_slice(ssid);
        f.extend_from_slice(&[3, 1, channel]);
        if let Some(r) = rsn {
            f.extend_from_slice(r);
        }
        f
    }

    #[test]
    fn beacon_parsing_and_wpa2_detection() {
        let b = parse_bss(&beacon(b"AeonNet", 36, Some(&RSN_WPA2_PSK_CCMP))).unwrap();
        assert_eq!((b.bssid, b.channel, b.privacy), (AP, 36, true));
        assert_eq!(b.ssid_str(), "AeonNet");
        assert!(b.supports_wpa2_psk_ccmp());
        // 802.1X (enterprise) is not supported, nor is a network with no RSN element at all.
        let mut ent = RSN_WPA2_PSK_CCMP;
        ent[19] = 0x01;
        assert!(!parse_bss(&beacon(b"Corp", 1, Some(&ent))).unwrap().supports_wpa2_psk_ccmp());
        assert!(!parse_bss(&beacon(b"Open", 1, None)).unwrap().supports_wpa2_psk_ccmp());
        // Group cipher TKIP is refused.
        let mut tkip = RSN_WPA2_PSK_CCMP;
        tkip[7] = 0x02;
        assert!(!parse_bss(&beacon(b"Old", 1, Some(&tkip))).unwrap().supports_wpa2_psk_ccmp());
    }

    #[test]
    fn truncated_and_random_frames_never_panic() {
        let good = beacon(b"AeonNet", 6, Some(&RSN_WPA2_PSK_CCMP));
        for n in 0..good.len() {
            let _ = parse_bss(&good[..n]);
            let _ = parse_auth_response(&good[..n], &AP);
            let _ = parse_assoc_response(&good[..n], &AP);
            let _ = parse_data_plain(&good[..n], &AP, &ME);
            let mut l = 0;
            let _ = data_unprotect(&[0; 16], &mut l, &good[..n], &AP, &ME);
        }
        let mut seed = 99u32;
        for len in 0..200 {
            let f: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 16) as u8
                })
                .collect();
            let _ = parse_bss(&f);
            let mut l = 0;
            let _ = data_unprotect(&[1; 16], &mut l, &f, &AP, &ME);
        }
    }

    #[test]
    fn management_frames_have_the_right_shape() {
        let a = auth_request(&ME, &AP, 1);
        assert_eq!(a.len(), HDR_LEN + 6);
        assert_eq!((a[0], &a[4..10], &a[10..16]), (FC_AUTH, &AP[..], &ME[..]));
        let bss = parse_bss(&beacon(b"AeonNet", 1, Some(&RSN_WPA2_PSK_CCMP))).unwrap();
        let r = assoc_request(&ME, &bss, &RSN_WPA2_PSK_CCMP, 2);
        assert!(r.ends_with(&RSN_WPA2_PSK_CCMP));
        let ssid_ie = elements(&r[HDR_LEN + 4..]).next().unwrap();
        assert_eq!(ssid_ie, (0, &b"AeonNet"[..]));
    }

    /// A frame the AP would send to us: from-DS, protected, addressed to ME from `sa`.
    fn ap_to_us(key: &[u8; 16], pn: u64, sa: &[u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut f = header(FC_DATA, FLAG_FROMDS | FLAG_PROTECTED, &ME, &AP, sa, 3);
        let mut plain = LLC_SNAP.to_vec();
        plain.extend_from_slice(&ethertype.to_be_bytes());
        plain.extend_from_slice(payload);
        let sealed = aes::ccm_encrypt(key, &ccmp_nonce(&AP, pn), &ccmp_aad(&f), &plain);
        let p = pn.to_le_bytes();
        f.extend_from_slice(&[p[0], p[1], 0, 0x20, p[2], p[3], p[4], p[5]]);
        f.extend_from_slice(&sealed);
        f
    }

    #[test]
    fn ccmp_round_trip_replay_and_tamper() {
        let key = [0x5A; 16];
        let src = [0x02, 1, 2, 3, 4, 5];
        let f = ap_to_us(&key, 10, &src, 0x0800, b"hello world");
        let mut last = 0;
        let eth = data_unprotect(&key, &mut last, &f, &AP, &ME).unwrap();
        assert_eq!(&eth[..6], &ME);
        assert_eq!(&eth[6..12], &src);
        assert_eq!(&eth[12..14], &[0x08, 0x00]);
        assert_eq!(&eth[14..], b"hello world");
        assert_eq!(last, 10);
        assert_eq!(data_unprotect(&key, &mut last, &f, &AP, &ME), Err(FrameError::Replay));
        let mut bad = ap_to_us(&key, 11, &src, 0x0800, b"hello world");
        let n = bad.len();
        bad[n - 3] ^= 0x01;
        assert_eq!(data_unprotect(&key, &mut last, &bad, &AP, &ME), Err(FrameError::BadMic));
        assert_eq!(last, 10, "a rejected frame must not advance the replay counter");
        let other = ap_to_us(&[0u8; 16], 12, &src, 0x0800, b"x");
        assert_eq!(data_unprotect(&key, &mut last, &other, &AP, &ME), Err(FrameError::BadMic));
    }

    #[test]
    fn our_protected_frame_is_decryptable_by_the_ccmp_rules() {
        // Encrypt as the station, then verify with the AAD/nonce rules the AP would apply (A2 = us, to-DS).
        let tk = [0x33; 16];
        let mut eth = Vec::new();
        eth.extend_from_slice(&[0x02, 9, 9, 9, 9, 9]);
        eth.extend_from_slice(&ME);
        eth.extend_from_slice(&[0x08, 0x06]);
        eth.extend_from_slice(b"payload-bytes");
        let f = data_protected(&tk, 77, 0, &ME, &AP, &eth, 5).unwrap();
        assert_eq!(f[1] & 0x43, FLAG_TODS | FLAG_PROTECTED);
        assert_eq!(&f[4..10], &AP);
        assert_eq!(&f[10..16], &ME);
        let plain = aes::ccm_decrypt(&tk, &ccmp_nonce(&ME, 77), &ccmp_aad(&f[..HDR_LEN]), &f[HDR_LEN + 8..]).unwrap();
        assert_eq!(&plain[..6], &LLC_SNAP);
        assert_eq!(&plain[6..8], &[0x08, 0x06]);
        assert_eq!(&plain[8..], b"payload-bytes");
        assert!(data_protected(&tk, 1, 0, &ME, &AP, &[0; 5], 0).is_none());
    }

    #[test]
    fn plain_eapol_data_frames_round_trip() {
        let f = data_plain(&ME, &AP, &AP, 0x888E, b"eapol", 1);
        assert_eq!(f[1] & 0x03, FLAG_TODS);
        // Build the from-DS twin by hand and parse it.
        let mut r = header(FC_DATA, FLAG_FROMDS, &ME, &AP, &AP, 1);
        r.extend_from_slice(&LLC_SNAP);
        r.extend_from_slice(&0x888Eu16.to_be_bytes());
        r.extend_from_slice(b"eapol");
        assert_eq!(parse_data_plain(&r, &AP, &ME).unwrap(), (AP, 0x888E, b"eapol".to_vec()));
        assert_eq!(parse_data_plain(&r, &[1; 6], &ME), Err(FrameError::NotForUs));
        assert_eq!(parse_data_plain(&r, &AP, &[3; 6]), Err(FrameError::NotForUs));
    }
}
