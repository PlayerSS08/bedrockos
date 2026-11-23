//! WPA2-Personal (IEEE 802.11i, CCMP, PSK): passphrase to PMK, the 4-way EAPOL handshake as a supplicant,
//! PTK derivation, MIC and GTK unwrap. Pure and host-testable; the radio is not involved here.
//!
//! Verified on the host against RFC and IEEE vectors (`sha1`, `aes`) and against an independent Python
//! implementation for the PTK and the message 2 bytes. Not verified against a real access point.

use alloc::vec::Vec;

use crate::aes;
use crate::sha1;

pub const PMK_LEN: usize = 32;
pub const NONCE_LEN: usize = 32;
const KEY_FIXED: usize = 99; // EAPOL header (4) + key descriptor body up to and including the data length

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WpaError {
    BadPassphrase,
    BadSsid,
    Truncated,
    NotEapolKey,
    UnsupportedDescriptor,
    BadMic,
    ReplayedMessage,
    UnexpectedMessage,
    NonceChanged,
    BadKeyData,
    RsnMismatch,
}

/// The passphrase must be 8..=63 printable ASCII characters and the SSID 1..=32 bytes (IEEE 802.11i H.4).
pub fn derive_pmk(passphrase: &str, ssid: &[u8]) -> Result<[u8; PMK_LEN], WpaError> {
    let p = passphrase.as_bytes();
    if p.len() < 8 || p.len() > 63 || p.iter().any(|&b| !(32..=126).contains(&b)) {
        return Err(WpaError::BadPassphrase);
    }
    if ssid.is_empty() || ssid.len() > 32 {
        return Err(WpaError::BadSsid);
    }
    let mut pmk = [0u8; PMK_LEN];
    sha1::pbkdf2(p, ssid, 4096, &mut pmk);
    Ok(pmk)
}

/// IEEE 802.11 PRF-X using HMAC-SHA1.
pub fn prf(key: &[u8], label: &str, data: &[u8], out: &mut [u8]) {
    let mut counter = 0u8;
    for chunk in out.chunks_mut(sha1::DIGEST_LEN) {
        let h = sha1::hmac_parts(key, &[label.as_bytes(), &[0], data, &[counter]]);
        chunk.copy_from_slice(&h[..chunk.len()]);
        counter = counter.wrapping_add(1);
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

pub fn derive_ptk(pmk: &[u8; PMK_LEN], aa: &[u8; 6], spa: &[u8; 6], anonce: &[u8; 32], snonce: &[u8; 32]) -> Ptk {
    let mut data = [0u8; 76];
    let (lo_a, hi_a) = if aa < spa { (aa, spa) } else { (spa, aa) };
    let (lo_n, hi_n) = if anonce < snonce { (anonce, snonce) } else { (snonce, anonce) };
    data[..6].copy_from_slice(lo_a);
    data[6..12].copy_from_slice(hi_a);
    data[12..44].copy_from_slice(lo_n);
    data[44..76].copy_from_slice(hi_n);
    let mut out = [0u8; 48];
    prf(pmk, "Pairwise key expansion", &data, &mut out);
    Ptk {
        kck: out[..16].try_into().unwrap(),
        kek: out[16..32].try_into().unwrap(),
        tk: out[32..48].try_into().unwrap(),
    }
}

// Key Information bits.
pub const KI_VERSION_MASK: u16 = 0x0007;
pub const KI_PAIRWISE: u16 = 1 << 3;
pub const KI_INSTALL: u16 = 1 << 6;
pub const KI_ACK: u16 = 1 << 7;
pub const KI_MIC: u16 = 1 << 8;
pub const KI_SECURE: u16 = 1 << 9;
pub const KI_ENCRYPTED: u16 = 1 << 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EapolKey {
    pub info: u16,
    pub key_len: u16,
    pub replay: u64,
    pub nonce: [u8; 32],
    pub iv: [u8; 16],
    pub rsc: [u8; 8],
    pub mic: [u8; 16],
    pub data: Vec<u8>,
}

impl EapolKey {
    /// `frame` starts at the EAPOL header (version, type, length).
    pub fn parse(frame: &[u8]) -> Result<Self, WpaError> {
        if frame.len() < KEY_FIXED {
            return Err(WpaError::Truncated);
        }
        if frame[1] != 3 {
            return Err(WpaError::NotEapolKey);
        }
        let body_len = u16::from_be_bytes([frame[2], frame[3]]) as usize;
        if 4 + body_len > frame.len() || body_len < KEY_FIXED - 4 {
            return Err(WpaError::Truncated);
        }
        if frame[4] != 2 {
            return Err(WpaError::UnsupportedDescriptor);
        }
        let info = u16::from_be_bytes([frame[5], frame[6]]);
        if info & KI_VERSION_MASK != 2 {
            // Only HMAC-SHA1 + AES (WPA2-CCMP) is implemented.
            return Err(WpaError::UnsupportedDescriptor);
        }
        let dlen = u16::from_be_bytes([frame[97], frame[98]]) as usize;
        if KEY_FIXED + dlen > 4 + body_len {
            return Err(WpaError::Truncated);
        }
        Ok(Self {
            info,
            key_len: u16::from_be_bytes([frame[7], frame[8]]),
            replay: u64::from_be_bytes(frame[9..17].try_into().unwrap()),
            nonce: frame[17..49].try_into().unwrap(),
            iv: frame[49..65].try_into().unwrap(),
            rsc: frame[65..73].try_into().unwrap(),
            mic: frame[81..97].try_into().unwrap(),
            data: frame[KEY_FIXED..KEY_FIXED + dlen].to_vec(),
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let body = KEY_FIXED - 4 + self.data.len();
        let mut f = Vec::with_capacity(4 + body);
        f.extend_from_slice(&[1, 3]);
        f.extend_from_slice(&(body as u16).to_be_bytes());
        f.push(2);
        f.extend_from_slice(&self.info.to_be_bytes());
        f.extend_from_slice(&self.key_len.to_be_bytes());
        f.extend_from_slice(&self.replay.to_be_bytes());
        f.extend_from_slice(&self.nonce);
        f.extend_from_slice(&self.iv);
        f.extend_from_slice(&self.rsc);
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&self.mic);
        f.extend_from_slice(&(self.data.len() as u16).to_be_bytes());
        f.extend_from_slice(&self.data);
        f
    }
}

/// MIC of a built EAPOL-Key frame: HMAC-SHA1 over the frame with the MIC field zeroed, first 16 bytes.
pub fn compute_mic(kck: &[u8; 16], frame: &[u8]) -> [u8; 16] {
    let mut copy = frame.to_vec();
    copy[81..97].fill(0);
    sha1::hmac(kck, &copy)[..16].try_into().unwrap()
}

/// Builds the frame, fills in the MIC and returns the finished bytes.
pub fn sign(kck: &[u8; 16], key: &EapolKey) -> Vec<u8> {
    let mut f = key.build();
    let mic = compute_mic(kck, &f);
    f[81..97].copy_from_slice(&mic);
    f
}

fn mic_ok(kck: &[u8; 16], frame: &[u8], mic: &[u8; 16]) -> bool {
    let want = compute_mic(kck, frame);
    let mut d = 0u8;
    for i in 0..16 {
        d |= want[i] ^ mic[i];
    }
    d == 0
}

/// RSN element for WPA2-Personal: group CCMP, pairwise CCMP, AKM PSK. This is what we offer in message 2
/// and in the association request.
pub const RSN_WPA2_PSK_CCMP: [u8; 22] = [
    0x30, 0x14, 0x01, 0x00, 0x00, 0x0F, 0xAC, 0x04, 0x01, 0x00, 0x00, 0x0F, 0xAC, 0x04, 0x01, 0x00, 0x00, 0x0F,
    0xAC, 0x02, 0x00, 0x00,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    WaitMsg1,
    WaitMsg3,
    Done,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Gtk {
    pub key_id: u8,
    pub key: [u8; 16],
}

/// What one message 1 would give us. Message 1 carries no MIC, so anyone in radio range can send one: it must
/// never overwrite what the real AP established. Each message 1 therefore gets its own candidate, and
/// message 3 is matched to the candidate with the same ANonce and accepted only if its MIC verifies.
struct Candidate {
    anonce: [u8; 32],
    ptk: Ptk,
    /// Replay counter of the message 1 that created it; message 3 must carry a larger one.
    replay: u64,
}

/// Candidates kept at once. A flood of forged message 1s can push the oldest out; the real AP retransmits
/// message 1, which brings its candidate back.
const MAX_CANDIDATES: usize = 8;

pub struct Supplicant {
    pmk: [u8; PMK_LEN],
    aa: [u8; 6],
    spa: [u8; 6],
    snonce: [u8; 32],
    state: State,
    cands: Vec<Candidate>,
    ptk: Option<Ptk>,
    gtk: Option<Gtk>,
    gtk_rsc: u64,
    /// The RSN element the AP advertised in its beacon or probe response, if known. Message 3 must carry the
    /// same one, otherwise someone downgraded the negotiation.
    ap_rsn: Option<Vec<u8>>,
}

impl Supplicant {
    /// `snonce` must come from the kernel entropy pool; it is the caller's job to make it unpredictable.
    pub fn new(pmk: [u8; PMK_LEN], aa: [u8; 6], spa: [u8; 6], snonce: [u8; 32], ap_rsn: Option<Vec<u8>>) -> Self {
        Self { pmk, aa, spa, snonce, state: State::WaitMsg1, cands: Vec::new(), ptk: None, gtk: None, gtk_rsc: 0, ap_rsn }
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn ptk(&self) -> Option<&Ptk> {
        self.ptk.as_ref()
    }
    pub fn gtk(&self) -> Option<&Gtk> {
        self.gtk.as_ref()
    }
    /// The group packet number the AP had reached when it sent message 3 (its Key RSC). Group frames with a
    /// packet number at or below it are replays from before we joined.
    pub fn gtk_rsc(&self) -> u64 {
        self.gtk_rsc
    }

    /// Feeds one EAPOL frame from the AP. Returns the reply frame to send, if any.
    pub fn handle(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, WpaError> {
        let m = EapolKey::parse(frame)?;
        if m.info & KI_PAIRWISE == 0 || m.info & KI_ACK == 0 {
            return Err(WpaError::UnexpectedMessage);
        }
        if m.info & KI_MIC == 0 {
            self.msg1(&m)
        } else {
            self.msg3(&m, frame)
        }
    }

    fn msg1(&mut self, m: &EapolKey) -> Result<Option<Vec<u8>>, WpaError> {
        if self.state == State::Done {
            return Err(WpaError::UnexpectedMessage);
        }
        let ptk = derive_ptk(&self.pmk, &self.aa, &self.spa, &m.nonce, &self.snonce);
        let reply = EapolKey {
            info: 2 | KI_PAIRWISE | KI_MIC,
            key_len: 0,
            replay: m.replay,
            nonce: self.snonce,
            iv: [0; 16],
            rsc: [0; 8],
            mic: [0; 16],
            data: RSN_WPA2_PSK_CCMP.to_vec(),
        };
        let out = sign(&ptk.kck, &reply);
        // Message 1 is unauthenticated, so a copy of the real one can be re-sent by anyone with any replay
        // counter. A candidate that already exists therefore keeps the lowest counter it has seen: replays
        // can never raise the floor that the real message 3 has to clear. A new ANonce adds a candidate.
        if let Some(c) = self.cands.iter_mut().find(|c| c.anonce == m.nonce) {
            c.replay = c.replay.min(m.replay);
        } else {
            if self.cands.len() == MAX_CANDIDATES {
                self.cands.remove(0);
            }
            self.cands.push(Candidate { anonce: m.nonce, ptk, replay: m.replay });
        }
        self.state = State::WaitMsg3;
        Ok(Some(out))
    }

    fn msg3(&mut self, m: &EapolKey, frame: &[u8]) -> Result<Option<Vec<u8>>, WpaError> {
        if self.state != State::WaitMsg3 {
            return Err(WpaError::UnexpectedMessage);
        }
        let cand = self.cands.iter().find(|c| c.anonce == m.nonce).ok_or(WpaError::NonceChanged)?;
        let (ptk, msg1_replay) = (cand.ptk.clone(), cand.replay);
        if !mic_ok(&ptk.kck, &frame[..KEY_FIXED + m.data.len()], &m.mic) {
            return Err(WpaError::BadMic);
        }
        // From here on the frame is authenticated: the AP that knows the PSK sent it.
        if m.replay <= msg1_replay {
            return Err(WpaError::ReplayedMessage);
        }
        if m.info & KI_INSTALL == 0 || m.info & KI_ENCRYPTED == 0 {
            return Err(WpaError::UnexpectedMessage);
        }
        let plain = aes::key_unwrap(&ptk.kek, &m.data).ok_or(WpaError::BadKeyData)?;
        let (rsn, gtk) = parse_key_data(&plain)?;
        if let (Some(want), Some(got)) = (&self.ap_rsn, &rsn) {
            if want != got {
                return Err(WpaError::RsnMismatch);
            }
        }
        self.gtk = Some(gtk.ok_or(WpaError::BadKeyData)?);
        let mut rsc = [0u8; 8];
        rsc[..6].copy_from_slice(&m.rsc[..6]);
        self.gtk_rsc = u64::from_le_bytes(rsc);
        let reply = EapolKey {
            info: 2 | KI_PAIRWISE | KI_MIC | KI_SECURE,
            key_len: 0,
            replay: m.replay,
            nonce: [0; 32],
            iv: [0; 16],
            rsc: [0; 8],
            mic: [0; 16],
            data: Vec::new(),
        };
        let out = sign(&ptk.kck, &reply);
        self.ptk = Some(ptk);
        self.cands.clear();
        self.state = State::Done;
        Ok(Some(out))
    }
}

/// Walks the decrypted key data: returns the RSN element (whole, with tag and length) and the GTK KDE.
fn parse_key_data(d: &[u8]) -> Result<(Option<Vec<u8>>, Option<Gtk>), WpaError> {
    let mut rsn = None;
    let mut gtk = None;
    let mut i = 0;
    while i + 2 <= d.len() {
        let (tag, len) = (d[i], d[i + 1] as usize);
        if tag == 0xDD && len == 0 {
            break; // padding after the last element
        }
        if i + 2 + len > d.len() {
            return Err(WpaError::BadKeyData);
        }
        let body = &d[i + 2..i + 2 + len];
        if tag == 0x30 {
            rsn = Some(d[i..i + 2 + len].to_vec());
        } else if tag == 0xDD && len >= 22 && body[..4] == [0x00, 0x0F, 0xAC, 0x01] {
            gtk = Some(Gtk { key_id: body[4] & 3, key: body[6..22].try_into().unwrap() });
        }
        i += 2 + len;
    }
    Ok((rsn, gtk))
}

/// Builds the key data of message 3 (RSN element plus GTK KDE, padded and wrapped). Used by the simulated
/// access point in the tests and by nothing else; it lives here so the layout is defined in one place.
pub fn build_msg3_key_data(kek: &[u8; 16], rsn: &[u8], gtk_id: u8, gtk: &[u8; 16]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(rsn);
    d.extend_from_slice(&[0xDD, 22, 0x00, 0x0F, 0xAC, 0x01, gtk_id & 3, 0]);
    d.extend_from_slice(gtk);
    d.push(0xDD);
    while d.len() % 8 != 0 || d.len() < 16 {
        d.push(0);
    }
    aes::key_wrap(kek, &d).expect("length is a multiple of 8")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap()).collect()
    }

    const AA: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    const SPA: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];

    fn nonce(mul: usize, add: usize) -> [u8; 32] {
        let mut n = [0u8; 32];
        for (i, b) in n.iter_mut().enumerate() {
            *b = ((i * mul + add) & 255) as u8;
        }
        n
    }

    fn pmk() -> [u8; 32] {
        derive_pmk("Aeon-pass-1234", b"AeonNet").unwrap()
    }

    #[test]
    fn pmk_and_ptk_match_the_python_reference() {
        assert_eq!(pmk().to_vec(), unhex("3459978fb1ed86abb3498609d3d09d94abd5674920e8130afff23254aab5f2f3"));
        let ptk = derive_ptk(&pmk(), &AA, &SPA, &nonce(3, 1), &nonce(7, 5));
        let want = unhex("c03d75dec292c502ceea235b08ca4a554d1ab1d69780b91fbfc4ab04577fd129ad5315a74917f832c628090852b6c05b");
        assert_eq!(ptk.kck.to_vec(), want[..16]);
        assert_eq!(ptk.kek.to_vec(), want[16..32]);
        assert_eq!(ptk.tk.to_vec(), want[32..48]);
        // The roles of the two sides do not change the result (min/max ordering).
        let swapped = derive_ptk(&pmk(), &SPA, &AA, &nonce(7, 5), &nonce(3, 1));
        assert!(swapped == ptk);
    }

    #[test]
    fn passphrase_and_ssid_limits() {
        assert_eq!(derive_pmk("short", b"x"), Err(WpaError::BadPassphrase));
        assert_eq!(derive_pmk("tab\there-12", b"x"), Err(WpaError::BadPassphrase));
        assert_eq!(derive_pmk("longenough", b""), Err(WpaError::BadSsid));
        assert_eq!(derive_pmk("longenough", &[b'a'; 33]), Err(WpaError::BadSsid));
    }

    /// A tiny access point: sends message 1 and message 3 and checks what the supplicant answers.
    struct Ap {
        anonce: [u8; 32],
        ptk: Option<Ptk>,
        rsn: Vec<u8>,
    }

    impl Ap {
        fn msg1(&self, replay: u64) -> Vec<u8> {
            EapolKey {
                info: 2 | KI_PAIRWISE | KI_ACK,
                key_len: 16,
                replay,
                nonce: self.anonce,
                iv: [0; 16],
                rsc: [0; 8],
                mic: [0; 16],
                data: Vec::new(),
            }
            .build()
        }

        fn msg3(&self, replay: u64, kek_override: Option<[u8; 16]>) -> Vec<u8> {
            self.msg3_rsc(replay, kek_override, [0; 8])
        }

        fn msg3_rsc(&self, replay: u64, kek_override: Option<[u8; 16]>, rsc: [u8; 8]) -> Vec<u8> {
            let ptk = self.ptk.as_ref().unwrap();
            let kd = build_msg3_key_data(&kek_override.unwrap_or(ptk.kek), &self.rsn, 1, &[0x42; 16]);
            let m = EapolKey {
                info: 2 | KI_PAIRWISE | KI_INSTALL | KI_ACK | KI_MIC | KI_SECURE | KI_ENCRYPTED,
                key_len: 16,
                replay,
                nonce: self.anonce,
                iv: [0; 16],
                rsc,
                mic: [0; 16],
                data: kd,
            };
            sign(&ptk.kck, &m)
        }
    }

    fn setup() -> (Supplicant, Ap) {
        let ap = Ap { anonce: nonce(3, 1), ptk: None, rsn: RSN_WPA2_PSK_CCMP.to_vec() };
        let sup = Supplicant::new(pmk(), AA, SPA, nonce(7, 5), Some(RSN_WPA2_PSK_CCMP.to_vec()));
        (sup, ap)
    }

    #[test]
    fn message_2_is_byte_identical_to_the_python_reference() {
        let (mut sup, ap) = setup();
        let m2 = sup.handle(&ap.msg1(1)).unwrap().unwrap();
        let want = unhex(
            "0103007502010a00000000000000000001050c131a21282f363d444b525960676e757c838a91989fa6adb4bbc2c9d0d7de0000000000000000000000000000000000000000000000000000000000000000bfd76f212416abd54df35fe007c27a2e001630140100000fac040100000fac040100000fac020000",
        );
        assert_eq!(m2, want);
        assert_eq!(sup.state(), State::WaitMsg3);
    }

    #[test]
    fn full_handshake_installs_ptk_and_gtk() {
        let (mut sup, mut ap) = setup();
        let m2 = sup.handle(&ap.msg1(1)).unwrap().unwrap();
        // The AP derives the same PTK from message 2 and checks the MIC the supplicant sent.
        let parsed = EapolKey::parse(&m2).unwrap();
        let ptk = derive_ptk(&pmk(), &AA, &SPA, &ap.anonce, &parsed.nonce);
        assert!(mic_ok(&ptk.kck, &m2, &parsed.mic));
        assert_eq!(parsed.data, RSN_WPA2_PSK_CCMP.to_vec());
        ap.ptk = Some(ptk.clone());
        let m4 = sup.handle(&ap.msg3(2, None)).unwrap().unwrap();
        let p4 = EapolKey::parse(&m4).unwrap();
        assert!(mic_ok(&ptk.kck, &m4, &p4.mic));
        assert_eq!(p4.replay, 2);
        assert_eq!(sup.state(), State::Done);
        assert!(sup.ptk().unwrap() == &ptk);
        let g = sup.gtk().unwrap();
        assert_eq!((g.key_id, g.key), (1, [0x42; 16]));
    }

    #[test]
    fn wrong_password_fails_the_mic_on_message_3() {
        let (mut sup, mut ap) = setup();
        let m2 = sup.handle(&ap.msg1(1)).unwrap().unwrap();
        let parsed = EapolKey::parse(&m2).unwrap();
        // An AP that knows a different passphrase derives a different PTK.
        let other = derive_pmk("Other-pass-9999", b"AeonNet").unwrap();
        ap.ptk = Some(derive_ptk(&other, &AA, &SPA, &ap.anonce, &parsed.nonce));
        assert_eq!(sup.handle(&ap.msg3(2, None)), Err(WpaError::BadMic));
        assert_ne!(sup.state(), State::Done);
    }

    #[test]
    fn replayed_tampered_and_downgraded_message_3_are_refused() {
        let build = |replay: u64, tweak: &dyn Fn(&mut Ap)| {
            let (mut sup, mut ap) = setup();
            let m2 = sup.handle(&ap.msg1(5)).unwrap().unwrap();
            let parsed = EapolKey::parse(&m2).unwrap();
            ap.ptk = Some(derive_ptk(&pmk(), &AA, &SPA, &ap.anonce, &parsed.nonce));
            tweak(&mut ap);
            let m3 = ap.msg3(replay, None);
            sup.handle(&m3)
        };
        assert_eq!(build(5, &|_| {}), Err(WpaError::ReplayedMessage));
        assert_eq!(build(4, &|_| {}), Err(WpaError::ReplayedMessage));
        // The AP now claims a different RSN element than its beacon had.
        assert_eq!(
            build(6, &|ap| {
                ap.rsn = RSN_WPA2_PSK_CCMP.to_vec();
                ap.rsn[19] = 0x01; // AKM becomes 00-0F-AC:01 (802.1X) instead of PSK
            }),
            Err(WpaError::RsnMismatch)
        );
        // A good one still works with the same setup.
        assert!(build(6, &|_| {}).is_ok());
    }

    #[test]
    fn message_3_with_a_changed_anonce_and_a_flipped_bit_are_refused() {
        let (mut sup, mut ap) = setup();
        let m2 = sup.handle(&ap.msg1(1)).unwrap().unwrap();
        let parsed = EapolKey::parse(&m2).unwrap();
        ap.ptk = Some(derive_ptk(&pmk(), &AA, &SPA, &ap.anonce, &parsed.nonce));
        let mut m3 = ap.msg3(2, None);
        let last = m3.len() - 1;
        m3[last] ^= 1; // corrupt the wrapped key data: the MIC covers it
        assert_eq!(sup.handle(&m3), Err(WpaError::BadMic));
        ap.anonce = nonce(11, 2);
        // Valid MIC but a different ANonce than message 1 carried: refused as a changed nonce.
        assert_eq!(sup.handle(&ap.msg3(3, None)), Err(WpaError::NonceChanged));
    }

    #[test]
    fn a_forged_message_1_cannot_break_the_real_handshake() {
        let (mut sup, mut ap) = setup();
        let m2 = sup.handle(&ap.msg1(1)).unwrap().unwrap();
        let parsed = EapolKey::parse(&m2).unwrap();
        ap.ptk = Some(derive_ptk(&pmk(), &AA, &SPA, &ap.anonce, &parsed.nonce));
        // An attacker, with no key, injects message 1s with other ANonces and huge replay counters.
        for i in 0..20u64 {
            let forged = Ap { anonce: nonce(13, i as usize), ptk: None, rsn: Vec::new() };
            assert!(sup.handle(&forged.msg1(1_000_000 + i)).unwrap().is_some());
        }
        // The real AP retransmits message 1 (its candidate may have been pushed out), then sends message 3.
        sup.handle(&ap.msg1(1)).unwrap();
        let m4 = sup.handle(&ap.msg3_rsc(2, None, [9, 0, 0, 0, 0, 0, 0, 0])).unwrap();
        assert!(m4.is_some());
        assert_eq!(sup.state(), State::Done);
        assert_eq!(sup.gtk_rsc(), 9);
    }

    #[test]
    fn a_replayed_copy_of_the_real_message_1_cannot_raise_the_replay_floor() {
        let (mut sup, mut ap) = setup();
        let m2 = sup.handle(&ap.msg1(1)).unwrap().unwrap();
        let parsed = EapolKey::parse(&m2).unwrap();
        ap.ptk = Some(derive_ptk(&pmk(), &AA, &SPA, &ap.anonce, &parsed.nonce));
        // The attacker captured message 1 and sends it again with a huge replay counter.
        let again = sup.handle(&ap.msg1(u64::MAX - 1)).unwrap().unwrap();
        assert_eq!(EapolKey::parse(&again).unwrap().replay, u64::MAX - 1, "message 2 echoes what message 1 said");
        // The real message 3 (replay 2) still clears the floor of 1.
        assert!(sup.handle(&ap.msg3(2, None)).unwrap().is_some());
        assert_eq!(sup.state(), State::Done);
    }

    #[test]
    fn a_bad_message_3_does_not_kill_the_attempt() {
        let (mut sup, mut ap) = setup();
        let m2 = sup.handle(&ap.msg1(1)).unwrap().unwrap();
        let parsed = EapolKey::parse(&m2).unwrap();
        ap.ptk = Some(derive_ptk(&pmk(), &AA, &SPA, &ap.anonce, &parsed.nonce));
        let mut junk = ap.msg3(2, None);
        let n = junk.len();
        junk[n - 2] ^= 0x55;
        assert_eq!(sup.handle(&junk), Err(WpaError::BadMic));
        assert_eq!(sup.state(), State::WaitMsg3);
        assert!(sup.handle(&ap.msg3(2, None)).unwrap().is_some());
        assert_eq!(sup.state(), State::Done);
    }

    #[test]
    fn garbage_never_panics() {
        let (mut sup, _) = setup();
        let mut seed = 0x1234_5678u32;
        for len in 0..260usize {
            let mut f = alloc::vec![0u8; len];
            for b in f.iter_mut() {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (seed >> 24) as u8;
            }
            if len > 4 {
                f[1] = 3;
                f[4] = 2;
            }
            let _ = sup.handle(&f);
        }
    }
}
