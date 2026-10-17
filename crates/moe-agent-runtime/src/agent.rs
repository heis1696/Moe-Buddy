use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_memory_core::{
    ContextBlock, MemoryContextAccess, MemoryError, MemoryService, PetIdentity, ReadPurpose,
    RequestContext, RetrievalQuery, SessionId as MemorySessionId, SourceRef,
};

use crate::{
    ChannelId, ChannelScope, GatewayError, InteractionId, PresetId, PresetPurpose, PresetRegistry,
    PresetVersion, ReplyTarget, RequestIsolation, SessionAccess, SessionId, SessionRuntime,
    SessionSnapshot, StagedEvent, TerminalReceipt, TerminalState, TrustedIngress, TrustedRequest,
    validation,
};

static NEXT_AGENT_RUNTIME: AtomicU64 = AtomicU64::new(1);
const MAX_GRANT_SOURCES: usize = 256;
const MAX_PRESENTATION_EVENTS: usize = 16;

fn allocate_instance(counter: &AtomicU64) -> Result<u64, GatewayError> {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| GatewayError::IdentifierExhausted)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentError<E = MemoryError> {
    Gateway(GatewayError),
    Memory(E),
    MemoryScopeDenied,
    PersonaUnavailable,
    TaskAlreadyIssued,
    InvalidTaskState,
}

impl<E: fmt::Display> fmt::Display for AgentError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gateway(error) => error.fmt(formatter),
            Self::Memory(error) => error.fmt(formatter),
            Self::MemoryScopeDenied => {
                formatter.write_str("actual read set exceeds session source grant")
            }
            Self::PersonaUnavailable => {
                formatter.write_str("authorized protected persona is unavailable")
            }
            Self::TaskAlreadyIssued => formatter.write_str("interaction already has an agent task"),
            Self::InvalidTaskState => {
                formatter.write_str("agent task is not in the required state")
            }
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for AgentError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gateway(error) => Some(error),
            Self::Memory(error) => Some(error),
            _ => None,
        }
    }
}

impl<E> From<GatewayError> for AgentError<E> {
    fn from(error: GatewayError) -> Self {
        Self::Gateway(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentLimits {
    bindings: usize,
    tasks: usize,
    context_chars: usize,
    reply_chars: usize,
}

impl AgentLimits {
    pub fn new(
        bindings: usize,
        tasks: usize,
        context_chars: usize,
        reply_chars: usize,
    ) -> Result<Self, GatewayError> {
        if [bindings, tasks, context_chars, reply_chars].contains(&0) {
            return Err(GatewayError::InvalidInput("agent limits must be positive"));
        }
        Ok(Self {
            bindings,
            tasks,
            context_chars,
            reply_chars,
        })
    }
}

pub struct AgentAccess<'a> {
    ingress: &'a TrustedIngress,
    request: &'a TrustedRequest,
    presets: &'a PresetRegistry,
    now_ms: u64,
}

impl<'a> AgentAccess<'a> {
    pub fn new(
        ingress: &'a TrustedIngress,
        request: &'a TrustedRequest,
        presets: &'a PresetRegistry,
        now_ms: u64,
    ) -> Self {
        Self {
            ingress,
            request,
            presets,
            now_ms,
        }
    }

    pub(crate) fn session(&self) -> SessionAccess<'_> {
        SessionAccess::new(self.ingress, self.request, self.now_ms)
    }
}

pub struct AgentServices<'a, M: MemoryContextAccess + ?Sized = MemoryService> {
    sessions: &'a mut SessionRuntime,
    memory: &'a mut M,
}

impl From<MemoryError> for AgentError<MemoryError> {
    fn from(error: MemoryError) -> Self {
        Self::Memory(error)
    }
}

impl<'a, M: MemoryContextAccess + ?Sized> AgentServices<'a, M> {
    pub fn new(sessions: &'a mut SessionRuntime, memory: &'a mut M) -> Self {
        Self { sessions, memory }
    }
}

#[derive(Clone)]
pub struct AgentBinding {
    owner: Arc<()>,
    index: usize,
    memory_session: MemorySessionId,
}

impl AgentBinding {
    pub fn memory_session(&self) -> &MemorySessionId {
        &self.memory_session
    }
}

impl fmt::Debug for AgentBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AgentBinding(REDACTED)")
    }
}

struct Binding {
    handle: AgentBinding,
    session: SessionId,
    isolation: RequestIsolation,
    generation: u64,
    sources: BTreeSet<SourceRef>,
    revoked: bool,
}

pub struct AgentRuntime {
    owner: Arc<()>,
    instance: u64,
    ingress_run: u64,
    limits: AgentLimits,
    bindings: Vec<Binding>,
    issued: Vec<InteractionId>,
}

impl fmt::Debug for AgentRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentRuntime")
            .field("bindings", &self.bindings.len())
            .field("issued_tasks", &self.issued.len())
            .finish_non_exhaustive()
    }
}

impl AgentRuntime {
    pub fn new(ingress: &TrustedIngress, limits: AgentLimits) -> Result<Self, GatewayError> {
        let instance = allocate_instance(&NEXT_AGENT_RUNTIME)?;
        Ok(Self {
            owner: Arc::new(()),
            instance,
            ingress_run: ingress.run_instance(),
            limits,
            bindings: Vec::new(),
            issued: Vec::new(),
        })
    }

    fn check<E>(&self, access: &AgentAccess<'_>) -> Result<(), AgentError<E>> {
        access.request.validate(access.ingress)?;
        if access.ingress.run_instance() != self.ingress_run {
            return Err(GatewayError::ForeignRequest.into());
        }
        Ok(())
    }

    fn binding<'a, E>(
        &'a self,
        access: &AgentAccess<'_>,
        handle: &AgentBinding,
    ) -> Result<&'a Binding, AgentError<E>> {
        self.check(access)?;
        if !Arc::ptr_eq(&self.owner, &handle.owner) {
            return Err(GatewayError::ForeignRequest.into());
        }
        let binding = self
            .bindings
            .get(handle.index)
            .ok_or(GatewayError::StaleHandle)?;
        if binding.revoked
            || binding.generation != access.request.generation()
            || &binding.isolation != access.request.isolation()
        {
            return Err(GatewayError::Revoked.into());
        }
        Ok(binding)
    }

    pub fn bind_session(
        &mut self,
        access: &AgentAccess<'_>,
        sessions: &mut SessionRuntime,
        session: &SessionId,
        sources: Vec<SourceRef>,
    ) -> Result<AgentBinding, AgentError> {
        self.check(access)?;
        if sources.len() > MAX_GRANT_SOURCES {
            return Err(GatewayError::CapacityExceeded.into());
        }
        sessions.read_active(access.session(), session)?;
        let sources: BTreeSet<_> = sources.into_iter().collect();
        if let Some(binding) = self
            .bindings
            .iter()
            .find(|binding| &binding.session == session)
        {
            self.binding(access, &binding.handle)?;
            if binding.sources != sources {
                return Err(GatewayError::EventConflict.into());
            }
            return Ok(binding.handle.clone());
        }
        if self.bindings.len() >= self.limits.bindings {
            return Err(GatewayError::CapacityExceeded.into());
        }
        let handle = AgentBinding {
            owner: self.owner.clone(),
            index: self.bindings.len(),
            memory_session: MemorySessionId::new(format!(
                "agent-{}-{}",
                self.instance,
                self.bindings.len()
            ))?,
        };
        self.bindings.push(Binding {
            handle: handle.clone(),
            session: session.clone(),
            isolation: access.request.isolation().clone(),
            generation: access.request.generation(),
            sources,
            revoked: false,
        });
        Ok(handle)
    }

    pub fn revoke_binding(&mut self, handle: &AgentBinding) -> Result<(), GatewayError> {
        if !Arc::ptr_eq(&self.owner, &handle.owner) {
            return Err(GatewayError::ForeignRequest);
        }
        self.bindings
            .get_mut(handle.index)
            .ok_or(GatewayError::StaleHandle)?
            .revoked = true;
        Ok(())
    }

    pub fn prepare<M: MemoryContextAccess + ?Sized>(
        &mut self,
        access: &AgentAccess<'_>,
        services: AgentServices<'_, M>,
        binding: &AgentBinding,
        interaction: &InteractionId,
        query: &RetrievalQuery,
    ) -> Result<AgentTask, AgentError<M::Error>> {
        self.prepare_for(
            access,
            services,
            binding,
            interaction,
            query,
            ReadPurpose::OfflineReasoning,
        )
    }

    /// Prepare model input only after explicit trusted consent for the current preset.
    /// Memory sources additionally require `ReadPolicy::model_transmission`.
    pub fn prepare_external<M: MemoryContextAccess + ?Sized>(
        &mut self,
        access: &AgentAccess<'_>,
        services: AgentServices<'_, M>,
        binding: &AgentBinding,
        interaction: &InteractionId,
        query: &RetrievalQuery,
    ) -> Result<AgentTask, AgentError<M::Error>> {
        self.prepare_for(
            access,
            services,
            binding,
            interaction,
            query,
            ReadPurpose::ModelTransmission,
        )
    }

    fn prepare_for<M: MemoryContextAccess + ?Sized>(
        &mut self,
        access: &AgentAccess<'_>,
        services: AgentServices<'_, M>,
        binding: &AgentBinding,
        interaction: &InteractionId,
        query: &RetrievalQuery,
        purpose: ReadPurpose,
    ) -> Result<AgentTask, AgentError<M::Error>> {
        let grant = self.binding(access, binding)?;
        if self.issued.contains(interaction) {
            return Err(AgentError::TaskAlreadyIssued);
        }
        if self.issued.len() >= self.limits.tasks {
            return Err(GatewayError::CapacityExceeded.into());
        }
        let (conversation, session_revision) = services
            .sessions
            .capture_reasoning(access.session(), interaction)?;
        if conversation.id != grant.session {
            return Err(GatewayError::ForeignRequest.into());
        }
        let resolved = access.presets.resolve(PresetPurpose::Chat)?;
        let memory_task = services
            .memory
            .begin_memory_task(
                access.request.identity(),
                binding.memory_session(),
                purpose,
                query,
                access.now_ms,
            )
            .map_err(AgentError::Memory)?;
        let identity = memory_task
            .identity_view()
            .ok_or(AgentError::PersonaUnavailable)?;
        if !identity
            .sources()
            .iter()
            .chain(
                memory_task
                    .result()
                    .items()
                    .iter()
                    .flat_map(|item| item.sources()),
            )
            .chain(
                memory_task
                    .transmissions()
                    .iter()
                    .flat_map(|evidence| evidence.sent_sources()),
            )
            .all(|source| grant.sources.contains(source))
        {
            return Err(AgentError::MemoryScopeDenied);
        }
        let context = services
            .memory
            .assemble_context(&memory_task, self.limits.context_chars, access.now_ms)
            .map_err(AgentError::Memory)?;
        let task = AgentTask {
            owner: self.owner.clone(),
            result_owner: Arc::new(()),
            binding: binding.clone(),
            request_sequence: access.request.sequence(),
            interaction: interaction.clone(),
            session_revision,
            preset_configuration: access.presets.configuration().clone(),
            preset_id: resolved.id().clone(),
            preset_version: resolved.version(),
            context,
            conversation,
            phase: Phase::Prepared,
            external: purpose == ReadPurpose::ModelTransmission,
            last_now_ms: access.now_ms,
        };
        self.issued.push(interaction.clone());
        Ok(task)
    }

    fn validate<M: MemoryContextAccess + ?Sized>(
        &self,
        access: &AgentAccess<'_>,
        services: &mut AgentServices<'_, M>,
        task: &AgentTask,
    ) -> Result<(), AgentError<M::Error>> {
        self.binding(access, &task.binding)?;
        if !Arc::ptr_eq(&self.owner, &task.owner)
            || task.request_sequence != access.request.sequence()
        {
            return Err(GatewayError::ForeignRequest.into());
        }
        if access.now_ms < task.last_now_ms {
            return Err(GatewayError::ClockWentBackwards.into());
        }
        if !Arc::ptr_eq(access.presets.configuration(), &task.preset_configuration) {
            return Err(GatewayError::VersionConflict.into());
        }
        services.sessions.validate_reasoning(
            access.session(),
            &task.interaction,
            task.session_revision,
        )?;
        services
            .memory
            .context_view(&task.context, access.now_ms)
            .map_err(AgentError::Memory)?;
        Ok(())
    }

    pub fn infer<M: MemoryContextAccess + ?Sized>(
        &self,
        access: &AgentAccess<'_>,
        mut services: AgentServices<'_, M>,
        task: &mut AgentTask,
        model: &mut dyn OfflineModel,
    ) -> Result<AgentResult, AgentError<M::Error>> {
        self.validate(access, &mut services, task)?;
        if task.phase != Phase::Prepared || task.external {
            return Err(AgentError::InvalidTaskState);
        }
        let context = services
            .memory
            .context_view(&task.context, access.now_ms)
            .map_err(AgentError::Memory)?;
        let outcome = model.infer(OfflineInput {
            identity: access.request.identity(),
            channel: access.request.channel(),
            scope: access.request.scope(),
            user_text: access.request.text(),
            persona: context
                .identity()
                .ok_or(AgentError::PersonaUnavailable)?
                .text(),
            memory: context.blocks(),
            conversation: &task.conversation.events,
            preset_id: &task.preset_id,
            preset_version: task.preset_version,
        });
        let outcome = match outcome {
            ModelOutcome::Text {
                ref text,
                ref presentation,
            } if validation::text(text, self.limits.reply_chars).is_err()
                || presentation.len() > MAX_PRESENTATION_EVENTS =>
            {
                ModelOutcome::Failed(ModelFailure::InvalidOutput)
            }
            other => other,
        };
        task.phase = Phase::Inferred;
        task.last_now_ms = access.now_ms;
        Ok(AgentResult {
            owner: task.result_owner.clone(),
            outcome,
        })
    }

    /// Revalidate the exact context, preset, session and source revisions at dispatch.
    /// The host must send immediately to this endpoint, without redirects or retries.
    /// Returned plaintext cannot be recalled; cancellation prevents accepting late output.
    pub fn dispatch_external<M: MemoryContextAccess + ?Sized>(
        &self,
        access: &AgentAccess<'_>,
        mut services: AgentServices<'_, M>,
        task: &mut AgentTask,
    ) -> Result<ExternalInput, AgentError<M::Error>> {
        self.validate(access, &mut services, task)?;
        if task.phase != Phase::Prepared || !task.external {
            return Err(AgentError::InvalidTaskState);
        }
        let context = services
            .memory
            .context_view(&task.context, access.now_ms)
            .map_err(AgentError::Memory)?;
        let preset = access.presets.resolve(PresetPurpose::Chat)?;
        let input = ExternalInput {
            endpoint: preset.config().endpoint().to_owned(),
            model: preset.config().default_model().to_owned(),
            persona: context
                .identity()
                .ok_or(AgentError::PersonaUnavailable)?
                .text()
                .to_owned(),
            memory: context
                .blocks()
                .iter()
                .map(|block| block.text().to_owned())
                .collect(),
            conversation: task.conversation.events.clone(),
        };
        task.phase = Phase::Dispatched;
        task.last_now_ms = access.now_ms;
        Ok(input)
    }

    /// Accept one host transport result. This grants no authority to model proposals.
    /// Revocation/version/cancellation checks run again before the normal `finish` path.
    pub fn complete_external<M: MemoryContextAccess + ?Sized>(
        &self,
        access: &AgentAccess<'_>,
        mut services: AgentServices<'_, M>,
        task: &mut AgentTask,
        outcome: ModelOutcome,
    ) -> Result<AgentResult, AgentError<M::Error>> {
        self.validate(access, &mut services, task)?;
        if task.phase != Phase::Dispatched || !task.external {
            return Err(AgentError::InvalidTaskState);
        }
        let outcome = match outcome {
            ModelOutcome::Text {
                ref text,
                ref presentation,
            } if validation::text(text, self.limits.reply_chars).is_err()
                || presentation.len() > MAX_PRESENTATION_EVENTS =>
            {
                ModelOutcome::Failed(ModelFailure::InvalidOutput)
            }
            other => other,
        };
        task.phase = Phase::Inferred;
        task.last_now_ms = access.now_ms;
        Ok(AgentResult {
            owner: task.result_owner.clone(),
            outcome,
        })
    }

    pub fn finish<M: MemoryContextAccess + ?Sized>(
        &self,
        access: &AgentAccess<'_>,
        mut services: AgentServices<'_, M>,
        task: &mut AgentTask,
        result: &AgentResult,
    ) -> Result<AgentDelivery, AgentError<M::Error>> {
        self.validate(access, &mut services, task)?;
        if task.phase != Phase::Inferred {
            return Err(AgentError::InvalidTaskState);
        }
        if !Arc::ptr_eq(&task.result_owner, &result.owner) {
            return Err(GatewayError::ForeignRequest.into());
        }
        let (state, text, presentation, failure) = match &result.outcome {
            ModelOutcome::Text { text, presentation } => (
                TerminalState::Completed,
                Some(text.as_str()),
                presentation.as_slice(),
                None,
            ),
            ModelOutcome::Failed(failure) => (TerminalState::Failed, None, &[][..], Some(*failure)),
            ModelOutcome::Cancelled => (TerminalState::Cancelled, None, &[][..], None),
            ModelOutcome::Unknown => (TerminalState::Unknown, None, &[][..], None),
        };
        let receipt = services.sessions.finish_interaction(
            access.session(),
            &task.interaction,
            state,
            text,
        )?;
        task.phase = Phase::Finished;
        task.last_now_ms = access.now_ms;
        Ok(AgentDelivery {
            receipt,
            channel: access.request.channel().clone(),
            text: text.map(str::to_owned),
            presentation: presentation.to_vec(),
            failure,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Prepared,
    Dispatched,
    Inferred,
    Finished,
}

pub struct AgentTask {
    owner: Arc<()>,
    result_owner: Arc<()>,
    binding: AgentBinding,
    request_sequence: u64,
    interaction: InteractionId,
    session_revision: u64,
    preset_configuration: Arc<()>,
    preset_id: PresetId,
    preset_version: PresetVersion,
    context: RequestContext,
    conversation: SessionSnapshot,
    phase: Phase,
    external: bool,
    last_now_ms: u64,
}

impl fmt::Debug for AgentTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentTask")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

pub struct OfflineInput<'a> {
    identity: &'a PetIdentity,
    channel: &'a ChannelId,
    scope: &'a ChannelScope,
    user_text: &'a str,
    persona: &'a str,
    memory: &'a [ContextBlock],
    conversation: &'a [StagedEvent],
    preset_id: &'a PresetId,
    preset_version: PresetVersion,
}

impl OfflineInput<'_> {
    pub fn identity(&self) -> &PetIdentity {
        self.identity
    }
    pub fn channel(&self) -> &ChannelId {
        self.channel
    }
    pub fn scope(&self) -> &ChannelScope {
        self.scope
    }
    pub fn user_text(&self) -> &str {
        self.user_text
    }
    pub fn persona(&self) -> &str {
        self.persona
    }
    pub fn memory(&self) -> &[ContextBlock] {
        self.memory
    }
    pub fn conversation(&self) -> &[StagedEvent] {
        self.conversation
    }
    pub fn preset_id(&self) -> &PresetId {
        self.preset_id
    }
    pub fn preset_version(&self) -> PresetVersion {
        self.preset_version
    }
    pub fn tools_enabled(&self) -> bool {
        false
    }
}

impl fmt::Debug for OfflineInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OfflineInput(REDACTED; offline only; tools disabled)")
    }
}

pub trait OfflineModel {
    fn infer(&mut self, input: OfflineInput<'_>) -> ModelOutcome;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationEvent {
    Neutral,
    Happy,
    Thinking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelFailure {
    Unavailable,
    InvalidOutput,
}

pub enum ModelOutcome {
    Text {
        text: String,
        presentation: Vec<PresentationEvent>,
    },
    Failed(ModelFailure),
    Cancelled,
    Unknown,
}

impl fmt::Debug for ModelOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { .. } => formatter.write_str("ModelOutcome::Text(REDACTED)"),
            Self::Failed(failure) => formatter
                .debug_tuple("ModelOutcome::Failed")
                .field(failure)
                .finish(),
            Self::Cancelled => formatter.write_str("ModelOutcome::Cancelled"),
            Self::Unknown => formatter.write_str("ModelOutcome::Unknown"),
        }
    }
}

pub struct AgentResult {
    owner: Arc<()>,
    outcome: ModelOutcome,
}

impl fmt::Debug for AgentResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentResult")
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

pub struct AgentDelivery {
    receipt: TerminalReceipt,
    channel: ChannelId,
    text: Option<String>,
    presentation: Vec<PresentationEvent>,
    failure: Option<ModelFailure>,
}

impl AgentDelivery {
    pub fn state(&self) -> TerminalState {
        self.receipt.state
    }
    pub fn reply_target(&self) -> &ReplyTarget {
        &self.receipt.reply_target
    }
    pub fn channel(&self) -> &ChannelId {
        &self.channel
    }
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
    pub fn presentation(&self) -> &[PresentationEvent] {
        &self.presentation
    }
    pub fn failure(&self) -> Option<ModelFailure> {
        self.failure
    }
}

impl fmt::Debug for AgentDelivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentDelivery")
            .field("state", &self.receipt.state)
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

/// Immutable plaintext for a trusted host transport. No credentials or executable tools.
/// Do not log or persist this request; the source context remains runtime-owned.
pub struct ExternalInput {
    endpoint: String,
    model: String,
    persona: String,
    memory: Vec<String>,
    conversation: Vec<StagedEvent>,
}
impl ExternalInput {
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn persona(&self) -> &str {
        &self.persona
    }
    pub fn memory(&self) -> &[String] {
        &self.memory
    }
    pub fn conversation(&self) -> &[StagedEvent] {
        &self.conversation
    }
}
impl fmt::Debug for ExternalInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExternalInput(REDACTED)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_identifier_exhaustion_does_not_wrap_or_reuse_memory_sessions() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_instance(&counter), Ok(u64::MAX - 1));
        assert_eq!(
            allocate_instance(&counter),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(
            allocate_instance(&counter),
            Err(GatewayError::IdentifierExhausted)
        );
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
