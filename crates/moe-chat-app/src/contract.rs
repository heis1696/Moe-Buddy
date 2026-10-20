//! Narrow commands and read-only snapshots for the trusted text-chat host.

use std::fmt;
use std::sync::Arc;

use moe_agent_runtime::{ExternalInput, PresentationEvent, WorkflowStatus, WorkflowStepProgress};

pub const MAX_TEXT_BYTES: usize = 8_000;
pub const MAX_LOGICAL_SESSIONS: usize = 64;
pub const MAX_SESSION_ID_BYTES: usize = 64;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// 解析后进入展示的正文上限；模型原文仍按运行层回复上限登记。
pub const MAX_PARSED_REPLY_CHARS: usize = 4_096;
/// 单轮回复可携带的表现提议上限；提议不代表已经执行动画或工具。
pub const MAX_PRESENTATION_PROPOSALS: usize = 16;

/// Trusted desktop settings. Consent covers persona, authorized private notes and
/// the current conversation sent to this exact endpoint. Credentials stay in the host.
#[derive(Clone)]
pub struct ModelSettings {
    pub endpoint: String,
    pub model: String,
    pub persona: String,
    pub allow_transmission: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Chat,
    Presets,
    Memory,
    Diagnostics,
}

impl PanelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Presets => "presets",
            Self::Memory => "memory",
            Self::Diagnostics => "diagnostics",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "chat" => Some(Self::Chat),
            "presets" => Some(Self::Presets),
            "memory" => Some(Self::Memory),
            "diagnostics" => Some(Self::Diagnostics),
            _ => None,
        }
    }
}

pub enum Action {
    Open(PanelKind),
    Hide(PanelKind),
    Close(PanelKind),
    ConfirmClose {
        panel: PanelKind,
        discard: bool,
    },
    /// Create an isolated in-memory logical session after validating the directory revision.
    NewSession {
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    /// Select an existing session; archived sessions remain view-only.
    SwitchSession {
        id: String,
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    /// Archive only the currently selected session without deleting its history.
    ArchiveSession {
        id: String,
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    /// Trusted synchronous host action; queued IPC hosts should use `SetDraftGuarded`.
    SetDraft(String),
    /// Trusted synchronous host action; queued IPC hosts should use `SendGuarded`.
    Send,
    /// Session-checked variant for commands which may wait in a host queue.
    SetDraftGuarded {
        text: String,
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    /// Session-checked variant for commands which may wait in a host queue.
    SendGuarded {
        expected_session_id: String,
        expected_sessions_revision: u64,
    },
    SelectPreset(String),
    Remember(String),
    /// Search authorized private notes; an empty query lists recent notes.
    SearchMemories(String),
    /// Replace an explicitly selected note, rejecting stale revisions.
    UpdateMemory {
        id: String,
        revision: u64,
        text: String,
    },
    /// Delete an explicitly confirmed note, rejecting stale revisions.
    DeleteMemory {
        id: String,
        revision: u64,
    },
    RefreshMemories,
    RefreshDiagnostics,
    Revoke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelVisibility {
    Visible,
    Hidden,
    Closed,
}

impl PanelVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
            Self::Closed => "closed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatStatus {
    Ready,
    Sending,
    Failed,
    Unknown,
}

impl ChatStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Sending => "sending",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone)]
pub struct AppSnapshot {
    pub revision: u64,
    /// Selected trusted logical session; `None` after revocation.
    pub active_session_id: Option<String>,
    /// JavaScript-safe optimistic concurrency revision for the session directory.
    pub sessions_revision: u64,
    /// Body-free directory, empty after revocation.
    pub sessions: Vec<SessionSnapshot>,
    /// Only permits an explicit new session after a locally finished external Unknown.
    pub session_isolation_allowed: bool,
    pub mode: &'static str,
    pub companion: String,
    pub active_preset: String,
    pub external_transmission_allowed: bool,
    pub revoked: bool,
    pub panels: Vec<PanelSnapshot>,
    pub chat: ChatSnapshot,
    pub presets: Vec<PresetSnapshot>,
    pub memories: Vec<MemorySnapshot>,
    pub diagnostics: Vec<DiagnosticSnapshot>,
    pub notice: Option<String>,
}

/// Body-free, read-only view of the application-owned chat draft set.
///
/// Hosts use it to consume an explicit draft discard without guessing from an
/// empty snapshot. It never carries draft text, queue state or execution rights.
#[derive(Clone)]
pub struct ChatDraftSnapshot {
    /// Revision of the draft set as a JavaScript-safe integer. Every accepted
    /// draft mutation advances it; a discard also advances it.
    pub revision: u64,
    /// Latest explicit discard receipt, if any. Historical receipts stay readable
    /// (with their original revision) but no longer match the current revision
    /// once a later draft edit is accepted.
    pub discard: Option<ChatDraftDiscard>,
    /// Per-session draft watermarks, sorted by ID and bounded by
    /// [`MAX_LOGICAL_SESSIONS`]. Empty after revocation, so no stale session
    /// entry can outlive the directory that authorized it.
    pub sessions: Vec<ChatDraftSession>,
}

/// Body-free per-session draft watermark for one live logical session.
///
/// Hosts consume a discard per session instead of inferring one from the
/// global draft-set revision. An edit in another session therefore never
/// invalidates an older receipt for this session, and a host that missed
/// intermediate discard events can still read the latest per-session
/// discard watermark.
#[derive(Clone)]
pub struct ChatDraftSession {
    /// Stable logical session ID issued by the application.
    pub id: String,
    /// Draft-set revision of the latest accepted draft change in this session;
    /// never lower than this session's `discarded_revision`.
    pub revision: u64,
    /// Draft-set revision of the latest explicit discard that consumed this
    /// session's unsent text, if any. Historical watermarks stay readable.
    pub discarded_revision: Option<u64>,
}

/// Authoritative receipt for one explicit discard of unsent chat drafts.
///
/// It is issued by the application only after an accepted `CloseChat` discard or
/// a confirmed whole-application Quit discard. Ordinary empty snapshots, hiding
/// a panel or a mere status re-read never produce one.
#[derive(Clone)]
pub struct ChatDraftDiscard {
    /// The draft-set revision created by this discard.
    pub revision: u64,
    /// Sessions whose unsent drafts were discarded by that exact action, sorted
    /// and deduplicated. At most [`MAX_LOGICAL_SESSIONS`] IDs and never any body.
    pub session_ids: Vec<String>,
}

/// Body-free logical conversation directory entry.
#[derive(Clone)]
pub struct SessionSnapshot {
    pub id: String,
    pub label: String,
    pub archived: bool,
    pub status: ChatStatus,
}

#[derive(Clone)]
pub struct PanelSnapshot {
    pub kind: PanelKind,
    pub visibility: PanelVisibility,
    pub confirmation: bool,
    pub mounts: u64,
    pub renders: u64,
    pub unmounts: u64,
}

#[derive(Clone)]
pub struct ChatSnapshot {
    pub draft: String,
    pub messages: Vec<ChatMessage>,
    pub status: ChatStatus,
    pub failure: Option<String>,
}

#[derive(Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone)]
pub struct PresetSnapshot {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
}

#[derive(Clone)]
pub struct MemorySnapshot {
    pub id: String,
    pub revision: u64,
    pub text: String,
}

#[derive(Clone)]
pub struct DiagnosticSnapshot {
    pub sequence: u64,
    pub run: String,
    pub event: String,
    pub status: String,
}

/// A prepared external request together with the opaque completion ticket that
/// belongs to exactly this turn and workflow run instance.
///
/// The host may move the input to a worker thread, but the ticket must come back
/// with the host result. There is no completion entry point that omits it.
pub struct ExternalRequest {
    input: ExternalInput,
    ticket: ExternalRequestTicket,
}

impl ExternalRequest {
    pub(crate) fn new(input: ExternalInput, ticket: ExternalRequestTicket) -> Self {
        Self { input, ticket }
    }

    /// The immutable plaintext request for the host transport.
    pub fn input(&self) -> &ExternalInput {
        &self.input
    }

    /// The completion ticket; callers normally split it out with [`Self::into_parts`].
    pub fn ticket(&self) -> &ExternalRequestTicket {
        &self.ticket
    }

    /// Split ownership so only the input travels to the worker.
    pub fn into_parts(self) -> (ExternalInput, ExternalRequestTicket) {
        (self.input, self.ticket)
    }
}

/// Opaque proof that a completion callback belongs to the in-flight turn and
/// the exact chat workflow run that produced the request.
///
/// Only [`ChatApplication`](crate::ChatApplication) issues tickets. Constructing
/// one from outside the application is impossible; cloning an issued ticket is
/// allowed but the second completion is rejected before it can consume the
/// current pending request.
#[derive(Clone)]
pub struct ExternalRequestTicket {
    owner: Arc<()>,
    turn: u64,
}

impl ExternalRequestTicket {
    pub(crate) fn new(owner: Arc<()>, turn: u64) -> Self {
        Self { owner, turn }
    }

    pub(crate) fn matches(&self, owner: &Arc<()>, turn: u64) -> bool {
        self.turn == turn && Arc::ptr_eq(&self.owner, owner)
    }
}

impl fmt::Debug for ExternalRequestTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExternalRequestTicket(REDACTED)")
    }
}

/// The raw model reply exactly as accepted by the runtime, before display
/// parsing. It always carries the model's presentation proposals separately so
/// a parser cannot smuggle them into model text.
pub struct RawModelReply<'a> {
    text: &'a str,
    presentation: &'a [PresentationEvent],
}

impl<'a> RawModelReply<'a> {
    pub(crate) fn new(text: &'a str, presentation: &'a [PresentationEvent]) -> Self {
        Self { text, presentation }
    }

    /// The unmodified model text.
    pub fn text(&self) -> &str {
        self.text
    }

    /// Presentation proposals reported by the model; not executed actions.
    pub fn presentation(&self) -> &[PresentationEvent] {
        self.presentation
    }
}

impl fmt::Debug for RawModelReply<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RawModelReply(REDACTED)")
    }
}

/// Display and proposal result of the parse node.
///
/// This is deliberately separate from the raw model result: only
/// [`Self::display_text`] reaches the visible conversation, while the raw text
/// remains in the runtime session ledger. Presentation entries are proposals and
/// do not execute animations, scripts or tools.
#[derive(Clone, Debug)]
pub struct ParsedReply {
    display_text: String,
    presentation: Vec<PresentationEvent>,
}

impl ParsedReply {
    /// Build a bounded parsed reply. The first implementation accepts plain text
    /// only; no tag, script or tool syntax is interpreted.
    pub fn new(
        display_text: String,
        presentation: Vec<PresentationEvent>,
    ) -> Result<Self, ReplyParseError> {
        if display_text.trim().is_empty() {
            return Err(ReplyParseError::new("parsed display text is empty"));
        }
        if display_text.chars().count() > MAX_PARSED_REPLY_CHARS {
            return Err(ReplyParseError::new(
                "parsed display text exceeds the bounded capacity",
            ));
        }
        if presentation.len() > MAX_PRESENTATION_PROPOSALS {
            return Err(ReplyParseError::new(
                "parsed presentation proposals exceed the bounded capacity",
            ));
        }
        Ok(Self {
            display_text,
            presentation,
        })
    }

    pub fn display_text(&self) -> &str {
        &self.display_text
    }

    pub fn presentation(&self) -> &[PresentationEvent] {
        &self.presentation
    }
}

/// A bounded, injectable plaintext parser. The application calls it exactly once
/// per textual model result; it is never called for Failed/Cancelled/Unknown.
pub trait ReplyParser {
    fn parse(&self, raw: &RawModelReply<'_>) -> Result<ParsedReply, ReplyParseError>;
}

/// Fixed, body-free parse failure. The original model text is never copied into
/// the error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplyParseError {
    reason: &'static str,
}

impl ReplyParseError {
    pub fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl fmt::Display for ReplyParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for ReplyParseError {}

/// First parser: bounded plaintext. It copies the model's presentation proposals
/// without adding tags, scripts or tool execution.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainTextReplyParser;

impl ReplyParser for PlainTextReplyParser {
    fn parse(&self, raw: &RawModelReply<'_>) -> Result<ParsedReply, ReplyParseError> {
        ParsedReply::new(raw.text().to_owned(), raw.presentation().to_vec())
    }
}

/// Authoritative business (model-result) terminal state registered by
/// `AgentRuntime::finish`. It is independent of whether the application finished
/// displaying the turn: a `Succeeded` publish with an `Unknown` business outcome
/// still means the model result is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusinessOutcome {
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

impl BusinessOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

/// Read-only phase progress for the current or most recent chat turn.
///
/// No caller can mutate phases. `committed` reports the authoritative
/// `AgentRuntime::finish` receipt; `published` is only true after the application
/// display facade actually rendered the turn. Workflow success alone never means
/// the request was sent, the reply was accepted or the reply is visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatTurnProgress {
    pub turn: u64,
    pub definition_id: &'static str,
    pub definition_version: u64,
    pub status: WorkflowStatus,
    pub next_step: Option<&'static str>,
    pub steps: Vec<WorkflowStepProgress>,
    pub committed: bool,
    pub published: bool,
    /// Registered business terminal state; `None` when no `AgentDelivery` receipt
    /// exists (for example a turn rejected before registration).
    pub business_outcome: Option<BusinessOutcome>,
}
