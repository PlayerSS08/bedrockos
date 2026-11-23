//! DNS: building an A-record query and reading the answer (RFC 1035), pure and host-testable.
//! Names in answers may be compressed with pointers; the parser follows them with a hard limit so a
//! malicious reply cannot loop it.

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsError {
    BadName,
    Truncated,
    NotAnAnswer,
    WrongId,
    /// The server says the name does not exist (RCODE 3).
    NoSuchName,
    /// Any other non-zero RCODE.
    ServerError(u8),
    NoAddress,
    BadPointer,
    /// The reply is for a different question than the one we asked.
    WrongQuestion,
}

/// Builds a recursive A query for `name` with transaction id `id`.
pub fn build_query(id: u16, name: &str) -> Result<Vec<u8>, DnsError> {
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty() || name.len() > 253 {
        return Err(DnsError::BadName);
    }
    let mut q = Vec::with_capacity(18 + name.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // standard query, recursion desired
    q.extend_from_slice(&1u16.to_be_bytes()); // one question
    q.extend_from_slice(&[0; 6]); // no answer, authority or additional records
    for label in name.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !ok {
            return Err(DnsError::BadName);
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&1u16.to_be_bytes()); // type A
    q.extend_from_slice(&1u16.to_be_bytes()); // class IN
    Ok(q)
}

/// Advances past a (possibly compressed) name starting at `pos`; returns the position after it.
fn skip_name(d: &[u8], mut pos: usize) -> Result<usize, DnsError> {
    loop {
        let len = *d.get(pos).ok_or(DnsError::Truncated)?;
        match len & 0xC0 {
            0x00 if len == 0 => return Ok(pos + 1),
            0x00 => pos += 1 + usize::from(len),
            0xC0 => {
                d.get(pos + 1).ok_or(DnsError::Truncated)?;
                return Ok(pos + 2); // a pointer ends the name
            }
            _ => return Err(DnsError::BadName),
        }
    }
}

/// Reads the question name (labels only: a question never needs compression) as lower-case dotted text.
fn read_question_name(d: &[u8], mut pos: usize) -> Result<(alloc::string::String, usize), DnsError> {
    let mut name = alloc::string::String::new();
    loop {
        let len = usize::from(*d.get(pos).ok_or(DnsError::Truncated)?);
        if len == 0 {
            return Ok((name, pos + 1));
        }
        if len > 63 {
            return Err(DnsError::BadName);
        }
        let label = d.get(pos + 1..pos + 1 + len).ok_or(DnsError::Truncated)?;
        if !name.is_empty() {
            name.push('.');
        }
        name.extend(label.iter().map(|&b| char::from(b.to_ascii_lowercase())));
        pos += 1 + len;
    }
}

/// Returns the IPv4 addresses in the answer section of a response to our query `id` for `name`. The reply must
/// repeat exactly that one question (type A, class IN): an answer to anything else is refused.
pub fn parse_response(id: u16, name: &str, d: &[u8]) -> Result<Vec<[u8; 4]>, DnsError> {
    if d.len() < 12 {
        return Err(DnsError::Truncated);
    }
    if u16::from_be_bytes([d[0], d[1]]) != id {
        return Err(DnsError::WrongId);
    }
    let flags = u16::from_be_bytes([d[2], d[3]]);
    if flags & 0x8000 == 0 {
        return Err(DnsError::NotAnAnswer);
    }
    match (flags & 0x000F) as u8 {
        0 => {}
        3 => return Err(DnsError::NoSuchName),
        other => return Err(DnsError::ServerError(other)),
    }
    let questions = u16::from_be_bytes([d[4], d[5]]);
    let answers = u16::from_be_bytes([d[6], d[7]]);
    if questions != 1 {
        return Err(DnsError::WrongQuestion);
    }
    let (qname, after) = read_question_name(d, 12)?;
    let q = d.get(after..after + 4).ok_or(DnsError::Truncated)?;
    let want = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    if qname != want || u16::from_be_bytes([q[0], q[1]]) != 1 || u16::from_be_bytes([q[2], q[3]]) != 1 {
        return Err(DnsError::WrongQuestion);
    }
    let mut pos = after + 4;
    let mut out = Vec::new();
    for _ in 0..answers {
        pos = skip_name(d, pos)?;
        let h = d.get(pos..pos + 10).ok_or(DnsError::Truncated)?;
        let (rtype, class, rdlen) = (u16::from_be_bytes([h[0], h[1]]), u16::from_be_bytes([h[2], h[3]]), usize::from(u16::from_be_bytes([h[8], h[9]])));
        pos += 10;
        let rdata = d.get(pos..pos + rdlen).ok_or(DnsError::Truncated)?;
        if rtype == 1 && class == 1 && rdlen == 4 {
            out.push([rdata[0], rdata[1], rdata[2], rdata[3]]);
        }
        pos += rdlen;
    }
    if out.is_empty() { Err(DnsError::NoAddress) } else { Ok(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_layout() {
        let q = build_query(0xBEEF, "archive.ubuntu.com").unwrap();
        assert_eq!(&q[..12], &[0xBE, 0xEF, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&q[12..20], b"\x07archive");
        assert_eq!(&q[q.len() - 5..], &[0, 0, 1, 0, 1]);
        assert_eq!(build_query(1, "example.com.").unwrap(), build_query(1, "example.com").unwrap());
    }

    #[test]
    fn bad_names_are_refused() {
        for n in ["", ".", "a..b", "-.", &"a".repeat(64), "exa mple.com", "ex\u{e9}.com", &format!("{}.com", "a.".repeat(130))] {
            assert_eq!(build_query(1, n), Err(DnsError::BadName), "{n}");
        }
        assert!(build_query(1, &"a".repeat(63)).is_ok());
    }

    /// What a real resolver sends: the question echoed, a CNAME and two A records, names compressed.
    fn reply(id: u16, rcode: u16) -> Vec<u8> {
        let mut r = build_query(id, "www.example.com").unwrap();
        r[2] = 0x81; // response, recursion desired
        r[3] = 0x80 | rcode as u8; // recursion available
        r[6..8].copy_from_slice(&3u16.to_be_bytes());
        // CNAME www.example.com -> edge.example.net (name pointer to offset 12)
        r.extend_from_slice(&[0xC0, 12, 0, 5, 0, 1, 0, 0, 1, 0x2C, 0, 7, 4, b'e', b'd', b'g', b'e', 0xC0, 20]);
        r.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34]);
        r.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 35]);
        r
    }

    #[test]
    fn answers_with_cname_and_compression_are_read() {
        assert_eq!(parse_response(7, "www.example.com", &reply(7, 0)).unwrap(), alloc::vec![[93, 184, 216, 34], [93, 184, 216, 35]]);
    }

    #[test]
    fn an_answer_to_a_different_question_is_refused() {
        // Right id, valid answer, but for another name: a spoofer who guessed the id still cannot redirect us.
        assert_eq!(parse_response(7, "bank.example.org", &reply(7, 0)), Err(DnsError::WrongQuestion));
        assert_eq!(parse_response(7, "WWW.Example.COM.", &reply(7, 0)).unwrap().len(), 2, "case and trailing dot do not matter");
        let mut two_questions = reply(7, 0);
        two_questions[5] = 2;
        assert_eq!(parse_response(7, "www.example.com", &two_questions), Err(DnsError::WrongQuestion));
        let mut wrong_type = reply(7, 0);
        let qtype_at = 12 + 1 + 3 + 1 + 7 + 1 + 3 + 1; // after www.example.com and its terminator
        wrong_type[qtype_at + 1] = 28; // AAAA
        assert_eq!(parse_response(7, "www.example.com", &wrong_type), Err(DnsError::WrongQuestion));
    }

    #[test]
    fn errors_are_told_apart() {
        assert_eq!(parse_response(8, "www.example.com", &reply(7, 0)), Err(DnsError::WrongId));
        assert_eq!(parse_response(7, "www.example.com", &reply(7, 3)), Err(DnsError::NoSuchName));
        assert_eq!(parse_response(7, "www.example.com", &reply(7, 2)), Err(DnsError::ServerError(2)));
        let mut q = build_query(7, "a.b").unwrap();
        assert_eq!(parse_response(7, "a.b", &q), Err(DnsError::NotAnAnswer));
        q[2] = 0x81;
        q[3] = 0x80;
        assert_eq!(parse_response(7, "a.b", &q), Err(DnsError::NoAddress));
    }

    #[test]
    fn truncated_and_hostile_replies_never_panic() {
        let good = reply(7, 0);
        for n in 0..good.len() {
            let _ = parse_response(7, "www.example.com", &good[..n]);
        }
        // A name that is a pointer to itself, and a reserved label type.
        let mut evil = reply(7, 0);
        let at = evil.len() - 16 - 2 - 12 - 2;
        evil[at] = 0x80;
        let _ = parse_response(7, "www.example.com", &evil);
        let mut seed = 5u32;
        for len in 0..300 {
            let f: Vec<u8> = (0..len).map(|_| { seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345); (seed >> 16) as u8 }).collect();
            let _ = parse_response(7, "www.example.com", &f);
        }
    }
}
