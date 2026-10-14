//! Immutable animation graphs and a host-driven, single-channel animator.
//!
//! No I/O, decoding, drawing or clock reads. Tickets isolate different playbacks,
//! not individual operations within one playback. Backends echo received tickets.

mod definition;
pub use definition::*;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::Arc;

/// Maximum transitions or playback boundaries processed by one advancement.
pub const ANIMATION_ADVANCE_BUDGET: usize = 128;

/// Opaque instance identity, compared by allocation identity.
#[derive(Clone, Debug)]
pub struct AnimatorIdentity(Arc<()>);

impl PartialEq for AnimatorIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for AnimatorIdentity {}

/// Issued only by an Animator. Backends return clones of received tickets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackTicket {
    instance: AnimatorIdentity,
    definition_version: u64,
    clip: String,
    asset_version: u64,
    sequence: u64,
}

impl PlaybackTicket {
    pub fn instance(&self) -> &AnimatorIdentity {
        &self.instance
    }
    pub fn definition_version(&self) -> u64 {
        self.definition_version
    }
    pub fn clip(&self) -> &str {
        &self.clip
    }
    pub fn asset_version(&self) -> u64 {
        self.asset_version
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// Preparation checks the exact asset version and requested playback mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipCapabilities {
    pub pause_resume: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackAck {
    pub ticket: PlaybackTicket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackBoundary {
    Completed,
    LoopBoundary,
}

/// At most one boundary. Without a boundary all supplied time is consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackAdvance {
    pub ticket: PlaybackTicket,
    pub consumed_ms: u64,
    pub boundary: Option<PlaybackBoundary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackPosition {
    pub ticket: PlaybackTicket,
    pub elapsed_ms: u64,
    pub paused: bool,
}

/// Exclusive synchronous backend. Only supplied milliseconds may advance playback.
/// Stop success confirms an empty channel, including after unknown effects.
/// Prepare must not stop or replace an existing playback.
pub trait ClipPlayer {
    type Error: Error + 'static;
    fn prepare(
        &mut self,
        clip: &AnimationClip,
        mode: PlaybackMode,
    ) -> Result<ClipCapabilities, Self::Error>;
    fn start(
        &mut self,
        clip: &AnimationClip,
        mode: PlaybackMode,
        ticket: &PlaybackTicket,
    ) -> Result<PlaybackAck, Self::Error>;
    fn advance(
        &mut self,
        ticket: &PlaybackTicket,
        delta_ms: u64,
    ) -> Result<PlaybackAdvance, Self::Error>;
    fn pause(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error>;
    fn resume(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error>;
    fn stop(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error>;
    fn progress(&self, ticket: &PlaybackTicket) -> Result<PlaybackPosition, Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackOperation {
    Prepare,
    Start,
    Advance,
    Pause,
    Resume,
    Stop,
    Progress,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackProtocolError {
    WrongTicket,
    InvalidConsumption,
    InvalidBoundary,
    InvalidPosition,
}

#[derive(Debug)]
pub enum AnimationError<BackendError> {
    Invalid(AnimationValidationError),
    UnknownAction,
    UnknownParameter,
    UnknownTrigger,
    Paused,
    PauseUnsupported,
    Faulted,
    TimeOverflow,
    SequenceExhausted,
    Backend {
        operation: PlaybackOperation,
        source: BackendError,
    },
    Protocol {
        operation: PlaybackOperation,
        reason: PlaybackProtocolError,
    },
}

impl<BackendError: Error + 'static> fmt::Display for AnimationError<BackendError> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => write!(formatter, "invalid animation command: {error}"),
            Self::UnknownAction => formatter.write_str("unknown animation action"),
            Self::UnknownParameter => formatter.write_str("unknown animation parameter"),
            Self::UnknownTrigger => formatter.write_str("unknown animation trigger"),
            Self::Paused => formatter.write_str("positive advancement while paused"),
            Self::PauseUnsupported => formatter.write_str("playback does not support pause/resume"),
            Self::Faulted => formatter.write_str("animation channel requires confirmed stop"),
            Self::TimeOverflow => formatter.write_str("pending animation milliseconds overflow"),
            Self::SequenceExhausted => formatter.write_str("animation playback sequence exhausted"),
            Self::Backend { operation, source } => {
                write!(formatter, "animation backend {operation:?}: {source}")
            }
            Self::Protocol { operation, reason } => write!(
                formatter,
                "animation backend protocol {operation:?}: {reason:?}"
            ),
        }
    }
}

impl<BackendError: Error + 'static> Error for AnimationError<BackendError> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend { source, .. } => Some(source),
            Self::Invalid(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationRequest {
    Accepted,
    RejectedLowerPriority,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationStatus {
    Idle,
    Playing,
    Paused,
    Yielded,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimationProgress {
    pub status: AnimationStatus,
    pub processed: usize,
    pub pending_ms: u64,
}

/// Owned report; modifications cannot mutate the running instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimatorSnapshot {
    pub instance: AnimatorIdentity,
    pub definition_version: u64,
    pub action: Option<String>,
    pub node: Option<String>,
    pub ticket: Option<PlaybackTicket>,
    pub state: Option<String>,
    pub selected_action: Option<String>,
    pub candidate: Option<String>,
    pub paused: bool,
    pub faulted: bool,
    pub exit_requested: bool,
    pub exiting: bool,
    pub pending_ms: u64,
}

struct Playing {
    action: usize,
    node: usize,
    ticket: PlaybackTicket,
    capabilities: ClipCapabilities,
}

/// One immutable definition and one exclusive subject channel, owned by the host.
/// Dropping the instance does not prove stop; the host must explicitly confirm it.
/// Stop never resets issuance. Exhausted playback sequences require a new instance.
pub struct Animator<Player: ClipPlayer> {
    definition: AnimationDefinition,
    player: Player,
    identity: AnimatorIdentity,
    sequence: u64,
    current: Option<Playing>,
    parameters: BTreeMap<String, bool>,
    triggers: BTreeSet<String>,
    state: Option<String>,
    selected: Option<usize>,
    candidate: Option<usize>,
    immediate: bool,
    exit_requested: bool,
    exiting: bool,
    paused: bool,
    faulted: bool,
    pending_ms: u64,
}

impl<Player: ClipPlayer> Animator<Player> {
    pub fn new(definition: AnimationDefinition, player: Player) -> Self {
        Self {
            parameters: definition.parameters().iter().cloned().collect(),
            definition,
            player,
            identity: AnimatorIdentity(Arc::new(())),
            sequence: 0,
            current: None,
            triggers: BTreeSet::new(),
            state: None,
            selected: None,
            candidate: None,
            immediate: false,
            exit_requested: false,
            exiting: false,
            paused: false,
            faulted: false,
            pending_ms: 0,
        }
    }

    pub fn snapshot(&self) -> AnimatorSnapshot {
        let action_name = |index: usize| self.definition.actions()[index].id.clone();
        AnimatorSnapshot {
            instance: self.identity.clone(),
            definition_version: self.definition.version(),
            action: self
                .current
                .as_ref()
                .map(|current| action_name(current.action)),
            node: self.current.as_ref().map(|current| {
                self.definition.actions()[current.action].nodes[current.node]
                    .id
                    .clone()
            }),
            ticket: self.current.as_ref().map(|current| current.ticket.clone()),
            state: self.state.clone(),
            selected_action: self.selected.map(action_name),
            candidate: self.candidate.map(action_name),
            paused: self.paused,
            faulted: self.faulted,
            exit_requested: self.exit_requested,
            exiting: self.exiting,
            pending_ms: self.pending_ms,
        }
    }

    pub fn parameter(&self, name: &str) -> Option<bool> {
        self.parameters.get(name).copied()
    }

    /// Repeated state/selection pairs do not revoke an explicit queued request.
    pub fn sync_state(&mut self, state: &str) -> Result<(), AnimationError<Player::Error>> {
        self.ready()?;
        if !valid_command_id(state) {
            return Err(AnimationError::Invalid(AnimationValidationError::InvalidId));
        }
        let selected = self.select(state, None);
        if self.state.as_deref() != Some(state) || self.selected != selected {
            self.preflight_selection(selected)?;
            let unchanged_selection = self.selected == selected;
            self.state = Some(state.to_owned());
            self.selected = selected;
            if !(unchanged_selection && selected.is_some() && self.current.is_none()) {
                self.queue_selection(selected);
            }
        }
        self.command_boundary()
    }

    /// Updates a declared boolean and deterministically reevaluates state bindings.
    pub fn set_parameter(
        &mut self,
        name: &str,
        value: bool,
    ) -> Result<(), AnimationError<Player::Error>> {
        self.ready()?;
        let previous = self
            .parameters
            .get(name)
            .ok_or(AnimationError::UnknownParameter)?;
        if *previous != value {
            let selected = self
                .state
                .as_deref()
                .and_then(|state| self.select(state, Some((name, value))));
            self.preflight_selection(selected)?;
            self.parameters.insert(name.to_owned(), value);
            if self.selected != selected {
                self.selected = selected;
                self.queue_selection(selected);
            }
        }
        self.command_boundary()
    }

    /// Coalesces a declared trigger for the current action only.
    pub fn trigger(&mut self, name: &str) -> Result<(), AnimationError<Player::Error>> {
        self.ready()?;
        if !self
            .definition
            .triggers()
            .iter()
            .any(|trigger| trigger == name)
        {
            return Err(AnimationError::UnknownTrigger);
        }
        self.preflight_sequence()?;
        if self.current.is_some() {
            self.triggers.insert(name.to_owned());
        }
        self.command_boundary()
    }

    /// Explicit requests replay even the same action; lower priority is side-effect free.
    /// While paused, accepted requests only replace the single queued candidate.
    pub fn request_action(
        &mut self,
        action: &str,
    ) -> Result<AnimationRequest, AnimationError<Player::Error>> {
        self.ready()?;
        let action = self
            .definition
            .actions()
            .iter()
            .position(|item| item.id == action)
            .ok_or(AnimationError::UnknownAction)?;
        if let Some(current) = &self.current
            && self.definition.actions()[action].priority
                < self.definition.actions()[current.action].priority
        {
            return Ok(AnimationRequest::RejectedLowerPriority);
        }
        self.preflight_sequence()?;
        self.queue(Some(action));
        self.command_boundary()?;
        Ok(AnimationRequest::Accepted)
    }

    /// Graceful exit never re-enters a cleanup already in progress.
    pub fn request_exit(&mut self) -> Result<(), AnimationError<Player::Error>> {
        self.ready()?;
        self.queue(None);
        self.command_boundary()
    }

    /// Adds checked host time and processes at most 128 transitions/boundaries.
    /// Remaining time survives Yielded; zero increment drains it unless paused.
    pub fn advance(
        &mut self,
        delta_ms: u64,
    ) -> Result<AnimationProgress, AnimationError<Player::Error>> {
        self.ready()?;
        if self.paused {
            if delta_ms != 0 {
                return Err(AnimationError::Paused);
            }
            return Ok(self.report(AnimationStatus::Paused, 0));
        }
        let pending_ms = self
            .pending_ms
            .checked_add(delta_ms)
            .ok_or(AnimationError::TimeOverflow)?;
        self.preflight_sequence()?;
        self.pending_ms = pending_ms;
        self.drain(true)
    }

    /// Freezes cursor and debt; repeated successful calls are idempotent.
    pub fn pause(&mut self) -> Result<(), AnimationError<Player::Error>> {
        self.ready()?;
        if self.paused {
            return Ok(());
        }
        if let Some(current) = &self.current {
            if !current.capabilities.pause_resume {
                return Err(AnimationError::PauseUnsupported);
            }
            let ticket = current.ticket.clone();
            let result = self.player.pause(&ticket);
            let ack = self.backend(PlaybackOperation::Pause, result)?;
            self.check_ticket(PlaybackOperation::Pause, &ticket, &ack.ticket)?;
        }
        self.paused = true;
        Ok(())
    }

    /// Resumes the original cursor and processes queued zero-time intentions.
    pub fn resume(&mut self) -> Result<(), AnimationError<Player::Error>> {
        self.ready()?;
        if !self.paused {
            return Ok(());
        }
        self.preflight_sequence()?;
        if let Some(current) = &self.current {
            let ticket = current.ticket.clone();
            let result = self.player.resume(&ticket);
            let ack = self.backend(PlaybackOperation::Resume, result)?;
            self.check_ticket(PlaybackOperation::Resume, &ticket, &ack.ticket)?;
        }
        self.paused = false;
        self.command_boundary()
    }

    /// The only command allowed while faulted. Success confirms channel clearance.
    pub fn stop(&mut self) -> Result<(), AnimationError<Player::Error>> {
        self.stop_playback()?;
        self.clear_intent();
        self.paused = false;
        self.faulted = false;
        Ok(())
    }

    /// Queries and validates position without advancing time. Invalid reports lock the channel.
    pub fn playback_progress(
        &mut self,
    ) -> Result<Option<PlaybackPosition>, AnimationError<Player::Error>> {
        self.ready()?;
        let Some(current) = &self.current else {
            return Ok(None);
        };
        let ticket = current.ticket.clone();
        let node = &self.definition.actions()[current.action].nodes[current.node];
        let mode = node.mode;
        let duration = self.clip(&node.clip).duration_ms();
        let result = self.player.progress(&ticket);
        let position = self.backend(PlaybackOperation::Progress, result)?;
        self.check_ticket(PlaybackOperation::Progress, &ticket, &position.ticket)?;
        if position.elapsed_ms > duration
            || (mode == PlaybackMode::Loop && position.elapsed_ms == duration)
            || position.paused != self.paused
        {
            return self.protocol(
                PlaybackOperation::Progress,
                PlaybackProtocolError::InvalidPosition,
            );
        }
        Ok(Some(position))
    }

    fn ready(&self) -> Result<(), AnimationError<Player::Error>> {
        if self.faulted {
            Err(AnimationError::Faulted)
        } else {
            Ok(())
        }
    }

    fn preflight_sequence(&self) -> Result<(), AnimationError<Player::Error>> {
        if next_playback_sequence(self.sequence).is_none() {
            Err(AnimationError::SequenceExhausted)
        } else {
            Ok(())
        }
    }

    fn preflight_selection(
        &self,
        selected: Option<usize>,
    ) -> Result<(), AnimationError<Player::Error>> {
        if selected.is_some() {
            self.preflight_sequence()?;
        }
        Ok(())
    }

    fn select(&self, state: &str, parameter: Option<(&str, bool)>) -> Option<usize> {
        let mut winner: Option<&AnimationBinding> = None;
        for binding in self.definition.bindings() {
            let matches = binding.state == state
                && binding.parameter.as_ref().is_none_or(|(name, expected)| {
                    let actual = parameter
                        .filter(|(updated, _)| *updated == name)
                        .map(|(_, value)| value)
                        .or_else(|| self.parameters.get(name).copied());
                    actual == Some(*expected)
                });
            if matches && winner.is_none_or(|previous| binding.priority > previous.priority) {
                winner = Some(binding);
            }
        }
        winner.and_then(|binding| {
            self.definition
                .actions()
                .iter()
                .position(|action| action.id == binding.action)
        })
    }

    fn queue_selection(&mut self, selected: Option<usize>) {
        if !self.exiting
            && selected.is_some()
            && self
                .current
                .as_ref()
                .is_some_and(|current| Some(current.action) == selected)
        {
            self.candidate = None;
            self.immediate = false;
            self.exit_requested = false;
        } else {
            self.queue(selected);
        }
    }

    fn queue(&mut self, candidate: Option<usize>) {
        self.candidate = candidate;
        self.exit_requested = self.current.is_some();
        self.immediate = candidate.is_some_and(|next| {
            self.current.as_ref().is_none_or(|current| {
                let next = &self.definition.actions()[next];
                next.priority > self.definition.actions()[current.action].priority
                    && next.preemption == PreemptionPolicy::Immediate
            })
        });
    }

    fn command_boundary(&mut self) -> Result<(), AnimationError<Player::Error>> {
        if !self.paused {
            self.drain(false)?;
        }
        Ok(())
    }

    fn report(&self, status: AnimationStatus, processed: usize) -> AnimationProgress {
        AnimationProgress {
            status,
            processed,
            pending_ms: self.pending_ms,
        }
    }

    fn drain(
        &mut self,
        allow_time: bool,
    ) -> Result<AnimationProgress, AnimationError<Player::Error>> {
        let mut processed = 0;
        loop {
            if self.current.is_none() && self.candidate.is_none() {
                self.clear_intent();
                return Ok(self.report(AnimationStatus::Idle, processed));
            }
            if processed == ANIMATION_ADVANCE_BUDGET {
                return Ok(self.report(AnimationStatus::Yielded, processed));
            }
            if self.current.is_none() || self.immediate {
                let action = self.candidate.expect("pending animation action");
                let entry = self.definition.actions()[action].entry.clone();
                self.switch(action, &entry)?;
                self.candidate = None;
                self.immediate = false;
                self.exit_requested = false;
                self.triggers.clear();
                processed += 1;
                continue;
            }
            if !self.exit_requested
                && !self.exiting
                && let Some(edge) = self.edge(None)
            {
                self.follow(edge)?;
                processed += 1;
                continue;
            }
            if !allow_time || self.pending_ms == 0 {
                return Ok(self.report(AnimationStatus::Playing, processed));
            }
            let current = self.current.as_ref().expect("active animation playback");
            let ticket = current.ticket.clone();
            let mode = self.definition.actions()[current.action].nodes[current.node].mode;
            let result = self.player.advance(&ticket, self.pending_ms);
            let advance = self.backend(PlaybackOperation::Advance, result)?;
            self.check_ticket(PlaybackOperation::Advance, &ticket, &advance.ticket)?;
            if advance.consumed_ms > self.pending_ms
                || (advance.boundary.is_none() && advance.consumed_ms != self.pending_ms)
                || (advance.boundary.is_some() && advance.consumed_ms == 0)
            {
                return self.protocol(
                    PlaybackOperation::Advance,
                    PlaybackProtocolError::InvalidConsumption,
                );
            }
            if advance.boundary.is_some_and(|boundary| {
                !matches!(
                    (mode, boundary),
                    (PlaybackMode::Once, PlaybackBoundary::Completed)
                        | (PlaybackMode::Loop, PlaybackBoundary::LoopBoundary)
                )
            }) {
                return self.protocol(
                    PlaybackOperation::Advance,
                    PlaybackProtocolError::InvalidBoundary,
                );
            }
            self.pending_ms -= advance.consumed_ms;
            let Some(boundary) = advance.boundary else {
                return Ok(self.report(AnimationStatus::Playing, processed));
            };
            processed += 1;
            if self.exit_requested && !self.exiting {
                let action = self
                    .current
                    .as_ref()
                    .expect("active animation playback")
                    .action;
                if let Some(exit) = self.definition.actions()[action].exit.clone() {
                    self.switch(action, &exit)?;
                    self.exiting = true;
                } else {
                    self.finish()?;
                }
            } else if let Some(edge) = self.edge(Some(boundary)) {
                self.follow(edge)?;
            } else if boundary == PlaybackBoundary::Completed {
                self.finish()?;
            }
        }
    }

    fn edge(&self, boundary: Option<PlaybackBoundary>) -> Option<AnimationEdge> {
        let current = self.current.as_ref()?;
        let node = &self.definition.actions()[current.action].nodes[current.node];
        let mut winner: Option<&AnimationEdge> = None;
        for edge in &node.edges {
            let matches = match &edge.condition {
                EdgeCondition::Completed => boundary == Some(PlaybackBoundary::Completed),
                EdgeCondition::LoopBoundary => boundary == Some(PlaybackBoundary::LoopBoundary),
                EdgeCondition::Parameter { name, value } => {
                    self.parameters.get(name) == Some(value)
                }
                EdgeCondition::Trigger(name) => self.triggers.contains(name),
            };
            if matches && winner.is_none_or(|previous| edge.priority > previous.priority) {
                winner = Some(edge);
            }
        }
        winner.cloned()
    }

    fn follow(&mut self, edge: AnimationEdge) -> Result<(), AnimationError<Player::Error>> {
        match edge.target {
            AnimationTarget::Node(node) => {
                let action = self
                    .current
                    .as_ref()
                    .expect("active animation playback")
                    .action;
                self.switch(action, &node)?;
            }
            AnimationTarget::Finish => self.finish()?,
        }
        if let EdgeCondition::Trigger(name) = edge.condition {
            self.triggers.remove(&name);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), AnimationError<Player::Error>> {
        if let Some(action) = self.candidate {
            let entry = self.definition.actions()[action].entry.clone();
            self.switch(action, &entry)?;
            self.candidate = None;
            self.exit_requested = false;
            self.immediate = false;
            self.triggers.clear();
        } else {
            self.stop_playback()?;
            self.clear_intent();
        }
        Ok(())
    }

    fn clip(&self, id: &str) -> &AnimationClip {
        self.definition
            .clips()
            .iter()
            .find(|clip| clip.id() == id)
            .expect("validated animation clip")
    }

    fn switch(
        &mut self,
        action: usize,
        node_id: &str,
    ) -> Result<(), AnimationError<Player::Error>> {
        let sequence =
            next_playback_sequence(self.sequence).ok_or(AnimationError::SequenceExhausted)?;
        let node = self.definition.actions()[action]
            .nodes
            .iter()
            .position(|node| node.id == node_id)
            .expect("validated animation node");
        let target = &self.definition.actions()[action].nodes[node];
        let mode = target.mode;
        let clip = self.clip(&target.clip).clone();
        let result = self.player.prepare(&clip, mode);
        let capabilities = self.backend(PlaybackOperation::Prepare, result)?;
        self.stop_playback()?;
        self.sequence = sequence;
        let ticket = PlaybackTicket {
            instance: self.identity.clone(),
            definition_version: self.definition.version(),
            clip: clip.id().to_owned(),
            asset_version: clip.asset_version(),
            sequence: self.sequence,
        };
        self.current = Some(Playing {
            action,
            node,
            ticket: ticket.clone(),
            capabilities,
        });
        let result = self.player.start(&clip, mode, &ticket);
        let ack = self.backend(PlaybackOperation::Start, result)?;
        self.check_ticket(PlaybackOperation::Start, &ticket, &ack.ticket)?;
        self.exiting = self.is_exit_node(action, node_id);
        Ok(())
    }

    fn is_exit_node(&self, action: usize, node: &str) -> bool {
        let action = &self.definition.actions()[action];
        let mut cursor = action.exit.as_deref();
        while let Some(current) = cursor {
            if current == node {
                return true;
            }
            let current = action
                .nodes
                .iter()
                .find(|candidate| candidate.id == current)
                .expect("validated exit node");
            cursor = current.edges.first().and_then(|edge| match &edge.target {
                AnimationTarget::Node(next) => Some(next.as_str()),
                AnimationTarget::Finish => None,
            });
        }
        false
    }

    fn stop_playback(&mut self) -> Result<(), AnimationError<Player::Error>> {
        if let Some(current) = &self.current {
            let ticket = current.ticket.clone();
            let result = self.player.stop(&ticket);
            let ack = self.backend(PlaybackOperation::Stop, result)?;
            self.check_ticket(PlaybackOperation::Stop, &ticket, &ack.ticket)?;
            self.current = None;
        }
        Ok(())
    }

    fn clear_intent(&mut self) {
        self.candidate = None;
        self.immediate = false;
        self.exit_requested = false;
        self.exiting = false;
        self.triggers.clear();
        self.pending_ms = 0;
    }

    fn backend<Value>(
        &mut self,
        operation: PlaybackOperation,
        result: Result<Value, Player::Error>,
    ) -> Result<Value, AnimationError<Player::Error>> {
        result.map_err(|source| {
            self.faulted = true;
            AnimationError::Backend { operation, source }
        })
    }

    fn check_ticket(
        &mut self,
        operation: PlaybackOperation,
        expected: &PlaybackTicket,
        actual: &PlaybackTicket,
    ) -> Result<(), AnimationError<Player::Error>> {
        if expected != actual {
            return self.protocol(operation, PlaybackProtocolError::WrongTicket);
        }
        Ok(())
    }

    fn protocol<Value>(
        &mut self,
        operation: PlaybackOperation,
        reason: PlaybackProtocolError,
    ) -> Result<Value, AnimationError<Player::Error>> {
        self.faulted = true;
        Err(AnimationError::Protocol { operation, reason })
    }
}

fn valid_command_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn next_playback_sequence(sequence: u64) -> Option<u64> {
    sequence.checked_add(1)
}

#[cfg(test)]
mod tests {
    use super::next_playback_sequence;

    #[test]
    fn playback_sequence_exhaustion_never_reuses_zero_or_old_values() {
        assert_eq!(next_playback_sequence(0), Some(1));
        assert_eq!(next_playback_sequence(u64::MAX - 1), Some(u64::MAX));
        assert_eq!(next_playback_sequence(u64::MAX), None);
        assert_eq!(next_playback_sequence(u64::MAX), None);
    }
}
