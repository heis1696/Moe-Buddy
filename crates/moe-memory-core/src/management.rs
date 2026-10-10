use std::fmt;
use std::sync::Arc;

use crate::{
    CommandResult, IdentityView, MemoryClient, MemoryCommand, MemoryDraft, MemoryError, MemoryId,
    MemoryRecord, MemoryScope, MemoryService, PetIdentity, ReadPurpose, RetrievalItem,
    RetrievalQuery, RetrievalResult, SessionId, SourceRef, WriteOperation, WriteReceipt, policy,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordVersion {
    id: MemoryId,
    revision: u64,
}

impl RecordVersion {
    pub fn new(id: MemoryId, revision: u64) -> Result<Self, MemoryError> {
        if revision == 0 {
            return Err(MemoryError::InvalidInput(
                "record revision must be positive",
            ));
        }
        Ok(Self { id, revision })
    }
    pub fn id(&self) -> MemoryId {
        self.id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordState {
    Active,
    Expired,
    Archived,
    Superseded(MemoryId),
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryStatus {
    id: MemoryId,
    revision: u64,
    state: RecordState,
    available: bool,
}

impl MemoryStatus {
    pub(crate) fn deleted(id: MemoryId, revision: u64) -> Self {
        Self {
            id,
            revision,
            state: RecordState::Deleted,
            available: false,
        }
    }
    pub fn id(&self) -> MemoryId {
        self.id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn state(&self) -> RecordState {
        self.state
    }
    pub fn is_available(&self) -> bool {
        self.available
    }
    pub fn physical_erasure_performed(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementReceipt {
    previous: WriteReceipt,
    replacement: WriteReceipt,
}

impl ReplacementReceipt {
    pub fn previous(&self) -> &WriteReceipt {
        &self.previous
    }
    pub fn replacement(&self) -> &WriteReceipt {
        &self.replacement
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TaskStamp {
    pub(crate) generation: u64,
    pub(crate) configuration: u64,
    pub(crate) policy: u64,
    pub(crate) session: SessionId,
    pub(crate) request: Vec<u8>,
}

impl fmt::Debug for TaskStamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskStamp")
            .field("generation", &self.generation)
            .field("configuration", &self.configuration)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct MemoryTask {
    pub(crate) epoch: Arc<()>,
    pub(crate) identity: PetIdentity,
    pub(crate) purpose: ReadPurpose,
    pub(crate) stamp: TaskStamp,
    pub(crate) query: RetrievalQuery,
    pub(crate) result: RetrievalResult,
    pub(crate) identity_view: Option<IdentityView>,
    pub(crate) issued_at_ms: u64,
}

impl fmt::Debug for MemoryTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryTask")
            .field("generation", &self.stamp.generation)
            .finish_non_exhaustive()
    }
}

impl MemoryTask {
    pub fn result(&self) -> &RetrievalResult {
        &self.result
    }
    pub fn identity_view(&self) -> Option<&IdentityView> {
        self.identity_view.as_ref()
    }
    pub fn generation(&self) -> u64 {
        self.stamp.generation
    }

    pub fn propose_create(
        &self,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<MemoryCommand, MemoryError> {
        let (draft, stamp) = self.derived_draft(draft)?;
        Ok(MemoryCommand::fenced_create(
            self.identity.clone(),
            draft,
            now_ms,
            stamp,
            self.clone(),
        ))
    }

    pub(crate) fn derived_draft(
        &self,
        mut draft: MemoryDraft,
    ) -> Result<(MemoryDraft, TaskStamp), MemoryError> {
        if draft.dependencies.len() > crate::validation::MAX_SOURCES {
            return Err(MemoryError::InvalidEvidence);
        }
        let mut stamp = self.stamp.clone();
        stamp
            .request
            .extend_from_slice(&(draft.dependencies.len() as u64).to_le_bytes());
        for dependency in &draft.dependencies {
            stamp
                .request
                .extend_from_slice(&dependency.id().0.to_le_bytes());
            stamp
                .request
                .extend_from_slice(&dependency.revision().to_le_bytes());
        }
        for item in self.result.items() {
            let RetrievalItem::LongTerm(record) = item else {
                return Err(MemoryError::InvalidInput(
                    "session-derived output cannot be persisted",
                ));
            };
            if !record
                .sources()
                .iter()
                .all(|source| draft.sources.contains(source))
            {
                return Err(MemoryError::InvalidEvidence);
            }
            if !draft.dependencies.contains(&record.version_ref()) {
                draft.dependencies.push(record.version_ref());
            }
        }
        if self.identity_view.as_ref().is_some_and(|view| {
            !view
                .sources()
                .iter()
                .all(|source| draft.sources.contains(source))
        }) {
            return Err(MemoryError::InvalidEvidence);
        }
        Ok((draft, stamp))
    }
}

impl MemoryService {
    pub fn generation(&self) -> u64 {
        self.store.generation
    }

    pub fn cancel_tasks(&mut self, expected_generation: u64) -> Result<u64, MemoryError> {
        if self.store.generation != expected_generation {
            return Err(MemoryError::TaskCancelled);
        }
        self.store.generation = self.store.next_generation()?;
        Ok(self.store.generation)
    }

    pub fn revoke_source(&mut self, source: &SourceRef) -> Result<SourceRef, MemoryError> {
        let current = self
            .store
            .sources
            .get(&source.key)
            .ok_or(MemoryError::InvalidEvidence)?;
        if current.revision != source.revision || current.revoked {
            return Err(MemoryError::SourceVersionConflict);
        }
        let generation = self.store.next_generation()?;
        self.store
            .sources
            .get_mut(&source.key)
            .expect("validated source")
            .revoked = true;
        self.store.generation = generation;
        Ok(source.clone())
    }

    pub fn restore_source(
        &mut self,
        source: &SourceRef,
        new_revision: u64,
    ) -> Result<SourceRef, MemoryError> {
        let current = self
            .store
            .sources
            .get(&source.key)
            .ok_or(MemoryError::InvalidEvidence)?;
        if !current.revoked
            || current.revision != source.revision
            || new_revision <= current.revision
        {
            return Err(MemoryError::SourceVersionConflict);
        }
        let generation = self.store.next_generation()?;
        let current = self
            .store
            .sources
            .get_mut(&source.key)
            .expect("validated source");
        current.revision = new_revision;
        current.revoked = false;
        self.store.generation = generation;
        Ok(SourceRef {
            key: source.key.clone(),
            revision: new_revision,
        })
    }

    pub fn validate_task(&mut self, task: &MemoryTask, now_ms: u64) -> Result<(), MemoryError> {
        if !Arc::ptr_eq(&self.reads.epoch, &task.epoch)
            || self.store.generation != task.stamp.generation
            || self.reads.configuration != task.stamp.configuration
            || now_ms < task.issued_at_ms
        {
            return Err(MemoryError::TaskCancelled);
        }
        let reader = self.for_session(&task.identity, &task.stamp.session, task.purpose);
        if reader
            .retrieve(&task.query, now_ms)
            .map_err(|_| MemoryError::TaskCancelled)?
            != task.result
            || reader.identity().ok() != task.identity_view
        {
            return Err(MemoryError::TaskCancelled);
        }
        Ok(())
    }

    pub fn create_from_task(
        &mut self,
        task: &MemoryTask,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<WriteReceipt, MemoryError> {
        self.validate_task(task, now_ms)?;
        match task.propose_create(draft, now_ms)?.execute(self)? {
            CommandResult::Write(receipt) => Ok(receipt),
            _ => unreachable!(),
        }
    }
}

impl MemoryClient<'_> {
    pub fn status(&self, id: MemoryId, now_ms: u64) -> Result<MemoryStatus, MemoryError> {
        if let Some(record) = self.store.records.get(&id) {
            if record.scope != MemoryScope::Private(self.identity.clone()) {
                return Err(MemoryError::NotFound);
            }
            let state = if record.state == RecordState::Active
                && record
                    .expires_at_ms()
                    .is_some_and(|deadline| now_ms >= deadline)
            {
                RecordState::Expired
            } else {
                record.state
            };
            return Ok(MemoryStatus {
                id,
                revision: record.revision,
                state,
                available: policy::readable(record, &self.identity, self.store, now_ms),
            });
        }
        self.store
            .tombstones
            .get(&id)
            .filter(|(owner, _)| owner == &self.identity)
            .map(|(_, status)| status.clone())
            .ok_or(MemoryError::NotFound)
    }

    pub fn archive(
        &mut self,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<WriteReceipt, MemoryError> {
        self.transition(id, expected_revision, now_ms, false)
    }

    pub fn restore_archive(
        &mut self,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<WriteReceipt, MemoryError> {
        self.transition(id, expected_revision, now_ms, true)
    }

    fn transition(
        &mut self,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
        restore: bool,
    ) -> Result<WriteReceipt, MemoryError> {
        let record = self.store.records.get(&id).ok_or(MemoryError::NotFound)?;
        policy::writable(record, &self.identity)?;
        crate::api::validate_mutation(record, expected_revision, now_ms)?;
        let expected_state = if restore {
            RecordState::Archived
        } else {
            RecordState::Active
        };
        if record.state != expected_state {
            return Err(MemoryError::InvalidState);
        }
        if restore {
            policy::validate_draft(&record.draft, &self.identity, self.store, now_ms)?;
        }
        let revision = record
            .revision
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        let generation = self.store.next_generation()?;
        let record = self.store.records.get_mut(&id).expect("validated record");
        record.state = if restore {
            RecordState::Active
        } else {
            RecordState::Archived
        };
        record.revision = revision;
        record.updated_at_ms = now_ms;
        self.store.generation = generation;
        Ok(WriteReceipt {
            id,
            revision,
            operation: if restore {
                WriteOperation::Restored
            } else {
                WriteOperation::Archived
            },
        })
    }

    pub fn replace(
        &mut self,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<ReplacementReceipt, MemoryError> {
        let record = self.store.records.get(&id).ok_or(MemoryError::NotFound)?;
        policy::writable(record, &self.identity)?;
        crate::api::validate_mutation(record, expected_revision, now_ms)?;
        if record.state != RecordState::Active {
            return Err(MemoryError::InvalidState);
        }
        policy::validate_sensitivity_change(record.sensitivity(), draft.sensitivity)?;
        policy::validate_dependency_target(&draft, id, self.store)?;
        policy::validate_draft(&draft, &self.identity, self.store, now_ms)?;
        if self.store.records.len() >= crate::validation::MAX_RECORDS {
            return Err(MemoryError::CapacityExceeded);
        }
        let revision = record
            .revision
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        let generation = self.store.next_generation()?;
        let replacement_id = self.store.allocate_id()?;
        let previous = self.store.records.get_mut(&id).expect("validated record");
        previous.state = RecordState::Superseded(replacement_id);
        previous.revision = revision;
        previous.updated_at_ms = now_ms;
        self.store.records.insert(
            replacement_id,
            MemoryRecord {
                id: replacement_id,
                revision: 1,
                scope: MemoryScope::Private(self.identity.clone()),
                draft,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
                protected: false,
                state: RecordState::Active,
            },
        );
        self.store.generation = generation;
        Ok(ReplacementReceipt {
            previous: WriteReceipt {
                id,
                revision,
                operation: WriteOperation::Superseded,
            },
            replacement: WriteReceipt {
                id: replacement_id,
                revision: 1,
                operation: WriteOperation::Created,
            },
        })
    }
}
