//! Bounded, single-shot HTTP/1.1 framing for the loopback listener.
//!
//! Every read and write happens under an absolute deadline and inside a fixed memory
//! budget. Framing ambiguity is refused before any body byte is read, so a request with
//! `Transfer-Encoding`, a repeated or non-canonical `Content-Length`, or an unknown method
//! never reaches routing.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::PublicError;

/// Largest header block (request line, fields and terminator) accepted per connection.
pub(crate) const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Largest number of header fields accepted per connection.
pub(crate) const MAX_HEADER_COUNT: usize = 64;
/// Largest single header field value accepted per connection.
pub(crate) const MAX_HEADER_VALUE_BYTES: usize = 4 * 1024;
/// Largest request target (path plus query) accepted per connection.
pub(crate) const MAX_TARGET_BYTES: usize = 512;
/// Largest request body accepted per connection.
///
/// The application text limit is 8000 UTF-8 bytes. A conforming JSON request may escape
/// every one of those bytes as `\uXXXX`, which turns 8000 bytes into 48000 characters, so
/// the transport budget sits above that expansion plus the request envelope.
pub(crate) const MAX_BODY_BYTES: usize = 64 * 1024;
/// Absolute budget for reading one complete request.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Absolute budget for writing one response.
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Budget for draining what a refused client is still sending before the socket closes.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

const HEADER_TERMINATOR: &[u8; 4] = b"\r\n\r\n";
/// Header fields whose repetition is ambiguous for framing or for policy decisions.
const CRITICAL_HEADERS: &[&str] = &[
    "host",
    "origin",
    "cookie",
    "content-length",
    "content-type",
    "content-encoding",
    "x-csrf-token",
    "transfer-encoding",
    "expect",
    "upgrade",
    "te",
    "proxy-connection",
];
/// Header fields that make the body framing ambiguous for a single-shot request.
const FORBIDDEN_FRAMING_HEADERS: &[&str] = &[
    "transfer-encoding",
    "expect",
    "upgrade",
    "te",
    "proxy-connection",
];

pub(crate) const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; \
style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; \
form-action 'none'; frame-ancestors 'none'; object-src 'none'";

/// An absolute instant by which all remaining work on one connection must be finished.
#[derive(Clone, Copy)]
pub(crate) struct Deadline {
    at: Instant,
}

impl Deadline {
    pub(crate) fn after(start: Instant, budget: Duration) -> Self {
        Self { at: start + budget }
    }

    fn remaining(&self, now: Instant) -> io::Result<Duration> {
        match self.at.checked_duration_since(now) {
            Some(remaining) if !remaining.is_zero() => Ok(remaining),
            _ => Err(deadline_error()),
        }
    }
}

fn deadline_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "loopback deadline elapsed")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
}

/// A fully read request: method, query-free path and bounded body.
pub(crate) struct Request {
    pub(crate) method: Method,
    pub(crate) target: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Result of reading one connection.
pub(crate) enum Incoming {
    Request(Request),
    Refused(Reject),
}

/// A framing refusal that can be answered without touching application state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reject {
    Malformed,
    AmbiguousHeaders,
    HeadersTooLarge,
    UnsupportedVersion,
    MethodNotAllowed,
    LengthRequired,
    PayloadTooLarge,
    UnsupportedEncoding,
    Timeout,
}

impl Reject {
    fn status(self) -> u16 {
        match self {
            Self::Malformed | Self::AmbiguousHeaders => 400,
            Self::HeadersTooLarge => 431,
            Self::UnsupportedVersion => 505,
            Self::MethodNotAllowed => 405,
            Self::LengthRequired => 411,
            Self::PayloadTooLarge => 413,
            Self::UnsupportedEncoding => 415,
            Self::Timeout => 408,
        }
    }

    fn error(self) -> PublicError {
        match self {
            Self::Malformed => {
                PublicError::new("bad_request", "request is not a valid HTTP/1.1 message")
            }
            Self::AmbiguousHeaders => PublicError::new(
                "ambiguous_request",
                "request repeats or conflicts in its framing headers",
            ),
            Self::HeadersTooLarge => PublicError::new(
                "headers_too_large",
                "request headers exceed the permitted size",
            ),
            Self::UnsupportedVersion => {
                PublicError::new("unsupported_version", "only HTTP/1.1 is supported")
            }
            Self::MethodNotAllowed => {
                PublicError::new("method_not_allowed", "only GET and POST are supported")
            }
            Self::LengthRequired => PublicError::new(
                "length_required",
                "POST requires an explicit Content-Length",
            ),
            Self::PayloadTooLarge => PublicError::new(
                "payload_too_large",
                "request body exceeds the permitted size",
            ),
            Self::UnsupportedEncoding => {
                PublicError::new("unsupported_encoding", "content encoding is not supported")
            }
            Self::Timeout => PublicError::new(
                "timeout",
                "the request deadline elapsed before it was complete",
            ),
        }
    }
}

/// Reads one complete request, refusing framing the host will not interpret.
pub(crate) fn read_request(stream: &mut TcpStream, deadline: &Deadline) -> io::Result<Incoming> {
    let mut buffer = Vec::with_capacity(1024);
    let head_end = loop {
        if let Some(position) = find_head_end(&buffer) {
            if position > MAX_HEADER_BYTES {
                return Ok(Incoming::Refused(Reject::HeadersTooLarge));
            }
            break position;
        }
        if buffer.len() >= MAX_HEADER_BYTES {
            return Ok(Incoming::Refused(Reject::HeadersTooLarge));
        }
        let mut chunk = [0_u8; 1024];
        let wanted = (MAX_HEADER_BYTES - buffer.len()).min(chunk.len());
        match read_some(stream, &mut chunk[..wanted], deadline) {
            Ok(0) if buffer.is_empty() => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "client closed before sending a request",
                ));
            }
            Ok(0) => return Ok(Incoming::Refused(Reject::Malformed)),
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                return Ok(Incoming::Refused(Reject::Timeout));
            }
            Err(err) => return Err(err),
        }
    };

    let head = match parse_head(&buffer[..head_end]) {
        Ok(head) => head,
        Err(reject) => return Ok(Incoming::Refused(reject)),
    };
    let length = match framing(&head) {
        Ok(length) => length,
        Err(reject) => return Ok(Incoming::Refused(reject)),
    };

    let mut body = buffer[head_end..].to_vec();
    if body.len() > length {
        return Ok(Incoming::Refused(Reject::Malformed));
    }
    while body.len() < length {
        let mut chunk = [0_u8; 4096];
        let wanted = (length - body.len()).min(chunk.len());
        match read_some(stream, &mut chunk[..wanted], deadline) {
            Ok(0) => return Ok(Incoming::Refused(Reject::Malformed)),
            Ok(read) => body.extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                return Ok(Incoming::Refused(Reject::Timeout));
            }
            Err(err) => return Err(err),
        }
    }

    Ok(Incoming::Request(Request {
        method: head.method,
        target: head.target,
        headers: head.headers,
        body,
    }))
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(HEADER_TERMINATOR.len())
        .position(|window| window == HEADER_TERMINATOR)
        .map(|position| position + HEADER_TERMINATOR.len())
}

struct Head {
    method: Method,
    target: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }
}

fn parse_head(head: &[u8]) -> Result<Head, Reject> {
    let text = std::str::from_utf8(head).map_err(|_| Reject::Malformed)?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().ok_or(Reject::Malformed)?;
    let (method, target) = parse_request_line(request_line)?;

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(Reject::Malformed);
        }
        let (name, value) = line.split_once(':').ok_or(Reject::Malformed)?;
        if !is_token(name) || name.len() > 64 {
            return Err(Reject::Malformed);
        }
        let value = value.trim_matches(|character| character == ' ' || character == '\t');
        if value.len() > MAX_HEADER_VALUE_BYTES {
            return Err(Reject::HeadersTooLarge);
        }
        if value
            .bytes()
            .any(|byte| byte < 0x20 && byte != b'\t' || byte == 0x7f)
        {
            return Err(Reject::Malformed);
        }
        if headers.len() >= MAX_HEADER_COUNT {
            return Err(Reject::HeadersTooLarge);
        }
        let name = name.to_ascii_lowercase();
        if CRITICAL_HEADERS.contains(&name.as_str())
            && headers.iter().any(|(existing, _)| existing == &name)
        {
            return Err(Reject::AmbiguousHeaders);
        }
        headers.push((name, value.to_owned()));
    }

    Ok(Head {
        method,
        target,
        headers,
    })
}

fn parse_request_line(line: &str) -> Result<(Method, String), Reject> {
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some() || !is_token(method) {
        return Err(Reject::Malformed);
    }
    let method = match method {
        "GET" => Method::Get,
        "POST" => Method::Post,
        _ => return Err(Reject::MethodNotAllowed),
    };
    if !version.starts_with("HTTP/") {
        return Err(Reject::Malformed);
    }
    if version != "HTTP/1.1" {
        return Err(Reject::UnsupportedVersion);
    }
    Ok((method, parse_target(target)?))
}

/// Accepts origin-form targets only and strips the query, which never selects a route.
fn parse_target(target: &str) -> Result<String, Reject> {
    if target.len() > MAX_TARGET_BYTES || !target.starts_with('/') || target.starts_with("//") {
        return Err(Reject::Malformed);
    }
    if target
        .bytes()
        .any(|byte| !(0x21..=0x7e).contains(&byte) || byte == b'#')
    {
        return Err(Reject::Malformed);
    }
    Ok(target.split('?').next().unwrap_or(target).to_owned())
}

/// Decides the exact body length before a single body byte is read.
fn framing(head: &Head) -> Result<usize, Reject> {
    if head
        .headers
        .iter()
        .any(|(name, _)| FORBIDDEN_FRAMING_HEADERS.contains(&name.as_str()))
    {
        return Err(Reject::AmbiguousHeaders);
    }
    if let Some(encoding) = head.header("content-encoding")
        && !encoding.eq_ignore_ascii_case("identity")
    {
        return Err(Reject::UnsupportedEncoding);
    }
    let declared = match head.header("content-length") {
        Some(value) => Some(parse_length(value)?),
        None => None,
    };
    match head.method {
        Method::Get => match declared {
            None | Some(0) => Ok(0),
            Some(_) => Err(Reject::Malformed),
        },
        Method::Post => {
            let length = declared.ok_or(Reject::LengthRequired)?;
            if length > MAX_BODY_BYTES {
                return Err(Reject::PayloadTooLarge);
            }
            Ok(length)
        }
    }
}

/// A `Content-Length` is canonical when it is a plain decimal without signs, padding or
/// list syntax; anything else is ambiguous framing. A decimal that exceeds a machine word
/// is far beyond the body budget and reported as too large.
fn parse_length(value: &str) -> Result<usize, Reject> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Reject::AmbiguousHeaders);
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(Reject::AmbiguousHeaders);
    }
    value.parse::<usize>().map_err(|_| Reject::PayloadTooLarge)
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn read_some(stream: &mut TcpStream, buffer: &mut [u8], deadline: &Deadline) -> io::Result<usize> {
    let remaining = deadline.remaining(Instant::now())?;
    stream.set_read_timeout(Some(remaining))?;
    match stream.read(buffer) {
        Ok(read) => Ok(read),
        Err(err) if is_timeout(&err) => Err(deadline_error()),
        Err(err) => Err(err),
    }
}

fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// One complete response; the framing headers are written by [`write_response`].
pub(crate) struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    set_cookie: Option<String>,
    allow: Option<&'static str>,
}

impl Response {
    pub(crate) fn asset(status: u16, content_type: &'static str, body: &'static [u8]) -> Self {
        Self {
            status,
            content_type,
            body: body.to_vec(),
            set_cookie: None,
            allow: None,
        }
    }

    pub(crate) fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(value).unwrap_or_default(),
            set_cookie: None,
            allow: None,
        }
    }

    pub(crate) fn with_cookie(mut self, cookie: String) -> Self {
        self.set_cookie = Some(cookie);
        self
    }

    pub(crate) fn with_allow(mut self, allow: &'static str) -> Self {
        self.allow = Some(allow);
        self
    }
}

/// The refusal answer for framing the host never routed.
pub(crate) fn refusal_response(reject: Reject) -> Response {
    let error = reject.error();
    let mut response = Response::json(
        reject.status(),
        &serde_json::json!({"error": {"code": error.code, "message": error.message}}),
    );
    if reject == Reject::MethodNotAllowed {
        response = response.with_allow("GET, POST");
    }
    response
}

/// Writes a complete response under an absolute write deadline.
pub(crate) fn write_response(
    stream: &mut TcpStream,
    deadline: &Deadline,
    response: &Response,
) -> io::Result<()> {
    let mut head = String::with_capacity(512);
    head.push_str("HTTP/1.1 ");
    head.push_str(&response.status.to_string());
    head.push(' ');
    head.push_str(reason_phrase(response.status));
    head.push_str("\r\nContent-Type: ");
    head.push_str(response.content_type);
    head.push_str("\r\nContent-Length: ");
    head.push_str(&response.body.len().to_string());
    head.push_str("\r\nCache-Control: no-store, no-cache\r\n");
    head.push_str("X-Content-Type-Options: nosniff\r\n");
    head.push_str("Referrer-Policy: no-referrer\r\n");
    head.push_str("Content-Security-Policy: ");
    head.push_str(CONTENT_SECURITY_POLICY);
    head.push_str("\r\n");
    if let Some(allow) = response.allow {
        head.push_str("Allow: ");
        head.push_str(allow);
        head.push_str("\r\n");
    }
    if let Some(cookie) = &response.set_cookie {
        head.push_str("Set-Cookie: ");
        head.push_str(cookie);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");

    write_all(stream, head.as_bytes(), deadline)?;
    write_all(stream, &response.body, deadline)?;
    stream.flush()
}

fn write_all(stream: &mut TcpStream, mut buffer: &[u8], deadline: &Deadline) -> io::Result<()> {
    while !buffer.is_empty() {
        let remaining = deadline.remaining(Instant::now())?;
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(buffer) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "connection refused the response",
                ));
            }
            Ok(written) => buffer = &buffer[written..],
            Err(err) if is_timeout(&err) => return Err(deadline_error()),
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Half-closes the connection, then reads and discards at most `budget` bytes.
///
/// A refusal is written before the client necessarily stopped sending: closing a socket
/// with unread data makes the stack answer with a reset, which can destroy the response the
/// browser needs to see. Draining what is already in flight keeps the refusal observable,
/// and the byte budget plus [`DRAIN_TIMEOUT`] keep it bounded.
pub(crate) fn drain_before_close(stream: &mut TcpStream, budget: usize) {
    let _ = stream.shutdown(Shutdown::Write);
    let deadline = Deadline::after(Instant::now(), DRAIN_TIMEOUT);
    let mut scratch = [0_u8; 2048];
    let mut remaining = budget;
    while remaining > 0 {
        let wanted = remaining.min(scratch.len());
        match read_some(stream, &mut scratch[..wanted], &deadline) {
            Ok(0) => break,
            Ok(read) => remaining -= read,
            Err(_) => break,
        }
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        505 => "HTTP Version Not Supported",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(request: &str) -> Result<Head, Reject> {
        let end = find_head_end(request.as_bytes()).expect("terminated head");
        parse_head(&request.as_bytes()[..end])
    }

    #[test]
    fn request_line_accepts_only_the_two_whitelisted_methods_on_http_1_1() {
        assert_eq!(
            head("GET / HTTP/1.1\r\n\r\n").expect("valid").method,
            Method::Get
        );
        assert_eq!(
            head("POST /api/action HTTP/1.1\r\n\r\n")
                .expect("valid")
                .method,
            Method::Post
        );
        assert_eq!(
            head("PUT / HTTP/1.1\r\n\r\n").err(),
            Some(Reject::MethodNotAllowed)
        );
        assert_eq!(
            head("get / HTTP/1.1\r\n\r\n").err(),
            Some(Reject::MethodNotAllowed)
        );
        assert_eq!(
            head("GET / HTTP/1.0\r\n\r\n").err(),
            Some(Reject::UnsupportedVersion)
        );
        assert_eq!(
            head("GET / HTTP/2\r\n\r\n").err(),
            Some(Reject::UnsupportedVersion)
        );
        assert_eq!(head("GET / nope\r\n\r\n").err(), Some(Reject::Malformed));
        assert_eq!(
            head("GET  / HTTP/1.1\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        assert_eq!(
            head("GET / HTTP/1.1 extra\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
    }

    #[test]
    fn targets_must_be_bounded_origin_form_paths() {
        assert_eq!(
            head("GET /app.js?cache=1 HTTP/1.1\r\n\r\n")
                .expect("valid")
                .target,
            "/app.js"
        );
        assert_eq!(
            head("GET http://evil.example/ HTTP/1.1\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        assert_eq!(
            head("GET //evil.example HTTP/1.1\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        assert_eq!(
            head("GET /a#b HTTP/1.1\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        assert_eq!(
            head("GET /a b HTTP/1.1\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        assert_eq!(
            head("GET /日本 HTTP/1.1\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_TARGET_BYTES));
        assert_eq!(head(&long).err(), Some(Reject::Malformed));
    }

    #[test]
    fn header_fields_are_lowercased_and_validated() {
        let parsed =
            head("POST /api/action HTTP/1.1\r\nHost: 127.0.0.1:1\r\nX-CSRF-Token:\t abc \r\n\r\n")
                .expect("valid");
        assert_eq!(parsed.header("host"), Some("127.0.0.1:1"));
        assert_eq!(parsed.header("x-csrf-token"), Some("abc"));
        assert!(parsed.header("missing").is_none());

        assert_eq!(
            head("GET / HTTP/1.1\r\nHost : x\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        assert_eq!(
            head("GET / HTTP/1.1\r\nBad Header: x\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        assert_eq!(
            head("GET / HTTP/1.1\r\n folded: x\r\n\r\n").err(),
            Some(Reject::Malformed)
        );
        assert_eq!(
            head("GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n").err(),
            Some(Reject::AmbiguousHeaders)
        );
    }

    #[test]
    fn framing_rejects_ambiguity_and_requires_explicit_lengths() {
        let get_with_body = head("GET / HTTP/1.1\r\nContent-Length: 3\r\n\r\n").expect("head");
        assert_eq!(framing(&get_with_body).err(), Some(Reject::Malformed));

        let post_without_length = head("POST /api/action HTTP/1.1\r\n\r\n").expect("head");
        assert_eq!(
            framing(&post_without_length).err(),
            Some(Reject::LengthRequired)
        );

        let chunked = head(
            "POST /api/action HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n",
        )
        .expect("head");
        assert_eq!(framing(&chunked).err(), Some(Reject::AmbiguousHeaders));

        let expecting =
            head("POST /api/action HTTP/1.1\r\nExpect: 100-continue\r\n\r\n").expect("head");
        assert_eq!(framing(&expecting).err(), Some(Reject::AmbiguousHeaders));

        let encoded = head(
            "POST /api/action HTTP/1.1\r\nContent-Encoding: gzip\r\nContent-Length: 3\r\n\r\n",
        )
        .expect("head");
        assert_eq!(framing(&encoded).err(), Some(Reject::UnsupportedEncoding));

        let oversized =
            head("POST /api/action HTTP/1.1\r\nContent-Length: 65537\r\n\r\n").expect("head");
        assert_eq!(framing(&oversized).err(), Some(Reject::PayloadTooLarge));

        let accepted =
            head("POST /api/action HTTP/1.1\r\nContent-Length: 12\r\n\r\n").expect("head");
        assert_eq!(framing(&accepted), Ok(12));
    }

    #[test]
    fn content_length_must_be_a_canonical_decimal() {
        assert_eq!(parse_length("0"), Ok(0));
        assert_eq!(parse_length("12"), Ok(12));
        assert_eq!(parse_length("65536"), Ok(65_536));
        assert_eq!(parse_length("007"), Err(Reject::AmbiguousHeaders));
        assert_eq!(parse_length("+7"), Err(Reject::AmbiguousHeaders));
        assert_eq!(parse_length("-1"), Err(Reject::AmbiguousHeaders));
        assert_eq!(parse_length("7, 7"), Err(Reject::AmbiguousHeaders));
        assert_eq!(parse_length("0x10"), Err(Reject::AmbiguousHeaders));
        assert_eq!(parse_length(""), Err(Reject::AmbiguousHeaders));
        assert_eq!(
            parse_length("99999999999999999999999999"),
            Err(Reject::PayloadTooLarge)
        );
    }

    #[test]
    fn deadlines_are_absolute_and_never_zero() {
        let start = Instant::now();
        let deadline = Deadline::after(start, Duration::from_millis(50));
        assert!(deadline.remaining(start).is_ok());
        assert!(
            deadline
                .remaining(start + Duration::from_millis(60))
                .is_err()
        );
        assert!(
            deadline
                .remaining(start + Duration::from_millis(50))
                .is_err()
        );
    }
}
