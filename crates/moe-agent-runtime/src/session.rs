use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use crate::{
    GatewayError, ReplyTarget, RequestIsolation, TrustedIngress, TrustedRequest, validation,
};

mod lifecycle;
mod persistence;
mod types;
pub use lifecycle::{
    LifecycleReceipt, MaintenanceReport, ManagedFragmentSnapshot, ManagedFragmentSummary,
    ManagementCapability, ManagementPermissions, PurgeReceipt, RawRetention, RecycleEmptyReport,
    RetentionZone,
};
pub use persistence::{
    RAW_SNAPSHOT_MAX_TOTAL_BYTES, RawDomainKey, RawHistoryGrant, RawImportReceipt,
    RawSnapshotSweep, RawSnapshotV1, RestoredRawFragment,
};
pub use types::*;

use self::lifecycle::{CapabilityRecord, CleanedFragment, FragmentLifecycle};
use self::persistence::{RawGrantRecord, RawImportRecord};

#[cfg(test)]
mod tests;

struct Interaction {
    request_sequence: u64,
    reply_target: ReplyTarget,
    deadline_ms: u64,
    reply_event: Option<EventId>,
}

struct EventRecord {
    staged: StagedEvent,
    host_id: Option<HostEventId>,
    input: Option<UserEvent>,
    interaction: Option<Interaction>,
    parent: Option<usize>,
}

struct BatchRecord {
    ticket: BatchTicket,
    complete: bool,
}

struct Session {
    id: SessionId,
    isolation: RequestIsolation,
    authorization_generation: u64,
    revoked: bool,
    active_until_ms: Option<u64>,
    last_user_ms: Option<u64>,
    revision: u64,
    processed_through: usize,
    events: Vec<EventRecord>,
    batch: Option<BatchRecord>,
    char_count: usize,
    lifecycle: Option<FragmentLifecycle>,
    raw_key: Option<u64>,
}

impl Session {
    fn matches(&self, request: &TrustedRequest) -> bool {
        self.authorization_generation == request.generation()
            && &self.isolation == request.isolation()
    }

    fn active(&self, now_ms: u64) -> bool {
        !self.revoked
            && self
                .active_until_ms
                .is_some_and(|deadline| now_ms < deadline)
    }

    fn zone(&self) -> RetentionZone {
        self.lifecycle
            .as_ref()
            .map_or(RetentionZone::Normal, |lifecycle| lifecycle.zone)
    }

    /// 逻辑期限门槛：未配置留存时恒通过；正常区过期或已回收均拒绝。
    fn lifecycle_usable(&self, now_ms: u64) -> Result<(), GatewayError> {
        let Some(lifecycle) = &self.lifecycle else {
            return Ok(());
        };
        match lifecycle.zone {
            RetentionZone::Normal => {
                if now_ms >= lifecycle.normal_deadline_ms {
                    Err(GatewayError::RawExpired)
                } else {
                    Ok(())
                }
            }
            RetentionZone::Recycle => Err(GatewayError::Recycled),
        }
    }
}

pub struct SessionRuntime {
    owner: Arc<()>,
    ingress_run: u64,
    limits: SessionLimits,
    timing: SessionTiming,
    retention: Option<RawRetention>,
    last_now_ms: u64,
    session_sequence: u64,
    batch_sequence: u64,
    lifecycle_generation: u64,
    events_total: usize,
    chars_total: usize,
    recycle_events: usize,
    recycle_chars: usize,
    sessions: Vec<Session>,
    cleaned: Vec<CleanedFragment>,
    cleaned_requests: BTreeSet<u64>,
    replay_protection_exhausted: bool,
    capabilities: Vec<CapabilityRecord>,
    raw_key_sequence: u64,
    raw_grants: Vec<RawGrantRecord>,
    raw_imports: Vec<RawImportRecord>,
}

impl SessionRuntime {
    pub fn new(
        ingress: &TrustedIngress,
        limits: SessionLimits,
        timing: SessionTiming,
        now_ms: u64,
    ) -> Self {
        Self {
            owner: Arc::new(()),
            ingress_run: ingress.run_instance(),
            limits,
            timing,
            retention: None,
            last_now_ms: now_ms,
            session_sequence: 0,
            batch_sequence: 0,
            lifecycle_generation: 0,
            events_total: 0,
            chars_total: 0,
            recycle_events: 0,
            recycle_chars: 0,
            sessions: Vec::new(),
            cleaned: Vec::new(),
            cleaned_requests: BTreeSet::new(),
            replay_protection_exhausted: false,
            capabilities: Vec::new(),
            raw_key_sequence: 0,
            raw_grants: Vec::new(),
            raw_imports: Vec::new(),
        }
    }

    fn check(&self, access: &SessionAccess<'_>) -> Result<(), GatewayError> {
        access.request.validate(access.ingress)?;
        if access.ingress.run_instance() != self.ingress_run {
            return Err(GatewayError::ForeignRequest);
        }
        if access.now_ms < self.last_now_ms {
            return Err(GatewayError::ClockWentBackwards);
        }
        Ok(())
    }

    fn session_index(
        &self,
        access: &SessionAccess<'_>,
        id: &SessionId,
    ) -> Result<usize, GatewayError> {
        self.check(access)?;
        if !Arc::ptr_eq(&self.owner, &id.owner) {
            return Err(GatewayError::ForeignRequest);
        }
        let index = self
            .sessions
            .iter()
            .position(|session| &session.id == id)
            .ok_or(GatewayError::StaleHandle)?;
        let session = &self.sessions[index];
        if !session.matches(access.request) {
            return Err(GatewayError::Revoked);
        }
        if session.revoked {
            return Err(GatewayError::Revoked);
        }
        Ok(index)
    }

    fn interaction_index(
        &self,
        access: &SessionAccess<'_>,
        id: &InteractionId,
    ) -> Result<(usize, usize), GatewayError> {
        let session = self.session_index(access, &id.0.session)?;
        let event =
            id.0.ordinal
                .checked_sub(1)
                .ok_or(GatewayError::StaleHandle)?;
        let interaction = self.sessions[session]
            .events
            .get(event)
            .and_then(|event| event.interaction.as_ref())
            .ok_or(GatewayError::StaleHandle)?;
        if interaction.request_sequence != access.request.sequence() {
            return Err(GatewayError::ForeignRequest);
        }
        Ok((session, event))
    }

    /// 正常区片段计数：启用留存时回收区片段不计入正常区片段配额。
    fn normal_fragment_count(&self) -> usize {
        match &self.retention {
            None => self.sessions.len(),
            Some(_) => self
                .sessions
                .iter()
                .filter(|session| session.zone() == RetentionZone::Normal)
                .count(),
        }
    }

    fn capacity(&self, session: Option<usize>, body: &str) -> Result<usize, GatewayError> {
        validation::text(body, usize::MAX)?;
        let chars = body.chars().count();
        if chars > self.limits.body_chars
            || self.events_total >= self.limits.events_total
            || self
                .chars_total
                .checked_add(chars)
                .is_none_or(|total| total > self.limits.total_chars)
            || session.is_some_and(|index| {
                self.sessions[index].events.len() >= self.limits.events_per_session
            })
            || (session.is_none() && self.normal_fragment_count() >= self.limits.sessions)
        {
            return Err(GatewayError::CapacityExceeded);
        }
        Ok(chars)
    }

    pub fn register_user(
        &mut self,
        access: SessionAccess<'_>,
        input: UserEvent,
    ) -> Result<Registration, GatewayError> {
        self.check(&access)?;
        if self.cleaned_requests.contains(&access.request.sequence()) {
            return Err(GatewayError::Revoked);
        }
        self.check_cleaned_host_event(access.request.isolation(), input.id.as_ref())?;
        let mut latest = None;
        let (mut highest_request, mut highest_source) = self
            .cleaned
            .iter()
            .find(|entry| &entry.isolation == access.request.isolation())
            .map_or((0, None), |entry| {
                (entry.highest_request, entry.highest_source)
            });
        for (index, session) in self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| &session.isolation == access.request.isolation())
        {
            if session.matches(access.request) {
                latest = Some(index);
            }
            for event in &session.events {
                if let Some(interaction) = &event.interaction {
                    highest_request = highest_request.max(interaction.request_sequence);
                    if let Some(order) = event.staged.source_order {
                        highest_source =
                            Some(highest_source.map_or(order, |current: u64| current.max(order)));
                    }
                    if interaction.request_sequence == access.request.sequence()
                        || (input.id.is_some() && event.host_id == input.id)
                    {
                        if session.revoked || !session.matches(access.request) {
                            return Err(GatewayError::Revoked);
                        }
                        session.lifecycle_usable(access.now_ms)?;
                        if event.input.as_ref() != Some(&input)
                            || event.staged.body != access.request.text()
                        {
                            return Err(GatewayError::EventConflict);
                        }
                        let result = Registration {
                            session: session.id.clone(),
                            interaction: InteractionId(event.staged.id.clone()),
                            duplicate: true,
                            renewed: false,
                        };
                        self.last_now_ms = access.now_ms;
                        return Ok(result);
                    }
                } else if input.id.is_some() && event.host_id == input.id {
                    return Err(GatewayError::EventConflict);
                }
            }
        }
        let renews = input.kind == UserEventKind::New
            && access.request.sequence() > highest_request
            && input
                .source_order
                .is_none_or(|order| highest_source.is_none_or(|highest| order > highest));
        self.check_replay_budget()?;
        let existing = latest.filter(|index| {
            let session = &self.sessions[*index];
            !session.revoked
                && session.lifecycle_usable(access.now_ms).is_ok()
                && (!renews || session.active(access.now_ms))
        });
        let chars = self.capacity(existing, access.request.text())?;
        let lifecycle = self
            .retention
            .as_ref()
            .map(|retention| {
                let normal_deadline_ms = access
                    .now_ms
                    .checked_add(retention.normal_ms())
                    .ok_or(GatewayError::IdentifierExhausted)?;
                let hard_deadline_ms = normal_deadline_ms
                    .checked_add(retention.recycle_ms())
                    .ok_or(GatewayError::IdentifierExhausted)?;
                Ok(FragmentLifecycle {
                    zone: RetentionZone::Normal,
                    created_at_ms: access.now_ms,
                    normal_deadline_ms,
                    hard_deadline_ms,
                    final_deadline_ms: None,
                    version: 0,
                })
            })
            .transpose()?;
        let active_until = if renews {
            Some(
                access
                    .now_ms
                    .checked_add(self.timing.active_ms)
                    .ok_or(GatewayError::IdentifierExhausted)?,
            )
        } else {
            None
        };
        let deadline_ms = if renews {
            access
                .now_ms
                .checked_add(self.timing.interaction_ms)
                .ok_or(GatewayError::IdentifierExhausted)?
        } else {
            access.now_ms
        };
        let session_sequence = if existing.is_none() {
            self.session_sequence
                .checked_add(1)
                .ok_or(GatewayError::IdentifierExhausted)?
        } else {
            self.session_sequence
        };
        let index = existing.unwrap_or(self.sessions.len());
        if existing.is_none() {
            self.sessions.push(Session {
                id: SessionId {
                    owner: self.owner.clone(),
                    sequence: session_sequence,
                },
                isolation: access.request.isolation().clone(),
                authorization_generation: access.request.generation(),
                revoked: false,
                active_until_ms: None,
                last_user_ms: None,
                revision: 0,
                processed_through: 0,
                events: Vec::new(),
                batch: None,
                char_count: 0,
                lifecycle,
                raw_key: None,
            });
            self.session_sequence = session_sequence;
        }
        let session = &mut self.sessions[index];
        let id = EventId {
            session: session.id.clone(),
            ordinal: session.events.len() + 1,
        };
        session.events.push(EventRecord {
            staged: StagedEvent {
                id: id.clone(),
                role: if renews {
                    EventRole::User
                } else {
                    EventRole::UserHistory
                },
                body: access.request.text().to_owned(),
                received_ms: access.now_ms,
                source_timestamp_ms: input.source_timestamp_ms,
                source_order: input.source_order,
                client_request_hint: access.request.client_request_id().map(str::to_owned),
                terminal: if renews {
                    None
                } else {
                    Some(TerminalState::Unknown)
                },
                acknowledgement: None,
            },
            host_id: input.id.clone(),
            input: Some(input),
            interaction: Some(Interaction {
                request_sequence: access.request.sequence(),
                reply_target: access.request.reply_target().clone(),
                deadline_ms,
                reply_event: None,
            }),
            parent: None,
        });
        session.char_count += chars;
        if renews {
            session.active_until_ms = active_until;
            session.last_user_ms = Some(access.now_ms);
        }
        self.events_total += 1;
        self.chars_total += chars;
        self.last_now_ms = access.now_ms;
        Ok(Registration {
            session: session.id.clone(),
            interaction: InteractionId(id),
            duplicate: false,
            renewed: renews,
        })
    }

    pub fn append_related(
        &mut self,
        access: SessionAccess<'_>,
        interaction: &InteractionId,
        event: RelatedEvent,
    ) -> Result<EventId, GatewayError> {
        let (session_index, parent) = self.interaction_index(&access, interaction)?;
        self.sessions[session_index].lifecycle_usable(access.now_ms)?;
        let session = &self.sessions[session_index];
        let origin = &session.events[parent];
        if origin.staged.terminal == Some(TerminalState::Cancelled) {
            return Err(GatewayError::InteractionClosed);
        }
        if access.now_ms
            >= origin
                .interaction
                .as_ref()
                .expect("interaction checked")
                .deadline_ms
        {
            return Err(GatewayError::DeadlineExceeded);
        }
        if matches!(event.role, EventRole::User | EventRole::UserHistory) {
            return Err(GatewayError::InvalidInput(
                "use user registration for user evidence",
            ));
        }
        self.check_cleaned_host_event(access.request.isolation(), event.id.as_ref())?;
        if let Some(id) = &event.id {
            for known_session in self
                .sessions
                .iter()
                .filter(|session| &session.isolation == access.request.isolation())
            {
                if let Some(known) = known_session
                    .events
                    .iter()
                    .find(|known| known.host_id.as_ref() == Some(id))
                {
                    if known_session.id != session.id
                        || known.parent != Some(parent + 1)
                        || known.staged.role != event.role
                        || known.staged.body != event.body
                        || known.staged.source_timestamp_ms != event.source_timestamp_ms
                    {
                        return Err(GatewayError::EventConflict);
                    }
                    let result = known.staged.id.clone();
                    self.last_now_ms = access.now_ms;
                    return Ok(result);
                }
            }
        }
        if origin.staged.terminal.is_some() {
            return Err(GatewayError::InteractionClosed);
        }
        self.check_replay_budget()?;
        let chars = self.capacity(Some(session_index), &event.body)?;
        let session = &mut self.sessions[session_index];
        let id = EventId {
            session: session.id.clone(),
            ordinal: session.events.len() + 1,
        };
        session.events.push(EventRecord {
            staged: StagedEvent {
                id: id.clone(),
                role: event.role,
                body: event.body,
                received_ms: access.now_ms,
                source_timestamp_ms: event.source_timestamp_ms,
                source_order: None,
                client_request_hint: None,
                terminal: None,
                acknowledgement: None,
            },
            host_id: event.id,
            input: None,
            interaction: None,
            parent: Some(parent + 1),
        });
        session.char_count += chars;
        self.events_total += 1;
        self.chars_total += chars;
        self.last_now_ms = access.now_ms;
        Ok(id)
    }

    pub fn finish_interaction(
        &mut self,
        access: SessionAccess<'_>,
        id: &InteractionId,
        state: TerminalState,
        actual_reply: Option<&str>,
    ) -> Result<TerminalReceipt, GatewayError> {
        if state == TerminalState::Cancelled {
            if actual_reply.is_some() {
                self.check(&access)?;
                return Err(GatewayError::InvalidInput(
                    "cancellation cannot deliver a reply",
                ));
            }
            return self.cancel_interaction(access, id);
        }
        let (session_index, event_index) = self.interaction_index(&access, id)?;
        self.sessions[session_index].lifecycle_usable(access.now_ms)?;
        let origin = &self.sessions[session_index].events[event_index];
        let interaction = origin.interaction.as_ref().expect("interaction checked");
        if origin.staged.terminal == Some(TerminalState::Cancelled) {
            return Err(GatewayError::InteractionClosed);
        }
        if access.now_ms >= interaction.deadline_ms {
            return Err(GatewayError::DeadlineExceeded);
        }
        if let Some(previous) = origin.staged.terminal {
            let previous_reply = interaction.reply_event.as_ref().map(|event| {
                self.sessions[session_index].events[event.ordinal - 1]
                    .staged
                    .body
                    .as_str()
            });
            if previous != state || previous_reply != actual_reply {
                return Err(GatewayError::EventConflict);
            }
            let receipt = TerminalReceipt {
                state,
                reply_target: interaction.reply_target.clone(),
                reply_event: interaction.reply_event.clone(),
            };
            self.last_now_ms = access.now_ms;
            return Ok(receipt);
        }
        let chars = actual_reply
            .map(|body| self.capacity(Some(session_index), body))
            .transpose()?;
        let session = &mut self.sessions[session_index];
        let reply_event = actual_reply.map(|body| {
            let reply_id = EventId {
                session: session.id.clone(),
                ordinal: session.events.len() + 1,
            };
            session.events.push(EventRecord {
                staged: StagedEvent {
                    id: reply_id.clone(),
                    role: EventRole::Assistant,
                    body: body.to_owned(),
                    received_ms: access.now_ms,
                    source_timestamp_ms: None,
                    source_order: None,
                    client_request_hint: None,
                    terminal: Some(state),
                    acknowledgement: None,
                },
                host_id: None,
                input: None,
                interaction: None,
                parent: Some(event_index + 1),
            });
            reply_id
        });
        session.events[event_index].staged.terminal = Some(state);
        let interaction = session.events[event_index]
            .interaction
            .as_mut()
            .expect("interaction checked");
        interaction.reply_event = reply_event.clone();
        let receipt = TerminalReceipt {
            state,
            reply_target: interaction.reply_target.clone(),
            reply_event,
        };
        if let Some(chars) = chars {
            session.char_count += chars;
            self.events_total += 1;
            self.chars_total += chars;
        }
        self.last_now_ms = access.now_ms;
        Ok(receipt)
    }

    pub fn cancel_interaction(
        &mut self,
        access: SessionAccess<'_>,
        id: &InteractionId,
    ) -> Result<TerminalReceipt, GatewayError> {
        let (session_index, event_index) = self.interaction_index(&access, id)?;
        let session = &self.sessions[session_index];
        let event = &session.events[event_index];
        if event
            .staged
            .terminal
            .is_some_and(|state| state != TerminalState::Cancelled)
        {
            return Err(GatewayError::InteractionClosed);
        }
        let receipt = TerminalReceipt {
            state: TerminalState::Cancelled,
            reply_target: event
                .interaction
                .as_ref()
                .expect("interaction checked")
                .reply_target
                .clone(),
            reply_event: None,
        };
        if event.staged.terminal != Some(TerminalState::Cancelled) {
            let revision = session
                .revision
                .checked_add(1)
                .ok_or(GatewayError::IdentifierExhausted)?;
            let session = &mut self.sessions[session_index];
            session.events[event_index].staged.terminal = Some(TerminalState::Cancelled);
            session.revision = revision;
            session.batch = None;
        }
        self.last_now_ms = access.now_ms;
        Ok(receipt)
    }

    pub fn revoke_session(
        &mut self,
        access: SessionAccess<'_>,
        id: &SessionId,
    ) -> Result<(), GatewayError> {
        let index = self.session_index(&access, id)?;
        if self.retention.is_some() {
            self.cleanup_fragment(index)?;
            self.last_now_ms = access.now_ms;
            return Ok(());
        }
        let revision = self.sessions[index]
            .revision
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let session = &mut self.sessions[index];
        session.revoked = true;
        session.revision = revision;
        session.batch = None;
        self.last_now_ms = access.now_ms;
        Ok(())
    }

    pub fn read_active(
        &mut self,
        access: SessionAccess<'_>,
        id: &SessionId,
    ) -> Result<SessionSnapshot, GatewayError> {
        let index = self.session_index(&access, id)?;
        let session = &self.sessions[index];
        if !session.active(access.now_ms) {
            return Err(GatewayError::SessionExpired);
        }
        session.lifecycle_usable(access.now_ms)?;
        let snapshot = SessionSnapshot {
            id: session.id.clone(),
            active_until_ms: session.active_until_ms,
            processed_through: session.processed_through,
            events: session
                .events
                .iter()
                .map(|event| event.staged.clone())
                .collect(),
        };
        self.last_now_ms = access.now_ms;
        Ok(snapshot)
    }

    pub(crate) fn capture_reasoning(
        &mut self,
        access: SessionAccess<'_>,
        interaction: &InteractionId,
    ) -> Result<(SessionSnapshot, u64), GatewayError> {
        let (index, _) = self.interaction_index(&access, interaction)?;
        let revision = self.sessions[index].revision;
        self.validate_reasoning(
            SessionAccess::new(access.ingress, access.request, access.now_ms),
            interaction,
            revision,
        )?;
        Ok((self.read_active(access, &interaction.0.session)?, revision))
    }

    pub(crate) fn validate_reasoning(
        &mut self,
        access: SessionAccess<'_>,
        interaction: &InteractionId,
        revision: u64,
    ) -> Result<(), GatewayError> {
        let (session, event) = self.interaction_index(&access, interaction)?;
        let session = &self.sessions[session];
        if session.revision != revision {
            return Err(GatewayError::StaleHandle);
        }
        session.lifecycle_usable(access.now_ms)?;
        let origin = &session.events[event];
        if origin.staged.terminal.is_some() {
            return Err(GatewayError::InteractionClosed);
        }
        if access.now_ms
            >= origin
                .interaction
                .as_ref()
                .expect("interaction checked")
                .deadline_ms
        {
            return Err(GatewayError::DeadlineExceeded);
        }
        self.last_now_ms = access.now_ms;
        Ok(())
    }

    pub fn issue_idle_batch(
        &mut self,
        access: SessionAccess<'_>,
        id: &SessionId,
        budget_allowed: bool,
    ) -> Result<BatchTicket, GatewayError> {
        let index = self.session_index(&access, id)?;
        self.sessions[index].lifecycle_usable(access.now_ms)?;
        let session = &self.sessions[index];
        let related = self
            .sessions
            .iter()
            .filter(|candidate| candidate.matches(access.request));
        let last_user_ms = related
            .clone()
            .filter_map(|candidate| candidate.last_user_ms)
            .max();
        if !budget_allowed
            || last_user_ms.is_none_or(|last| access.now_ms - last < self.timing.quiet_ms)
            || session
                .events
                .iter()
                .any(|event| event.interaction.is_some() && event.staged.terminal.is_none())
            || session.processed_through == session.events.len()
        {
            return Err(GatewayError::NotReady);
        }
        if related
            .filter(|candidate| {
                !candidate.revoked && candidate.lifecycle_usable(access.now_ms).is_ok()
            })
            .any(|candidate| {
                candidate.batch.as_ref().is_some_and(|batch| {
                    !batch.complete && access.now_ms < batch.ticket.deadline_ms
                })
            })
        {
            return Err(GatewayError::BatchInFlight);
        }
        let running = self
            .sessions
            .iter()
            .filter(|session| {
                !session.revoked
                    && session.lifecycle_usable(access.now_ms).is_ok()
                    && session.authorization_generation == access.request.generation()
            })
            .filter(|session| {
                session.batch.as_ref().is_some_and(|batch| {
                    !batch.complete && access.now_ms < batch.ticket.deadline_ms
                })
            })
            .count();
        if running >= self.limits.concurrent_batches {
            return Err(GatewayError::CapacityExceeded);
        }
        let sequence = self
            .batch_sequence
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let deadline_ms = access
            .now_ms
            .checked_add(self.timing.batch_ms)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let ticket = BatchTicket {
            session: id.clone(),
            sequence,
            request_sequence: access.request.sequence(),
            revision: session.revision,
            start: session.processed_through + 1,
            end: session.events.len(),
            watermark: session.processed_through,
            deadline_ms,
        };
        self.sessions[index].batch = Some(BatchRecord {
            ticket: ticket.clone(),
            complete: false,
        });
        self.batch_sequence = sequence;
        self.last_now_ms = access.now_ms;
        Ok(ticket)
    }

    fn ticket_index(
        &self,
        access: &SessionAccess<'_>,
        ticket: &BatchTicket,
    ) -> Result<usize, GatewayError> {
        let index = self.session_index(access, &ticket.session)?;
        let session = &self.sessions[index];
        if ticket.request_sequence != access.request.sequence() {
            return Err(GatewayError::ForeignRequest);
        }
        if ticket.revision != session.revision
            || session
                .batch
                .as_ref()
                .is_none_or(|batch| &batch.ticket != ticket)
        {
            return Err(GatewayError::StaleHandle);
        }
        if access.now_ms >= ticket.deadline_ms {
            return Err(GatewayError::DeadlineExceeded);
        }
        self.sessions[index].lifecycle_usable(access.now_ms)?;
        Ok(index)
    }

    pub fn read_batch(
        &mut self,
        access: SessionAccess<'_>,
        ticket: &BatchTicket,
    ) -> Result<Vec<StagedEvent>, GatewayError> {
        let index = self.ticket_index(&access, ticket)?;
        let events = self.sessions[index].events[ticket.start - 1..ticket.end]
            .iter()
            .map(|event| event.staged.clone())
            .collect();
        self.last_now_ms = access.now_ms;
        Ok(events)
    }

    pub fn acknowledge_batch(
        &mut self,
        access: SessionAccess<'_>,
        ticket: &BatchTicket,
        event: &EventId,
        acknowledgement: BatchAcknowledgement,
    ) -> Result<BatchProgress, GatewayError> {
        let index = self.ticket_index(&access, ticket)?;
        if event.session != ticket.session
            || event.ordinal < ticket.start
            || event.ordinal > ticket.end
        {
            return Err(GatewayError::StaleHandle);
        }
        let staged = &self.sessions[index].events[event.ordinal - 1].staged;
        if staged
            .acknowledgement
            .is_some_and(|existing| existing != acknowledgement)
        {
            return Err(GatewayError::EventConflict);
        }
        let session = &mut self.sessions[index];
        session.events[event.ordinal - 1].staged.acknowledgement = Some(acknowledgement);
        let acknowledged = session.events[ticket.start - 1..ticket.end]
            .iter()
            .filter(|event| event.staged.acknowledgement.is_some())
            .count();
        let total = ticket.end - ticket.start + 1;
        if acknowledged == total {
            session.processed_through = ticket.end;
            session.batch.as_mut().expect("ticket checked").complete = true;
        }
        let progress = BatchProgress {
            acknowledged,
            total,
            processed_through: session.processed_through,
        };
        self.last_now_ms = access.now_ms;
        Ok(progress)
    }

    pub fn cancel_batch(
        &mut self,
        access: SessionAccess<'_>,
        ticket: &BatchTicket,
    ) -> Result<(), GatewayError> {
        let index = self.session_index(&access, &ticket.session)?;
        let session = &self.sessions[index];
        if ticket.request_sequence != access.request.sequence() {
            return Err(GatewayError::ForeignRequest);
        }
        if session
            .batch
            .as_ref()
            .is_none_or(|batch| &batch.ticket != ticket)
        {
            return Err(GatewayError::StaleHandle);
        }
        let revision = session
            .revision
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let session = &mut self.sessions[index];
        session.revision = revision;
        session.batch = None;
        self.last_now_ms = access.now_ms;
        Ok(())
    }
}

impl fmt::Debug for SessionRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionRuntime")
            .field("sessions", &self.sessions.len())
            .field("events", &self.events_total)
            .field("body_chars", &self.chars_total)
            .field("recycle_events", &self.recycle_events)
            .field("recycle_chars", &self.recycle_chars)
            .field(
                "replay_protection_exhausted",
                &self.replay_protection_exhausted,
            )
            .finish_non_exhaustive()
    }
}
