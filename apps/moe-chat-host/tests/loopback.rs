//! Real loopback socket tests for the browser presentation protocol.
//!
//! Every test binds a real `127.0.0.1` listener through the public API, drives a real TCP
//! client and asserts on the raw HTTP bytes. The application behind the port is a synthetic
//! adapter with a fixed snapshot except where a test drives the real `ChatApplication`; the
//! wire assertions prove transport and policy behaviour, not business coverage.

use std::io::{Read, Write};
use std::marker::PhantomData;
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

use moe_chat_app::{
    Action, AppSnapshot, ChatApplication, ChatMessage, ChatSnapshot, ChatStatus,
    DiagnosticSnapshot, MemorySnapshot, MessageRole, PanelKind, PanelSnapshot, PanelVisibility,
    PresetSnapshot,
};
use moe_chat_host::{ApplicationPort, Assets, LoopbackServer, PublicError};
use serde_json::Value;

const INDEX: &[u8] = b"<!doctype html><html><body>fixture shell</body></html>";
const APP_JS: &[u8] = b"console.log('fixture app');";
const STYLE_CSS: &[u8] = b"body{color:#252B3D}";
/// Stands in for an internal cause that must never reach the browser.
const INTERNAL_CAUSE: &str = "internal-secret-cause";
/// Header budget of the host, mirrored here so the socket regressions pin the boundary.
const HEAD_BUDGET: usize = 8 * 1024;
/// Largest single header value the host accepts.
const FILL_FIELD_VALUE_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy)]
enum Outcome {
    Accept,
    Refuse(PublicError),
}

struct Adapter {
    snapshot: AppSnapshot,
    dispatched: Vec<String>,
    outcome: Outcome,
    /// The host must accept an application that is not `Send`: a raw-pointer marker keeps
    /// this adapter non-`Send` at compile time.
    _not_send: PhantomData<*const ()>,
}

impl Adapter {
    fn new() -> Self {
        Self {
            snapshot: AppSnapshot {
                revision: 1,
                mode: "offline",
                companion: "moe",
                active_preset: "offline-default".to_owned(),
                external_transmission_allowed: false,
                revoked: false,
                notice: None,
                panels: vec![PanelSnapshot {
                    kind: PanelKind::Chat,
                    visibility: PanelVisibility::Visible,
                    confirmation: false,
                    mounts: 1,
                    renders: 2,
                    unmounts: 0,
                }],
                chat: ChatSnapshot {
                    draft: "hello".to_owned(),
                    messages: vec![ChatMessage {
                        role: MessageRole::User,
                        text: "hi".to_owned(),
                    }],
                    status: ChatStatus::Ready,
                    failure: None,
                },
                presets: vec![PresetSnapshot {
                    id: "offline-default".to_owned(),
                    label: "Offline".to_owned(),
                    provider: "none".to_owned(),
                    model: "fixture".to_owned(),
                    endpoint: "memory".to_owned(),
                }],
                memories: vec![MemorySnapshot {
                    revision: 1,
                    id: "m1".to_owned(),
                    text: "likes tea".to_owned(),
                }],
                diagnostics: vec![DiagnosticSnapshot {
                    sequence: 3,
                    run: "run-1".to_owned(),
                    event: "dispatch".to_owned(),
                    status: "completed".to_owned(),
                }],
            },
            dispatched: Vec::new(),
            outcome: Outcome::Accept,
            _not_send: PhantomData,
        }
    }
}

impl ApplicationPort for Adapter {
    fn snapshot(&self) -> AppSnapshot {
        self.snapshot.clone()
    }

    fn dispatch(&mut self, action: Action, now_ms: u64) -> Result<(), PublicError> {
        self.dispatched.push(describe(&action));
        match self.outcome {
            Outcome::Accept => {
                assert!(
                    now_ms < 3_600_000,
                    "host time must be monotonic milliseconds"
                );
                self.snapshot.revision += 1;
                Ok(())
            }
            Outcome::Refuse(error) => Err(error),
        }
    }
}

fn describe(action: &Action) -> String {
    match action {
        Action::Open(panel) => format!("open:{}", panel.as_str()),
        Action::Hide(panel) => format!("hide:{}", panel.as_str()),
        Action::Close(panel) => format!("close:{}", panel.as_str()),
        Action::ConfirmClose { panel, discard } => {
            format!("confirm_close:{}:{discard}", panel.as_str())
        }
        Action::SetDraft(text) => format!("set_draft:{text}"),
        Action::Send => "send".to_owned(),
        Action::SelectPreset(preset) => format!("select_preset:{preset}"),
        Action::Remember(text) => format!("remember:{text}"),
        Action::RefreshMemories => "refresh_memories".to_owned(),
        Action::RefreshDiagnostics => "refresh_diagnostics".to_owned(),
        Action::Revoke => "revoke".to_owned(),
        Action::SearchMemories(_) => "search_memories".to_owned(),
        Action::UpdateMemory { .. } => "update_memory".to_owned(),
        Action::DeleteMemory { .. } => "delete_memory".to_owned(),
    }
}

struct RealAdapter(ChatApplication);

impl ApplicationPort for RealAdapter {
    fn snapshot(&self) -> AppSnapshot {
        self.0.snapshot()
    }

    fn dispatch(&mut self, action: Action, now_ms: u64) -> Result<(), PublicError> {
        self.0
            .dispatch(action, now_ms)
            .map_err(|error| PublicError::new(error.public_code(), error.public_message()))
    }
}

struct Harness<A> {
    server: LoopbackServer,
    adapter: A,
}

impl Harness<Adapter> {
    fn start() -> Self {
        Self::start_with(|_| Adapter::new())
    }
}

impl<A: ApplicationPort> Harness<A> {
    fn start_with(application: impl FnOnce(u64) -> A) -> Self {
        let server = LoopbackServer::bind(0, Assets::new(INDEX, APP_JS, STYLE_CSS))
            .expect("bind loopback listener");
        let adapter = application(server.process_generation());
        Self { server, adapter }
    }

    fn port(&self) -> u16 {
        self.server
            .origin()
            .rsplit(':')
            .next()
            .expect("origin port")
            .parse()
            .expect("numeric port")
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port())
    }

    fn origin(&self) -> String {
        self.server.origin().to_owned()
    }

    /// Runs one client exchange and the matching single-shot serve call concurrently, so
    /// large request bodies can never fill the socket buffers before the server reads.
    fn exchange(&mut self, request: Vec<u8>) -> String {
        let port = self.port();
        let server = &mut self.server;
        let adapter = &mut self.adapter;
        let response = thread::scope(|scope| {
            let client = scope.spawn(move || send(port, &request));
            server
                .serve_once(adapter)
                .expect("serve one loopback connection");
            client.join().expect("client thread")
        });
        String::from_utf8(response).expect("utf-8 response")
    }

    fn exchange_parts(&mut self, parts: Vec<Vec<u8>>, gap: Duration) -> String {
        let port = self.port();
        let server = &mut self.server;
        let adapter = &mut self.adapter;
        let response = thread::scope(|scope| {
            let client = scope.spawn(move || send_parts(port, &parts, gap));
            server
                .serve_once(adapter)
                .expect("serve one loopback connection");
            client.join().expect("client thread")
        });
        String::from_utf8(response).expect("utf-8 response")
    }

    /// Sends a `GET` for this listener's authority.
    fn get(&mut self, path: &str, headers: &[(&str, &str)]) -> String {
        let host = self.host();
        self.exchange(get(path, &host, headers))
    }

    /// Opens the shell and returns the session cookie pair (`name=value`).
    fn session(&mut self) -> String {
        let response = self.get("/", &[]);
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        header(&response, "set-cookie")
            .expect("session cookie")
            .split(';')
            .next()
            .expect("cookie pair")
            .to_owned()
    }

    fn csrf(&mut self, cookie: &str) -> String {
        let response = self.get("/api/bootstrap", &[("Cookie", cookie)]);
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        json(&response)["csrf"]
            .as_str()
            .expect("csrf token")
            .to_owned()
    }

    fn post_action(&mut self, cookie: &str, csrf: &str, payload: &str) -> String {
        let host = self.host();
        let origin = self.origin();
        let request = raw(
            "POST",
            "/api/action",
            &host,
            &[
                ("Origin", origin.as_str()),
                ("Cookie", cookie),
                ("X-CSRF-Token", csrf),
                ("Content-Type", "application/json"),
            ],
            payload.as_bytes(),
        );
        self.exchange(request)
    }

    fn authenticated(&mut self) -> (String, String) {
        let cookie = self.session();
        let csrf = self.csrf(&cookie);
        (cookie, csrf)
    }
}

fn send(port: u16, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("client read timeout");
    stream.write_all(request).expect("write request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn send_parts(port: u16, parts: &[Vec<u8>], gap: Duration) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("client read timeout");
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            thread::sleep(gap);
        }
        stream.write_all(part).expect("write request part");
        stream.flush().expect("flush request part");
    }
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn raw(method: &str, path: &str, host: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut bytes = request.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn get(path: &str, host: &str, headers: &[(&str, &str)]) -> Vec<u8> {
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n");
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.into_bytes()
}

fn pad_head_to(mut head: String, head_bytes: usize) -> String {
    let mut remaining = head_bytes - 2 - head.len();
    let mut index = 0;
    while remaining > 0 {
        index += 1;
        let prefix = format!("X-Fill-{index}: ");
        assert!(
            remaining > prefix.len() + 2,
            "padding cannot reach the requested head size"
        );
        let line = remaining.min(prefix.len() + 2 + FILL_FIELD_VALUE_BYTES);
        head.push_str(&prefix);
        head.push_str(&"a".repeat(line - prefix.len() - 2));
        head.push_str("\r\n");
        remaining -= line;
    }
    head.push_str("\r\n");
    assert_eq!(head.len(), head_bytes, "padded head size");
    head
}

fn padded_action_request(
    host: &str,
    origin: &str,
    cookie: &str,
    csrf: &str,
    payload: &str,
    head_bytes: usize,
) -> Vec<u8> {
    let head = pad_head_to(
        format!(
            "POST /api/action HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\n\
X-CSRF-Token: {csrf}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ),
        head_bytes,
    );
    let mut request = head.into_bytes();
    request.extend_from_slice(payload.as_bytes());
    request
}

fn padded_get_request(host: &str, head_bytes: usize) -> Vec<u8> {
    pad_head_to(format!("GET / HTTP/1.1\r\nHost: {host}\r\n"), head_bytes).into_bytes()
}

fn head(response: &str) -> &str {
    response.split_once("\r\n\r\n").expect("response head").0
}

fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").expect("response head").1
}

fn status(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status")
}

fn header(response: &str, name: &str) -> Option<String> {
    head(response).lines().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        field
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

fn json(response: &str) -> Value {
    serde_json::from_str(body(response)).expect("json response body")
}

fn public_error(response: &str) -> (String, String) {
    let value = json(response);
    let error = &value["error"];
    (
        error["code"].as_str().expect("error code").to_owned(),
        error["message"].as_str().expect("error message").to_owned(),
    )
}

#[test]
fn static_shell_is_served_with_a_hardened_session_cookie() {
    let mut harness = Harness::start();
    let response = harness.get("/", &[]);

    assert_eq!(status(&response), 200);
    assert_eq!(body(&response), String::from_utf8_lossy(INDEX));
    assert_eq!(
        header(&response, "content-type").as_deref(),
        Some("text/html; charset=utf-8")
    );
    let cookie = header(&response, "set-cookie").expect("session cookie");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert_eq!(
        header(&response, "cache-control").as_deref(),
        Some("no-store, no-cache")
    );
    assert_eq!(
        header(&response, "x-content-type-options").as_deref(),
        Some("nosniff")
    );
    assert_eq!(
        header(&response, "referrer-policy").as_deref(),
        Some("no-referrer")
    );
    let csp = header(&response, "content-security-policy").expect("csp");
    assert!(csp.contains("script-src 'self'"), "{csp}");
    assert!(csp.contains("style-src 'self'"), "{csp}");
    assert!(csp.contains("connect-src 'self'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(csp.contains("object-src 'none'"), "{csp}");
    assert!(header(&response, "access-control-allow-origin").is_none());
}

#[test]
fn static_assets_come_from_injected_bytes_and_unknown_paths_are_refused() {
    let mut harness = Harness::start();

    let script = harness.get("/app.js", &[]);
    assert_eq!(status(&script), 200);
    assert_eq!(body(&script), String::from_utf8_lossy(APP_JS));
    assert_eq!(
        header(&script, "content-type").as_deref(),
        Some("text/javascript; charset=utf-8")
    );

    let style = harness.get("/style.css", &[]);
    assert_eq!(status(&style), 200);
    assert_eq!(body(&style), String::from_utf8_lossy(STYLE_CSS));
    assert_eq!(
        header(&style, "content-type").as_deref(),
        Some("text/css; charset=utf-8")
    );

    let missing = harness.get("/secrets.txt", &[]);
    assert_eq!(status(&missing), 404);
    assert_eq!(public_error(&missing).0, "not_found");
    assert!(!missing.contains("snapshot"));

    let method = harness.get("/api/action", &[]);
    assert_eq!(status(&method), 405);
    assert_eq!(header(&method, "allow").as_deref(), Some("POST"));
}

#[test]
fn bootstrap_and_state_require_the_session_and_report_the_snapshot_shape() {
    let mut harness = Harness::start();

    let anonymous_bootstrap = harness.get("/api/bootstrap", &[]);
    assert_eq!(status(&anonymous_bootstrap), 401);
    assert_eq!(public_error(&anonymous_bootstrap).0, "unauthenticated");
    assert!(!anonymous_bootstrap.contains("snapshot"));
    assert!(!anonymous_bootstrap.contains("csrf"));

    let anonymous_state = harness.get("/api/state", &[]);
    assert_eq!(status(&anonymous_state), 401);
    assert!(!anonymous_state.contains("snapshot"));
    assert!(!anonymous_state.contains("likes tea"));

    let cookie = harness.session();
    let bootstrap = harness.get("/api/bootstrap", &[("Cookie", cookie.as_str())]);
    assert_eq!(status(&bootstrap), 200);
    let value = json(&bootstrap);
    let csrf = value["csrf"].as_str().expect("csrf token").to_owned();
    assert_eq!(csrf.len(), 64);
    assert!(csrf.chars().all(|character| character.is_ascii_hexdigit()));
    let snapshot = &value["snapshot"];
    assert_eq!(snapshot["mode"], "offline");
    assert_eq!(snapshot["external_transmission_allowed"], false);
    assert_eq!(snapshot["panels"][0]["kind"], "chat");
    assert_eq!(snapshot["panels"][0]["visibility"], "visible");
    assert_eq!(snapshot["chat"]["status"], "ready");
    assert_eq!(snapshot["presets"][0]["endpoint"], "memory");
    assert_eq!(snapshot["memories"][0]["text"], "likes tea");
    assert_eq!(snapshot["diagnostics"][0]["event"], "dispatch");

    let state = harness.get("/api/state", &[("Cookie", cookie.as_str())]);
    assert_eq!(status(&state), 200);
    assert_eq!(json(&state)["snapshot"]["revision"], 1);
    assert!(json(&state).get("csrf").is_none());
    assert!(!state.contains(csrf.as_str()));
}

#[test]
fn host_and_origin_must_match_the_exact_loopback_authority() {
    let mut harness = Harness::start();
    let port = harness.port();

    for hostile_host in [
        format!("localhost:{port}"),
        "127.0.0.1:1".to_owned(),
        "evil.example".to_owned(),
        format!("127.0.0.1:{port}.evil.example"),
    ] {
        let response = harness.exchange(get("/", &hostile_host, &[]));
        assert_eq!(status(&response), 403, "host {hostile_host}");
        assert_eq!(public_error(&response).0, "cross_site");
        assert!(!response.contains("snapshot"));
    }

    let no_host = harness.exchange(b"GET / HTTP/1.1\r\n\r\n".to_vec());
    assert_eq!(status(&no_host), 400);

    let cross_site = harness.get("/api/state", &[("Origin", "http://evil.example")]);
    assert_eq!(status(&cross_site), 403);
    assert_eq!(public_error(&cross_site).0, "cross_site");

    let null_origin = harness.get("/api/state", &[("Origin", "null")]);
    assert_eq!(status(&null_origin), 403);

    let origin = harness.origin();
    let same_origin = harness.get("/", &[("Origin", origin.as_str())]);
    assert_eq!(status(&same_origin), 200);
}

#[test]
fn an_action_needs_origin_session_and_csrf_before_it_can_execute() {
    let mut harness = Harness::start();
    let host = harness.host();
    let origin = harness.origin();
    let cookie = harness.session();
    let csrf = harness.csrf(&cookie);
    let payload = r#"{"id":"gated","action":"send"}"#;
    let json_type = ("Content-Type", "application/json");

    let without_origin = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Cookie", cookie.as_str()),
            ("X-CSRF-Token", csrf.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&without_origin), 403);
    assert_eq!(public_error(&without_origin).0, "cross_site");

    let foreign_origin = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", "http://evil.example"),
            ("Cookie", cookie.as_str()),
            ("X-CSRF-Token", csrf.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&foreign_origin), 403);

    let without_cookie = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", origin.as_str()),
            ("X-CSRF-Token", csrf.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&without_cookie), 401);
    assert!(!without_cookie.contains("snapshot"));

    let forged_cookie = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", origin.as_str()),
            ("Cookie", "moe_chat_session=deadbeef"),
            ("X-CSRF-Token", csrf.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&forged_cookie), 401);

    let without_csrf = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", origin.as_str()),
            ("Cookie", cookie.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&without_csrf), 403);
    assert_eq!(public_error(&without_csrf).0, "csrf_rejected");

    let zero_token = "0".repeat(64);
    let wrong_csrf = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", origin.as_str()),
            ("Cookie", cookie.as_str()),
            ("X-CSRF-Token", zero_token.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&wrong_csrf), 403);

    assert!(harness.adapter.dispatched.is_empty());

    let wrong_type = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", origin.as_str()),
            ("Cookie", cookie.as_str()),
            ("X-CSRF-Token", csrf.as_str()),
            ("Content-Type", "text/plain"),
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&wrong_type), 415);
    assert_eq!(public_error(&wrong_type).0, "unsupported_media_type");
    assert!(harness.adapter.dispatched.is_empty());
}

#[test]
fn an_accepted_action_returns_the_fresh_snapshot_and_dispatches_once() {
    let mut harness = Harness::start();
    let (cookie, csrf) = harness.authenticated();

    let response = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"act-1","action":"set_draft","text":"hello"}"#,
    );
    assert_eq!(status(&response), 200);
    assert_eq!(json(&response)["snapshot"]["revision"], 2);

    let panel = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"act-2","action":"confirm_close","panel":"chat","discard":false}"#,
    );
    assert_eq!(status(&panel), 200);
    let remember = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"act-3","action":"remember","text":"likes tea"}"#,
    );
    assert_eq!(status(&remember), 200);
    let revoke = harness.post_action(&cookie, &csrf, r#"{"id":"act-4","action":"revoke"}"#);
    assert_eq!(status(&revoke), 200);
    assert_eq!(
        harness.adapter.dispatched,
        vec![
            "set_draft:hello",
            "confirm_close:chat:false",
            "remember:likes tea",
            "revoke"
        ]
    );
}

#[test]
fn a_replayed_request_id_is_refused_without_a_second_dispatch() {
    let mut harness = Harness::start();
    let (cookie, csrf) = harness.authenticated();
    let payload = r#"{"id":"replay-1","action":"send"}"#;

    let first = harness.post_action(&cookie, &csrf, payload);
    assert_eq!(status(&first), 200);
    let replay = harness.post_action(&cookie, &csrf, payload);
    assert_eq!(status(&replay), 409);
    let (code, message) = public_error(&replay);
    assert_eq!(code, "duplicate_request");
    assert!(!message.is_empty());
    assert_eq!(json(&replay)["snapshot"]["revision"], 2);
    assert_eq!(harness.adapter.dispatched, vec!["send"]);
}

#[test]
fn rejected_bodies_never_dispatch() {
    let mut harness = Harness::start();
    let (cookie, csrf) = harness.authenticated();

    let cases: [(String, &str); 8] = [
        (
            r#"{"id":"bad-1","action":"send""#.to_owned(),
            "malformed_body",
        ),
        (
            r#"{"id":"bad-2","action":"delete_everything"}"#.to_owned(),
            "unknown_action",
        ),
        (
            r#"{"id":"bad-3","action":"send","extra":"x"}"#.to_owned(),
            "unknown_field",
        ),
        (
            r#"{"id":"bad-4","action":"set_draft","text":5}"#.to_owned(),
            "invalid_request",
        ),
        (
            r#"{"id":"bad-5","action":"remember","text":"   "}"#.to_owned(),
            "invalid_text",
        ),
        (
            r#"{"id":"bad-6","action":"open","panel":"unknown"}"#.to_owned(),
            "invalid_request",
        ),
        (
            format!(
                r#"{{"id":"bad-7","action":"set_draft","text":"{}"}}"#,
                "a".repeat(8_001)
            ),
            "invalid_text",
        ),
        (
            format!(r#"{{"id":"{}","action":"send"}}"#, "a".repeat(65)),
            "invalid_request",
        ),
    ];

    for (payload, expected_code) in &cases {
        let response = harness.post_action(&cookie, &csrf, payload);
        assert_eq!(status(&response), 400, "{payload}");
        assert_eq!(public_error(&response).0, *expected_code, "{payload}");
    }
    assert!(harness.adapter.dispatched.is_empty());
}

#[test]
fn escaped_text_at_the_application_byte_limit_is_still_one_legal_request() {
    let mut harness = Harness::start();
    let (cookie, csrf) = harness.authenticated();

    let escaped = r"\u0041".repeat(8_000);
    let payload = format!(r#"{{"id":"escaped","action":"remember","text":"{escaped}"}}"#);
    assert!(payload.len() > 8_000);
    let response = harness.post_action(&cookie, &csrf, &payload);
    assert_eq!(status(&response), 200, "{response}");
    let dispatched = harness.adapter.dispatched.first().expect("one dispatch");
    assert_eq!(dispatched.len(), "remember:".len() + 8_000);
}

#[test]
fn ambiguous_and_oversized_http_is_refused_before_routing() {
    let mut harness = Harness::start();
    let host = harness.host();
    let origin = harness.origin();

    let chunked = harness.exchange(
        format!(
            "POST /api/action HTTP/1.1\r\nHost: {host}\r\nTransfer-Encoding: chunked\r\nContent-Length: 5\r\n\r\nhello"
        )
        .into_bytes(),
    );
    assert_eq!(status(&chunked), 400);
    assert_eq!(public_error(&chunked).0, "ambiguous_request");

    let repeated_length = harness.exchange(
        format!(
            "POST /api/action HTTP/1.1\r\nHost: {host}\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\nhello"
        )
        .into_bytes(),
    );
    assert_eq!(status(&repeated_length), 400);

    let listed_length = harness.exchange(
        format!("POST /api/action HTTP/1.1\r\nHost: {host}\r\nContent-Length: 5, 5\r\n\r\nhello")
            .into_bytes(),
    );
    assert_eq!(status(&listed_length), 400);

    let padded_length = harness.exchange(
        format!("POST /api/action HTTP/1.1\r\nHost: {host}\r\nContent-Length: 005\r\n\r\nhello")
            .into_bytes(),
    );
    assert_eq!(status(&padded_length), 400);

    let get_with_body = harness.exchange(
        format!("GET / HTTP/1.1\r\nHost: {host}\r\nContent-Length: 5\r\n\r\nhello").into_bytes(),
    );
    assert_eq!(status(&get_with_body), 400);

    let missing_length = harness.exchange(
        format!("POST /api/action HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\n\r\n")
            .into_bytes(),
    );
    assert_eq!(status(&missing_length), 411);

    let oversized = harness.exchange(
        format!("POST /api/action HTTP/1.1\r\nHost: {host}\r\nContent-Length: 65537\r\n\r\n")
            .into_bytes(),
    );
    assert_eq!(status(&oversized), 413);
    assert_eq!(public_error(&oversized).0, "payload_too_large");
    assert!(!oversized.contains("snapshot"));

    let old_version =
        harness.exchange(format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n").into_bytes());
    assert_eq!(status(&old_version), 505);

    let other_method = harness.exchange(
        format!("PUT /api/action HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n")
            .into_bytes(),
    );
    assert_eq!(status(&other_method), 405);
    assert_eq!(header(&other_method, "allow").as_deref(), Some("GET, POST"));

    let folded = harness
        .exchange(format!("GET / HTTP/1.1\r\nHost: {host}\r\n X-Folded: yes\r\n\r\n").into_bytes());
    assert_eq!(status(&folded), 400);

    let mut flood = format!("GET / HTTP/1.1\r\nHost: {host}\r\n");
    for index in 0..400 {
        flood.push_str(&format!("X-Filler-{index}: {}\r\n", "a".repeat(64)));
    }
    flood.push_str("\r\n");
    let too_large = harness.exchange(flood.into_bytes());
    assert_eq!(status(&too_large), 431);

    assert!(harness.adapter.dispatched.is_empty());
}

#[test]
fn business_refusals_carry_only_the_public_error_and_a_snapshot() {
    let mut harness = Harness::start();
    let (cookie, csrf) = harness.authenticated();
    harness.adapter.outcome = Outcome::Refuse(PublicError::new("denied", "not permitted here"));

    let response = harness.post_action(&cookie, &csrf, r#"{"id":"denied-1","action":"send"}"#);
    assert_eq!(status(&response), 400);
    let (code, message) = public_error(&response);
    assert_eq!(code, "denied");
    assert_eq!(message, "not permitted here");
    assert_eq!(json(&response)["snapshot"]["revision"], 1);
    assert!(!response.contains(INTERNAL_CAUSE));
    assert_eq!(harness.adapter.dispatched, vec!["send"]);
}

#[test]
fn secrets_never_leak_outside_the_cookie_and_the_bootstrap_answer() {
    let mut harness = Harness::start();
    let cookie = harness.session();
    let session_token = cookie.split_once('=').expect("cookie pair").1.to_owned();
    assert_eq!(session_token.len(), 64);

    let bootstrap = harness.get("/api/bootstrap", &[("Cookie", cookie.as_str())]);
    let csrf = json(&bootstrap)["csrf"].as_str().expect("csrf").to_owned();
    assert_ne!(csrf, session_token);

    let state = harness.get("/api/state", &[("Cookie", cookie.as_str())]);
    assert!(!state.contains(session_token.as_str()));
    assert!(!state.contains(csrf.as_str()));

    let denied = harness.get("/api/state", &[]);
    assert!(!denied.contains(session_token.as_str()));
    assert!(!denied.contains(csrf.as_str()));
    assert!(!denied.contains("likes tea"));

    let shell = harness.get("/", &[]);
    assert!(header(&shell, "set-cookie").is_some());
    assert!(!body(&shell).contains(session_token.as_str()));
    assert!(!body(&shell).contains(csrf.as_str()));
}

#[test]
fn a_silent_client_is_cut_off_by_the_absolute_deadline() {
    let mut harness = Harness::start();
    let port = harness.port();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("client read timeout");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1")
        .expect("partial request");

    let started = Instant::now();
    harness
        .server
        .serve_once(&mut harness.adapter)
        .expect("serve one connection");
    let elapsed = started.elapsed();

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert_eq!(status(&response), 408);
    assert_eq!(public_error(&response).0, "timeout");
    assert!(elapsed >= Duration::from_secs(4), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(15), "{elapsed:?}");
}

#[test]
fn an_abandoned_connection_does_not_stop_the_serve_loop() {
    let mut harness = Harness::start();
    let port = harness.port();
    {
        let mut abandoned = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
        abandoned
            .write_all(b"POST /api/action HTTP/1.1\r\nHost: ")
            .expect("partial request");
    }
    harness
        .server
        .serve_once(&mut harness.adapter)
        .expect("abandoned connection is not fatal");

    let response = harness.get("/", &[]);
    assert_eq!(status(&response), 200);
    assert!(harness.adapter.dispatched.is_empty());
}

#[test]
fn origin_and_process_generation_identify_the_listener() {
    let harness = Harness::start();
    let port = harness.port();
    assert_eq!(harness.server.origin(), format!("http://127.0.0.1:{port}"));
    assert_ne!(harness.server.process_generation(), 0);

    let other = LoopbackServer::bind(0, Assets::new(INDEX, APP_JS, STYLE_CSS)).expect("bind");
    assert_ne!(
        harness.server.process_generation(),
        other.process_generation()
    );
}

#[test]
fn clearing_the_draft_reaches_the_real_application_while_blank_send_is_its_own_decision() {
    let mut harness = Harness::start_with(|generation| {
        RealAdapter(ChatApplication::new(generation).expect("real application"))
    });
    let (cookie, csrf) = harness.authenticated();

    let opened = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"draft-open","action":"open","panel":"chat"}"#,
    );
    assert_eq!(status(&opened), 200, "{opened}");

    let typed = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"draft-typed","action":"set_draft","text":"hello"}"#,
    );
    assert_eq!(status(&typed), 200, "{typed}");
    assert_eq!(json(&typed)["snapshot"]["chat"]["draft"], "hello");

    let cleared = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"draft-clear","action":"set_draft","text":""}"#,
    );
    assert_eq!(status(&cleared), 200, "{cleared}");
    assert_eq!(json(&cleared)["snapshot"]["chat"]["draft"], "");

    let blank_send = harness.post_action(&cookie, &csrf, r#"{"id":"draft-send","action":"send"}"#);
    assert_eq!(status(&blank_send), 400, "{blank_send}");
    assert_eq!(public_error(&blank_send).0, "empty_input");
    assert!(
        json(&blank_send)["snapshot"]["chat"]["messages"]
            .as_array()
            .expect("messages")
            .is_empty()
    );

    let whitespace = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"draft-space","action":"set_draft","text":"   "}"#,
    );
    assert_eq!(status(&whitespace), 200, "{whitespace}");
    assert_eq!(json(&whitespace)["snapshot"]["chat"]["draft"], "   ");

    let whitespace_send = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"draft-space-send","action":"send"}"#,
    );
    assert_eq!(status(&whitespace_send), 400, "{whitespace_send}");
    assert_eq!(public_error(&whitespace_send).0, "empty_input");

    let blank_memory = harness.post_action(
        &cookie,
        &csrf,
        r#"{"id":"draft-memory","action":"remember","text":"   "}"#,
    );
    assert_eq!(status(&blank_memory), 400, "{blank_memory}");
    assert_eq!(public_error(&blank_memory).0, "invalid_text");
}

#[test]
fn draft_boundary_refusals_never_reach_the_application() {
    let mut harness = Harness::start();
    let (cookie, csrf) = harness.authenticated();

    let cases = [
        (
            r#"{"id":"draft-a","action":"set_draft","text":""}"#.to_owned(),
            None,
        ),
        (
            r#"{"id":"draft-b","action":"set_draft","text":"   "}"#.to_owned(),
            None,
        ),
        (
            r#"{"id":"draft-c","action":"set_draft","text":5}"#.to_owned(),
            Some("invalid_request"),
        ),
        (
            r#"{"id":"draft-d","action":"set_draft","text":"","extra":"x"}"#.to_owned(),
            Some("unknown_field"),
        ),
        (
            format!(
                r#"{{"id":"draft-e","action":"set_draft","text":"{}"}}"#,
                "a".repeat(8_001)
            ),
            Some("invalid_text"),
        ),
        (
            r#"{"id":"draft-f","action":"remember","text":"   "}"#.to_owned(),
            Some("invalid_text"),
        ),
    ];

    for (payload, expected) in &cases {
        let response = harness.post_action(&cookie, &csrf, payload);
        match expected {
            None => assert_eq!(status(&response), 200, "{payload}"),
            Some(code) => {
                assert_eq!(status(&response), 400, "{payload}");
                assert_eq!(public_error(&response).0, *code, "{payload}");
            }
        }
    }
    assert_eq!(
        harness.adapter.dispatched,
        vec!["set_draft:", "set_draft:   "]
    );
}

#[test]
fn clearing_the_draft_still_requires_the_full_authentication_gate() {
    let mut harness = Harness::start();
    let host = harness.host();
    let origin = harness.origin();
    let cookie = harness.session();
    let csrf = harness.csrf(&cookie);
    let payload = r#"{"id":"clear-gate","action":"set_draft","text":""}"#;
    let json_type = ("Content-Type", "application/json");

    let without_cookie = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", origin.as_str()),
            ("X-CSRF-Token", csrf.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&without_cookie), 401, "{without_cookie}");
    assert_eq!(public_error(&without_cookie).0, "unauthenticated");
    assert!(!without_cookie.contains("snapshot"));

    let without_csrf = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", origin.as_str()),
            ("Cookie", cookie.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&without_csrf), 403, "{without_csrf}");
    assert_eq!(public_error(&without_csrf).0, "csrf_rejected");

    let foreign_origin = harness.exchange(raw(
        "POST",
        "/api/action",
        &host,
        &[
            ("Origin", "http://evil.example"),
            ("Cookie", cookie.as_str()),
            ("X-CSRF-Token", csrf.as_str()),
            json_type,
        ],
        payload.as_bytes(),
    ));
    assert_eq!(status(&foreign_origin), 403, "{foreign_origin}");
    assert_eq!(public_error(&foreign_origin).0, "cross_site");

    assert!(harness.adapter.dispatched.is_empty());
}

#[test]
fn a_fragmented_head_over_the_budget_is_refused_even_when_every_field_is_legal() {
    let mut harness = Harness::start();
    let (cookie, csrf) = harness.authenticated();
    let payload = r#"{"id":"frag-1","action":"send"}"#;
    let request = padded_action_request(
        &harness.host(),
        &harness.origin(),
        &cookie,
        &csrf,
        payload,
        HEAD_BUDGET + 1,
    );
    assert_eq!(request.len() - payload.len(), HEAD_BUDGET + 1);

    let split = HEAD_BUDGET - 1;
    let first = request[..split].to_vec();
    let second = request[split..].to_vec();
    assert!(!first.windows(4).any(|window| window == b"\r\n\r\n"));

    let response = harness.exchange_parts(vec![first, second], Duration::from_millis(150));
    assert_eq!(status(&response), 431, "{response}");
    assert_eq!(public_error(&response).0, "headers_too_large");
    assert!(!response.contains("snapshot"));

    let unsplit = harness.exchange(request);
    assert_eq!(status(&unsplit), 431, "{unsplit}");
    assert!(harness.adapter.dispatched.is_empty());
}

#[test]
fn a_head_of_exactly_the_budget_is_accepted_with_a_body_in_the_same_write() {
    let mut harness = Harness::start();
    let (cookie, csrf) = harness.authenticated();
    let payload = r#"{"id":"exact-1","action":"send"}"#;
    let request = padded_action_request(
        &harness.host(),
        &harness.origin(),
        &cookie,
        &csrf,
        payload,
        HEAD_BUDGET,
    );
    assert_eq!(request.len() - payload.len(), HEAD_BUDGET);

    let response = harness.exchange(request);
    assert_eq!(status(&response), 200, "{response}");
    assert_eq!(harness.adapter.dispatched, vec!["send"]);

    let shell_request = padded_get_request(&harness.host(), HEAD_BUDGET);
    let shell = harness.exchange(shell_request);
    assert_eq!(status(&shell), 200, "{shell}");
    assert_eq!(body(&shell), String::from_utf8_lossy(INDEX));

    let fragmented = padded_action_request(
        &harness.host(),
        &harness.origin(),
        &cookie,
        &csrf,
        r#"{"id":"exact-2","action":"send"}"#,
        HEAD_BUDGET,
    );
    let split = HEAD_BUDGET / 2;
    let response = harness.exchange_parts(
        vec![fragmented[..split].to_vec(), fragmented[split..].to_vec()],
        Duration::from_millis(150),
    );
    assert_eq!(status(&response), 200, "{response}");
    assert_eq!(harness.adapter.dispatched, vec!["send", "send"]);
}
