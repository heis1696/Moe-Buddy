use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::management::TaskStamp;
use crate::{
    Category, Epistemic, Lifecycle, MemoryCommand, MemoryDraft, MemoryError, MemoryId,
    MemoryRecord, MemoryScope, MemoryService, MemoryTask, Namespace, PetIdentity, RecordState,
    RetrievalItem, Sensitivity, SourceRef, WriteOperation, WriteReceipt, policy, validation,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationPolicy {
    version: u64,
    sources: BTreeSet<SourceRef>,
}

impl PublicationPolicy {
    pub fn deny_all(version: u64) -> Self {
        Self::allow_sources(version, Vec::new())
    }

    pub fn allow_sources(version: u64, sources: Vec<SourceRef>) -> Self {
        Self {
            version,
            sources: sources.into_iter().collect(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

#[derive(Clone, Default)]
pub(crate) struct PublicationState {
    publishers: BTreeMap<PetIdentity, PublicationPolicy>,
    formal_authorities: BTreeMap<Namespace, PublicationPolicy>,
}

#[derive(Clone)]
pub struct SharedCandidate {
    pub(crate) task: MemoryTask,
    pub(crate) draft: MemoryDraft,
    pub(crate) stamp: TaskStamp,
    pub(crate) now_ms: u64,
}

impl SharedCandidate {
    pub fn text(&self) -> &str {
        &self.draft.text
    }
    pub fn sources(&self) -> &[SourceRef] {
        &self.draft.sources
    }
    pub fn dependencies(&self) -> &[crate::RecordVersion] {
        &self.draft.dependencies
    }
    pub fn task(&self) -> &MemoryTask {
        &self.task
    }
    pub fn proposed_at_ms(&self) -> u64 {
        self.now_ms
    }
}

impl fmt::Debug for SharedCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedCandidate")
            .field("draft", &self.draft)
            .field("generation", &self.stamp.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PublicationProof {
    epoch: Arc<()>,
    configuration: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FormalStamp {
    pub(crate) generation: u64,
    pub(crate) configuration: u64,
    pub(crate) policy: u64,
}

impl MemoryTask {
    pub fn propose_shared(
        &self,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<SharedCandidate, MemoryError> {
        if now_ms < self.issued_at_ms {
            return Err(MemoryError::TimeWentBackwards);
        }
        if self.identity_view.is_some()
            || self.result.items().iter().any(|item| match item {
                RetrievalItem::LongTerm(record) => {
                    record.scope() != &MemoryScope::Shared(self.identity.namespace().clone())
                        || record.sensitivity() != Sensitivity::Normal
                        || record.lifecycle() != Lifecycle::LongTerm
                        || record.is_protected()
                }
                RetrievalItem::WorkingContext(_) => true,
            })
        {
            return Err(MemoryError::PublicationDenied);
        }
        let (draft, mut stamp) = self.derived_draft(draft)?;
        stamp.request.extend_from_slice(&now_ms.to_le_bytes());
        validate_shared_shape(&draft, self.identity.namespace(), now_ms, false)?;
        Ok(SharedCandidate {
            task: self.clone(),
            draft,
            stamp,
            now_ms,
        })
    }
}

impl MemoryService {
    pub fn set_publication_policy(
        &mut self,
        identity: &PetIdentity,
        policy: PublicationPolicy,
    ) -> Result<(), MemoryError> {
        self.validate_publication_policy(identity.namespace(), &policy)?;
        validate_policy_version(self.publications.publishers.get(identity), &policy)?;
        if !self.publications.publishers.contains_key(identity)
            && self.publications.publishers.len() >= validation::MAX_RECORDS
        {
            return Err(MemoryError::CapacityExceeded);
        }
        let configuration = self.next_configuration()?;
        self.publications
            .publishers
            .insert(identity.clone(), policy);
        self.reads.configuration = configuration;
        Ok(())
    }

    pub fn set_formal_publication_policy(
        &mut self,
        namespace: &Namespace,
        policy: PublicationPolicy,
    ) -> Result<(), MemoryError> {
        self.validate_publication_policy(namespace, &policy)?;
        validate_policy_version(self.publications.formal_authorities.get(namespace), &policy)?;
        if !self.publications.formal_authorities.contains_key(namespace)
            && self.publications.formal_authorities.len() >= validation::MAX_RECORDS
        {
            return Err(MemoryError::CapacityExceeded);
        }
        let configuration = self.next_configuration()?;
        self.publications
            .formal_authorities
            .insert(namespace.clone(), policy);
        self.reads.configuration = configuration;
        Ok(())
    }

    fn validate_publication_policy(
        &self,
        namespace: &Namespace,
        policy: &PublicationPolicy,
    ) -> Result<(), MemoryError> {
        if policy.version == 0 || policy.sources.len() > validation::MAX_RECORDS {
            return Err(MemoryError::InvalidInput("invalid publication policy"));
        }
        if !policy.sources.iter().all(|source| {
            source.key.scope == MemoryScope::Shared(namespace.clone())
                && self
                    .store
                    .sources
                    .get(&source.key)
                    .is_some_and(|current| !current.revoked && current.revision == source.revision)
        }) {
            return Err(MemoryError::InvalidEvidence);
        }
        Ok(())
    }

    pub fn prepare_shared_publication(
        &mut self,
        candidate: &SharedCandidate,
        now_ms: u64,
    ) -> Result<MemoryCommand, MemoryError> {
        if now_ms < candidate.now_ms {
            return Err(MemoryError::TimeWentBackwards);
        }
        self.validate_task(&candidate.task, now_ms)?;
        self.validate_shared_draft(
            candidate.task.identity.namespace(),
            &candidate.draft,
            now_ms,
            false,
        )?;
        let version = require_grant(
            self.publications.publishers.get(&candidate.task.identity),
            &candidate.draft,
        )?;
        Ok(MemoryCommand::shared_publication(
            candidate,
            version,
            now_ms,
            self.publication_proof(),
        ))
    }

    pub fn prepare_automatic_publication(
        &self,
        _candidate: &SharedCandidate,
    ) -> Result<MemoryCommand, MemoryError> {
        Err(MemoryError::AutomaticPublicationDisabled)
    }

    pub fn prepare_formal_publication(
        &self,
        namespace: &Namespace,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<MemoryCommand, MemoryError> {
        self.prepare_formal(namespace, None, draft, now_ms)
    }

    pub fn prepare_formal_revision(
        &self,
        namespace: &Namespace,
        id: MemoryId,
        expected_revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<MemoryCommand, MemoryError> {
        self.prepare_formal(namespace, Some((id, expected_revision)), draft, now_ms)
    }

    fn prepare_formal(
        &self,
        namespace: &Namespace,
        target: Option<(MemoryId, u64)>,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<MemoryCommand, MemoryError> {
        self.validate_shared_draft(namespace, &draft, now_ms, true)?;
        if let Some((id, revision)) = target {
            self.validate_formal_target(namespace, id, revision, &draft, now_ms)?;
        }
        let policy = require_grant(self.publications.formal_authorities.get(namespace), &draft)?;
        Ok(MemoryCommand::formal_publication(
            namespace.clone(),
            target,
            draft,
            now_ms,
            FormalStamp {
                generation: self.store.generation,
                configuration: self.reads.configuration,
                policy,
            },
            self.publication_proof(),
        ))
    }

    fn publication_proof(&self) -> PublicationProof {
        PublicationProof {
            epoch: self.reads.epoch.clone(),
            configuration: self.reads.configuration,
        }
    }

    pub(crate) fn validate_publication_proof(
        &self,
        proof: Option<&PublicationProof>,
    ) -> Result<(), MemoryError> {
        if !proof.is_some_and(|proof| {
            Arc::ptr_eq(&self.reads.epoch, &proof.epoch)
                && self.reads.configuration == proof.configuration
        }) {
            return Err(MemoryError::PublicationDenied);
        }
        Ok(())
    }

    pub(crate) fn validate_shared_grant(
        &self,
        identity: &PetIdentity,
        draft: &MemoryDraft,
        version: u64,
    ) -> Result<(), MemoryError> {
        if require_grant(self.publications.publishers.get(identity), draft)? != version {
            return Err(MemoryError::PublicationDenied);
        }
        Ok(())
    }

    pub(crate) fn validate_formal_grant(
        &self,
        namespace: &Namespace,
        draft: &MemoryDraft,
        stamp: &FormalStamp,
    ) -> Result<(), MemoryError> {
        if self.reads.configuration != stamp.configuration
            || require_grant(self.publications.formal_authorities.get(namespace), draft)?
                != stamp.policy
        {
            return Err(MemoryError::PublicationDenied);
        }
        Ok(())
    }

    pub(crate) fn validate_shared_draft(
        &self,
        namespace: &Namespace,
        draft: &MemoryDraft,
        now_ms: u64,
        formal: bool,
    ) -> Result<(), MemoryError> {
        validate_shared_shape(draft, namespace, now_ms, formal)?;
        let scope = MemoryScope::Shared(namespace.clone());
        if !policy::sources_valid(draft, &scope, &self.store)
            || !policy::dependencies_valid(draft, &scope, &self.store, now_ms)
        {
            return Err(MemoryError::InvalidEvidence);
        }
        Ok(())
    }

    pub(crate) fn insert_shared(
        &mut self,
        namespace: &Namespace,
        draft: MemoryDraft,
        now_ms: u64,
        formal: bool,
    ) -> Result<WriteReceipt, MemoryError> {
        self.validate_shared_draft(namespace, &draft, now_ms, formal)?;
        if self.store.records.len() >= validation::MAX_RECORDS {
            return Err(MemoryError::CapacityExceeded);
        }
        let generation = self.store.next_generation()?;
        let id = self.store.allocate_id()?;
        self.store.records.insert(
            id,
            MemoryRecord {
                id,
                revision: 1,
                scope: MemoryScope::Shared(namespace.clone()),
                draft,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
                protected: formal,
                state: RecordState::Active,
            },
        );
        self.store.generation = generation;
        Ok(WriteReceipt {
            id,
            revision: 1,
            operation: WriteOperation::Created,
        })
    }

    fn validate_formal_target(
        &self,
        namespace: &Namespace,
        id: MemoryId,
        revision: u64,
        draft: &MemoryDraft,
        now_ms: u64,
    ) -> Result<(), MemoryError> {
        let current = self.store.records.get(&id).ok_or(MemoryError::NotFound)?;
        if current.scope != MemoryScope::Shared(namespace.clone()) || !current.protected {
            return Err(MemoryError::NotFound);
        }
        crate::api::validate_mutation(current, revision, now_ms)?;
        if current.state != RecordState::Active
            || current
                .expires_at_ms()
                .is_some_and(|expiration| now_ms >= expiration)
            || current.category() != draft.category
        {
            return Err(MemoryError::InvalidState);
        }
        Ok(())
    }

    pub(crate) fn revise_formal(
        &mut self,
        namespace: &Namespace,
        target: (MemoryId, u64),
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<WriteReceipt, MemoryError> {
        self.validate_shared_draft(namespace, &draft, now_ms, true)?;
        self.validate_formal_target(namespace, target.0, target.1, &draft, now_ms)?;
        let revision = target
            .1
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        let generation = self.store.next_generation()?;
        let current = self
            .store
            .records
            .get_mut(&target.0)
            .expect("validated formal target");
        current.draft = draft;
        current.revision = revision;
        current.updated_at_ms = now_ms;
        self.store.generation = generation;
        Ok(WriteReceipt {
            id: target.0,
            revision,
            operation: WriteOperation::Updated,
        })
    }
}

fn require_grant(
    policy: Option<&PublicationPolicy>,
    draft: &MemoryDraft,
) -> Result<u64, MemoryError> {
    policy
        .filter(|policy| {
            !draft.sources.is_empty()
                && draft
                    .sources
                    .iter()
                    .all(|source| policy.sources.contains(source))
        })
        .map(|policy| policy.version)
        .ok_or(MemoryError::PublicationDenied)
}

fn validate_policy_version(
    previous: Option<&PublicationPolicy>,
    next: &PublicationPolicy,
) -> Result<(), MemoryError> {
    if previous.is_some_and(|previous| next.version <= previous.version) {
        return Err(MemoryError::PublicationVersionConflict);
    }
    Ok(())
}

fn validate_shared_shape(
    draft: &MemoryDraft,
    namespace: &Namespace,
    now_ms: u64,
    formal: bool,
) -> Result<(), MemoryError> {
    validation::text(&draft.text)?;
    let category_valid = if formal {
        matches!(
            draft.category,
            Category::ToolDefinition | Category::Protocol
        ) && draft.epistemic == Epistemic::Reference
            && draft.dependencies.is_empty()
    } else {
        matches!(draft.category, Category::Knowledge | Category::Procedure)
            && !matches!(
                draft.epistemic,
                Epistemic::AuthoritativeState | Epistemic::Fiction
            )
    };
    if !category_valid
        || draft.sensitivity != Sensitivity::Normal
        || draft.lifecycle != Lifecycle::LongTerm
        || draft.sources.is_empty()
        || draft.sources.len() > validation::MAX_SOURCES
        || draft.dependencies.len() > validation::MAX_SOURCES
        || !draft
            .sources
            .iter()
            .all(|source| source.key.scope == MemoryScope::Shared(namespace.clone()))
    {
        return Err(MemoryError::PublicationDenied);
    }
    if draft
        .expires_at_ms
        .is_some_and(|deadline| deadline <= now_ms)
    {
        return Err(MemoryError::InvalidInput(
            "expiration must be in the future",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
