mod codec;

use crate::publication::{FormalStamp, PublicationProof};
use crate::{
    Epistemic, MemoryDraft, MemoryError, MemoryId, MemoryScope, MemoryService, Namespace,
    PetIdentity, ReplacementReceipt, SourceRef, WriteReceipt,
};

#[derive(Clone, Debug)]
pub struct MemoryCommand(Command, u8, Option<Box<LiveProof>>);

#[derive(Clone, Debug)]
enum LiveProof {
    Task(crate::MemoryTask),
    Publication(PublicationProof, Option<crate::MemoryTask>),
}

impl PartialEq for MemoryCommand {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && self.1 == other.1
    }
}

impl Eq for MemoryCommand {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Source(MemoryScope, String, u64, Epistemic),
    Create(PetIdentity, MemoryDraft, u64),
    Update(PetIdentity, MemoryId, u64, MemoryDraft, u64),
    Delete(PetIdentity, MemoryId, u64, u64),
    Archive(PetIdentity, MemoryId, u64, u64),
    RestoreArchive(PetIdentity, MemoryId, u64, u64),
    Replace(PetIdentity, MemoryId, u64, MemoryDraft, u64),
    RevokeSource(SourceRef),
    RestoreSource(SourceRef, u64),
    Cancel(u64),
    FencedCreate(PetIdentity, MemoryDraft, u64, crate::management::TaskStamp),
    PublishShared(
        PetIdentity,
        MemoryDraft,
        u64,
        crate::management::TaskStamp,
        u64,
    ),
    Formal(
        Namespace,
        Option<(MemoryId, u64)>,
        MemoryDraft,
        u64,
        FormalStamp,
    ),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandResult {
    Source(SourceRef),
    Write(WriteReceipt),
    Replacement(ReplacementReceipt),
    Generation(u64),
}

impl MemoryCommand {
    pub fn register_source(
        scope: MemoryScope,
        source_id: impl Into<String>,
        revision: u64,
        epistemic: Epistemic,
    ) -> Self {
        Self(
            Command::Source(scope, source_id.into(), revision, epistemic),
            1,
            None,
        )
    }

    pub fn create(identity: PetIdentity, draft: MemoryDraft, now_ms: u64) -> Self {
        let version = if draft.dependencies.is_empty() { 1 } else { 2 };
        Self(Command::Create(identity, draft, now_ms), version, None)
    }

    pub fn update(
        identity: PetIdentity,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Self {
        let version = if draft.dependencies.is_empty() { 1 } else { 2 };
        Self(
            Command::Update(identity, id, expected_revision, draft, now_ms),
            version,
            None,
        )
    }

    pub fn delete(
        identity: PetIdentity,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Self {
        Self(
            Command::Delete(identity, id, expected_revision, now_ms),
            1,
            None,
        )
    }

    pub fn archive(
        identity: PetIdentity,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Self {
        Self(
            Command::Archive(identity, id, expected_revision, now_ms),
            2,
            None,
        )
    }

    pub fn restore_archive(
        identity: PetIdentity,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Self {
        Self(
            Command::RestoreArchive(identity, id, expected_revision, now_ms),
            2,
            None,
        )
    }

    pub fn replace(
        identity: PetIdentity,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Self {
        Self(
            Command::Replace(identity, id, expected_revision, draft, now_ms),
            2,
            None,
        )
    }

    pub fn revoke_source(source: SourceRef) -> Self {
        Self(Command::RevokeSource(source), 2, None)
    }
    pub fn restore_source(source: SourceRef, new_revision: u64) -> Self {
        Self(Command::RestoreSource(source, new_revision), 2, None)
    }
    pub fn cancel_tasks(expected_generation: u64) -> Self {
        Self(Command::Cancel(expected_generation), 2, None)
    }

    pub(crate) fn fenced_create(
        identity: PetIdentity,
        draft: MemoryDraft,
        now_ms: u64,
        stamp: crate::management::TaskStamp,
        task: crate::MemoryTask,
    ) -> Self {
        Self(
            Command::FencedCreate(identity, draft, now_ms, stamp),
            2,
            Some(Box::new(LiveProof::Task(task))),
        )
    }

    pub(crate) fn shared_publication(
        candidate: &crate::SharedCandidate,
        policy: u64,
        now_ms: u64,
        proof: PublicationProof,
    ) -> Self {
        Self(
            Command::PublishShared(
                candidate.task.identity.clone(),
                candidate.draft.clone(),
                now_ms,
                candidate.stamp.clone(),
                policy,
            ),
            2,
            Some(Box::new(LiveProof::Publication(
                proof,
                Some(candidate.task.clone()),
            ))),
        )
    }

    pub(crate) fn formal_publication(
        namespace: Namespace,
        target: Option<(MemoryId, u64)>,
        draft: MemoryDraft,
        now_ms: u64,
        stamp: FormalStamp,
        proof: PublicationProof,
    ) -> Self {
        Self(
            Command::Formal(namespace, target, draft, now_ms, stamp),
            2,
            Some(Box::new(LiveProof::Publication(proof, None))),
        )
    }

    pub fn is_publication(&self) -> bool {
        matches!(self.0, Command::PublishShared(..) | Command::Formal(..))
    }

    fn live_task(&self) -> Option<&crate::MemoryTask> {
        match self.2.as_deref()? {
            LiveProof::Task(task) | LiveProof::Publication(_, Some(task)) => Some(task),
            _ => None,
        }
    }

    fn publication_proof(&self) -> Option<&PublicationProof> {
        match self.2.as_deref()? {
            LiveProof::Publication(proof, _) => Some(proof),
            _ => None,
        }
    }

    pub fn execute(&self, service: &mut MemoryService) -> Result<CommandResult, MemoryError> {
        self.apply(service, false)
    }

    pub fn replay(&self, service: &mut MemoryService) -> Result<CommandResult, MemoryError> {
        self.apply(service, true)
    }

    fn apply(
        &self,
        service: &mut MemoryService,
        replay: bool,
    ) -> Result<CommandResult, MemoryError> {
        match &self.0 {
            Command::PublishShared(identity, draft, now_ms, stamp, policy) => {
                if !replay {
                    service.validate_publication_proof(self.publication_proof())?;
                    service.validate_task(
                        self.live_task().ok_or(MemoryError::TaskCancelled)?,
                        *now_ms,
                    )?;
                    service.validate_shared_grant(identity, draft, *policy)?;
                    if !service.task_configuration_current(identity, stamp) {
                        return Err(MemoryError::TaskCancelled);
                    }
                }
                if service.generation() != stamp.generation {
                    return Err(MemoryError::TaskCancelled);
                }
                service
                    .insert_shared(identity.namespace(), draft.clone(), *now_ms, false)
                    .map(CommandResult::Write)
            }
            Command::Formal(namespace, target, draft, now_ms, stamp) => {
                if !replay {
                    service.validate_publication_proof(self.publication_proof())?;
                    service.validate_formal_grant(namespace, draft, stamp)?;
                }
                if service.generation() != stamp.generation {
                    return Err(MemoryError::TaskCancelled);
                }
                if let Some(target) = target {
                    service.revise_formal(namespace, *target, draft.clone(), *now_ms)
                } else {
                    service.insert_shared(namespace, draft.clone(), *now_ms, true)
                }
                .map(CommandResult::Write)
            }
            Command::Archive(identity, id, revision, now_ms) => service
                .for_pet(identity)
                .archive(*id, *revision, *now_ms)
                .map(CommandResult::Write),
            Command::RestoreArchive(identity, id, revision, now_ms) => service
                .for_pet(identity)
                .restore_archive(*id, *revision, *now_ms)
                .map(CommandResult::Write),
            Command::Replace(identity, id, revision, draft, now_ms) => service
                .for_pet(identity)
                .replace(*id, *revision, draft.clone(), *now_ms)
                .map(CommandResult::Replacement),
            Command::RevokeSource(source) => {
                service.revoke_source(source).map(CommandResult::Source)
            }
            Command::RestoreSource(source, revision) => service
                .restore_source(source, *revision)
                .map(CommandResult::Source),
            Command::Cancel(generation) => service
                .cancel_tasks(*generation)
                .map(CommandResult::Generation),
            Command::FencedCreate(identity, draft, now_ms, stamp) => {
                if !replay {
                    service.validate_task(
                        self.live_task().ok_or(MemoryError::TaskCancelled)?,
                        *now_ms,
                    )?;
                }
                if service.generation() != stamp.generation
                    || (!replay && !service.task_configuration_current(identity, stamp))
                {
                    return Err(MemoryError::TaskCancelled);
                }
                service
                    .for_pet(identity)
                    .create(draft.clone(), *now_ms)
                    .map(CommandResult::Write)
            }
            Command::Source(scope, id, revision, epistemic) => service
                .register_source(scope.clone(), id.clone(), *revision, *epistemic)
                .map(CommandResult::Source),
            Command::Create(identity, draft, now_ms) => service
                .for_pet(identity)
                .create(draft.clone(), *now_ms)
                .map(CommandResult::Write),
            Command::Update(identity, id, revision, draft, now_ms) => service
                .for_pet(identity)
                .update_checked(
                    *id,
                    *revision,
                    draft.clone(),
                    *now_ms,
                    replay && self.1 == 1,
                )
                .map(CommandResult::Write),
            Command::Delete(identity, id, revision, now_ms) => service
                .for_pet(identity)
                .delete(*id, *revision, *now_ms)
                .map(CommandResult::Write),
        }
    }
}
