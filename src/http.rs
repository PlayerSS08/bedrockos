//! HTTP/1.1 client pieces, pure and host-testable: URL parsing, the GET request and an incremental response
//! parser (Content-Length, chunked transfer encoding, read-until-close, redirects, size limits).
//! There is no TLS in BedrockOS yet, so `https://` URLs parse but cannot be fetched.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpError {
    BadUrl,
    Malformed,
    HeadersTooLarge,
    TooLarge,
    /// The connection ended before the whole body arrived.
    Truncated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(u: &str) -> Result<Url, HttpError> {
    let (https, rest) = if let Some(r) = u.strip_prefix("http://") {
        (false, r)
    } else if let Some(r) = u.strip_prefix("https://") {
        (true, r)
    } else {
        return Err(HttpError::BadUrl);
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], alloc::format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() || authority.contains('@') || path.bytes().any(|b| b <= b' ' || b == 0x7F) {
        return Err(HttpError::BadUrl);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| HttpError::BadUrl)?),
        None => (authority, if https { 443 } else { 80 }),
    };
    if host.is_empty() || port == 0 {
        return Err(HttpError::BadUrl);
    }
    Ok(Url { https, host: host.to_string(), port, path })
}

pub fn request(url: &Url) -> Vec<u8> {
    let host = if url.port == 80 { url.host.clone() } else { alloc::format!("{}:{}", url.host, url.port) };
    alloc::format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: aeon/{}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        url.path,
        host,
        crate::VERSION
    )
    .into_bytes()
}

/// Resolves a `Location` header against the URL it came from.
pub fn resolve_location(base: &Url, location: &str) -> Result<Url, HttpError> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return parse_url(location);
    }
    let scheme = if base.https { "https" } else { "http" };
    let path = if location.starts_with('/') {
        location.to_string()
    } else {
        let dir = base.path.rsplit_once('/').map_or("", |(d, _)| d);
        alloc::format!("{dir}/{location}")
    };
    parse_url(&alloc::format!("{scheme}://{}:{}{}", base.host, base.port, path))
}

const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Headers,
    Body,
    ChunkSize,
    ChunkData(usize),
    ChunkCrlf,
    Trailers,
    Done,
}

pub struct Parser {
    buf: Vec<u8>,
    phase: Phase,
    status: u16,
    headers: Vec<(String, String)>,
    content_length: Option<usize>,
    chunked: bool,
    body: Vec<u8>,
    max_body: usize,
}

impl Parser {
    pub fn new(max_body: usize) -> Self {
        Self { buf: Vec::new(), phase: Phase::Headers, status: 0, headers: Vec::new(), content_length: None, chunked: false, body: Vec::new(), max_body }
    }

    pub fn status(&self) -> u16 {
        self.status
    }
    pub fn headers_done(&self) -> bool {
        self.phase != Phase::Headers
    }
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
    pub fn content_length(&self) -> Option<usize> {
        self.content_length
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    /// Body bytes received so far, for progress output.
    pub fn received(&self) -> usize {
        self.body.len()
    }

    /// Feeds bytes from the connection.
    pub fn feed(&mut self, data: &[u8]) -> Result<(), HttpError> {
        self.buf.extend_from_slice(data);
        loop {
            match self.phase {
                Phase::Done => {
                    self.buf.clear(); // ignore anything after the end of the message
                    return Ok(());
                }
                Phase::Headers => {
                    let Some(end) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        if self.buf.len() > MAX_HEADER_BYTES {
                            return Err(HttpError::HeadersTooLarge);
                        }
                        return Ok(());
                    };
                    let head: Vec<u8> = self.buf.drain(..end + 4).collect();
                    self.parse_head(&head[..end])?;
                }
                Phase::Body => {
                    let want = self.content_length.map_or(self.buf.len(), |n| n.saturating_sub(self.body.len()));
                    let take = want.min(self.buf.len());
                    self.push_body(take)?;
                    if self.content_length.is_some_and(|n| self.body.len() >= n) {
                        self.phase = Phase::Done;
                    } else {
                        return Ok(());
                    }
                }
                Phase::ChunkSize => {
                    let Some(eol) = self.buf.windows(2).position(|w| w == b"\r\n") else {
                        if self.buf.len() > 64 {
                            return Err(HttpError::Malformed);
                        }
                        return Ok(());
                    };
                    let line: Vec<u8> = self.buf.drain(..eol + 2).collect();
                    let text = core::str::from_utf8(&line[..eol]).map_err(|_| HttpError::Malformed)?;
                    let size = usize::from_str_radix(text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| HttpError::Malformed)?;
                    self.phase = if size == 0 { Phase::Trailers } else { Phase::ChunkData(size) };
                }
                Phase::ChunkData(left) => {
                    let take = left.min(self.buf.len());
                    self.push_body(take)?;
                    if take == left {
                        self.phase = Phase::ChunkCrlf;
                    } else {
                        self.phase = Phase::ChunkData(left - take);
                        return Ok(());
                    }
                }
                Phase::ChunkCrlf => {
                    if self.buf.len() < 2 {
                        return Ok(());
                    }
                    if self.buf[..2] != *b"\r\n" {
                        return Err(HttpError::Malformed);
                    }
                    self.buf.drain(..2);
                    self.phase = Phase::ChunkSize;
                }
                Phase::Trailers => {
                    // Trailer lines until an empty line.
                    let Some(eol) = self.buf.windows(2).position(|w| w == b"\r\n") else {
                        if self.buf.len() > MAX_HEADER_BYTES {
                            return Err(HttpError::HeadersTooLarge);
                        }
                        return Ok(());
                    };
                    let empty = eol == 0;
                    self.buf.drain(..eol + 2);
                    if empty {
                        self.phase = Phase::Done;
                    }
                }
            }
        }
    }

    fn push_body(&mut self, take: usize) -> Result<(), HttpError> {
        if self.body.len() + take > self.max_body {
            return Err(HttpError::TooLarge);
        }
        self.body.extend(self.buf.drain(..take));
        Ok(())
    }

    fn parse_head(&mut self, head: &[u8]) -> Result<(), HttpError> {
        let text = core::str::from_utf8(head).map_err(|_| HttpError::Malformed)?;
        let mut lines = text.split("\r\n");
        let status_line = lines.next().ok_or(HttpError::Malformed)?;
        let mut parts = status_line.splitn(3, ' ');
        if !parts.next().is_some_and(|v| v.starts_with("HTTP/1.")) {
            return Err(HttpError::Malformed);
        }
        self.status = parts.next().and_then(|c| c.parse().ok()).ok_or(HttpError::Malformed)?;
        for line in lines {
            let (k, v) = line.split_once(':').ok_or(HttpError::Malformed)?;
            self.headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        self.chunked = self.header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
        self.content_length = match self.header("content-length") {
            Some(v) if !self.chunked => Some(v.parse().map_err(|_| HttpError::Malformed)?),
            _ => None,
        };
        if let Some(n) = self.content_length {
            if n > self.max_body {
                return Err(HttpError::TooLarge);
            }
        }
        // Responses that never have a body.
        let no_body = (100..200).contains(&self.status) || self.status == 204 || self.status == 304 || self.content_length == Some(0);
        self.phase = if no_body {
            Phase::Done
        } else if self.chunked {
            Phase::ChunkSize
        } else {
            Phase::Body
        };
        Ok(())
    }

    /// The connection closed. Fine for a message that ends with the connection; an error when the length was
    /// announced and the body is short, or a chunked body has no last chunk.
    pub fn finish(&mut self) -> Result<(), HttpError> {
        match self.phase {
            Phase::Done => Ok(()),
            Phase::Body if self.content_length.is_none() => {
                self.phase = Phase::Done;
                Ok(())
            }
            _ => Err(HttpError::Truncated),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = parse_url("http://archive.ubuntu.com/ubuntu/dists/x").unwrap();
        assert_eq!((u.https, u.host.as_str(), u.port, u.path.as_str()), (false, "archive.ubuntu.com", 80, "/ubuntu/dists/x"));
        let u = parse_url("http://10.0.2.2:8000").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("10.0.2.2", 8000, "/"));
        assert_eq!(parse_url("https://example.org/a?b=c").unwrap().port, 443);
        assert_eq!(parse_url("http://h?x=1").unwrap().path, "/?x=1");
        for bad in ["ftp://x", "http://", "http://u@h/", "http://h:0/", "http://h:99999/", "http://h/a b", "example.com"] {
            assert_eq!(parse_url(bad), Err(HttpError::BadUrl), "{bad}");
        }
    }

    #[test]
    fn request_and_location_resolution() {
        let u = parse_url("http://h.example:8080/p/q").unwrap();
        let r = String::from_utf8(request(&u)).unwrap();
        assert!(r.starts_with("GET /p/q HTTP/1.1\r\nHost: h.example:8080\r\n"));
        assert!(r.ends_with("Connection: close\r\n\r\n"));
        assert_eq!(resolve_location(&u, "/x").unwrap().path, "/x");
        assert_eq!(resolve_location(&u, "r").unwrap().path, "/p/r");
        assert_eq!(resolve_location(&u, "http://other/z").unwrap().host, "other");
        assert_eq!(resolve_location(&u, "https://secure/z").unwrap().https, true);
    }

    fn feed_in_pieces(p: &mut Parser, data: &[u8], piece: usize) {
        for c in data.chunks(piece) {
            p.feed(c).unwrap();
        }
    }

    #[test]
    fn content_length_body_in_any_split() {
        let msg = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 11\r\n\r\nhello world EXTRA";
        for piece in [1, 2, 7, 50, 1000] {
            let mut p = Parser::new(100);
            feed_in_pieces(&mut p, msg, piece);
            assert!(p.is_done(), "piece {piece}");
            assert_eq!((p.status(), p.body()), (200, &b"hello world"[..]));
            assert_eq!(p.header("content-type"), Some("text/plain"));
        }
    }

    #[test]
    fn chunked_body_with_extensions_and_trailers() {
        let msg = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nX-Sum: 1\r\n\r\n";
        for piece in [1, 3, 1000] {
            let mut p = Parser::new(100);
            feed_in_pieces(&mut p, msg, piece);
            assert!(p.is_done(), "piece {piece}");
            assert_eq!(p.body(), b"hello world");
        }
    }

    #[test]
    fn read_until_close_and_truncation() {
        let mut p = Parser::new(100);
        p.feed(b"HTTP/1.0 200 OK\r\n\r\npartial").unwrap();
        assert!(!p.is_done());
        p.finish().unwrap();
        assert_eq!(p.body(), b"partial");
        let mut p = Parser::new(100);
        p.feed(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort").unwrap();
        assert_eq!(p.finish(), Err(HttpError::Truncated));
        let mut p = Parser::new(100);
        p.feed(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel").unwrap();
        assert_eq!(p.finish(), Err(HttpError::Truncated));
    }

    #[test]
    fn redirects_and_empty_bodies() {
        let mut p = Parser::new(100);
        p.feed(b"HTTP/1.1 301 Moved\r\nLocation: http://x/y\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert!(p.is_done());
        assert_eq!((p.status(), p.header("Location")), (301, Some("http://x/y")));
        let mut p = Parser::new(100);
        p.feed(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert!(p.is_done());
        let mut p = Parser::new(100);
        p.feed(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnope").unwrap();
        assert_eq!((p.status(), p.body()), (404, &b"nope"[..]));
    }

    #[test]
    fn limits_and_garbage() {
        let mut p = Parser::new(10);
        assert_eq!(p.feed(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n"), Err(HttpError::TooLarge));
        let mut p = Parser::new(10);
        assert_eq!(p.feed(b"HTTP/1.1 200 OK\r\n\r\n0123456789ABC"), Err(HttpError::TooLarge));
        let mut p = Parser::new(10);
        assert_eq!(p.feed(b"NOT HTTP\r\n\r\n"), Err(HttpError::Malformed));
        let mut p = Parser::new(10);
        assert_eq!(p.feed(&alloc::vec![b'a'; 20_000]), Err(HttpError::HeadersTooLarge));
        let mut p = Parser::new(100);
        assert_eq!(p.feed(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"), Err(HttpError::Malformed));
        let mut seed = 3u32;
        for len in 0..300 {
            let f: Vec<u8> = (0..len).map(|_| { seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345); (seed >> 16) as u8 }).collect();
            let mut p = Parser::new(1000);
            let _ = p.feed(&f);
            let _ = p.finish();
        }
    }
}
