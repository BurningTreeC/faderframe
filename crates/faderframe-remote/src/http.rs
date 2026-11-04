//! Just enough HTTP/1.1 for the remote: a request's line and headers
//! (bounded: a header block past [`MAX_HEADER_BYTES`] is refused), and
//! responses that close the connection. Only `GET` is served; nothing
//! reads a body.

use std::io::{self, BufRead};

/// The most a request's line and headers may take.
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
/// The most header lines.
const MAX_HEADERS: usize = 64;

/// A request as read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The target as sent (path and query).
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// A header's value (names compare without case; the first of them).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The path, without the query.
    pub fn path(&self) -> &str {
        self.target.split(['?', '#']).next().unwrap_or("")
    }

    /// Whether a header lists `token` among its comma-separated values
    /// (without case: `Connection: keep-alive, Upgrade`).
    pub fn header_has(&self, name: &str, token: &str) -> bool {
        self.header(name)
            .is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    }

    /// A WebSocket opening handshake (RFC 6455 §4.2.1).
    pub fn is_websocket(&self) -> bool {
        self.method == "GET"
            && self.header_has("Upgrade", "websocket")
            && self.header_has("Connection", "upgrade")
    }
}

/// Why a request was not read.
#[derive(Debug)]
pub enum HttpError {
    /// The peer closed before a whole request came.
    Closed,
    /// Not a request this server reads.
    Malformed(&'static str),
    TooLarge,
    Io(io::Error),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Closed => write!(f, "closed"),
            HttpError::Malformed(why) => write!(f, "malformed request: {why}"),
            HttpError::TooLarge => write!(f, "request headers too large"),
            HttpError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// One line, without its CR LF, counting towards `left`.
fn line(r: &mut impl BufRead, left: &mut usize) -> Result<Option<String>, HttpError> {
    let mut buf = Vec::new();
    loop {
        let available = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(HttpError::Io(e)),
        };
        if available.is_empty() {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(HttpError::Closed)
            };
        }
        let (taken, done) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        if taken > *left {
            return Err(HttpError::TooLarge);
        }
        *left -= taken;
        buf.extend_from_slice(&available[..taken]);
        r.consume(taken);
        if done {
            break;
        }
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| HttpError::Malformed("not UTF-8"))
}

/// Read a request's line and headers.
pub fn read_request(r: &mut impl BufRead) -> Result<Request, HttpError> {
    let mut left = MAX_HEADER_BYTES;
    let first = line(r, &mut left)?.ok_or(HttpError::Closed)?;
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(HttpError::Malformed("request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(HttpError::Malformed("version"));
    }
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(HttpError::Malformed("method"));
    }
    if !target.starts_with('/') {
        return Err(HttpError::Malformed("target"));
    }
    let mut headers = Vec::new();
    loop {
        let l = line(r, &mut left)?.ok_or(HttpError::Closed)?;
        if l.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(HttpError::TooLarge);
        }
        let Some((name, value)) = l.split_once(':') else {
            return Err(HttpError::Malformed("header"));
        };
        if name.is_empty() || name.contains([' ', '\t']) {
            return Err(HttpError::Malformed("header name"));
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }
    Ok(Request {
        method: method.to_string(),
        target: target.to_string(),
        headers,
    })
}

/// A response with `body`, closing the connection after it. `extra`
/// headers come after the standard ones.
pub fn response(
    status: u16,
    reason: &str,
    content_type: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    out
}

/// A plain-text error response.
pub fn error(status: u16, reason: &str) -> Vec<u8> {
    response(
        status,
        reason,
        "text/plain; charset=utf-8",
        &[],
        format!("{status} {reason}\n").as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_request_reads_with_its_headers() {
        let raw = b"GET /ws?x=1 HTTP/1.1\r\nHost: 192.168.1.2:8642\r\nUpgrade: websocket\r\n\
                    Connection: keep-alive, Upgrade\r\nSec-WebSocket-Key: abc\r\n\r\nrest";
        let mut r = Cursor::new(&raw[..]);
        let req = read_request(&mut r).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path(), "/ws");
        assert_eq!(req.header("host"), Some("192.168.1.2:8642"));
        assert!(req.is_websocket());
        // What follows the headers stays in the reader.
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut r, &mut rest).unwrap();
        assert_eq!(rest, "rest");
    }

    #[test]
    fn oversized_and_malformed_requests_are_refused() {
        let mut big = b"GET / HTTP/1.1\r\n".to_vec();
        for i in 0..2000 {
            big.extend_from_slice(format!("X-{i}: y\r\n").as_bytes());
        }
        big.extend_from_slice(b"\r\n");
        assert!(matches!(
            read_request(&mut Cursor::new(big)),
            Err(HttpError::TooLarge)
        ));
        for bad in [
            &b"GET\r\n\r\n"[..],
            b"get / HTTP/1.1\r\n\r\n",
            b"GET / SPDY/3\r\n\r\n",
            b"GET http://x/ HTTP/1.1\r\n\r\n",
            b"GET / HTTP/1.1\r\nNoColon\r\n\r\n",
        ] {
            assert!(
                matches!(
                    read_request(&mut Cursor::new(bad)),
                    Err(HttpError::Malformed(_))
                ),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        assert!(matches!(
            read_request(&mut Cursor::new(&b""[..])),
            Err(HttpError::Closed)
        ));
        assert!(matches!(
            read_request(&mut Cursor::new(&b"GET / HTTP/1.1\r\nHost: x"[..])),
            Err(HttpError::Closed)
        ));
    }

    #[test]
    fn responses_close_and_say_their_length() {
        let r =
            String::from_utf8(response(200, "OK", "text/plain", &[("X-A", "b")], b"hi")).unwrap();
        assert!(r.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(r.contains("Content-Length: 2\r\n"));
        assert!(r.contains("Connection: close\r\n"));
        assert!(r.contains("X-A: b\r\n"));
        assert!(r.ends_with("\r\n\r\nhi"));
    }
}
