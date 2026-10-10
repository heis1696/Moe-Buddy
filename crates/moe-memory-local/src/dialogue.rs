use std::fmt;

use moe_memory_core::{
    CacheStatus, CommandResult, Epistemic, IdentityView, MemoryCommand, MemoryDraft, MemoryError,
    MemoryRecord, MemoryTask, PetIdentity, ReadPurpose, RecordVersion, RetrievalQuery,
    RetrievalResult, SessionId, WriteReceipt,
};

use crate::{Committed, LocalMemory, OperationId, PersistenceError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextPreference {
    Entries,
    PreferEntity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextPath {
    Entries,
    Entity(CacheStatus),
    EntriesFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextReport {
    pub path: ContextPath,
    pub item_count: usize,
    pub identity_available: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogueStage {
    Binding,
    Context,
    Generation,
    Review,
    Commit,
    Correction,
    Confirmation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogueFailure {
    PurposeDenied,
    BindingMismatch,
    ModelUnavailable,
    InvalidModelOutput,
    HostRejected,
    OriginalEvidenceRequired,
    Memory(MemoryError),
    OperationConflict,
    RecoveryRequired,
    CommitIndeterminate,
    StorageUnavailable,
    CapacityExceeded,
}

#[derive(Clone, Debug)]
pub struct DialogueError {
    stage: DialogueStage,
    failure: DialogueFailure,
    request: Option<Box<DialogueCommit>>,
}

impl DialogueError {
    fn new(stage: DialogueStage, failure: DialogueFailure) -> Self {
        Self {
            stage,
            failure,
            request: None,
        }
    }

    fn storage(stage: DialogueStage, error: PersistenceError) -> Self {
        let failure = match error {
            PersistenceError::Memory(error) => DialogueFailure::Memory(error),
            PersistenceError::OperationConflict => DialogueFailure::OperationConflict,
            PersistenceError::RecoveryRequired => DialogueFailure::RecoveryRequired,
            PersistenceError::Io(_) => DialogueFailure::CommitIndeterminate,
            PersistenceError::CapacityExceeded => DialogueFailure::CapacityExceeded,
            _ => DialogueFailure::StorageUnavailable,
        };
        Self::new(stage, failure)
    }

    pub fn stage(&self) -> DialogueStage {
        self.stage
    }
    pub fn failure(&self) -> &DialogueFailure {
        &self.failure
    }
    pub fn request(&self) -> Option<&DialogueCommit> {
        self.request.as_deref()
    }
}

impl fmt::Display for DialogueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "offline dialogue {:?}: {:?}; no new successful receipt",
            self.stage, self.failure
        )
    }
}

impl std::error::Error for DialogueError {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    identity: PetIdentity,
    session: SessionId,
}

pub struct OfflineDialogue<'memory> {
    memory: &'memory mut LocalMemory,
    binding: Binding,
}

#[derive(Debug)]
pub struct OfflineTurn {
    binding: Binding,
    task: MemoryTask,
    report: ContextReport,
}

impl OfflineTurn {
    pub fn context(&self) -> &RetrievalResult {
        self.task.result()
    }
    pub fn identity_view(&self) -> Option<&IdentityView> {
        self.task.identity_view()
    }
    pub fn report(&self) -> ContextReport {
        self.report
    }
}

#[derive(Debug)]
pub enum OfflineFixture {
    Candidate(MemoryDraft),
    Unavailable,
    Malformed,
}

#[derive(Debug)]
pub struct OfflineProposal {
    turn: OfflineTurn,
    draft: MemoryDraft,
    generated_at_ms: u64,
}

impl OfflineProposal {
    pub fn candidate(&self) -> &MemoryDraft {
        &self.draft
    }
    pub fn report(&self) -> ContextReport {
        self.turn.report
    }
}

pub struct OfflineReview<'proposal> {
    proposal: &'proposal OfflineProposal,
}

impl OfflineReview<'_> {
    pub fn candidate(&self) -> &MemoryDraft {
        &self.proposal.draft
    }
    pub fn context(&self) -> &RetrievalResult {
        self.proposal.turn.context()
    }
    pub fn identity_view(&self) -> Option<&IdentityView> {
        self.proposal.turn.identity_view()
    }
    pub fn report(&self) -> ContextReport {
        self.proposal.report()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostDecision {
    Reject,
    CreatePrivate { now_ms: u64 },
    PublishShared { now_ms: u64 },
}

pub struct CorrectionReview<'correction> {
    current: &'correction MemoryRecord,
    correction: &'correction MemoryDraft,
}

impl CorrectionReview<'_> {
    pub fn current(&self) -> &MemoryRecord {
        self.current
    }
    pub fn correction(&self) -> &MemoryDraft {
        self.correction
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorrectionDecision {
    Reject,
    Commit { now_ms: u64 },
}

#[derive(Clone, Debug)]
pub struct DialogueCommit {
    operation_id: OperationId,
    command: MemoryCommand,
}

impl DialogueCommit {
    pub fn from_bytes(operation_id: OperationId, bytes: &[u8]) -> Result<Self, MemoryError> {
        Ok(Self {
            operation_id,
            command: MemoryCommand::from_bytes(bytes)?,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, MemoryError> {
        self.command.to_bytes()
    }

    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
}

#[derive(Clone, Debug)]
pub struct DialogueReceipt {
    committed: Committed<WriteReceipt>,
    request: DialogueCommit,
}

impl DialogueReceipt {
    pub fn committed(&self) -> &Committed<WriteReceipt> {
        &self.committed
    }
    pub fn request(&self) -> &DialogueCommit {
        &self.request
    }
}

impl LocalMemory {
    pub fn offline_dialogue(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
    ) -> Result<OfflineDialogue<'_>, DialogueError> {
        self.ready()
            .map_err(|error| DialogueError::storage(DialogueStage::Binding, error))?;
        if purpose != ReadPurpose::OfflineReasoning {
            return Err(DialogueError::new(
                DialogueStage::Binding,
                DialogueFailure::PurposeDenied,
            ));
        }
        Ok(OfflineDialogue {
            memory: self,
            binding: Binding {
                identity: identity.clone(),
                session: session.clone(),
            },
        })
    }

    pub fn confirm_dialogue(
        &self,
        request: &DialogueCommit,
    ) -> Result<DialogueReceipt, DialogueError> {
        let committed = self
            .confirm_committed(&request.operation_id, &request.command)
            .map_err(|error| DialogueError::storage(DialogueStage::Confirmation, error))?;
        if !matches!(committed.value(), CommandResult::Write(_)) {
            return Err(DialogueError::new(
                DialogueStage::Confirmation,
                DialogueFailure::Memory(MemoryError::InvalidInput("not a write receipt")),
            ));
        }
        let committed = committed.map(|result| match result {
            CommandResult::Write(receipt) => receipt,
            _ => unreachable!(),
        });
        Ok(DialogueReceipt {
            committed,
            request: request.clone(),
        })
    }
}

impl OfflineDialogue<'_> {
    pub fn begin_turn(
        &mut self,
        query: &RetrievalQuery,
        preference: ContextPreference,
        now_ms: u64,
    ) -> Result<OfflineTurn, DialogueError> {
        let mut reader = self.memory.for_session(
            &self.binding.identity,
            &self.binding.session,
            ReadPurpose::OfflineReasoning,
        );
        let task = reader
            .begin_task(query, now_ms)
            .map_err(|error| DialogueError::storage(DialogueStage::Context, error))?;
        let path = match preference {
            ContextPreference::Entries => ContextPath::Entries,
            ContextPreference::PreferEntity => match reader.entity(query, now_ms) {
                Ok(entity) if entity.cache_status() != CacheStatus::Fallback => {
                    ContextPath::Entity(entity.cache_status())
                }
                Ok(_) | Err(PersistenceError::Memory(MemoryError::InvalidInput(_))) => {
                    ContextPath::EntriesFallback
                }
                Err(error) => return Err(DialogueError::storage(DialogueStage::Context, error)),
            },
        };
        let report = ContextReport {
            path,
            item_count: task.result().items().len(),
            identity_available: task.identity_view().is_some(),
        };
        Ok(OfflineTurn {
            binding: self.binding.clone(),
            task,
            report,
        })
    }

    pub fn generate(
        &mut self,
        turn: OfflineTurn,
        fixture: OfflineFixture,
        now_ms: u64,
    ) -> Result<OfflineProposal, DialogueError> {
        self.check_binding(&turn.binding, DialogueStage::Generation)?;
        self.memory
            .validate_task(&turn.task, now_ms)
            .map_err(|error| DialogueError::storage(DialogueStage::Generation, error))?;
        let draft = match fixture {
            OfflineFixture::Candidate(draft) => draft,
            OfflineFixture::Unavailable => {
                return Err(DialogueError::new(
                    DialogueStage::Generation,
                    DialogueFailure::ModelUnavailable,
                ));
            }
            OfflineFixture::Malformed => {
                return Err(DialogueError::new(
                    DialogueStage::Generation,
                    DialogueFailure::InvalidModelOutput,
                ));
            }
        };
        Ok(OfflineProposal {
            turn,
            draft,
            generated_at_ms: now_ms,
        })
    }

    pub fn review_and_commit(
        &mut self,
        proposal: &OfflineProposal,
        operation_id: OperationId,
        reviewer: impl FnOnce(OfflineReview<'_>) -> HostDecision,
    ) -> Result<DialogueReceipt, DialogueError> {
        self.check_binding(&proposal.turn.binding, DialogueStage::Review)?;
        self.memory
            .ready()
            .map_err(|error| DialogueError::storage(DialogueStage::Review, error))?;
        let decision = reviewer(OfflineReview { proposal });
        let now_ms = match decision {
            HostDecision::Reject => {
                return Err(DialogueError::new(
                    DialogueStage::Review,
                    DialogueFailure::HostRejected,
                ));
            }
            HostDecision::CreatePrivate { now_ms } | HostDecision::PublishShared { now_ms } => {
                now_ms
            }
        };
        if now_ms < proposal.generated_at_ms {
            return Err(DialogueError::new(
                DialogueStage::Review,
                DialogueFailure::Memory(MemoryError::TimeWentBackwards),
            ));
        }
        self.memory
            .validate_task(&proposal.turn.task, now_ms)
            .map_err(|error| DialogueError::storage(DialogueStage::Review, error))?;
        let command = match decision {
            HostDecision::CreatePrivate { .. } => proposal
                .turn
                .task
                .propose_create(proposal.draft.clone(), now_ms)
                .map_err(|error| {
                    DialogueError::new(DialogueStage::Review, DialogueFailure::Memory(error))
                })?,
            HostDecision::PublishShared { .. } => {
                let candidate = proposal
                    .turn
                    .task
                    .propose_shared(proposal.draft.clone(), proposal.generated_at_ms)
                    .map_err(|error| {
                        DialogueError::new(DialogueStage::Review, DialogueFailure::Memory(error))
                    })?;
                self.memory
                    .prepare_shared_publication(&candidate, now_ms)
                    .map_err(|error| DialogueError::storage(DialogueStage::Review, error))?
            }
            HostDecision::Reject => unreachable!(),
        };
        self.submit(DialogueCommit {
            operation_id,
            command,
        })
    }

    pub fn correct_original(
        &mut self,
        operation_id: OperationId,
        target: RecordVersion,
        correction: MemoryDraft,
        inspected_at_ms: u64,
        reviewer: impl FnOnce(CorrectionReview<'_>) -> CorrectionDecision,
    ) -> Result<DialogueReceipt, DialogueError> {
        let current = self
            .memory
            .for_pet(&self.binding.identity)
            .get(target.id(), inspected_at_ms)
            .map_err(|error| DialogueError::storage(DialogueStage::Correction, error))?;
        if current.revision() != target.revision() {
            return Err(DialogueError::new(
                DialogueStage::Correction,
                DialogueFailure::Memory(MemoryError::RevisionConflict {
                    expected: target.revision(),
                    actual: current.revision(),
                }),
            ));
        }
        if !current.dependencies().is_empty()
            || !correction.dependencies().is_empty()
            || !matches!(
                current.epistemic(),
                Epistemic::UserStatement | Epistemic::Observation
            )
            || !matches!(
                correction.epistemic(),
                Epistemic::UserStatement | Epistemic::Observation
            )
            || current.category() != correction.category()
        {
            return Err(DialogueError::new(
                DialogueStage::Correction,
                DialogueFailure::OriginalEvidenceRequired,
            ));
        }
        let now_ms = match reviewer(CorrectionReview {
            current: &current,
            correction: &correction,
        }) {
            CorrectionDecision::Reject => {
                return Err(DialogueError::new(
                    DialogueStage::Correction,
                    DialogueFailure::HostRejected,
                ));
            }
            CorrectionDecision::Commit { now_ms } => now_ms,
        };
        if now_ms < inspected_at_ms {
            return Err(DialogueError::new(
                DialogueStage::Correction,
                DialogueFailure::Memory(MemoryError::TimeWentBackwards),
            ));
        }
        let command = MemoryCommand::update(
            self.binding.identity.clone(),
            target.id(),
            target.revision(),
            correction,
            now_ms,
        );
        self.submit(DialogueCommit {
            operation_id,
            command,
        })
    }

    fn check_binding(&self, binding: &Binding, stage: DialogueStage) -> Result<(), DialogueError> {
        if &self.binding != binding {
            return Err(DialogueError::new(stage, DialogueFailure::BindingMismatch));
        }
        Ok(())
    }

    fn submit(&mut self, request: DialogueCommit) -> Result<DialogueReceipt, DialogueError> {
        let result = self
            .memory
            .commit(request.operation_id.clone(), request.command.clone());
        let committed = result
            .map_err(|error| {
                let mut error = DialogueError::storage(DialogueStage::Commit, error);
                error.request = Some(Box::new(request.clone()));
                error
            })?
            .map(|result| match result {
                CommandResult::Write(receipt) => receipt,
                _ => unreachable!(),
            });
        Ok(DialogueReceipt { committed, request })
    }
}
