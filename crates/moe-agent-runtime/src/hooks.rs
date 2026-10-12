use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::ingress::HookIngressProof;
use crate::{GatewayError, IngressToken, TrustedIngress, validation};

const HARD_MAX_EVENT_CHARS: usize = 16_384;
const HARD_MAX_CONTEXT_CHARS: usize = 16_384;
const HARD_MAX_TOTAL_PROMPT_CHARS: usize = 1_048_576;
const HARD_MAX_TOTAL_CONTEXT_CHARS: usize = 1_048_576;
const HARD_MAX_ACTIVE_BINDINGS: usize = 1_024;
const HARD_MAX_CONTEXTS: usize = 128;
const HARD_MAX_ISSUED_BINDINGS: usize = 8_192;
const HARD_MAX_ISSUED_INVOCATIONS: usize = 4_096;
const HARD_MAX_BINDING_LIFETIME_MS: u64 = 86_400_000;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookPermissions {
    pub observe_prompt: bool,
    pub return_context: bool,
    pub allow_model_input: bool,
}

impl HookPermissions {
    pub fn new(observe_prompt: bool, return_context: bool, allow_model_input: bool) -> Self {
        Self {
            observe_prompt,
            return_context,
            allow_model_input,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookLimits {
    max_active_bindings: usize,
    max_contexts: usize,
    max_issued_bindings: usize,
    max_issued_invocations: usize,
    max_event_chars: usize,
    max_context_chars: usize,
    max_lifetime_ms: u64,
}

impl HookLimits {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_active_bindings: usize,
        max_contexts: usize,
        max_issued_bindings: usize,
        max_issued_invocations: usize,
        max_event_chars: usize,
        max_context_chars: usize,
        max_lifetime_ms: u64,
    ) -> Result<Self, GatewayError> {
        if max_active_bindings == 0
            || max_contexts == 0
            || max_issued_bindings == 0
            || max_issued_invocations == 0
            || max_event_chars == 0
            || max_context_chars == 0
            || max_lifetime_ms == 0
            || max_active_bindings > HARD_MAX_ACTIVE_BINDINGS
            || max_contexts > HARD_MAX_CONTEXTS
            || max_issued_bindings > HARD_MAX_ISSUED_BINDINGS
            || max_issued_bindings < max_active_bindings
            || max_issued_invocations > HARD_MAX_ISSUED_INVOCATIONS
            || max_event_chars > HARD_MAX_EVENT_CHARS
            || max_context_chars > HARD_MAX_CONTEXT_CHARS
            || max_lifetime_ms > HARD_MAX_BINDING_LIFETIME_MS
        {
            return Err(GatewayError::InvalidInput("invalid hook limits"));
        }
        Ok(Self {
            max_active_bindings,
            max_contexts,
            max_issued_bindings,
            max_issued_invocations,
            max_event_chars,
            max_context_chars,
            max_lifetime_ms,
        })
    }
}

impl Default for HookLimits {
    fn default() -> Self {
        Self::new(16, 64, 256, 1_024, 16_384, 16_384, 3_600_000)
            .expect("default hook limits are valid")
    }
}

#[derive(Clone)]
pub struct HookEvent {
    session_id: String,
    prompt: String,
}

impl HookEvent {
    pub fn prompt_submitted(session_id: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            prompt: prompt.into(),
        }
    }
}

impl fmt::Debug for HookEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookEvent")
            .field("event", &"UserPromptSubmit")
            .field("session_id", &"REDACTED")
            .field("prompt", &"REDACTED")
            .finish()
    }
}

pub struct HookContext {
    source_id: String,
    revision: u64,
    text: String,
}

impl HookContext {
    pub fn new(source_id: impl Into<String>, revision: u64, text: impl Into<String>) -> Self {
        Self {
            source_id: source_id.into(),
            revision,
            text: text.into(),
        }
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl fmt::Debug for HookContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookContext")
            .field("source_id", &"REDACTED")
            .field("revision", &self.revision)
            .field("text", &"REDACTED")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HookBindingConfig {
    permissions: HookPermissions,
    cli_session_id: Option<String>,
    context_source_id: Option<String>,
    expires_at_ms: u64,
}

impl HookBindingConfig {
    pub fn new(permissions: HookPermissions, expires_at_ms: u64) -> Self {
        Self {
            permissions,
            cli_session_id: None,
            context_source_id: None,
            expires_at_ms,
        }
    }

    pub fn with_context_source(mut self, source_id: impl Into<String>) -> Self {
        self.context_source_id = Some(source_id.into());
        self
    }

    pub fn with_cli_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.cli_session_id = Some(session_id.into());
        self
    }

    pub fn permissions(&self) -> HookPermissions {
        self.permissions
    }

    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }
}

impl fmt::Debug for HookBindingConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookBindingConfig")
            .field("permissions", &self.permissions)
            .field(
                "cli_session_id",
                &self.cli_session_id.as_ref().map(|_| "present"),
            )
            .field(
                "context_source_id",
                &self.context_source_id.as_ref().map(|_| "present"),
            )
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

pub struct HookBinding {
    owner: Arc<()>,
    id: u64,
    revision: u64,
}

impl HookBinding {
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl fmt::Debug for HookBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookBinding")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

pub struct HookInvocation {
    owner: Arc<()>,
    id: u64,
}

impl fmt::Debug for HookInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookInvocation")
            .field("id", &self.id)
            .field("data", &"REDACTED")
            .finish()
    }
}

pub struct UnverifiedPrompt {
    text: String,
}

impl UnverifiedPrompt {
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Debug for UnverifiedPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UnverifiedPrompt(REDACTED)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookReplyRole {
    InjectedContext,
}

pub struct HookReply {
    owner: Arc<()>,
    invocation_id: u64,
    binding_id: u64,
    binding_revision: u64,
    context_source_id: String,
    context_revision: u64,
    text: String,
    reply_target: String,
    cli_session_id: String,
}

impl HookReply {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn role(&self) -> HookReplyRole {
        HookReplyRole::InjectedContext
    }

    pub fn reply_target(&self) -> &str {
        &self.reply_target
    }

    pub fn cli_session_id(&self) -> &str {
        &self.cli_session_id
    }
}

impl fmt::Debug for HookReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookReply")
            .field("role", &self.role())
            .field("text", &"REDACTED")
            .field("reply_target", &"REDACTED")
            .field("cli_session_id", &"REDACTED")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookTransportOutcome {
    Written,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HookReceipt {
    invocation_id: u64,
    outcome: HookTransportOutcome,
}

impl HookReceipt {
    pub fn outcome(&self) -> HookTransportOutcome {
        self.outcome
    }
}

impl fmt::Debug for HookReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookReceipt")
            .field("invocation_id", &self.invocation_id)
            .field("outcome", &self.outcome)
            .finish()
    }
}

struct BindingRecord {
    proof: HookIngressProof,
    config: HookBindingConfig,
    revision: u64,
}

struct InvocationRecord {
    binding_id: u64,
    binding_revision: u64,
    session_id: String,
    prompt: Option<String>,
    context_source_id: Option<String>,
    context_revision: Option<u64>,
    deadline_ms: u64,
    reply_prepared: bool,
    write_authorized: bool,
    outcome: Option<HookTransportOutcome>,
}

pub struct HookRuntime {
    owner: Arc<()>,
    limits: HookLimits,
    bindings: HashMap<u64, BindingRecord>,
    contexts: HashMap<String, HookContext>,
    context_revisions: HashMap<String, u64>,
    invocations: HashMap<u64, InvocationRecord>,
    receipts: Vec<HookReceipt>,
    binding_sequence: u64,
    invocation_sequence: u64,
    issued_bindings: usize,
    issued_invocations: usize,
    issued_prompt_chars: usize,
    last_now_ms: Option<u64>,
}

impl HookRuntime {
    pub fn new(limits: HookLimits) -> Self {
        Self {
            owner: Arc::new(()),
            limits,
            bindings: HashMap::new(),
            contexts: HashMap::new(),
            context_revisions: HashMap::new(),
            invocations: HashMap::new(),
            receipts: Vec::new(),
            binding_sequence: 0,
            invocation_sequence: 0,
            issued_bindings: 0,
            issued_invocations: 0,
            issued_prompt_chars: 0,
            last_now_ms: None,
        }
    }

    pub fn register_context(
        &mut self,
        context: HookContext,
        expected_revision: Option<u64>,
    ) -> Result<(), GatewayError> {
        validation::identifier(&context.source_id)?;
        validation::text(&context.text, self.limits.max_context_chars)?;
        let latest_revision = self.context_revisions.get(&context.source_id).copied();
        if latest_revision != expected_revision
            || context.revision == 0
            || latest_revision.is_some_and(|revision| context.revision <= revision)
        {
            return Err(GatewayError::VersionConflict);
        }
        if latest_revision.is_none() && self.context_revisions.len() >= self.limits.max_contexts {
            return Err(GatewayError::CapacityExceeded);
        }
        let current_total = self
            .contexts
            .values()
            .map(|entry| entry.text.chars().count())
            .sum::<usize>();
        let replaced_chars = self
            .contexts
            .get(&context.source_id)
            .map_or(0, |entry| entry.text.chars().count());
        let next_total = current_total
            .checked_sub(replaced_chars)
            .and_then(|total| total.checked_add(context.text.chars().count()))
            .ok_or(GatewayError::CapacityExceeded)?;
        if next_total > HARD_MAX_TOTAL_CONTEXT_CHARS {
            return Err(GatewayError::CapacityExceeded);
        }
        if let Some(previous_revision) = latest_revision {
            Self::clear_context_prompts(
                &mut self.invocations,
                &context.source_id,
                previous_revision,
            );
        }
        self.context_revisions
            .insert(context.source_id.clone(), context.revision);
        self.contexts.insert(context.source_id.clone(), context);
        Ok(())
    }

    pub fn revoke_context(
        &mut self,
        source_id: &str,
        expected_revision: u64,
        next_revision: u64,
    ) -> Result<(), GatewayError> {
        let latest = self
            .context_revisions
            .get(source_id)
            .copied()
            .ok_or(GatewayError::StaleHandle)?;
        if latest != expected_revision || next_revision <= latest {
            return Err(GatewayError::VersionConflict);
        }
        Self::clear_context_prompts(&mut self.invocations, source_id, latest);
        self.contexts.remove(source_id);
        self.context_revisions
            .insert(source_id.to_owned(), next_revision);
        Ok(())
    }

    pub fn bind(
        &mut self,
        ingress: &TrustedIngress,
        token: &IngressToken,
        alias: &str,
        config: HookBindingConfig,
        now_ms: u64,
    ) -> Result<HookBinding, GatewayError> {
        self.advance_time(now_ms)?;
        let proof = ingress.prove_hook_authorization(token, alias)?;
        self.validate_config(&config, now_ms)?;
        if self.bindings.len() >= self.limits.max_active_bindings {
            return Err(GatewayError::CapacityExceeded);
        }
        if self.issued_bindings >= self.limits.max_issued_bindings {
            return Err(GatewayError::ReplayProtectionExhausted);
        }
        let id = self
            .binding_sequence
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        self.binding_sequence = id;
        self.issued_bindings += 1;
        self.bindings.insert(
            id,
            BindingRecord {
                proof,
                config,
                revision: 1,
            },
        );
        Ok(self.binding_handle(id, 1))
    }

    pub fn update_binding(
        &mut self,
        ingress: &TrustedIngress,
        binding: &HookBinding,
        expected_revision: u64,
        config: HookBindingConfig,
        now_ms: u64,
    ) -> Result<HookBinding, GatewayError> {
        self.advance_time(now_ms)?;
        let record = self.binding_record(binding)?;
        if record.revision != expected_revision {
            return Err(GatewayError::VersionConflict);
        }
        ingress.validate_hook_proof(&record.proof)?;
        self.validate_config(&config, now_ms)?;
        let revision = record
            .revision
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let record = self
            .bindings
            .get_mut(&binding.id)
            .expect("binding was validated");
        record.config = config;
        record.revision = revision;
        Self::clear_binding_prompts(&mut self.invocations, binding.id, binding.revision);
        Ok(self.binding_handle(binding.id, revision))
    }

    pub fn revoke_binding(
        &mut self,
        binding: &HookBinding,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<(), GatewayError> {
        self.advance_time(now_ms)?;
        let record = self.binding_record(binding)?;
        if record.revision != expected_revision {
            return Err(GatewayError::VersionConflict);
        }
        self.bindings.remove(&binding.id);
        Self::clear_binding_prompts(&mut self.invocations, binding.id, binding.revision);
        Ok(())
    }

    pub fn accept_event(
        &mut self,
        ingress: &TrustedIngress,
        binding: &HookBinding,
        event: HookEvent,
        now_ms: u64,
    ) -> Result<HookInvocation, GatewayError> {
        self.advance_time(now_ms)?;
        let record = self.binding_record(binding)?;
        ingress.validate_hook_proof(&record.proof)?;
        self.ensure_binding_live(record, now_ms)?;
        let config = record.config.clone();
        if !config.permissions.observe_prompt && !config.permissions.return_context {
            return Err(GatewayError::Unauthorized);
        }
        validation::identifier(&event.session_id)?;
        if config.cli_session_id.as_deref() != Some(event.session_id.as_str()) {
            return Err(GatewayError::Unauthorized);
        }
        validation::text(&event.prompt, self.limits.max_event_chars)?;
        let (context_source_id, context_revision) = match config.context_source_id.as_deref() {
            Some(source_id) => {
                let context = self
                    .contexts
                    .get(source_id)
                    .ok_or(GatewayError::MissingBinding)?;
                (Some(source_id.to_owned()), Some(context.revision))
            }
            None if config.permissions.return_context => {
                return Err(GatewayError::MissingBinding);
            }
            None => (None, None),
        };
        let prompt_chars = event.prompt.chars().count();
        let next_prompt_chars = self
            .issued_prompt_chars
            .checked_add(prompt_chars)
            .ok_or(GatewayError::CapacityExceeded)?;
        if self.issued_invocations >= self.limits.max_issued_invocations
            || next_prompt_chars > HARD_MAX_TOTAL_PROMPT_CHARS
        {
            return Err(GatewayError::ReplayProtectionExhausted);
        }
        let id = self
            .invocation_sequence
            .checked_add(1)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let maximum_deadline = now_ms
            .checked_add(self.limits.max_lifetime_ms)
            .ok_or(GatewayError::IdentifierExhausted)?;
        let deadline_ms = maximum_deadline.min(config.expires_at_ms);
        self.invocation_sequence = id;
        self.issued_invocations += 1;
        self.issued_prompt_chars = next_prompt_chars;
        self.invocations.insert(
            id,
            InvocationRecord {
                binding_id: binding.id,
                binding_revision: binding.revision,
                session_id: event.session_id,
                prompt: config.permissions.observe_prompt.then_some(event.prompt),
                context_source_id,
                context_revision,
                deadline_ms,
                reply_prepared: false,
                write_authorized: false,
                outcome: None,
            },
        );
        Ok(HookInvocation {
            owner: Arc::clone(&self.owner),
            id,
        })
    }

    pub fn observe_prompt(
        &mut self,
        ingress: &TrustedIngress,
        binding: &HookBinding,
        invocation: &HookInvocation,
        now_ms: u64,
    ) -> Result<UnverifiedPrompt, GatewayError> {
        self.advance_time(now_ms)?;
        let (binding_record, invocation_record) =
            self.live_invocation(ingress, binding, invocation, now_ms)?;
        if !binding_record.config.permissions.observe_prompt {
            return Err(GatewayError::Unauthorized);
        }
        let text = invocation_record
            .prompt
            .as_ref()
            .ok_or(GatewayError::Unauthorized)?
            .clone();
        Ok(UnverifiedPrompt { text })
    }

    pub fn prepare_reply(
        &mut self,
        ingress: &TrustedIngress,
        binding: &HookBinding,
        invocation: &HookInvocation,
        now_ms: u64,
    ) -> Result<HookReply, GatewayError> {
        self.advance_time(now_ms)?;
        let (binding_record, invocation_record) =
            self.live_invocation(ingress, binding, invocation, now_ms)?;
        if !binding_record.config.permissions.return_context
            || !binding_record.config.permissions.allow_model_input
        {
            return Err(GatewayError::Unauthorized);
        }
        if invocation_record.reply_prepared {
            return Err(GatewayError::InteractionClosed);
        }
        let source_id = invocation_record
            .context_source_id
            .as_deref()
            .ok_or(GatewayError::MissingBinding)?;
        let expected_revision = invocation_record
            .context_revision
            .ok_or(GatewayError::MissingBinding)?;
        let context = self
            .contexts
            .get(source_id)
            .filter(|context| context.revision == expected_revision)
            .ok_or(GatewayError::Revoked)?;
        let text = context.text.clone();
        let context_source_id = source_id.to_owned();
        let context_revision = expected_revision;
        let reply_target = binding_record.proof.reply_target().as_str().to_owned();
        let binding_id = binding.id;
        let binding_revision = binding.revision;
        let cli_session_id = invocation_record.session_id.clone();
        self.invocations
            .get_mut(&invocation.id)
            .expect("invocation was validated")
            .reply_prepared = true;
        Ok(HookReply {
            owner: Arc::clone(&self.owner),
            invocation_id: invocation.id,
            binding_id,
            binding_revision,
            context_source_id,
            context_revision,
            text,
            reply_target,
            cli_session_id,
        })
    }

    pub fn authorize_before_write(
        &mut self,
        ingress: &TrustedIngress,
        binding: &HookBinding,
        invocation: &HookInvocation,
        reply: &HookReply,
        now_ms: u64,
    ) -> Result<(), GatewayError> {
        self.advance_time(now_ms)?;
        let (binding_record, invocation_record) =
            self.live_invocation(ingress, binding, invocation, now_ms)?;
        self.validate_reply(binding, invocation, reply, invocation_record)?;
        if !binding_record.config.permissions.return_context
            || !binding_record.config.permissions.allow_model_input
            || !invocation_record.reply_prepared
        {
            return Err(GatewayError::Unauthorized);
        }
        self.contexts
            .get(&reply.context_source_id)
            .filter(|context| {
                context.revision == reply.context_revision && context.text == reply.text
            })
            .ok_or(GatewayError::Revoked)?;
        self.invocations
            .get_mut(&invocation.id)
            .expect("invocation was validated")
            .write_authorized = true;
        Ok(())
    }

    pub fn record_transport_outcome(
        &mut self,
        ingress: &TrustedIngress,
        binding: &HookBinding,
        invocation: &HookInvocation,
        reply: &HookReply,
        now_ms: u64,
        outcome: HookTransportOutcome,
    ) -> Result<HookReceipt, GatewayError> {
        self.advance_time(now_ms)?;
        self.ensure_invocation_owner(invocation)?;
        self.ensure_reply_owner(reply, invocation)?;
        let invocation_record = self
            .invocations
            .get(&invocation.id)
            .ok_or(GatewayError::StaleHandle)?;
        if invocation_record.outcome.is_some() {
            return Err(GatewayError::InteractionClosed);
        }
        self.validate_reply(binding, invocation, reply, invocation_record)?;
        if !invocation_record.reply_prepared || !invocation_record.write_authorized {
            return Err(GatewayError::Unauthorized);
        }
        let live = match self.live_invocation(ingress, binding, invocation, now_ms) {
            Ok((binding_record, invocation_record)) => {
                self.validate_reply(binding, invocation, reply, invocation_record)?;
                if !binding_record.config.permissions.return_context
                    || !binding_record.config.permissions.allow_model_input
                    || !invocation_record.reply_prepared
                {
                    return Err(GatewayError::Unauthorized);
                }
                self.contexts
                    .get(&reply.context_source_id)
                    .is_some_and(|context| {
                        context.revision == reply.context_revision && context.text == reply.text
                    })
            }
            Err(GatewayError::ForeignRequest) => return Err(GatewayError::ForeignRequest),
            Err(GatewayError::InteractionClosed) => return Err(GatewayError::InteractionClosed),
            Err(_) => false,
        };
        let recorded_outcome = if live || outcome == HookTransportOutcome::Cancelled {
            outcome
        } else {
            HookTransportOutcome::Unknown
        };
        self.finish_invocation(invocation.id, recorded_outcome)
    }

    pub fn cancel(
        &mut self,
        invocation: &HookInvocation,
        now_ms: u64,
    ) -> Result<HookReceipt, GatewayError> {
        self.advance_time(now_ms)?;
        self.ensure_invocation_owner(invocation)?;
        let record = self
            .invocations
            .get(&invocation.id)
            .ok_or(GatewayError::StaleHandle)?;
        if record.outcome.is_some() {
            return Err(GatewayError::InteractionClosed);
        }
        self.finish_invocation(invocation.id, HookTransportOutcome::Cancelled)
    }

    pub fn receipts(&self) -> &[HookReceipt] {
        &self.receipts
    }

    pub fn fail_invocation(
        &mut self,
        invocation: &HookInvocation,
        outcome: HookTransportOutcome,
        now_ms: u64,
    ) -> Result<HookReceipt, GatewayError> {
        self.advance_time(now_ms)?;
        self.ensure_invocation_owner(invocation)?;
        if !matches!(
            outcome,
            HookTransportOutcome::Failed | HookTransportOutcome::Unknown
        ) {
            return Err(GatewayError::InvalidInput(
                "failure cleanup cannot prove delivery or cancellation",
            ));
        }
        self.finish_invocation(invocation.id, outcome)
    }

    fn finish_invocation(
        &mut self,
        invocation_id: u64,
        outcome: HookTransportOutcome,
    ) -> Result<HookReceipt, GatewayError> {
        let record = self
            .invocations
            .get_mut(&invocation_id)
            .ok_or(GatewayError::StaleHandle)?;
        if record.outcome.is_some() {
            return Err(GatewayError::InteractionClosed);
        }
        record.outcome = Some(outcome);
        record.prompt = None;
        record.session_id.clear();
        record.context_source_id = None;
        record.context_revision = None;
        let receipt = HookReceipt {
            invocation_id,
            outcome,
        };
        self.receipts.push(receipt);
        Ok(receipt)
    }

    fn validate_config(&self, config: &HookBindingConfig, now_ms: u64) -> Result<(), GatewayError> {
        let session_id = config
            .cli_session_id
            .as_deref()
            .ok_or(GatewayError::MissingBinding)?;
        validation::identifier(session_id)?;
        if config.expires_at_ms <= now_ms {
            return Err(GatewayError::DeadlineExceeded);
        }
        if config.expires_at_ms - now_ms > self.limits.max_lifetime_ms {
            return Err(GatewayError::InvalidInput("hook binding lifetime too long"));
        }
        if config.permissions.return_context && config.context_source_id.is_none() {
            return Err(GatewayError::MissingBinding);
        }
        if let Some(source_id) = &config.context_source_id {
            validation::identifier(source_id)?;
            if !self.contexts.contains_key(source_id) {
                return Err(GatewayError::MissingBinding);
            }
        }
        Ok(())
    }

    fn ensure_binding_live(&self, record: &BindingRecord, now_ms: u64) -> Result<(), GatewayError> {
        if now_ms >= record.config.expires_at_ms {
            return Err(GatewayError::DeadlineExceeded);
        }
        Ok(())
    }

    fn binding_handle(&self, id: u64, revision: u64) -> HookBinding {
        HookBinding {
            owner: Arc::clone(&self.owner),
            id,
            revision,
        }
    }

    fn clear_context_prompts(
        invocations: &mut HashMap<u64, InvocationRecord>,
        source_id: &str,
        revision: u64,
    ) {
        for invocation in invocations.values_mut() {
            if invocation.context_source_id.as_deref() == Some(source_id)
                && invocation.context_revision == Some(revision)
            {
                invocation.prompt = None;
            }
        }
    }

    fn clear_binding_prompts(
        invocations: &mut HashMap<u64, InvocationRecord>,
        binding_id: u64,
        revision: u64,
    ) {
        for invocation in invocations.values_mut() {
            if invocation.binding_id == binding_id && invocation.binding_revision == revision {
                invocation.prompt = None;
            }
        }
    }

    fn binding_record(&self, binding: &HookBinding) -> Result<&BindingRecord, GatewayError> {
        if !Arc::ptr_eq(&self.owner, &binding.owner) {
            return Err(GatewayError::ForeignRequest);
        }
        let record = self
            .bindings
            .get(&binding.id)
            .ok_or(GatewayError::StaleHandle)?;
        if record.revision != binding.revision {
            return Err(GatewayError::StaleHandle);
        }
        Ok(record)
    }

    fn live_invocation<'a>(
        &'a self,
        ingress: &TrustedIngress,
        binding: &HookBinding,
        invocation: &HookInvocation,
        now_ms: u64,
    ) -> Result<(&'a BindingRecord, &'a InvocationRecord), GatewayError> {
        self.ensure_invocation_owner(invocation)?;
        let binding_record = self.binding_record(binding)?;
        ingress.validate_hook_proof(&binding_record.proof)?;
        self.ensure_binding_live(binding_record, now_ms)?;
        let invocation_record = self
            .invocations
            .get(&invocation.id)
            .ok_or(GatewayError::StaleHandle)?;
        if invocation_record.binding_id != binding.id
            || invocation_record.binding_revision != binding.revision
        {
            return Err(GatewayError::StaleHandle);
        }
        if invocation_record.outcome.is_some() {
            return Err(GatewayError::InteractionClosed);
        }
        if now_ms >= invocation_record.deadline_ms {
            return Err(GatewayError::DeadlineExceeded);
        }
        if let (Some(source_id), Some(revision)) = (
            invocation_record.context_source_id.as_deref(),
            invocation_record.context_revision,
        ) && !self
            .contexts
            .get(source_id)
            .is_some_and(|context| context.revision == revision)
        {
            return Err(GatewayError::Revoked);
        }
        Ok((binding_record, invocation_record))
    }

    fn validate_reply(
        &self,
        binding: &HookBinding,
        invocation: &HookInvocation,
        reply: &HookReply,
        invocation_record: &InvocationRecord,
    ) -> Result<(), GatewayError> {
        self.ensure_reply_owner(reply, invocation)?;
        if !Arc::ptr_eq(&self.owner, &binding.owner) {
            return Err(GatewayError::ForeignRequest);
        }
        if reply.binding_id != binding.id
            || reply.binding_revision != binding.revision
            || reply.context_source_id
                != invocation_record.context_source_id.as_deref().unwrap_or("")
            || Some(reply.context_revision) != invocation_record.context_revision
            || reply.cli_session_id != invocation_record.session_id
        {
            return Err(GatewayError::StaleHandle);
        }
        Ok(())
    }

    fn ensure_reply_owner(
        &self,
        reply: &HookReply,
        invocation: &HookInvocation,
    ) -> Result<(), GatewayError> {
        self.ensure_invocation_owner(invocation)?;
        if !Arc::ptr_eq(&self.owner, &reply.owner) || reply.invocation_id != invocation.id {
            return Err(GatewayError::ForeignRequest);
        }
        Ok(())
    }

    fn ensure_invocation_owner(&self, invocation: &HookInvocation) -> Result<(), GatewayError> {
        if !Arc::ptr_eq(&self.owner, &invocation.owner) {
            return Err(GatewayError::ForeignRequest);
        }
        Ok(())
    }

    fn advance_time(&mut self, now_ms: u64) -> Result<(), GatewayError> {
        if self.last_now_ms.is_some_and(|last| now_ms < last) {
            return Err(GatewayError::ClockWentBackwards);
        }
        self.last_now_ms = Some(now_ms);
        Ok(())
    }
}

impl fmt::Debug for HookRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookRuntime")
            .field("active_bindings", &self.bindings.len())
            .field("contexts", &self.contexts.len())
            .field("issued_invocations", &self.issued_invocations)
            .field("receipts", &self.receipts.len())
            .finish()
    }
}
