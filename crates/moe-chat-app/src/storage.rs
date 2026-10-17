//! Narrow injected memory operations; implementations own persistence and its errors.
use crate::AppError;
use moe_memory_core::*;

/// Trusted storage adapter. No paths or I/O are chosen by the application.
pub trait ChatMemory: MemoryContextAccess<Error = AppError> {
    fn register_source(
        &mut self,
        scope: MemoryScope,
        name: &str,
        revision: u64,
        epistemic: Epistemic,
    ) -> Result<SourceRef, AppError>;
    fn set_identity_view(
        &mut self,
        identity: &PetIdentity,
        version: u64,
        text: &str,
        sources: Vec<SourceRef>,
    ) -> Result<(), AppError>;
    fn set_read_policy(
        &mut self,
        identity: &PetIdentity,
        policy: ReadPolicy,
    ) -> Result<(), AppError>;
    fn create(
        &mut self,
        identity: &PetIdentity,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError>;
    fn update(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError>;
    fn delete(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        now: u64,
    ) -> Result<WriteReceipt, AppError>;
    fn retrieve(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<RetrievalResult, AppError>;
}

/// Host adapter for the existing raw-history format; restoration never resumes execution.
pub trait ChatHistory {
    fn open(
        &mut self,
        access: HistoryAccess<'_>,
    ) -> Result<Vec<moe_agent_runtime::StagedEvent>, AppError>;
    fn checkpoint(&mut self, access: HistoryAccess<'_>) -> Result<(), AppError>;
}

/// Ephemeral trusted access for one persistence operation; never serialize its handles.
pub struct HistoryAccess<'a> {
    pub runtime: &'a mut moe_agent_runtime::SessionRuntime,
    pub ingress: &'a moe_agent_runtime::TrustedIngress,
    pub request: &'a moe_agent_runtime::TrustedRequest,
    pub grant: &'a moe_agent_runtime::RawHistoryGrant,
    pub retention: &'a moe_agent_runtime::RawRetention,
    pub now_ms: u64,
}

pub(crate) struct VolatileMemory(pub MemoryService);
impl MemoryContextAccess for VolatileMemory {
    type Error = AppError;
    fn begin_memory_task(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<MemoryTask, AppError> {
        self.0
            .begin_memory_task(identity, session, purpose, query, now)
            .map_err(AppError::from_memory)
    }
    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget: usize,
        now: u64,
    ) -> Result<RequestContext, AppError> {
        self.0
            .assemble_context(task, budget, now)
            .map_err(AppError::from_memory)
    }
    fn context_view<'a>(
        &mut self,
        context: &'a RequestContext,
        now: u64,
    ) -> Result<&'a ContextView, AppError> {
        self.0
            .context_view(context, now)
            .map_err(AppError::from_memory)
    }
}
impl ChatMemory for VolatileMemory {
    fn register_source(
        &mut self,
        scope: MemoryScope,
        name: &str,
        revision: u64,
        epistemic: Epistemic,
    ) -> Result<SourceRef, AppError> {
        self.0
            .register_source(scope, name, revision, epistemic)
            .map_err(AppError::from_memory)
    }
    fn set_identity_view(
        &mut self,
        identity: &PetIdentity,
        version: u64,
        text: &str,
        sources: Vec<SourceRef>,
    ) -> Result<(), AppError> {
        self.0
            .set_identity_view(identity, version, text, sources)
            .map_err(AppError::from_memory)
    }
    fn set_read_policy(
        &mut self,
        identity: &PetIdentity,
        policy: ReadPolicy,
    ) -> Result<(), AppError> {
        self.0
            .set_read_policy(identity, policy)
            .map_err(AppError::from_memory)
    }
    fn create(
        &mut self,
        identity: &PetIdentity,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .create(draft, now)
            .map_err(AppError::from_memory)
    }
    fn update(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        draft: MemoryDraft,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .update(id, revision, draft, now)
            .map_err(AppError::from_memory)
    }
    fn delete(
        &mut self,
        identity: &PetIdentity,
        id: MemoryId,
        revision: u64,
        now: u64,
    ) -> Result<WriteReceipt, AppError> {
        self.0
            .for_pet(identity)
            .delete(id, revision, now)
            .map_err(AppError::from_memory)
    }
    fn retrieve(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        query: &RetrievalQuery,
        now: u64,
    ) -> Result<RetrievalResult, AppError> {
        self.0
            .for_session(identity, session, ReadPurpose::OfflineReasoning)
            .retrieve(query, now)
            .map_err(AppError::from_memory)
    }
}
