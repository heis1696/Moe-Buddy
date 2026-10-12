use std::fmt;
use std::sync::Arc;

use crate::{GatewayError, ReplyTarget, TrustedIngress, TrustedRequest, validation};

pub struct SessionAccess<'a> {
    pub(super) ingress: &'a TrustedIngress,
    pub(super) request: &'a TrustedRequest,
    pub(super) now_ms: u64,
}

impl<'a> SessionAccess<'a> {
    pub fn new(ingress: &'a TrustedIngress, request: &'a TrustedRequest, now_ms: u64) -> Self {
        Self {
            ingress,
            request,
            now_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionLimits {
    pub(super) sessions: usize,
    pub(super) events_per_session: usize,
    pub(super) events_total: usize,
    pub(super) body_chars: usize,
    pub(super) total_chars: usize,
    pub(super) concurrent_batches: usize,
}

impl SessionLimits {
    pub fn new(
        sessions: usize,
        events_per_session: usize,
        events_total: usize,
        body_chars: usize,
        total_chars: usize,
        concurrent_batches: usize,
    ) -> Result<Self, GatewayError> {
        if [
            sessions,
            events_per_session,
            events_total,
            body_chars,
            total_chars,
            concurrent_batches,
        ]
        .contains(&0)
        {
            return Err(GatewayError::InvalidInput(
                "session limits must be positive",
            ));
        }
        Ok(Self {
            sessions,
            events_per_session,
            events_total,
            body_chars,
            total_chars,
            concurrent_batches,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionTiming {
    pub(super) active_ms: u64,
    pub(super) quiet_ms: u64,
    pub(super) interaction_ms: u64,
    pub(super) batch_ms: u64,
}

impl SessionTiming {
    pub fn new(
        active_ms: u64,
        quiet_ms: u64,
        interaction_ms: u64,
        batch_ms: u64,
    ) -> Result<Self, GatewayError> {
        if active_ms == 0 || quiet_ms < 180_000 || interaction_ms == 0 || batch_ms == 0 {
            return Err(GatewayError::InvalidInput("invalid session timing"));
        }
        Ok(Self {
            active_ms,
            quiet_ms,
            interaction_ms,
            batch_ms,
        })
    }

    pub fn active_ms(&self) -> u64 {
        self.active_ms
    }
    pub fn quiet_ms(&self) -> u64 {
        self.quiet_ms
    }
}

impl Default for SessionTiming {
    fn default() -> Self {
        Self {
            active_ms: 1_800_000,
            quiet_ms: 180_000,
            interaction_ms: 3_600_000,
            batch_ms: 300_000,
        }
    }
}

#[derive(Clone)]
pub struct SessionId {
    pub(super) owner: Arc<()>,
    pub(super) sequence: u64,
}

impl PartialEq for SessionId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner) && self.sequence == other.sequence
    }
}

impl Eq for SessionId {}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SessionId")
            .field(&self.sequence)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventId {
    pub(super) session: SessionId,
    pub(super) ordinal: usize,
}

impl EventId {
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }
    pub fn session(&self) -> &SessionId {
        &self.session
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionId(pub(super) EventId);

#[derive(Clone, PartialEq, Eq)]
pub struct HostEventId(pub(super) String);

impl HostEventId {
    pub fn new(value: impl Into<String>) -> Result<Self, GatewayError> {
        let value = value.into();
        validation::identifier(&value)?;
        Ok(Self(value))
    }
}

impl fmt::Debug for HostEventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HostEventId(REDACTED)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserEventKind {
    New,
    History,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserEvent {
    pub(super) id: Option<HostEventId>,
    pub(super) kind: UserEventKind,
    pub(super) source_order: Option<u64>,
    pub(super) source_timestamp_ms: Option<u64>,
}

impl UserEvent {
    pub fn new(kind: UserEventKind) -> Self {
        Self {
            id: None,
            kind,
            source_order: None,
            source_timestamp_ms: None,
        }
    }
    pub fn with_id(mut self, id: HostEventId) -> Self {
        self.id = Some(id);
        self
    }
    pub fn with_source_order(mut self, order: u64) -> Self {
        self.source_order = Some(order);
        self
    }
    pub fn with_source_timestamp_ms(mut self, timestamp: u64) -> Self {
        self.source_timestamp_ms = Some(timestamp);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventRole {
    User,
    UserHistory,
    Assistant,
    ExternalAgent,
    ToolReceipt,
    InjectedContext,
    Background,
}

pub struct RelatedEvent {
    pub(super) id: Option<HostEventId>,
    pub(super) role: EventRole,
    pub(super) body: String,
    pub(super) source_timestamp_ms: Option<u64>,
}

impl RelatedEvent {
    pub fn new(role: EventRole, body: impl Into<String>) -> Self {
        Self {
            id: None,
            role,
            body: body.into(),
            source_timestamp_ms: None,
        }
    }
    pub fn with_id(mut self, id: HostEventId) -> Self {
        self.id = Some(id);
        self
    }
    pub fn with_source_timestamp_ms(mut self, timestamp: u64) -> Self {
        self.source_timestamp_ms = Some(timestamp);
        self
    }
}

impl fmt::Debug for RelatedEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelatedEvent")
            .field("role", &self.role)
            .field("body_chars", &self.body.chars().count())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalState {
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchAcknowledgement {
    Confirmed,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub session: SessionId,
    pub interaction: InteractionId,
    pub duplicate: bool,
    pub renewed: bool,
}

#[derive(Clone)]
pub struct StagedEvent {
    pub(super) id: EventId,
    pub(super) role: EventRole,
    pub(super) body: String,
    pub(super) received_ms: u64,
    pub(super) source_timestamp_ms: Option<u64>,
    pub(super) source_order: Option<u64>,
    pub(super) client_request_hint: Option<String>,
    pub(super) terminal: Option<TerminalState>,
    pub(super) acknowledgement: Option<BatchAcknowledgement>,
}

impl StagedEvent {
    pub fn id(&self) -> &EventId {
        &self.id
    }
    pub fn role(&self) -> EventRole {
        self.role
    }
    pub fn body(&self) -> &str {
        &self.body
    }
    pub fn received_ms(&self) -> u64 {
        self.received_ms
    }
    pub fn source_timestamp_ms(&self) -> Option<u64> {
        self.source_timestamp_ms
    }
    pub fn source_order(&self) -> Option<u64> {
        self.source_order
    }
    pub fn client_request_hint(&self) -> Option<&str> {
        self.client_request_hint.as_deref()
    }
    pub fn terminal(&self) -> Option<TerminalState> {
        self.terminal
    }
    pub fn acknowledgement(&self) -> Option<BatchAcknowledgement> {
        self.acknowledgement
    }
}

impl fmt::Debug for StagedEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedEvent")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("body_chars", &self.body.chars().count())
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub id: SessionId,
    pub active_until_ms: Option<u64>,
    pub processed_through: usize,
    pub events: Vec<StagedEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchTicket {
    pub(super) session: SessionId,
    pub(super) sequence: u64,
    pub(super) request_sequence: u64,
    pub(super) revision: u64,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) watermark: usize,
    pub(super) deadline_ms: u64,
}

impl BatchTicket {
    pub fn session(&self) -> &SessionId {
        &self.session
    }
    pub fn first_ordinal(&self) -> usize {
        self.start
    }
    pub fn last_ordinal(&self) -> usize {
        self.end
    }
    pub fn watermark(&self) -> usize {
        self.watermark
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchProgress {
    pub acknowledged: usize,
    pub total: usize,
    pub processed_through: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalReceipt {
    pub state: TerminalState,
    pub reply_target: ReplyTarget,
    pub reply_event: Option<EventId>,
}
