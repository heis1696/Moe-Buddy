use std::collections::BTreeMap;
use std::path::Path;

use moe_memory_core::{
    CommandResult, EntityRead, Epistemic, IdentityView, MemoryCommand, MemoryDraft, MemoryError,
    MemoryId, MemoryRecord, MemoryScope, MemoryService, MemoryStatus, MemoryTask, Namespace,
    PetIdentity, PublicationPolicy, ReadPolicy, ReadPurpose, ReplacementReceipt, RetrievalQuery,
    RetrievalResult, SearchQuery, SessionId, SharedCandidate, SourceRef, WriteReceipt,
};

use crate::PersistenceError;
use crate::journal::{
    self, FileJournal, HEADER, Journal, MAX_FRAME_BYTES, MAX_JOURNAL_BYTES, Reader,
};

const MAX_OPERATIONS: usize = 100_000;

#[path = "context.rs"]
mod context;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationId(String);

impl OperationId {
    pub fn new(value: impl Into<String>) -> Result<Self, MemoryError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.chars().count() > 128
            || value.chars().any(char::is_control)
        {
            return Err(MemoryError::InvalidInput("invalid operation ID"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    FileSynced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupState {
    NotPerformed,
    HistoryRetained,
    PolicyUndetermined,
    Unmanaged,
    NotRecallable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeletionBoundary {
    physical_erasure: CleanupState,
    journal: CleanupState,
    backups: CleanupState,
    external_copies: CleanupState,
    delivered_snapshots: CleanupState,
}

impl DeletionBoundary {
    pub fn physical_erasure(&self) -> CleanupState {
        self.physical_erasure
    }
    pub fn journal(&self) -> CleanupState {
        self.journal
    }
    pub fn backups(&self) -> CleanupState {
        self.backups
    }
    pub fn external_copies(&self) -> CleanupState {
        self.external_copies
    }
    pub fn delivered_snapshots(&self) -> CleanupState {
        self.delivered_snapshots
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committed<Value> {
    operation_id: OperationId,
    sequence: u64,
    value: Value,
}

impl<Value> Committed<Value> {
    pub(crate) fn map<Next>(self, transform: impl FnOnce(Value) -> Next) -> Committed<Next> {
        Committed {
            operation_id: self.operation_id,
            sequence: self.sequence,
            value: transform(self.value),
        }
    }
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn durability(&self) -> Durability {
        Durability::FileSynced
    }
    pub fn value(&self) -> &Value {
        &self.value
    }
}

struct Entry {
    payload: Vec<u8>,
    result: CommandResult,
    sequence: u64,
}

pub struct LocalMemory {
    journal: Box<dyn Journal>,
    service: MemoryService,
    operations: BTreeMap<OperationId, Entry>,
    last_checksum: u64,
    length: usize,
    failed: bool,
}

impl LocalMemory {
    pub fn set_publication_policy(
        &mut self,
        identity: &PetIdentity,
        policy: PublicationPolicy,
    ) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self.service.set_publication_policy(identity, policy)?)
    }

    pub fn set_formal_publication_policy(
        &mut self,
        namespace: &Namespace,
        policy: PublicationPolicy,
    ) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self
            .service
            .set_formal_publication_policy(namespace, policy)?)
    }

    pub fn prepare_shared_publication(
        &mut self,
        candidate: &SharedCandidate,
        now_ms: u64,
    ) -> Result<MemoryCommand, PersistenceError> {
        self.ready()?;
        Ok(self.service.prepare_shared_publication(candidate, now_ms)?)
    }

    pub fn prepare_automatic_publication(
        &self,
        candidate: &SharedCandidate,
    ) -> Result<MemoryCommand, PersistenceError> {
        self.ready()?;
        Ok(self.service.prepare_automatic_publication(candidate)?)
    }

    pub fn prepare_formal_publication(
        &self,
        namespace: &Namespace,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<MemoryCommand, PersistenceError> {
        self.ready()?;
        Ok(self
            .service
            .prepare_formal_publication(namespace, draft, now_ms)?)
    }

    pub fn prepare_formal_revision(
        &self,
        namespace: &Namespace,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<MemoryCommand, PersistenceError> {
        self.ready()?;
        Ok(self
            .service
            .prepare_formal_revision(namespace, id, expected_revision, draft, now_ms)?)
    }

    pub fn commit_publication(
        &mut self,
        operation_id: OperationId,
        command: MemoryCommand,
    ) -> Result<Committed<WriteReceipt>, PersistenceError> {
        self.ready()?;
        if !command.is_publication() {
            return Err(MemoryError::PublicationDenied.into());
        }
        Ok(self
            .commit(operation_id, command)?
            .map(|result| match result {
                CommandResult::Write(receipt) => receipt,
                _ => unreachable!(),
            }))
    }

    pub fn deletion_boundary(&self) -> Result<DeletionBoundary, PersistenceError> {
        self.ready()?;
        Ok(DeletionBoundary {
            physical_erasure: CleanupState::NotPerformed,
            journal: CleanupState::HistoryRetained,
            backups: CleanupState::PolicyUndetermined,
            external_copies: CleanupState::Unmanaged,
            delivered_snapshots: CleanupState::NotRecallable,
        })
    }
    pub fn create(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        Self::recover(Box::new(FileJournal::open(path.as_ref(), true)?))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        Self::recover(Box::new(FileJournal::open(path.as_ref(), false)?))
    }

    pub fn register_source(
        &mut self,
        operation_id: OperationId,
        scope: MemoryScope,
        source_id: impl Into<String>,
        revision: u64,
        epistemic: Epistemic,
    ) -> Result<Committed<SourceRef>, PersistenceError> {
        let committed = self.commit(
            operation_id,
            MemoryCommand::register_source(scope, source_id, revision, epistemic),
        )?;
        match committed.value {
            CommandResult::Source(value) => Ok(Committed {
                operation_id: committed.operation_id,
                sequence: committed.sequence,
                value,
            }),
            _ => unreachable!(),
        }
    }

    pub fn for_pet(&mut self, identity: &PetIdentity) -> LocalMemoryClient<'_> {
        LocalMemoryClient {
            coordinator: self,
            identity: identity.clone(),
        }
    }

    pub fn generation(&self) -> Result<u64, PersistenceError> {
        self.ready()?;
        Ok(self.service.generation())
    }

    pub fn cancel_tasks(
        &mut self,
        operation_id: OperationId,
        expected_generation: u64,
    ) -> Result<Committed<u64>, PersistenceError> {
        Ok(self
            .commit(
                operation_id,
                MemoryCommand::cancel_tasks(expected_generation),
            )?
            .map(|value| match value {
                CommandResult::Generation(generation) => generation,
                _ => unreachable!(),
            }))
    }

    pub fn revoke_source(
        &mut self,
        operation_id: OperationId,
        source: &SourceRef,
    ) -> Result<Committed<SourceRef>, PersistenceError> {
        self.source_write(operation_id, MemoryCommand::revoke_source(source.clone()))
    }

    pub fn restore_source(
        &mut self,
        operation_id: OperationId,
        source: &SourceRef,
        new_revision: u64,
    ) -> Result<Committed<SourceRef>, PersistenceError> {
        self.source_write(
            operation_id,
            MemoryCommand::restore_source(source.clone(), new_revision),
        )
    }

    fn source_write(
        &mut self,
        operation_id: OperationId,
        command: MemoryCommand,
    ) -> Result<Committed<SourceRef>, PersistenceError> {
        Ok(self
            .commit(operation_id, command)?
            .map(|value| match value {
                CommandResult::Source(source) => source,
                _ => unreachable!(),
            }))
    }

    pub fn validate_task(
        &mut self,
        task: &MemoryTask,
        now_ms: u64,
    ) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self.service.validate_task(task, now_ms)?)
    }

    pub fn create_from_task(
        &mut self,
        operation_id: OperationId,
        task: &MemoryTask,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<Committed<WriteReceipt>, PersistenceError> {
        self.ready()?;
        let command = task.propose_create(draft, now_ms)?;
        if !self.operations.contains_key(&operation_id) {
            self.service.validate_task(task, now_ms)?;
        }
        Ok(self
            .commit(operation_id, command)?
            .map(|value| match value {
                CommandResult::Write(receipt) => receipt,
                _ => unreachable!(),
            }))
    }

    pub(crate) fn ready(&self) -> Result<(), PersistenceError> {
        if self.failed {
            Err(PersistenceError::RecoveryRequired)
        } else {
            Ok(())
        }
    }

    pub fn set_read_policy(
        &mut self,
        identity: &PetIdentity,
        policy: ReadPolicy,
    ) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self.service.set_read_policy(identity, policy)?)
    }

    pub fn put_working_context(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        context_id: impl Into<String>,
        revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self
            .service
            .put_working_context(identity, session, context_id, revision, draft, now_ms)?)
    }

    pub fn set_identity_view(
        &mut self,
        identity: &PetIdentity,
        version: u64,
        text: impl Into<String>,
        sources: Vec<SourceRef>,
    ) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self
            .service
            .set_identity_view(identity, version, text, sources)?)
    }

    pub fn set_entity_cache_capacity(&mut self, capacity: usize) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self.service.set_entity_cache_capacity(capacity)?)
    }

    pub fn revoke_access(
        &mut self,
        identity: &PetIdentity,
        policy_version: u64,
    ) -> Result<(), PersistenceError> {
        self.ready()?;
        Ok(self.service.revoke_access(identity, policy_version)?)
    }

    pub fn remove_working_context(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        context_id: &str,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<u64, PersistenceError> {
        self.ready()?;
        Ok(self.service.remove_working_context(
            identity,
            session,
            context_id,
            expected_revision,
            now_ms,
        )?)
    }

    pub fn remove_identity_view(
        &mut self,
        identity: &PetIdentity,
        expected_version: u64,
    ) -> Result<u64, PersistenceError> {
        self.ready()?;
        Ok(self
            .service
            .remove_identity_view(identity, expected_version)?)
    }

    pub fn for_session(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
    ) -> LocalMemoryReader<'_> {
        LocalMemoryReader {
            coordinator: self,
            identity: identity.clone(),
            session: session.clone(),
            purpose,
        }
    }

    pub(crate) fn confirm_committed(
        &self,
        operation_id: &OperationId,
        command: &MemoryCommand,
    ) -> Result<Committed<CommandResult>, PersistenceError> {
        self.ready()?;
        let payload = command.to_bytes()?;
        let entry = self
            .operations
            .get(operation_id)
            .ok_or(MemoryError::NotFound)?;
        if entry.payload != payload {
            return Err(PersistenceError::OperationConflict);
        }
        Ok(Committed {
            operation_id: operation_id.clone(),
            sequence: entry.sequence,
            value: entry.result.clone(),
        })
    }

    pub(crate) fn commit(
        &mut self,
        operation_id: OperationId,
        command: MemoryCommand,
    ) -> Result<Committed<CommandResult>, PersistenceError> {
        self.ready()?;
        let payload = command.to_bytes()?;
        if let Some(entry) = self.operations.get(&operation_id) {
            if payload != entry.payload {
                return Err(PersistenceError::OperationConflict);
            }
            return Ok(Committed {
                operation_id,
                sequence: entry.sequence,
                value: entry.result.clone(),
            });
        }
        if self.operations.len() >= MAX_OPERATIONS {
            return Err(PersistenceError::CapacityExceeded);
        }
        let mut candidate = self.service.clone();
        let result = command.execute(&mut candidate)?;
        let sequence = self.operations.len() as u64 + 1;
        let frame = journal::frame(
            sequence,
            self.last_checksum,
            operation_id.as_str(),
            &payload,
        );
        if frame.len() > MAX_FRAME_BYTES || self.length + frame.len() > MAX_JOURNAL_BYTES {
            return Err(PersistenceError::CapacityExceeded);
        }
        if let Err(error) = self.journal.append_synced(&frame) {
            self.failed = true;
            return Err(error.into());
        }
        self.service = candidate;
        self.last_checksum = journal::checksum(&frame[..frame.len() - 8]);
        self.length += frame.len();
        self.operations.insert(
            operation_id.clone(),
            Entry {
                payload,
                result: result.clone(),
                sequence,
            },
        );
        Ok(Committed {
            operation_id,
            sequence,
            value: result,
        })
    }

    fn recover(mut journal: Box<dyn Journal>) -> Result<Self, PersistenceError> {
        let bytes = journal.read_all()?;
        let mut reader = Reader {
            remaining: &bytes,
            offset: 0,
        };
        if reader.take(8)? != &HEADER[..8] {
            return Err(reader.corrupt("invalid header"));
        }
        let version = reader.number()?;
        if version != 1 {
            return Err(PersistenceError::UnsupportedVersion(version));
        }
        let mut service = MemoryService::new();
        let mut operations = BTreeMap::new();
        let mut previous = 0;
        while !reader.remaining.is_empty() {
            if operations.len() >= MAX_OPERATIONS {
                return Err(PersistenceError::CapacityExceeded);
            }
            let start = reader.offset as usize;
            let body = reader.sized(MAX_FRAME_BYTES - 16)?;
            let expected_hash = journal::checksum(&bytes[start..reader.offset as usize]);
            if reader.number()? != expected_hash {
                return Err(reader.corrupt("checksum mismatch"));
            }
            let mut frame = Reader {
                remaining: body,
                offset: start as u64 + 8,
            };
            let sequence = frame.number()?;
            if sequence != operations.len() as u64 + 1 || frame.number()? != previous {
                return Err(frame.corrupt("non-contiguous commit sequence"));
            }
            let operation_text = std::str::from_utf8(frame.sized(128 * 4)?)
                .map_err(|_| frame.corrupt("invalid operation encoding"))?;
            let operation_id = OperationId::new(operation_text)
                .map_err(|_| frame.corrupt("invalid operation ID"))?;
            if operations.contains_key(&operation_id) {
                return Err(frame.corrupt("duplicate operation ID"));
            }
            let payload = frame.sized(MAX_FRAME_BYTES)?;
            if !frame.remaining.is_empty() {
                return Err(frame.corrupt("trailing frame data"));
            }
            let command =
                MemoryCommand::from_bytes(payload).map_err(|_| frame.corrupt("invalid command"))?;
            let result = command
                .replay(&mut service)
                .map_err(|_| frame.corrupt("command violates core contract"))?;
            operations.insert(
                operation_id,
                Entry {
                    payload: payload.to_vec(),
                    result,
                    sequence,
                },
            );
            previous = expected_hash;
        }
        journal.sync()?;
        Ok(Self {
            journal,
            service,
            operations,
            last_checksum: previous,
            length: bytes.len(),
            failed: false,
        })
    }
}

pub struct LocalMemoryClient<'service> {
    coordinator: &'service mut LocalMemory,
    identity: PetIdentity,
}

pub struct LocalMemoryReader<'service> {
    coordinator: &'service mut LocalMemory,
    identity: PetIdentity,
    session: SessionId,
    purpose: ReadPurpose,
}

impl LocalMemoryReader<'_> {
    pub fn begin_task(
        &mut self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<MemoryTask, PersistenceError> {
        self.coordinator.ready()?;
        Ok(self
            .coordinator
            .service
            .for_session(&self.identity, &self.session, self.purpose)
            .begin_task(query, now_ms)?)
    }
    pub fn retrieve(
        &mut self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<RetrievalResult, PersistenceError> {
        self.coordinator.ready()?;
        Ok(self
            .coordinator
            .service
            .for_session(&self.identity, &self.session, self.purpose)
            .retrieve(query, now_ms)?)
    }

    pub fn identity(&mut self) -> Result<IdentityView, PersistenceError> {
        self.coordinator.ready()?;
        Ok(self
            .coordinator
            .service
            .for_session(&self.identity, &self.session, self.purpose)
            .identity()?)
    }

    pub fn entity(
        &mut self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<EntityRead, PersistenceError> {
        self.coordinator.ready()?;
        Ok(self
            .coordinator
            .service
            .for_session(&self.identity, &self.session, self.purpose)
            .entity(query, now_ms)?)
    }
}

impl LocalMemoryClient<'_> {
    pub fn status(&mut self, id: MemoryId, now_ms: u64) -> Result<MemoryStatus, PersistenceError> {
        self.coordinator.ready()?;
        Ok(self
            .coordinator
            .service
            .for_pet(&self.identity)
            .status(id, now_ms)?)
    }

    pub fn archive(
        &mut self,
        operation_id: OperationId,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<Committed<WriteReceipt>, PersistenceError> {
        self.write(
            operation_id,
            MemoryCommand::archive(self.identity.clone(), id, expected_revision, now_ms),
        )
    }

    pub fn restore_archive(
        &mut self,
        operation_id: OperationId,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<Committed<WriteReceipt>, PersistenceError> {
        self.write(
            operation_id,
            MemoryCommand::restore_archive(self.identity.clone(), id, expected_revision, now_ms),
        )
    }

    pub fn replace(
        &mut self,
        operation_id: OperationId,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<Committed<ReplacementReceipt>, PersistenceError> {
        Ok(self
            .coordinator
            .commit(
                operation_id,
                MemoryCommand::replace(self.identity.clone(), id, expected_revision, draft, now_ms),
            )?
            .map(|value| match value {
                CommandResult::Replacement(receipt) => receipt,
                _ => unreachable!(),
            }))
    }
    pub fn create(
        &mut self,
        operation_id: OperationId,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<Committed<WriteReceipt>, PersistenceError> {
        self.write(
            operation_id,
            MemoryCommand::create(self.identity.clone(), draft, now_ms),
        )
    }

    pub fn update(
        &mut self,
        operation_id: OperationId,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<Committed<WriteReceipt>, PersistenceError> {
        self.write(
            operation_id,
            MemoryCommand::update(self.identity.clone(), id, expected_revision, draft, now_ms),
        )
    }

    pub fn delete(
        &mut self,
        operation_id: OperationId,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<Committed<WriteReceipt>, PersistenceError> {
        self.write(
            operation_id,
            MemoryCommand::delete(self.identity.clone(), id, expected_revision, now_ms),
        )
    }

    pub fn get(&mut self, id: MemoryId, now_ms: u64) -> Result<MemoryRecord, PersistenceError> {
        self.coordinator.ready()?;
        Ok(self
            .coordinator
            .service
            .for_pet(&self.identity)
            .get(id, now_ms)?)
    }

    pub fn search(
        &mut self,
        query: &SearchQuery,
        now_ms: u64,
    ) -> Result<Vec<MemoryRecord>, PersistenceError> {
        self.coordinator.ready()?;
        Ok(self
            .coordinator
            .service
            .for_pet(&self.identity)
            .search(query, now_ms)?)
    }

    fn write(
        &mut self,
        operation_id: OperationId,
        command: MemoryCommand,
    ) -> Result<Committed<WriteReceipt>, PersistenceError> {
        let committed = self.coordinator.commit(operation_id, command)?;
        match committed.value {
            CommandResult::Write(value) => Ok(Committed {
                operation_id: committed.operation_id,
                sequence: committed.sequence,
                value,
            }),
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests;
