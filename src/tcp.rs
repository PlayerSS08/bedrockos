//! A TCP client connection as a pure state machine: no packets, clocks or devices in here. The caller feeds it
//! parsed segments and the current time (microseconds) and gets back the segments to transmit, so every rule
//! (sequence numbers, windows, retransmission, FIN/RST) is unit-tested on the host.
//!
//! What it does: active open with an MSS option, sliding send window limited by the peer's window and a small
//! congestion cap, cumulative ACKs, go-back-N retransmission with exponential backoff, in-order receive with
//! duplicate ACKs for gaps, graceful close from both sides, RST handling.
//! What it does not do: window scaling, SACK, timestamps, congestion control beyond a fixed cap, zero-window
//! probes, keep-alives, TIME-WAIT. Enough for HTTP downloads; not a general-purpose TCP.

use alloc::vec::Vec;

pub const FIN: u16 = 0x01;
pub const SYN: u16 = 0x02;
pub const RST: u16 = 0x04;
pub const PSH: u16 = 0x08;
pub const ACK: u16 = 0x10;

/// Largest segment we will send, and the MSS we offer (Ethernet MTU 1500 minus IP and TCP headers).
pub const MSS: usize = 1460;
/// Window we advertise. The data goes straight into `rx`, so the window never shrinks.
pub const RCV_WINDOW: u16 = 60_000;
/// At most this many segments in flight (a crude stand-in for congestion control).
const MAX_FLIGHT_SEGMENTS: usize = 8;
const INITIAL_RTO_US: u64 = 600_000;
const MAX_RTO_US: u64 = 4_000_000;
const MAX_RETRIES: u8 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    SynSent,
    Established,
    Closed,
}

/// A received segment, already parsed.
#[derive(Clone, Debug)]
pub struct Seg<'a> {
    pub seq: u32,
    pub ack: u32,
    pub flags: u16,
    pub window: u16,
    /// MSS option from a SYN, if present.
    pub mss: Option<u16>,
    pub payload: &'a [u8],
}

/// A segment to transmit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Out {
    pub seq: u32,
    pub ack: u32,
    pub flags: u16,
    pub window: u16,
    /// Put the MSS option on this segment (SYN only).
    pub mss_option: bool,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    None,
    Reset,
    TimedOut,
}

/// `a < b` in sequence space (modulo 2^32).
fn lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

fn le(a: u32, b: u32) -> bool {
    a == b || lt(a, b)
}

pub struct Tcp {
    state: State,
    failure: Failure,
    iss: u32,
    /// Oldest unacknowledged sequence number.
    snd_una: u32,
    /// Next sequence number to use for new data.
    snd_nxt: u32,
    snd_wnd: u32,
    peer_mss: usize,
    rcv_nxt: u32,
    /// Bytes queued for sending, starting at sequence `buf_seq`; the first `sent` of them are in flight.
    buf: Vec<u8>,
    buf_seq: u32,
    sent: usize,
    fin_queued: bool,
    fin_sent: bool,
    fin_acked: bool,
    peer_fin: bool,
    rx: Vec<u8>,
    rto: u64,
    last_tx: u64,
    retries: u8,
}

impl Tcp {
    /// Starts an active open. Returns the connection and the SYN to send.
    pub fn connect(iss: u32, now_us: u64) -> (Self, Vec<Out>) {
        let t = Self {
            state: State::SynSent,
            failure: Failure::None,
            iss,
            snd_una: iss,
            snd_nxt: iss.wrapping_add(1),
            snd_wnd: 0,
            peer_mss: 536,
            rcv_nxt: 0,
            buf: Vec::new(),
            buf_seq: iss.wrapping_add(1),
            sent: 0,
            fin_queued: false,
            fin_sent: false,
            fin_acked: false,
            peer_fin: false,
            rx: Vec::new(),
            rto: INITIAL_RTO_US,
            last_tx: now_us,
            retries: 0,
        };
        let syn = t.syn();
        (t, alloc::vec![syn])
    }

    fn syn(&self) -> Out {
        Out { seq: self.iss, ack: 0, flags: SYN, window: RCV_WINDOW, mss_option: true, payload: Vec::new() }
    }

    fn ack_only(&self) -> Out {
        Out { seq: self.snd_nxt, ack: self.rcv_nxt, flags: ACK, window: RCV_WINDOW, mss_option: false, payload: Vec::new() }
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn failure(&self) -> Failure {
        self.failure
    }
    pub fn is_established(&self) -> bool {
        self.state == State::Established
    }
    /// The peer sent FIN: no more data will arrive.
    pub fn peer_closed(&self) -> bool {
        self.peer_fin
    }
    /// Everything we sent, including the FIN if we closed, has been acknowledged.
    pub fn all_acked(&self) -> bool {
        self.snd_una == self.snd_nxt
    }
    pub fn rx_len(&self) -> usize {
        self.rx.len()
    }
    /// Takes the data received so far.
    pub fn take_rx(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.rx)
    }
    pub fn rx(&self) -> &[u8] {
        &self.rx
    }
    /// Bytes queued or in flight that the peer has not acknowledged.
    pub fn unacked_bytes(&self) -> usize {
        self.buf.len()
    }

    /// Queues `data` for sending and returns what can go out now.
    pub fn send(&mut self, data: &[u8], now_us: u64) -> Vec<Out> {
        if self.state == State::Closed || self.fin_queued {
            return Vec::new();
        }
        self.buf.extend_from_slice(data);
        self.pump(now_us)
    }

    /// Queues our FIN after the data already queued.
    pub fn close(&mut self, now_us: u64) -> Vec<Out> {
        if self.state == State::Closed {
            return Vec::new();
        }
        self.fin_queued = true;
        self.pump(now_us)
    }

    /// Abandons the connection; the caller should send the returned RST.
    pub fn abort(&mut self) -> Vec<Out> {
        if self.state == State::Closed {
            return Vec::new();
        }
        self.state = State::Closed;
        alloc::vec![Out { seq: self.snd_nxt, ack: self.rcv_nxt, flags: RST | ACK, window: 0, mss_option: false, payload: Vec::new() }]
    }

    /// Sends new data while the peer's window and our flight cap allow, and the FIN after the last byte.
    fn pump(&mut self, now_us: u64) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state != State::Established {
            return out;
        }
        let flight_cap = (self.peer_mss * MAX_FLIGHT_SEGMENTS) as u32;
        let window = self.snd_wnd.min(flight_cap);
        loop {
            let in_flight = self.snd_nxt.wrapping_sub(self.snd_una).wrapping_sub(u32::from(self.fin_sent));
            if self.sent >= self.buf.len() || in_flight >= window {
                break;
            }
            let room = (window - in_flight) as usize;
            let n = (self.buf.len() - self.sent).min(self.peer_mss).min(room);
            if n == 0 {
                break;
            }
            let payload = self.buf[self.sent..self.sent + n].to_vec();
            out.push(Out { seq: self.snd_nxt, ack: self.rcv_nxt, flags: ACK | PSH, window: RCV_WINDOW, mss_option: false, payload });
            self.sent += n;
            self.snd_nxt = self.snd_nxt.wrapping_add(n as u32);
        }
        if self.fin_queued && !self.fin_sent && self.sent == self.buf.len() {
            out.push(Out { seq: self.snd_nxt, ack: self.rcv_nxt, flags: FIN | ACK, window: RCV_WINDOW, mss_option: false, payload: Vec::new() });
            self.fin_sent = true;
            self.snd_nxt = self.snd_nxt.wrapping_add(1);
        }
        if !out.is_empty() {
            self.last_tx = now_us;
        }
        out
    }

    /// Handles one received segment; returns the segments to send in reply.
    pub fn on_segment(&mut self, s: &Seg, now_us: u64) -> Vec<Out> {
        if self.state == State::Closed {
            return Vec::new();
        }
        if s.flags & RST != 0 {
            // In SynSent a reset is valid when it acknowledges our SYN; later when its sequence number is in order.
            let ok = match self.state {
                State::SynSent => s.flags & ACK != 0 && s.ack == self.iss.wrapping_add(1),
                _ => s.seq == self.rcv_nxt,
            };
            if ok {
                self.state = State::Closed;
                self.failure = Failure::Reset;
            }
            return Vec::new();
        }
        if self.state == State::SynSent {
            if s.flags & (SYN | ACK) == (SYN | ACK) && s.ack == self.iss.wrapping_add(1) {
                self.rcv_nxt = s.seq.wrapping_add(1);
                self.snd_una = s.ack;
                self.snd_wnd = u32::from(s.window);
                if let Some(m) = s.mss {
                    self.peer_mss = usize::from(m).clamp(64, MSS);
                }
                self.state = State::Established;
                self.retries = 0;
                self.rto = INITIAL_RTO_US;
                let mut out = alloc::vec![self.ack_only()];
                out.extend(self.pump(now_us));
                return out;
            }
            return Vec::new();
        }

        let mut reply = Vec::new();
        let mut need_ack = false;

        // --- acknowledgements ---------------------------------------------------------------------------
        if s.flags & ACK != 0 && lt(self.snd_una, s.ack) && le(s.ack, self.snd_nxt) {
            let mut acked = s.ack.wrapping_sub(self.snd_una) as usize;
            if self.fin_sent && s.ack == self.snd_nxt {
                self.fin_acked = true;
                acked -= 1;
            }
            let drop = acked.min(self.buf.len());
            self.buf.drain(..drop);
            self.sent -= drop.min(self.sent);
            self.buf_seq = self.buf_seq.wrapping_add(drop as u32);
            self.snd_una = s.ack;
            self.retries = 0;
            self.rto = INITIAL_RTO_US;
            self.last_tx = now_us;
        }
        if s.flags & ACK != 0 {
            self.snd_wnd = u32::from(s.window);
        }

        // --- data ---------------------------------------------------------------------------------------
        let len = s.payload.len() as u32;
        if len > 0 {
            if s.seq == self.rcv_nxt {
                self.rx.extend_from_slice(s.payload);
                self.rcv_nxt = self.rcv_nxt.wrapping_add(len);
            } else if lt(s.seq, self.rcv_nxt) && lt(self.rcv_nxt, s.seq.wrapping_add(len)) {
                // A retransmission that overlaps what we already have: keep only the new tail.
                let skip = self.rcv_nxt.wrapping_sub(s.seq) as usize;
                self.rx.extend_from_slice(&s.payload[skip..]);
                self.rcv_nxt = self.rcv_nxt.wrapping_add(len - skip as u32);
            }
            // Old duplicate or a gap: nothing to keep, but tell the peer where we are.
            need_ack = true;
        }

        // --- FIN ----------------------------------------------------------------------------------------
        if s.flags & FIN != 0 && s.seq.wrapping_add(len) == self.rcv_nxt && !self.peer_fin {
            self.peer_fin = true;
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            need_ack = true;
        } else if s.flags & FIN != 0 {
            need_ack = true;
        }

        if need_ack {
            reply.push(self.ack_only());
        }
        reply.extend(self.pump(now_us));
        if self.fin_acked && self.peer_fin {
            self.state = State::Closed;
        }
        reply
    }

    /// Call regularly. Retransmits when an acknowledgement is overdue; gives up after repeated failures.
    pub fn on_tick(&mut self, now_us: u64) -> Vec<Out> {
        if self.state == State::Closed || self.snd_una == self.snd_nxt {
            return Vec::new();
        }
        if now_us.saturating_sub(self.last_tx) < self.rto {
            return Vec::new();
        }
        self.retries += 1;
        if self.retries > MAX_RETRIES {
            self.state = State::Closed;
            self.failure = Failure::TimedOut;
            return Vec::new();
        }
        self.rto = (self.rto * 2).min(MAX_RTO_US);
        self.last_tx = now_us;
        if self.state == State::SynSent {
            return alloc::vec![self.syn()];
        }
        // Go back: everything unacknowledged is sent again.
        self.snd_nxt = self.snd_una;
        self.sent = 0;
        self.fin_sent = false;
        self.pump(now_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER_ISS: u32 = 5000;

    fn seg(seq: u32, ack: u32, flags: u16, window: u16, payload: &[u8]) -> Seg<'_> {
        Seg { seq, ack, flags, window, mss: None, payload }
    }

    fn open(iss: u32) -> Tcp {
        let (mut t, syn) = Tcp::connect(iss, 0);
        assert_eq!(syn.len(), 1);
        assert_eq!((syn[0].flags, syn[0].seq, syn[0].mss_option), (SYN, iss, true));
        let mut sa = seg(PEER_ISS, iss.wrapping_add(1), SYN | ACK, 20_000, &[]);
        sa.mss = Some(1460);
        let out = t.on_segment(&sa, 10);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].flags, out[0].seq, out[0].ack), (ACK, iss.wrapping_add(1), PEER_ISS + 1));
        assert!(t.is_established());
        t
    }

    #[test]
    fn handshake_and_a_wrong_ack_is_ignored() {
        let (mut t, _) = Tcp::connect(100, 0);
        assert!(t.on_segment(&seg(PEER_ISS, 999, SYN | ACK, 1000, &[]), 1).is_empty());
        assert_eq!(t.state(), State::SynSent);
        let t = open(100);
        assert!(t.all_acked());
    }

    #[test]
    fn a_request_is_sent_acknowledged_and_released() {
        let mut t = open(100);
        let out = t.send(b"GET / HTTP/1.1\r\n\r\n", 20);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].seq, out[0].payload.len()), (101, 18));
        assert!(!t.all_acked());
        t.on_segment(&seg(PEER_ISS + 1, 101 + 18, ACK, 20_000, &[]), 30);
        assert!(t.all_acked());
        assert_eq!(t.unacked_bytes(), 0);
    }

    #[test]
    fn in_order_data_accumulates_and_gaps_get_duplicate_acks() {
        let mut t = open(100);
        let r = t.on_segment(&seg(PEER_ISS + 1, 101, ACK, 1000, b"hello "), 20);
        assert_eq!((r[0].ack, r[0].flags), (PEER_ISS + 1 + 6, ACK));
        // A segment from the future is dropped and the same ACK repeated.
        let r = t.on_segment(&seg(PEER_ISS + 1 + 100, 101, ACK, 1000, b"late"), 21);
        assert_eq!(r[0].ack, PEER_ISS + 1 + 6);
        assert_eq!(t.rx(), b"hello ");
        // The missing piece arrives, then an overlapping retransmission only adds its new tail.
        t.on_segment(&seg(PEER_ISS + 1 + 6, 101, ACK, 1000, b"world"), 22);
        let r = t.on_segment(&seg(PEER_ISS + 1 + 3, 101, ACK, 1000, b"lo world!"), 23);
        assert_eq!(t.rx(), b"hello world!");
        assert_eq!(r[0].ack, PEER_ISS + 1 + 12);
        // A pure duplicate changes nothing.
        t.on_segment(&seg(PEER_ISS + 1, 101, ACK, 1000, b"hello "), 24);
        assert_eq!(t.take_rx(), b"hello world!");
        assert_eq!(t.rx_len(), 0);
    }

    #[test]
    fn large_sends_are_split_and_respect_the_peer_window() {
        let mut t = open(100);
        let data = alloc::vec![7u8; 20_000];
        // Window 20_000 but the flight cap is 8 segments of 1460.
        let out = t.send(&data, 20);
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|o| o.payload.len() == 1460));
        assert_eq!(out[1].seq, out[0].seq + 1460);
        // Acknowledging the first four opens room for four more.
        let r = t.on_segment(&seg(PEER_ISS + 1, 101 + 4 * 1460, ACK, 20_000, &[]), 30);
        assert_eq!(r.iter().map(|o| o.payload.len()).sum::<usize>(), 4 * 1460);
        // Acknowledging everything sent so far lets the sender finish the rest.
        let r = t.on_segment(&seg(PEER_ISS + 1, 101 + 12 * 1460, ACK, 20_000, &[]), 40);
        assert_eq!(r.iter().map(|o| o.payload.len()).sum::<usize>(), 20_000 - 12 * 1460);
        t.on_segment(&seg(PEER_ISS + 1, 101 + 20_000, ACK, 20_000, &[]), 50);
        assert!(t.all_acked());
        assert_eq!(t.unacked_bytes(), 0);
        // A small peer window limits the flight.
        let mut u = open(100);
        u.on_segment(&seg(PEER_ISS + 1, 101, ACK, 2000, &[]), 11);
        let out = u.send(&data, 20);
        assert_eq!(out.iter().map(|o| o.payload.len()).sum::<usize>(), 2000);
    }

    #[test]
    fn lost_segments_are_retransmitted_with_backoff_then_the_connection_fails() {
        let mut t = open(100);
        t.send(b"abc", 1_000);
        assert!(t.on_tick(1_000 + INITIAL_RTO_US - 1).is_empty());
        let r = t.on_tick(1_000 + INITIAL_RTO_US);
        assert_eq!((r.len(), r[0].seq, r[0].payload.as_slice()), (1, 101, &b"abc"[..]));
        // The timeout doubled.
        assert!(t.on_tick(1_000 + INITIAL_RTO_US + INITIAL_RTO_US).is_empty());
        let mut now = 1_000 + 2 * INITIAL_RTO_US + 2 * INITIAL_RTO_US;
        for _ in 0..20 {
            t.on_tick(now);
            now += 10_000_000;
        }
        assert_eq!((t.state(), t.failure()), (State::Closed, Failure::TimedOut));
    }

    #[test]
    fn a_lost_syn_is_resent() {
        let (mut t, _) = Tcp::connect(42, 0);
        let r = t.on_tick(INITIAL_RTO_US);
        assert_eq!((r[0].flags, r[0].seq), (SYN, 42));
    }

    #[test]
    fn graceful_close_in_both_directions() {
        let mut t = open(100);
        t.send(b"req", 20);
        t.on_segment(&seg(PEER_ISS + 1, 104, ACK, 20_000, &[]), 30);
        // The peer sends data and FIN together.
        let r = t.on_segment(&seg(PEER_ISS + 1, 104, ACK | FIN, 20_000, b"bye"), 40);
        assert_eq!(r[0].ack, PEER_ISS + 1 + 3 + 1);
        assert!(t.peer_closed());
        assert_eq!(t.rx(), b"bye");
        // We close: FIN goes out, its ACK ends the connection.
        let fin = t.close(50);
        assert_eq!((fin[0].flags, fin[0].seq), (FIN | ACK, 104));
        assert_eq!(t.state(), State::Established);
        t.on_segment(&seg(PEER_ISS + 1 + 4, 105, ACK, 20_000, &[]), 60);
        assert_eq!((t.state(), t.failure()), (State::Closed, Failure::None));
        assert!(t.send(b"x", 70).is_empty());
    }

    #[test]
    fn a_lost_fin_is_retransmitted_after_the_data() {
        let mut t = open(100);
        t.send(b"abc", 20);
        let fin = t.close(21);
        assert_eq!(fin.len(), 1); // FIN only (the data went out with the first call)
        let r = t.on_tick(21 + INITIAL_RTO_US);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].payload.as_slice(), r[1].flags), (&b"abc"[..], FIN | ACK));
    }

    #[test]
    fn reset_closes_and_only_a_valid_one_counts() {
        let mut t = open(100);
        t.on_segment(&seg(PEER_ISS + 999, 0, RST, 0, &[]), 20);
        assert!(t.is_established(), "an out-of-window reset is ignored");
        t.on_segment(&seg(PEER_ISS + 1, 0, RST, 0, &[]), 21);
        assert_eq!((t.state(), t.failure()), (State::Closed, Failure::Reset));
        let (mut s, _) = Tcp::connect(100, 0);
        s.on_segment(&seg(0, 101, RST | ACK, 0, &[]), 1);
        assert_eq!(s.failure(), Failure::Reset);
    }

    #[test]
    fn sequence_numbers_survive_wraparound() {
        let mut t = open(u32::MAX - 5);
        let out = t.send(&[1u8; 20], 20);
        assert_eq!(out[0].seq, u32::MAX - 4);
        t.on_segment(&seg(PEER_ISS + 1, (u32::MAX - 4).wrapping_add(20), ACK, 20_000, &[]), 30);
        assert!(t.all_acked());
        let mut p = open(100);
        let mut peer_seq = u32::MAX - 2;
        // Receive across the 2^32 boundary.
        p.rcv_nxt = peer_seq;
        p.on_segment(&seg(peer_seq, 101, ACK, 1000, b"12345"), 20);
        peer_seq = peer_seq.wrapping_add(5);
        assert_eq!(p.rcv_nxt, peer_seq);
        assert_eq!(p.rx(), b"12345");
    }

    #[test]
    fn abort_sends_a_reset() {
        let mut t = open(100);
        let r = t.abort();
        assert_eq!(r[0].flags, RST | ACK);
        assert_eq!(t.state(), State::Closed);
        assert!(t.abort().is_empty());
    }

    #[test]
    fn random_segments_never_panic_or_break_the_sequence_invariant() {
        let mut seed = 0xC0FFEEu32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed
        };
        for _ in 0..200 {
            let mut t = open(next());
            t.send(&alloc::vec![1u8; (next() % 6000) as usize], 20);
            for step in 0..60u64 {
                let flags = (next() & 0x1F) as u16;
                let len = (next() % 40) as usize;
                let payload = alloc::vec![9u8; len];
                let s = Seg { seq: next() % 3 + PEER_ISS, ack: 100 + next() % 8000, flags, window: (next() % 5000) as u16, mss: None, payload: &payload };
                t.on_segment(&s, 100 + step * 700_000);
                t.on_tick(100 + step * 700_000 + 350_000);
                assert!(le(t.snd_una, t.snd_nxt), "snd_una must never pass snd_nxt");
            }
        }
    }
}
