use crate::model::SourceKey;
use crate::store::{SourceVersion, Store};
use crate::validation::MAX_RECORDS;
use crate::{
    Epistemic, MemoryDraft, MemoryError, MemoryId, MemoryRecord, MemoryScope, PetIdentity,
    SearchQuery, SourceRef, WriteOperation, WriteReceipt, policy, validation,
};

#[derive(Clone, Default)]
pub struct MemoryService {
    pub(crate) store: Store,
    pub(crate) reads: crate::read::ReadState,
    pub(crate) publications: crate::publication::PublicationState,
}

impl MemoryService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_source(
        &mut self,
        scope: MemoryScope,
        source_id: impl Into<String>,
        revision: u64,
        epistemic: Epistemic,
    ) -> Result<SourceRef, MemoryError> {
        let source_id = source_id.into();
        validation::identifier(&source_id)?;
        if revision == 0 {
            return Err(MemoryError::InvalidInput(
                "source revision must be positive",
            ));
        }
        let key = SourceKey {
            scope,
            id: source_id,
        };
        if self.store.sources.get(&key).is_some_and(|current| {
            current.revoked
                || revision < current.revision
                || (revision == current.revision && epistemic != current.epistemic)
        }) {
            return Err(MemoryError::SourceVersionConflict);
        }
        let generation = self.store.next_generation()?;
        self.store.sources.insert(
            key.clone(),
            SourceVersion {
                revision,
                epistemic,
                revoked: false,
            },
        );
        self.store.generation = generation;
        Ok(SourceRef { key, revision })
    }

    pub fn for_pet(&mut self, identity: &PetIdentity) -> MemoryClient<'_> {
        MemoryClient {
            store: &mut self.store,
            identity: identity.clone(),
        }
    }
}

pub struct MemoryClient<'service> {
    pub(crate) store: &'service mut Store,
    pub(crate) identity: PetIdentity,
}

impl MemoryClient<'_> {
    pub fn create(&mut self, draft: MemoryDraft, now_ms: u64) -> Result<WriteReceipt, MemoryError> {
        policy::validate_draft(&draft, &self.identity, self.store, now_ms)?;
        if self.store.records.len() >= MAX_RECORDS {
            return Err(MemoryError::CapacityExceeded);
        }
        let generation = self.store.next_generation()?;
        let id = self.store.allocate_id()?;
        self.store.records.insert(
            id,
            MemoryRecord {
                id,
                revision: 1,
                scope: MemoryScope::Private(self.identity.clone()),
                draft,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
                protected: false,
                state: crate::RecordState::Active,
            },
        );
        self.store.generation = generation;
        Ok(WriteReceipt {
            id,
            revision: 1,
            operation: WriteOperation::Created,
        })
    }

    pub fn get(&self, id: MemoryId, now_ms: u64) -> Result<MemoryRecord, MemoryError> {
        self.store
            .records
            .get(&id)
            .filter(|record| policy::readable(record, &self.identity, self.store, now_ms))
            .cloned()
            .ok_or(MemoryError::NotFound)
    }

    pub fn search(
        &self,
        query: &SearchQuery,
        now_ms: u64,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        policy::validate_query(query)?;
        let needle = query.text.to_lowercase();
        let mut records: Vec<_> = self
            .store
            .records
            .values()
            .filter(|record| policy::readable(record, &self.identity, self.store, now_ms))
            .filter(|record| policy::matches_scope(record, query))
            .filter(|record| record.text().to_lowercase().contains(&needle))
            .collect();
        records.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        records.truncate(query.limit);
        Ok(records.into_iter().cloned().collect())
    }

    pub fn update(
        &mut self,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<WriteReceipt, MemoryError> {
        self.update_checked(id, expected_revision, draft, now_ms, false)
    }

    pub(crate) fn update_checked(
        &mut self,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
        legacy_replay: bool,
    ) -> Result<WriteReceipt, MemoryError> {
        let current = self.store.records.get(&id).ok_or(MemoryError::NotFound)?;
        policy::writable(current, &self.identity)?;
        validate_mutation(current, expected_revision, now_ms)?;
        if current.state != crate::RecordState::Active {
            return Err(MemoryError::InvalidState);
        }
        if !legacy_replay
            && current
                .expires_at_ms()
                .is_some_and(|deadline| now_ms >= deadline)
        {
            return Err(MemoryError::InvalidState);
        }
        policy::validate_dependency_target(&draft, id, self.store)?;
        if !current
            .dependencies()
            .iter()
            .all(|dependency| draft.dependencies.contains(dependency))
        {
            return Err(MemoryError::InvalidEvidence);
        }
        policy::validate_sensitivity_change(current.sensitivity(), draft.sensitivity)?;
        policy::validate_draft(&draft, &self.identity, self.store, now_ms)?;
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        let generation = self.store.next_generation()?;
        let replacement = MemoryRecord {
            id,
            revision,
            scope: current.scope.clone(),
            draft,
            created_at_ms: current.created_at_ms,
            updated_at_ms: now_ms,
            protected: current.protected,
            state: crate::RecordState::Active,
        };
        self.store.records.insert(id, replacement);
        self.store.generation = generation;
        Ok(WriteReceipt {
            id,
            revision,
            operation: WriteOperation::Updated,
        })
    }

    pub fn delete(
        &mut self,
        id: MemoryId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<WriteReceipt, MemoryError> {
        let current = self.store.records.get(&id).ok_or(MemoryError::NotFound)?;
        policy::writable(current, &self.identity)?;
        validate_mutation(current, expected_revision, now_ms)?;
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        let generation = self.store.next_generation()?;
        if self.store.tombstones.len() >= crate::validation::MAX_TOMBSTONES {
            return Err(MemoryError::CapacityExceeded);
        }
        self.store.tombstones.insert(
            id,
            (
                self.identity.clone(),
                crate::MemoryStatus::deleted(id, revision),
            ),
        );
        self.store.records.remove(&id);
        self.store.generation = generation;
        Ok(WriteReceipt {
            id,
            revision,
            operation: WriteOperation::Deleted,
        })
    }
}

pub(crate) fn validate_mutation(
    current: &MemoryRecord,
    expected_revision: u64,
    now_ms: u64,
) -> Result<(), MemoryError> {
    if current.revision != expected_revision {
        return Err(MemoryError::RevisionConflict {
            expected: expected_revision,
            actual: current.revision,
        });
    }
    if now_ms < current.updated_at_ms {
        return Err(MemoryError::TimeWentBackwards);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
