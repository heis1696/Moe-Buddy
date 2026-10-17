//! Loopback HTTP and browser presentation adapter for the trusted chat application.
//!
//! This crate is the transport boundary of the text-chat host. It owns the loopback
//! listener, the browser session, the request-deduplication ledger and the static asset
//! bytes the host binary injects. It contains no chat, memory, preset, workflow or
//! diagnostics rule: every business decision stays behind [`ApplicationPort`].
//!
//! # Route table
//!
//! | Route | Method | Answer |
//! | --- | --- | --- |
//! | `/` | `GET` | Static shell; issues the session cookie. |
//! | `/app.js` | `GET` | Static script. |
//! | `/style.css` | `GET` | Static stylesheet. |
//! | `/api/bootstrap` | `GET` | `{ "csrf": ..., "snapshot": ... }` for an authenticated session. |
//! | `/api/state` | `GET` | `{ "snapshot": ... }` for an authenticated session. |
//! | `/api/action` | `POST` | One whitelisted action; the answer carries the fresh snapshot. |
//!
//! Every other path, method or malformed request is refused.
//!
//! # Wire discipline
//!
//! * Single-shot HTTP/1.1 connections with an absolute read deadline and an absolute write
//!   deadline; the listener binds `127.0.0.1` only and drops non-loopback peers.
//! * Bounded header block, bounded field count and a bounded body. `Transfer-Encoding`,
//!   `Expect`, repeated critical headers and repeated or non-canonical `Content-Length` are
//!   refused before a single body byte is read, and a request is fully validated before any
//!   action reaches the application.
//! * `Host` must be the exact bound authority. An `Origin` header, when present, must equal
//!   the bound origin, and `POST` requires it. No CORS header is ever emitted.
//! * `POST` additionally requires the session cookie, `X-CSRF-Token` and
//!   `Content-Type: application/json`; an unauthenticated request never receives a snapshot.
//! * Session and CSRF tokens come from the operating system random source and are never
//!   written to process output; the session token appears only in its `HttpOnly`,
//!   `SameSite=Strict` cookie.
//! * Request ids are remembered in a bounded ledger that never evicts an accepted id, so a
//!   replay is refused for the lifetime of the process and a full ledger refuses new work
//!   instead of forgetting history.
//!
//! # Threading
//!
//! The application may be non-`Send`: [`LoopbackServer::serve_once`] and
//! [`LoopbackServer::run`] drive it serially on the calling thread.

mod actions;
mod http;
mod ledger;
mod server;
mod snapshot;
mod tokens;

use moe_chat_app::{Action, AppSnapshot};

pub use server::LoopbackServer;

/// Static browser assets injected by the host binary.
///
/// The library never reads the filesystem: the binary decides which bytes to serve.
#[derive(Clone, Copy, Debug)]
pub struct Assets {
    /// Document served for `GET /`.
    pub index: &'static [u8],
    /// Script served for `GET /app.js`.
    pub app_js: &'static [u8],
    /// Stylesheet served for `GET /style.css`.
    pub style_css: &'static [u8],
}

impl Assets {
    /// Groups the three fixed asset payloads.
    pub const fn new(
        index: &'static [u8],
        app_js: &'static [u8],
        style_css: &'static [u8],
    ) -> Self {
        Self {
            index,
            app_js,
            style_css,
        }
    }
}

/// The only error shape that may cross to the browser.
///
/// `code` is a fixed category and `message` a fixed safe hint. An application must keep its
/// original error chain and any sensitive detail inside the application: neither the host
/// nor the browser may see or log it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicError {
    /// Fixed machine-readable category.
    pub code: &'static str,
    /// Fixed browser-safe hint.
    pub message: &'static str,
}

impl PublicError {
    /// Builds a public error in a `const` context.
    pub const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

/// The trusted application behind the browser surface.
///
/// The host supplies monotonic time and never lets the application read a clock, a socket
/// or a file on its own. `dispatch` receives an already validated action and is never
/// retried automatically: the host has recorded the request id before calling it.
pub trait ApplicationPort {
    /// Current read-only snapshot, rendered as `snapshot` in every authenticated answer.
    fn snapshot(&self) -> AppSnapshot;

    /// Applies one whitelisted action at the host-provided monotonic time.
    ///
    /// Returning a [`PublicError`] answers the browser with that fixed error and the
    /// current snapshot; the underlying cause stays inside the application.
    fn dispatch(&mut self, action: Action, now_ms: u64) -> Result<(), PublicError>;
}
