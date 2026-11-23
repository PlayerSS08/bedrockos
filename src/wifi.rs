//! Wi-Fi station: scan, join a WPA2-Personal network and move Ethernet frames through it. It sits on a
//! [`Radio`], the small trait a chip driver has to provide (set channel, transmit a raw 802.11 frame, receive
//! one, a clock). Once joined, a [`Station`] is a [`NetDev`], so the existing IP stack runs over it unchanged.
//!
//! The chip driver for the OMEN's Realtek RTL8852BE is NOT written: it needs Realtek's firmware blob, the MAC
//! and baseband setup tables and DMA rings, and cannot be proven without the laptop. Everything above the radio
//! is tested here against a simulated access point (`tests`), including the full 4-way handshake.
//!
//! Known limits: no group-key or pairwise rekey handshakes (after an AP rekeys the group key, broadcast and
//! multicast stop arriving while unicast keeps working), no power save, no roaming, WPA2-PSK/CCMP only.

use alloc::vec::Vec;

use crate::ieee80211 as dot11;
use crate::netdev::{NetDev, NetError};
use crate::wpa::{self, Supplicant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiError {
    Radio,
    NoSuchNetwork,
    UnsupportedSecurity,
    BadPassphrase,
    AuthRejected(u16),
    AssocRejected(u16),
    Timeout,
    HandshakeFailed(wpa::WpaError),
    Deauthenticated,
    NotConnected,
}

pub trait Radio {
    fn mac(&self) -> [u8; 6];
    fn set_channel(&mut self, channel: u8) -> Result<(), WifiError>;
    fn tx(&mut self, frame: &[u8]) -> Result<(), WifiError>;
    /// Returns the length of the next received frame copied into `buf`, or `None` if nothing is waiting.
    fn rx(&mut self, buf: &mut [u8]) -> Option<usize>;
    fn now_ms(&mut self) -> u64;
}

/// A name for a Wi-Fi chip by PCI id, for the `wifi` command. Only chips someone has met are listed; anything
/// else is reported by its raw id.
pub fn chip_name(vendor: u16, device: u16) -> Option<&'static str> {
    Some(match (vendor, device) {
        (0x10EC, 0xB520) => "Realtek RTL8852BE Wi-Fi 6",
        (0x10EC, 0xC822) | (0x10EC, 0xC82F) => "Realtek RTL8822CE Wi-Fi 5",
        (0x10EC, 0xB852) => "Realtek RTL8852BE (variant)",
        (0x8086, 0x2723) => "Intel Wi-Fi 6 AX200",
        (0x8086, 0x2725) => "Intel Wi-Fi 6E AX210",
        (0x8086, 0x51F0) | (0x8086, 0x51F1) | (0x8086, 0x7A70) => "Intel Wi-Fi 6E AX211",
        (0x8086, 0x06F0) | (0x8086, 0xA0F0) | (0x8086, 0x34F0) => "Intel Wi-Fi 6 AX201",
        (0x14C3, 0x0608) | (0x14C3, 0x7961) => "MediaTek Wi-Fi",
        (0x168C, _) => "Qualcomm Atheros Wi-Fi",
        (0x14E4, _) => "Broadcom Wi-Fi",
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Idle,
    Connected,
}

struct Session {
    bss: dot11::Bss,
    tk: [u8; 16],
    gtk: [u8; 16],
    gtk_id: u8,
    tx_pn: u64,
    rx_pn_ptk: u64,
    rx_pn_gtk: u64,
}

pub const CHANNELS_24: [u8; 11] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
pub const CHANNELS_5: [u8; 9] = [36, 40, 44, 48, 149, 153, 157, 161, 165];
const RX_BUF: usize = 2400;

pub struct Station<R: Radio> {
    radio: R,
    seq: u16,
    session: Option<Session>,
}

impl<R: Radio> Station<R> {
    pub fn new(radio: R) -> Self {
        Self { radio, seq: 0, session: None }
    }

    pub fn radio(&mut self) -> &mut R {
        &mut self.radio
    }

    pub fn state(&self) -> LinkState {
        if self.session.is_some() { LinkState::Connected } else { LinkState::Idle }
    }

    pub fn current(&self) -> Option<&dot11::Bss> {
        self.session.as_ref().map(|s| &s.bss)
    }

    fn next_seq(&mut self) -> u16 {
        self.seq = (self.seq + 1) & 0x0FFF;
        self.seq
    }

    /// Active scan: a broadcast probe request on every channel, then listen for `dwell_ms`. Results are
    /// de-duplicated by BSSID.
    pub fn scan(&mut self, channels: &[u8], dwell_ms: u64) -> Vec<dot11::Bss> {
        let mut found: Vec<dot11::Bss> = Vec::new();
        let mac = self.radio.mac();
        let mut buf = [0u8; RX_BUF];
        for &ch in channels {
            if self.radio.set_channel(ch).is_err() {
                continue;
            }
            let seq = self.next_seq();
            let _ = self.radio.tx(&dot11::probe_request(&mac, b"", seq));
            let end = self.radio.now_ms() + dwell_ms;
            while self.radio.now_ms() < end {
                if let Some(n) = self.radio.rx(&mut buf) {
                    if let Some(mut b) = dot11::parse_bss(&buf[..n]) {
                        if b.channel == 0 {
                            b.channel = ch;
                        }
                        match found.iter_mut().find(|x| x.bssid == b.bssid) {
                            Some(old) => *old = b,
                            None => found.push(b),
                        }
                    }
                }
            }
        }
        found
    }

    fn wait_frame(&mut self, timeout_ms: u64, mut want: impl FnMut(&[u8]) -> bool) -> Option<Vec<u8>> {
        let mut buf = [0u8; RX_BUF];
        let end = self.radio.now_ms() + timeout_ms;
        while self.radio.now_ms() < end {
            if let Some(n) = self.radio.rx(&mut buf) {
                if want(&buf[..n]) {
                    return Some(buf[..n].to_vec());
                }
            }
        }
        None
    }

    /// Joins `bss` with `passphrase`. `snonce` must be unpredictable (kernel entropy).
    pub fn connect(&mut self, bss: &dot11::Bss, passphrase: &str, snonce: [u8; 32]) -> Result<(), WifiError> {
        self.session = None;
        if !bss.supports_wpa2_psk_ccmp() {
            return Err(WifiError::UnsupportedSecurity);
        }
        let pmk = wpa::derive_pmk(passphrase, &bss.ssid).map_err(|_| WifiError::BadPassphrase)?;
        let me = self.radio.mac();
        self.radio.set_channel(bss.channel)?;

        // 1. Open-system authentication.
        let mut status = None;
        for _ in 0..3 {
            let seq = self.next_seq();
            self.radio.tx(&dot11::auth_request(&me, &bss.bssid, seq))?;
            let bssid = bss.bssid;
            if let Some(f) = self.wait_frame(300, |f| dot11::parse_auth_response(f, &bssid).is_some()) {
                status = dot11::parse_auth_response(&f, &bssid);
                break;
            }
        }
        match status {
            None => return Err(WifiError::Timeout),
            Some(0) => {}
            Some(code) => return Err(WifiError::AuthRejected(code)),
        }

        // 2. Association, offering exactly WPA2-PSK/CCMP.
        let mut assoc = None;
        for _ in 0..3 {
            let seq = self.next_seq();
            self.radio.tx(&dot11::assoc_request(&me, bss, &wpa::RSN_WPA2_PSK_CCMP, seq))?;
            let bssid = bss.bssid;
            if let Some(f) = self.wait_frame(300, |f| dot11::parse_assoc_response(f, &bssid).is_some()) {
                assoc = dot11::parse_assoc_response(&f, &bssid);
                break;
            }
        }
        match assoc {
            None => return Err(WifiError::Timeout),
            Some((0, _aid)) => {}
            Some((code, _)) => return Err(WifiError::AssocRejected(code)),
        }

        // 3. The 4-way handshake: the AP speaks first (message 1), then message 3 after our message 2.
        let mut sup = Supplicant::new(pmk, bss.bssid, me, snonce, bss.rsn.clone());
        let deadline = self.radio.now_ms() + 4000;
        // Frames that fail their checks are ignored (anyone can inject them); the exchange only fails by timing
        // out, or on an error found in an authenticated message. The last ignored error explains a timeout.
        let mut last_error = None;
        while sup.state() != wpa::State::Done {
            if self.radio.now_ms() >= deadline {
                return Err(match last_error {
                    Some(e) => WifiError::HandshakeFailed(e),
                    None => WifiError::Timeout,
                });
            }
            let bssid = bss.bssid;
            let Some(f) = self.wait_frame(200, |f| {
                matches!(dot11::frame_kind(f), Some(dot11::FC_DEAUTH) | Some(dot11::FC_DISASSOC) | Some(dot11::FC_DATA))
                    && (dot11::frame_kind(f) != Some(dot11::FC_DATA)
                        || matches!(dot11::parse_data_plain(f, &bssid, &me), Ok((_, 0x888E, _))))
            }) else {
                continue;
            };
            if dot11::frame_kind(&f) != Some(dot11::FC_DATA) {
                return Err(WifiError::Deauthenticated);
            }
            let (_, _, eapol) = dot11::parse_data_plain(&f, &bss.bssid, &me).map_err(|_| WifiError::Radio)?;
            match sup.handle(&eapol) {
                Ok(Some(reply)) => {
                    let seq = self.next_seq();
                    self.radio.tx(&dot11::data_plain(&me, &bss.bssid, &bss.bssid, 0x888E, &reply, seq))?;
                }
                Ok(None) => {}
                Err(
                    e @ (wpa::WpaError::UnexpectedMessage
                    | wpa::WpaError::BadMic
                    | wpa::WpaError::NonceChanged
                    | wpa::WpaError::ReplayedMessage
                    | wpa::WpaError::Truncated
                    | wpa::WpaError::NotEapolKey
                    | wpa::WpaError::UnsupportedDescriptor),
                ) => last_error = Some(e),
                // The MIC verified, so the AP itself sent something unacceptable (for example a downgraded RSN).
                Err(e) => return Err(WifiError::HandshakeFailed(e)),
            }
        }
        let (ptk, gtk) = (sup.ptk().ok_or(WifiError::Radio)?, sup.gtk().ok_or(WifiError::Radio)?);
        self.session = Some(Session {
            bss: bss.clone(),
            tk: ptk.tk,
            gtk: gtk.key,
            gtk_id: gtk.key_id,
            tx_pn: 0,
            rx_pn_ptk: 0,
            // Group frames must be newer than what the AP had sent when it gave us the key.
            rx_pn_gtk: sup.gtk_rsc(),
        });
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.session = None;
    }

    /// Sends one Ethernet frame through the link, CCMP-protected.
    pub fn send_eth(&mut self, eth: &[u8]) -> Result<(), WifiError> {
        let me = self.radio.mac();
        let seq = self.next_seq();
        let s = self.session.as_mut().ok_or(WifiError::NotConnected)?;
        s.tx_pn += 1;
        let f = dot11::data_protected(&s.tk, s.tx_pn, 0, &me, &s.bss.bssid, eth, seq).ok_or(WifiError::Radio)?;
        self.radio.tx(&f)
    }

    /// Polls the radio once. `Ok(Some(n))` = an Ethernet frame of `n` bytes is in `out`; `Ok(None)` = nothing
    /// for us (nothing waiting, or a frame that was not ours / failed its checks); `Err` = the AP dropped us.
    pub fn recv_eth(&mut self, out: &mut [u8]) -> Result<Option<usize>, WifiError> {
        let me = self.radio.mac();
        let mut buf = [0u8; RX_BUF];
        let Some(n) = self.radio.rx(&mut buf) else { return Ok(None) };
        let frame = &buf[..n];
        let Some(s) = self.session.as_mut() else { return Err(WifiError::NotConnected) };
        match dot11::frame_kind(frame) {
            Some(dot11::FC_DEAUTH) | Some(dot11::FC_DISASSOC) if frame.len() >= 16 && frame[10..16] == s.bss.bssid => {
                self.session = None;
                return Err(WifiError::Deauthenticated);
            }
            Some(dot11::FC_DATA) => {}
            _ => return Ok(None),
        }
        let unicast = frame.len() >= 10 && frame[4] & 1 == 0;
        let result = if unicast {
            dot11::data_unprotect(&s.tk, &mut s.rx_pn_ptk, frame, &s.bss.bssid, &me)
        } else if frame.len() > dot11::HDR_LEN + 3 && frame[dot11::HDR_LEN + 3] >> 6 == s.gtk_id {
            dot11::data_unprotect(&s.gtk, &mut s.rx_pn_gtk, frame, &s.bss.bssid, &me)
        } else {
            // A group frame under a key index we do not hold (for example after a rekey we cannot do yet).
            return Ok(None);
        };
        match result {
            Ok(eth) if eth.len() <= out.len() => {
                out[..eth.len()].copy_from_slice(&eth);
                Ok(Some(eth.len()))
            }
            _ => Ok(None),
        }
    }
}

impl<R: Radio> NetDev for Station<R> {
    fn name(&self) -> &'static str {
        "wlan0"
    }
    fn mac_address(&self) -> [u8; 6] {
        self.radio.mac()
    }
    fn link_up(&self) -> bool {
        self.session.is_some()
    }
    fn send(&mut self, packet: &[u8]) -> Result<(), NetError> {
        self.send_eth(packet).map_err(|e| match e {
            WifiError::NotConnected => NetError::LinkDown,
            _ => NetError::HardwareError,
        })
    }
    fn receive(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        match self.recv_eth(buf) {
            Ok(Some(n)) => Ok(n),
            Ok(None) => Err(NetError::RxEmpty),
            Err(WifiError::NotConnected) | Err(WifiError::Deauthenticated) => Err(NetError::LinkDown),
            Err(_) => Err(NetError::HardwareError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wpa::{EapolKey, KI_ACK, KI_ENCRYPTED, KI_INSTALL, KI_MIC, KI_PAIRWISE, KI_SECURE};
    use alloc::collections::VecDeque;

    const AP_MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    const STA_MAC: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
    const ANONCE: [u8; 32] = [0x77; 32];

    #[derive(Default)]
    struct Quirks {
        reject_auth: bool,
        reject_assoc: bool,
        silent: bool,
        deauth_after_msg2: bool,
        wrong_psk: bool,
    }

    /// An access point and a radio in one: everything the station transmits is answered from here.
    struct SimAp {
        ssid: Vec<u8>,
        psk: &'static str,
        channel: u8,
        on_channel: u8,
        q: VecDeque<Vec<u8>>,
        clock: u64,
        ptk: Option<wpa::Ptk>,
        ap_pn: u64,
        quirks: Quirks,
        seen_plain_ip: bool,
    }

    impl SimAp {
        fn new(quirks: Quirks) -> Self {
            Self {
                ssid: b"AeonNet".to_vec(),
                psk: "BedrockOS-pass-1234",
                channel: 6,
                on_channel: 0,
                q: VecDeque::new(),
                clock: 0,
                ptk: None,
                ap_pn: 0,
                quirks,
                seen_plain_ip: false,
            }
        }

        fn beacon_body(&self) -> Vec<u8> {
            let mut b = alloc::vec![0u8; 8];
            b.extend_from_slice(&100u16.to_le_bytes());
            b.extend_from_slice(&0x0431u16.to_le_bytes());
            b.extend_from_slice(&[0, self.ssid.len() as u8]);
            b.extend_from_slice(&self.ssid);
            b.extend_from_slice(&[3, 1, self.channel]);
            b.extend_from_slice(&wpa::RSN_WPA2_PSK_CCMP);
            b
        }

        fn pmk(&self) -> [u8; 32] {
            let psk = if self.quirks.wrong_psk { "Different-pass-0" } else { self.psk };
            wpa::derive_pmk(psk, &self.ssid).unwrap()
        }

        fn eapol(&mut self, info: u16, replay: u64, data: Vec<u8>, key: Option<&[u8; 16]>) {
            let m = EapolKey {
                info: 2 | info,
                key_len: 16,
                replay,
                nonce: ANONCE,
                iv: [0; 16],
                rsc: [0; 8],
                mic: [0; 16],
                data,
            };
            let bytes = match key {
                Some(k) => wpa::sign(k, &m),
                None => m.build(),
            };
            self.q.push_back(dot11::data_from_ap_plain(&AP_MAC, &STA_MAC, 0x888E, &bytes, 1));
        }
    }

    impl Radio for SimAp {
        fn mac(&self) -> [u8; 6] {
            STA_MAC
        }
        fn set_channel(&mut self, ch: u8) -> Result<(), WifiError> {
            self.on_channel = ch;
            Ok(())
        }
        fn now_ms(&mut self) -> u64 {
            self.clock += 1;
            self.clock
        }
        fn rx(&mut self, buf: &mut [u8]) -> Option<usize> {
            let f = self.q.pop_front()?;
            buf[..f.len()].copy_from_slice(&f);
            Some(f.len())
        }
        fn tx(&mut self, f: &[u8]) -> Result<(), WifiError> {
            if self.quirks.silent {
                return Ok(());
            }
            match dot11::frame_kind(f) {
                Some(dot11::FC_PROBE_REQ) if self.on_channel == self.channel => {
                    let body = self.beacon_body();
                    self.q.push_back(dot11::mgmt_from_ap(dot11::FC_PROBE_RESP, &AP_MAC, &STA_MAC, &body, 1));
                }
                Some(dot11::FC_AUTH) => {
                    let status: u16 = if self.quirks.reject_auth { 13 } else { 0 };
                    let mut b = Vec::new();
                    b.extend_from_slice(&0u16.to_le_bytes());
                    b.extend_from_slice(&2u16.to_le_bytes());
                    b.extend_from_slice(&status.to_le_bytes());
                    self.q.push_back(dot11::mgmt_from_ap(dot11::FC_AUTH, &AP_MAC, &STA_MAC, &b, 1));
                }
                Some(dot11::FC_ASSOC_REQ) => {
                    // The station must have offered exactly what we support.
                    assert!(f.ends_with(&wpa::RSN_WPA2_PSK_CCMP));
                    let status: u16 = if self.quirks.reject_assoc { 17 } else { 0 };
                    let mut b = Vec::new();
                    b.extend_from_slice(&0x0431u16.to_le_bytes());
                    b.extend_from_slice(&status.to_le_bytes());
                    b.extend_from_slice(&(0xC000u16 | 1).to_le_bytes());
                    self.q.push_back(dot11::mgmt_from_ap(dot11::FC_ASSOC_RESP, &AP_MAC, &STA_MAC, &b, 1));
                    if !self.quirks.reject_assoc {
                        self.eapol(KI_PAIRWISE | KI_ACK, 1, Vec::new(), None);
                    }
                }
                Some(dot11::FC_DATA) => self.data_from_station(f),
                _ => {}
            }
            Ok(())
        }
    }

    impl SimAp {
        fn data_from_station(&mut self, f: &[u8]) {
            if f[1] & 0x40 == 0 {
                // EAPOL message 2 or 4.
                let (_, et, payload) = dot11::parse_data_plain(&{
                    // Reuse the parser: flip the frame into from-DS form by rebuilding it.
                    let mut g = f.to_vec();
                    g[1] = 0x02;
                    g[4..10].copy_from_slice(&STA_MAC);
                    g[10..16].copy_from_slice(&AP_MAC);
                    g
                }, &AP_MAC, &STA_MAC)
                .unwrap();
                assert_eq!(et, 0x888E);
                let m = EapolKey::parse(&payload).unwrap();
                if m.info & KI_SECURE == 0 {
                    // Message 2: derive the PTK, check the MIC, send message 3.
                    let ptk = wpa::derive_ptk(&self.pmk(), &AP_MAC, &STA_MAC, &ANONCE, &m.nonce);
                    let ok = wpa::compute_mic(&ptk.kck, &payload) == m.mic;
                    // A wrong passphrase on the AP side: our own MIC check fails, and the AP stays silent.
                    if !ok {
                        return;
                    }
                    let kd = wpa::build_msg3_key_data(&ptk.kek, &wpa::RSN_WPA2_PSK_CCMP, 1, &[0x99; 16]);
                    let info = KI_PAIRWISE | KI_INSTALL | KI_ACK | KI_MIC | KI_SECURE | KI_ENCRYPTED;
                    self.eapol(info, 2, kd, Some(&ptk.kck));
                    self.ptk = Some(ptk);
                    if self.quirks.deauth_after_msg2 {
                        self.q.clear();
                        self.q.push_back(dot11::mgmt_from_ap(dot11::FC_DEAUTH, &AP_MAC, &STA_MAC, &[3, 0], 1));
                    }
                }
            } else if let Some(ptk) = &self.ptk {
                // Protected data: decrypt, and echo it back with source and destination swapped.
                let eth = dot11::data_unprotect_to_ap(&ptk.tk, f, &AP_MAC).expect("station frame must decrypt");
                self.seen_plain_ip = true;
                self.ap_pn += 1;
                let reply = dot11::data_from_ap_protected(
                    &ptk.tk,
                    self.ap_pn,
                    0,
                    &AP_MAC,
                    &STA_MAC,
                    &[0x02, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC],
                    u16::from_be_bytes([eth[12], eth[13]]),
                    &eth[14..],
                    9,
                );
                self.q.push_back(reply);
            }
        }
    }

    fn join(quirks: Quirks) -> (Station<SimAp>, Result<(), WifiError>) {
        let mut st = Station::new(SimAp::new(quirks));
        let found = st.scan(&CHANNELS_24, 5);
        let bss = found.iter().find(|b| b.ssid == b"AeonNet").cloned().expect("scan must find the AP");
        let r = st.connect(&bss, "BedrockOS-pass-1234", [0x31; 32]);
        (st, r)
    }

    #[test]
    fn chip_names() {
        assert_eq!(chip_name(0x10EC, 0xB520), Some("Realtek RTL8852BE Wi-Fi 6"));
        assert_eq!(chip_name(0x8086, 0x2725), Some("Intel Wi-Fi 6E AX210"));
        assert_eq!(chip_name(0x10EC, 0x8168), None, "the wired Realtek chip is not a Wi-Fi chip");
    }

    #[test]
    fn scan_finds_the_ap_on_its_channel_only() {
        let mut st = Station::new(SimAp::new(Quirks::default()));
        let found = st.scan(&CHANNELS_24, 5);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].channel, found[0].ssid_str().as_str()), (6, "AeonNet"));
        assert!(found[0].supports_wpa2_psk_ccmp());
        assert!(st.scan(&[1, 2, 3], 5).is_empty());
    }

    #[test]
    fn connects_and_carries_encrypted_traffic_both_ways() {
        let (mut st, r) = join(Quirks::default());
        assert_eq!(r, Ok(()));
        assert_eq!(st.state(), LinkState::Connected);
        assert!(NetDev::link_up(&st));
        let mut eth = Vec::new();
        eth.extend_from_slice(&[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
        eth.extend_from_slice(&STA_MAC);
        eth.extend_from_slice(&[0x08, 0x00]);
        eth.extend_from_slice(b"ping through wpa2");
        for round in 0..5u8 {
            let mut p = eth.clone();
            p.push(round);
            NetDev::send(&mut st, &p).unwrap();
            let mut out = [0u8; 200];
            let n = NetDev::receive(&mut st, &mut out).unwrap();
            assert_eq!(&out[14..n], &p[14..]);
            assert_eq!(&out[..6], &STA_MAC);
        }
        assert!(st.radio().seen_plain_ip, "the AP decrypted what the station sent");
        let mut out = [0u8; 10];
        assert_eq!(NetDev::receive(&mut st, &mut out), Err(NetError::RxEmpty));
    }

    #[test]
    fn replayed_frames_from_the_air_are_dropped() {
        let (mut st, _) = join(Quirks::default());
        let mut eth = alloc::vec![0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
        eth.extend_from_slice(&STA_MAC);
        eth.extend_from_slice(&[0x08, 0x00, 1, 2, 3]);
        NetDev::send(&mut st, &eth).unwrap();
        let copy = st.radio().q.front().unwrap().clone();
        let mut out = [0u8; 100];
        assert!(NetDev::receive(&mut st, &mut out).is_ok());
        st.radio().q.push_back(copy);
        assert_eq!(NetDev::receive(&mut st, &mut out), Err(NetError::RxEmpty));
    }

    #[test]
    fn group_frames_need_the_right_key_id_and_a_fresh_packet_number() {
        let (mut st, r) = join(Quirks::default());
        assert_eq!(r, Ok(()));
        let bcast = [0xFF; 6];
        let src = [0x02, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC];
        let mk = |key: &[u8; 16], pn: u64, id: u8| {
            dot11::data_from_ap_protected(key, pn, id, &AP_MAC, &bcast, &src, 0x0806, b"who-has", 4)
        };
        let mut out = [0u8; 100];
        // The AP gave us group key 0x99.. as key id 1.
        st.radio().q.push_back(mk(&[0x99; 16], 5, 1));
        assert!(matches!(st.recv_eth(&mut out), Ok(Some(_))));
        // The same frame again is a replay; a frame under another key id or with the wrong key is dropped.
        st.radio().q.push_back(mk(&[0x99; 16], 5, 1));
        st.radio().q.push_back(mk(&[0x99; 16], 6, 2));
        st.radio().q.push_back(mk(&[0x11; 16], 7, 1));
        for _ in 0..3 {
            assert_eq!(st.recv_eth(&mut out), Ok(None));
        }
        st.radio().q.push_back(mk(&[0x99; 16], 8, 1));
        assert!(matches!(st.recv_eth(&mut out), Ok(Some(_))));
    }

    #[test]
    fn failures_are_reported_precisely() {
        assert_eq!(join(Quirks { reject_auth: true, ..Default::default() }).1, Err(WifiError::AuthRejected(13)));
        assert_eq!(join(Quirks { reject_assoc: true, ..Default::default() }).1, Err(WifiError::AssocRejected(17)));
        assert_eq!(join(Quirks { deauth_after_msg2: true, ..Default::default() }).1, Err(WifiError::Deauthenticated));
        // The AP never answers an auth request.
        let mut st = Station::new(SimAp::new(Quirks { silent: true, ..Default::default() }));
        let bss = dot11::parse_bss(&dot11::mgmt_from_ap(
            dot11::FC_BEACON,
            &AP_MAC,
            &[0xFF; 6],
            &SimAp::new(Quirks::default()).beacon_body(),
            1,
        ))
        .unwrap();
        assert_eq!(st.connect(&bss, "BedrockOS-pass-1234", [1; 32]), Err(WifiError::Timeout));
        assert_eq!(st.send_eth(&[0; 20]), Err(WifiError::NotConnected));
    }

    #[test]
    fn a_wrong_password_never_connects() {
        let (st, r) = join(Quirks { wrong_psk: true, ..Default::default() });
        assert_eq!(r, Err(WifiError::Timeout));
        assert_eq!(st.state(), LinkState::Idle);
    }

    #[test]
    fn bad_passphrase_and_unsupported_networks_are_refused_before_any_frame() {
        let mut st = Station::new(SimAp::new(Quirks::default()));
        let mut bss = st.scan(&[6], 5).remove(0);
        assert_eq!(st.connect(&bss, "short", [0; 32]), Err(WifiError::BadPassphrase));
        bss.rsn = None;
        assert_eq!(st.connect(&bss, "BedrockOS-pass-1234", [0; 32]), Err(WifiError::UnsupportedSecurity));
    }
}
