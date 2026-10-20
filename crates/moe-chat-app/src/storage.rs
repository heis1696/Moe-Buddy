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

/// Body-free logical session entry recovered from host persistence.
#[derive(Clone)]
pub struct HistorySession {
    pub id: String,
    pub label: String,
    pub archived: bool,
    pub status: crate::ChatStatus,
}

/// Bounded startup/transition result. History is a read-only recent projection.
pub struct HistoryRecovery {
    pub sessions_revision: u64,
    pub active_session_id: String,
    pub active_storage_hint: String,
    /// Persisted evidence that the unresolved interaction had been handed to an external transport.
    pub active_external_unknown: bool,
    /// Persisted evidence that a trusted raw-history recovery found an unresolved interaction.
    pub active_recovered_unsettled: bool,
    pub sessions: Vec<HistorySession>,
    pub active_history: Vec<crate::ChatMessage>,
    pub receipts: HistoryCommitReceipts,
}

/// Application decision sent to the host adapter after its own optimistic checks.
pub enum HistoryChange {
    Create(HistorySession),
    Switch(String),
    Renew(String),
}

/// Scoped trusted request for a single cold logical session. The adapter creates
/// its own temporary runtime and raw-history grant when it processes this request.
pub struct HistoryRequest {
    pub logical_session_id: String,
    pub request: moe_agent_runtime::TrustedRequest,
    pub grant: moe_agent_runtime::RawHistoryGrant,
}

/// Work completed by one bounded live and cold retention pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryMaintenanceReport {
    pub live: moe_agent_runtime::MaintenanceReport,
    pub live_checkpointed: bool,
    pub cold_segments_processed: usize,
    pub cold_segments_remaining: usize,
    pub cold_blocked: Option<ColdMaintenanceBlock>,
    pub receipts: HistoryCommitReceipts,
}

/// Actual revision/durability evidence returned by independent storage commits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryCommitReceipts {
    pub raw: Vec<RawHistoryCommitReceipt>,
    pub directory: Option<DirectoryPublishReceipt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawHistoryCommitReceipt {
    pub revision: u64,
    pub committed_at_ms: u64,
    pub file_synced: bool,
    pub directory_synced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryPublishReceipt {
    pub revision: u64,
    pub file_synced: bool,
    pub directory_synced: bool,
}

/// Bounded cold maintenance outcome, including each segment commit receipt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColdHistoryMaintenance {
    pub segments_processed: usize,
    pub raw_commits: Vec<RawHistoryCommitReceipt>,
    pub blocked: Option<ColdMaintenanceBlock>,
}

/// A bounded maintenance pass stopped before its next segment could be committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColdMaintenanceBlock {
    CapacityExceeded,
}

/// Capacity reserved for the next send after a generation rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistorySendReservation {
    pub user_characters: usize,
    pub reserved_reply_characters: usize,
}

/// Host adapter for versioned logical sessions and independent raw segments.
/// Restoration returns only a bounded projection and never resumes execution.
pub trait ChatHistory {
    /// Read and lock the body-free directory before the application binds the active session.
    fn open_directory(&mut self) -> Result<Option<HistoryRecovery>, AppError>;

    fn last_commit_receipts(&self) -> HistoryCommitReceipts;

    fn session_storage_hint(&self, session_id: &str) -> Result<String, AppError>;

    fn open(
        &mut self,
        access: HistoryAccess<'_>,
        requests: &[HistoryRequest],
        expected_sessions_revision: u64,
    ) -> Result<HistoryRecovery, AppError>;

    fn checkpoint(
        &mut self,
        access: HistoryAccess<'_>,
        expected_sessions_revision: u64,
    ) -> Result<HistoryCommitReceipts, AppError>;

    /// Reserve a conservative peak budget before a user interaction is admitted.
    fn preflight_checkpoint(
        &mut self,
        access: HistoryAccess<'_>,
        expected_sessions_revision: u64,
        user_characters: usize,
        reserved_reply_characters: usize,
    ) -> Result<(), AppError>;

    /// Seal the old generation, prepare a blank baseline for `next`, then CAS-publish
    /// the directory. The old and new trusted access contexts are deliberately distinct.
    fn rotate(
        &mut self,
        current: HistoryAccess<'_>,
        next: HistoryAccess<'_>,
        expected_sessions_revision: u64,
        change: HistoryChange,
        send_reservation: Option<HistorySendReservation>,
    ) -> Result<HistoryRecovery, AppError>;

    fn archive(
        &mut self,
        access: HistoryAccess<'_>,
        session_id: &str,
        expected_sessions_revision: u64,
    ) -> Result<HistoryRecovery, AppError>;

    fn load_history(
        &mut self,
        access: HistoryAccess<'_>,
        session_id: &str,
        max_messages: usize,
    ) -> Result<Vec<crate::ChatMessage>, AppError>;

    fn cold_session_ids(&self, max_sessions: usize) -> Vec<String>;
    fn cold_segment_count(&self) -> usize;

    /// Maintain a bounded number of cold segments in addition to the live runtime.
    fn maintain_cold(
        &mut self,
        access: HistoryAccess<'_>,
        requests: &[HistoryRequest],
        max_segments: usize,
    ) -> Result<ColdHistoryMaintenance, AppError>;
}

/// Ephemeral trusted access for one persistence operation; never serialize its handles.
pub struct HistoryAccess<'a> {
    pub runtime: &'a mut moe_agent_runtime::SessionRuntime,
    pub ingress: &'a moe_agent_runtime::TrustedIngress,
    pub request: &'a moe_agent_runtime::TrustedRequest,
    pub grant: &'a moe_agent_runtime::RawHistoryGrant,
    pub retention: &'a moe_agent_runtime::RawRetention,
    pub now_ms: u64,
    pub logical_session_id: &'a str,
    pub status: crate::ChatStatus,
    pub online: bool,
    pub external_request_pending: bool,
    pub external_unknown: bool,
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
            .map_err(|error| AppError::from_memory(error).settled())
    }
    fn assemble_context(
        &mut self,
        task: &MemoryTask,
        budget: usize,
        now: u64,
    ) -> Result<RequestContext, AppError> {
        self.0
            .assemble_context(task, budget, now)
            .map_err(|error| AppError::from_memory(error).settled())
    }
    fn context_view<'a>(
        &mut self,
        context: &'a RequestContext,
        now: u64,
    ) -> Result<&'a ContextView, AppError> {
        self.0
            .context_view(context, now)
            .map_err(|error| AppError::from_memory(error).settled())
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
            .map_err(|error| AppError::from_memory(error).settled())
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
            .map_err(|error| AppError::from_memory(error).settled())
    }
    fn set_read_policy(
        &mut self,
        identity: &PetIdentity,
        policy: ReadPolicy,
    ) -> Result<(), AppError> {
        self.0
            .set_read_policy(identity, policy)
            .map_err(|error| AppError::from_memory(error).settled())
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
            .map_err(|error| AppError::from_memory(error).settled())
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
            .map_err(|error| AppError::from_memory(error).settled())
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
            .map_err(|error| AppError::from_memory(error).settled())
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
            .map_err(|error| AppError::from_memory(error).settled())
    }
}
