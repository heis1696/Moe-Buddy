//! Loopback listener, route table and browser policy enforcement.
//!
//! The server is deliberately serial: one connection is read, answered and closed before
//! the next [`LoopbackServer::serve_once`] call. The application is borrowed as `&mut dyn
//! ApplicationPort`, so a non-`Send` application is driven entirely on the calling thread.

use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::http::{self, Deadline, Incoming, Method, Request, Response};
use crate::ledger::{Ledger, Refusal};
use crate::snapshot;
use crate::tokens::{self, Tokens};
use crate::{ApplicationPort, Assets, PublicError};

const ERR_CROSS_SITE: PublicError = PublicError::new(
    "cross_site",
    "request did not originate from the loopback page",
);
const ERR_HOST: PublicError = PublicError::new(
    "bad_host",
    "request host does not match the loopback authority",
);
const ERR_UNAUTHENTICATED: PublicError =
    PublicError::new("unauthenticated", "a valid session cookie is required");
const ERR_CSRF: PublicError =
    PublicError::new("csrf_rejected", "request token is missing or invalid");
const ERR_CONTENT_TYPE: PublicError = PublicError::new(
    "unsupported_media_type",
    "Content-Type must be application/json",
);
const ERR_METHOD: PublicError = PublicError::new(
    "method_not_allowed",
    "method is not permitted for this route",
);
const ERR_NOT_FOUND: PublicError = PublicError::new("not_found", "resource is not available");
const ERR_DUPLICATE: PublicError = PublicError::new(
    "duplicate_request",
    "request id was already accepted by this process",
);
const ERR_LEDGER_FULL: PublicError = PublicError::new(
    "ledger_full",
    "request id ledger is full and never evicts an accepted id",
);

/// Timeout and capacity policy of one server instance.
#[derive(Clone, Copy)]
pub(crate) struct Config {
    pub(crate) read_timeout: Duration,
    pub(crate) write_timeout: Duration,
    pub(crate) ledger_capacity: usize,
}

impl Config {
    pub(crate) const fn production() -> Self {
        Self {
            read_timeout: http::READ_TIMEOUT,
            write_timeout: http::WRITE_TIMEOUT,
            ledger_capacity: crate::ledger::CAPACITY,
        }
    }
}

/// The loopback presentation server.
pub struct LoopbackServer {
    listener: TcpListener,
    assets: Assets,
    origin: String,
    authority: String,
    tokens: Tokens,
    ledger: Ledger,
    started: Instant,
    config: Config,
}

impl LoopbackServer {
    /// Binds the loopback listener on `port` (`0` picks a free port).
    ///
    /// Only `127.0.0.1` is ever bound. The returned server owns a fresh session token,
    /// CSRF token and non-zero process generation.
    pub fn bind(port: u16, assets: Assets) -> io::Result<Self> {
        Self::bind_with(port, assets, Config::production())
    }

    pub(crate) fn bind_with(port: u16, assets: Assets, config: Config) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let port = listener.local_addr()?.port();
        let authority = format!("127.0.0.1:{port}");
        Ok(Self {
            listener,
            assets,
            origin: format!("http://{authority}"),
            authority,
            tokens: Tokens::generate()?,
            ledger: Ledger::new(config.ledger_capacity),
            started: Instant::now(),
            config,
        })
    }

    /// Browser origin of this listener, for example `http://127.0.0.1:8787`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Non-zero generation of this host process; the application must not reuse it.
    pub fn process_generation(&self) -> u64 {
        self.tokens.process_generation()
    }

    /// Accepts and answers exactly one connection.
    ///
    /// Client-side failures (a peer that disconnects, or a request that misses the absolute
    /// deadline) are answered best-effort and reported as `Ok(())`, so a serve loop keeps
    /// running. Only a failure to accept the next connection is an error.
    pub fn serve_once(&mut self, application: &mut dyn ApplicationPort) -> io::Result<()> {
        let (mut stream, peer) = self.listener.accept()?;
        if !peer.ip().is_loopback() {
            return Ok(());
        }
        let _ = stream.set_nodelay(true);

        let started = Instant::now();
        let read_deadline = Deadline::after(started, self.config.read_timeout);
        let write_deadline = Deadline::after(
            started + self.config.read_timeout,
            self.config.write_timeout,
        );
        match http::read_request(&mut stream, &read_deadline) {
            Ok(Incoming::Request(request)) => {
                let response = self.route(&request, application);
                let _ = http::write_response(&mut stream, &write_deadline, &response);
            }
            Ok(Incoming::Refused(reject)) => {
                let response = http::refusal_response(reject);
                let _ = http::write_response(&mut stream, &write_deadline, &response);
                http::drain_before_close(
                    &mut stream,
                    http::MAX_BODY_BYTES + http::MAX_HEADER_BYTES,
                );
            }
            Err(_) => {}
        }
        Ok(())
    }

    /// Serves connections until the listener fails.
    pub fn run(&mut self, application: &mut dyn ApplicationPort) -> io::Result<()> {
        loop {
            self.serve_once(application)?;
        }
    }

    fn route(&mut self, request: &Request, application: &mut dyn ApplicationPort) -> Response {
        if let Err(response) = self.gate(request) {
            return response;
        }
        match (request.method, request.target.as_str()) {
            (Method::Get, "/") => {
                let cookie = self.session_cookie();
                Response::asset(200, "text/html; charset=utf-8", self.assets.index)
                    .with_cookie(cookie)
            }
            (Method::Get, "/app.js") => {
                Response::asset(200, "text/javascript; charset=utf-8", self.assets.app_js)
            }
            (Method::Get, "/style.css") => {
                Response::asset(200, "text/css; charset=utf-8", self.assets.style_css)
            }
            (Method::Get, "/api/bootstrap") => self.bootstrap(request, application),
            (Method::Get, "/api/state") => self.state(request, application),
            (Method::Post, "/api/action") => self.action(request, application),
            (_, "/" | "/app.js" | "/style.css" | "/api/bootstrap" | "/api/state") => {
                method_not_allowed("GET")
            }
            (_, "/api/action") => method_not_allowed("POST"),
            _ => error_only(404, ERR_NOT_FOUND),
        }
    }

    /// Exact `Host` and same-origin `Origin` policy, applied before any route decision.
    fn gate(&self, request: &Request) -> Result<(), Response> {
        match request.header("host") {
            Some(host) if host == self.authority => {}
            Some(_) => return Err(error_only(403, ERR_CROSS_SITE)),
            None => return Err(error_only(400, ERR_HOST)),
        }
        if let Some(origin) = request.header("origin")
            && origin != self.origin
        {
            return Err(error_only(403, ERR_CROSS_SITE));
        }
        Ok(())
    }

    fn bootstrap(&self, request: &Request, application: &mut dyn ApplicationPort) -> Response {
        if !self.authenticated(request) {
            return error_only(401, ERR_UNAUTHENTICATED);
        }
        let body = json!({
            "csrf": self.tokens.csrf(),
            "snapshot": snapshot::to_json(&application.snapshot()),
        });
        Response::json(200, &body)
    }

    fn state(&self, request: &Request, application: &mut dyn ApplicationPort) -> Response {
        if !self.authenticated(request) {
            return error_only(401, ERR_UNAUTHENTICATED);
        }
        let body = json!({"snapshot": snapshot::to_json(&application.snapshot())});
        Response::json(200, &body)
    }

    fn action(&mut self, request: &Request, application: &mut dyn ApplicationPort) -> Response {
        let same_origin = request
            .header("origin")
            .is_some_and(|origin| origin == self.origin);
        if !same_origin {
            return error_only(403, ERR_CROSS_SITE);
        }
        if !self.authenticated(request) {
            return error_only(401, ERR_UNAUTHENTICATED);
        }
        let csrf_valid = request
            .header("x-csrf-token")
            .is_some_and(|token| tokens::constant_time_eq(self.tokens.csrf(), token));
        if !csrf_valid {
            return error_only(403, ERR_CSRF);
        }
        if !json_content_type(request.header("content-type")) {
            return refusal(415, ERR_CONTENT_TYPE, application);
        }

        let prepared = match crate::actions::prepare(&request.body) {
            Ok(prepared) => prepared,
            Err(error) => return refusal(400, error, application),
        };
        match self.ledger.accept(&prepared.id) {
            Ok(()) => {}
            Err(Refusal::Duplicate) => return refusal(409, ERR_DUPLICATE, application),
            Err(Refusal::Full) => return refusal(429, ERR_LEDGER_FULL, application),
        }

        match application.dispatch(prepared.action, self.now_ms()) {
            Ok(()) => {
                let body = json!({"snapshot": snapshot::to_json(&application.snapshot())});
                Response::json(200, &body)
            }
            Err(error) => refusal(400, error, application),
        }
    }

    fn authenticated(&self, request: &Request) -> bool {
        request
            .header("cookie")
            .and_then(|header| cookie_value(header, tokens::COOKIE_NAME))
            .is_some_and(|value| tokens::constant_time_eq(self.tokens.session(), value))
    }

    fn session_cookie(&self) -> String {
        format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            tokens::COOKIE_NAME,
            self.tokens.session()
        )
    }

    /// Monotonic host time injected into the application; the application never reads a
    /// clock of its own.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// Error answer without a snapshot, used before authentication succeeds.
fn error_only(status: u16, error: PublicError) -> Response {
    Response::json(
        status,
        &json!({"error": {"code": error.code, "message": error.message}}),
    )
}

/// Error answer with the current snapshot, used only for an authenticated session.
fn refusal(status: u16, error: PublicError, application: &dyn ApplicationPort) -> Response {
    Response::json(
        status,
        &json!({
            "error": {"code": error.code, "message": error.message},
            "snapshot": snapshot::to_json(&application.snapshot()),
        }),
    )
}

fn method_not_allowed(allow: &'static str) -> Response {
    error_only(405, ERR_METHOD).with_allow(allow)
}

fn json_content_type(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let mut parts = value.split(';');
    match parts.next() {
        Some(media) if media.trim().eq_ignore_ascii_case("application/json") => {}
        _ => return false,
    }
    parts.all(|parameter| {
        let parameter = parameter.trim();
        parameter.is_empty() || parameter.eq_ignore_ascii_case("charset=utf-8")
    })
}

/// Reads one named cookie; a repeated name is ambiguous and treated as absent.
fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    let mut found: Option<&str> = None;
    for pair in header.split(';') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key.trim() != name {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(value.trim());
    }
    found
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use moe_chat_app::{Action, AppSnapshot, ChatSnapshot, ChatStatus};

    use super::*;

    const INDEX: &[u8] = b"<!doctype html><title>fixture</title>";
    const APP_JS: &[u8] = b"console.log('fixture');";
    const STYLE_CSS: &[u8] = b"body{color:#252B3D}";

    struct Adapter {
        snapshot: AppSnapshot,
        dispatched: Vec<&'static str>,
    }

    impl Adapter {
        fn new() -> Self {
            Self {
                snapshot: AppSnapshot {
                    revision: 1,
                    mode: "offline",
                    companion: "moe",
                    active_preset: String::new(),
                    external_transmission_allowed: false,
                    revoked: false,
                    panels: Vec::new(),
                    chat: ChatSnapshot {
                        draft: String::new(),
                        messages: Vec::new(),
                        status: ChatStatus::Ready,
                        failure: None,
                    },
                    presets: Vec::new(),
                    memories: Vec::new(),
                    diagnostics: Vec::new(),
                    notice: None,
                },
                dispatched: Vec::new(),
            }
        }
    }

    impl ApplicationPort for Adapter {
        fn snapshot(&self) -> AppSnapshot {
            self.snapshot.clone()
        }

        fn dispatch(&mut self, action: Action, _now_ms: u64) -> Result<(), PublicError> {
            self.dispatched.push(match action {
                Action::Send => "send",
                Action::Revoke => "revoke",
                _ => "other",
            });
            self.snapshot.revision += 1;
            Ok(())
        }
    }

    fn assets() -> Assets {
        Assets::new(INDEX, APP_JS, STYLE_CSS)
    }

    fn fast_config() -> Config {
        Config {
            read_timeout: Duration::from_millis(150),
            write_timeout: Duration::from_millis(500),
            ledger_capacity: 2,
        }
    }

    fn port(server: &LoopbackServer) -> u16 {
        server.listener.local_addr().expect("local addr").port()
    }

    /// Writes the request first and then drives the single connection: the listener accepts
    /// what the kernel already queued, so no client thread is needed for small requests.
    fn round_trip(server: &mut LoopbackServer, adapter: &mut Adapter, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port(server))).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        stream.write_all(request.as_bytes()).expect("write");
        server.serve_once(adapter).expect("serve");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        response
    }

    fn headers(response: &str) -> &str {
        response.split_once("\r\n\r\n").expect("head").0
    }

    fn body(response: &str) -> &str {
        response.split_once("\r\n\r\n").expect("head").1
    }

    fn session(server: &mut LoopbackServer, adapter: &mut Adapter) -> String {
        let host = format!("127.0.0.1:{}", port(server));
        let response = round_trip(
            server,
            adapter,
            &format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        headers(&response)
            .lines()
            .find_map(|line| line.strip_prefix("Set-Cookie: "))
            .expect("session cookie")
            .split(';')
            .next()
            .expect("cookie pair")
            .to_owned()
    }

    fn csrf(server: &mut LoopbackServer, adapter: &mut Adapter, cookie: &str) -> String {
        let host = format!("127.0.0.1:{}", port(server));
        let response = round_trip(
            server,
            adapter,
            &format!("GET /api/bootstrap HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n"),
        );
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        body(&response)
            .split("\"csrf\":\"")
            .nth(1)
            .expect("csrf token")
            .split('"')
            .next()
            .expect("token end")
            .to_owned()
    }

    fn action_request(server: &LoopbackServer, cookie: &str, token: &str, payload: &str) -> String {
        let host = format!("127.0.0.1:{}", port(server));
        format!(
            "POST /api/action HTTP/1.1\r\nHost: {host}\r\nOrigin: {}\r\nCookie: {cookie}\r\n\
X-CSRF-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
            server.origin(),
            payload.len()
        )
    }

    fn server() -> LoopbackServer {
        LoopbackServer::bind_with(0, assets(), fast_config()).expect("bind")
    }

    #[test]
    fn a_client_that_never_finishes_its_request_is_cut_off_by_the_deadline() {
        let mut server = server();
        let mut adapter = Adapter::new();
        let mut stream = TcpStream::connect(("127.0.0.1", port(&server))).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: ")
            .expect("partial write");
        let started = Instant::now();
        server.serve_once(&mut adapter).expect("serve");
        let elapsed = started.elapsed();
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        assert!(response.starts_with("HTTP/1.1 408 "), "{response}");
        assert!(elapsed >= Duration::from_millis(100), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }

    #[test]
    fn an_abandoned_connection_does_not_stop_the_serve_loop() {
        let mut server = server();
        let mut adapter = Adapter::new();
        {
            let mut abandoned = TcpStream::connect(("127.0.0.1", port(&server))).expect("connect");
            abandoned
                .write_all(b"GET / HTTP/1.1\r\nHost: ")
                .expect("write");
        }
        server.serve_once(&mut adapter).expect("abandoned");
        let request = format!(
            "GET /app.js HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            port(&server)
        );
        let response = round_trip(&mut server, &mut adapter, &request);
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        assert_eq!(body(&response), String::from_utf8_lossy(APP_JS));
    }

    #[test]
    fn the_session_cookie_is_required_before_a_snapshot_is_returned() {
        let mut server = server();
        let mut adapter = Adapter::new();
        let host = format!("127.0.0.1:{}", port(&server));

        let anonymous = round_trip(
            &mut server,
            &mut adapter,
            &format!("GET /api/state HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert!(anonymous.starts_with("HTTP/1.1 401 "), "{anonymous}");
        assert!(!anonymous.contains("snapshot"));

        let cookie = session(&mut server, &mut adapter);
        let authenticated = round_trip(
            &mut server,
            &mut adapter,
            &format!("GET /api/state HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n"),
        );
        assert!(
            authenticated.starts_with("HTTP/1.1 200 "),
            "{authenticated}"
        );
        assert!(body(&authenticated).contains("\"snapshot\""));
    }

    #[test]
    fn a_full_ledger_refuses_new_ids_without_dispatching_them() {
        let mut server = server();
        let mut adapter = Adapter::new();
        let cookie = session(&mut server, &mut adapter);
        let token = csrf(&mut server, &mut adapter, &cookie);

        let first_request =
            action_request(&server, &cookie, &token, r#"{"id":"one","action":"send"}"#);
        let first = round_trip(&mut server, &mut adapter, &first_request);
        assert!(first.starts_with("HTTP/1.1 200 "), "{first}");
        let second_request =
            action_request(&server, &cookie, &token, r#"{"id":"two","action":"send"}"#);
        let second = round_trip(&mut server, &mut adapter, &second_request);
        assert!(second.starts_with("HTTP/1.1 200 "), "{second}");

        let third_request = action_request(
            &server,
            &cookie,
            &token,
            r#"{"id":"three","action":"send"}"#,
        );
        let third = round_trip(&mut server, &mut adapter, &third_request);
        assert!(third.starts_with("HTTP/1.1 429 "), "{third}");
        assert!(body(&third).contains("\"ledger_full\""), "{third}");
        let replay_request =
            action_request(&server, &cookie, &token, r#"{"id":"one","action":"send"}"#);
        let replay = round_trip(&mut server, &mut adapter, &replay_request);
        assert!(replay.starts_with("HTTP/1.1 409 "), "{replay}");
        assert_eq!(adapter.dispatched, vec!["send", "send"]);
    }

    #[test]
    fn content_type_and_cookie_helpers_are_strict_but_practical() {
        assert!(json_content_type(Some("application/json")));
        assert!(json_content_type(Some("application/json; charset=UTF-8")));
        assert!(!json_content_type(Some("text/plain")));
        assert!(!json_content_type(Some("application/json; charset=latin1")));
        assert!(!json_content_type(None));

        assert_eq!(
            cookie_value("a=1; moe_chat_session=abc", "moe_chat_session"),
            Some("abc")
        );
        assert_eq!(
            cookie_value("moe_chat_session=abc", "moe_chat_session"),
            Some("abc")
        );
        assert_eq!(
            cookie_value(
                "moe_chat_session=abc; moe_chat_session=def",
                "moe_chat_session"
            ),
            None
        );
        assert_eq!(cookie_value("other=1", "moe_chat_session"), None);
    }
}
