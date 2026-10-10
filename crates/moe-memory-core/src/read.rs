mod model;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub use model::*;

use crate::validation::{MAX_RECORDS, MAX_RESULTS, MAX_SOURCES, MAX_TEXT_CHARS};
use crate::{
    Category, Epistemic, Lifecycle, MemoryDraft, MemoryError, MemoryId, MemoryScope, MemoryService,
    PetIdentity, SearchScope, Sensitivity, SourceRef, policy, validation,
};

#[derive(Clone)]
pub(crate) struct ReadState {
    pub(crate) epoch: Arc<()>,
    pub(crate) configuration: u64,
    policies: BTreeMap<PetIdentity, ReadPolicy>,
    contexts: BTreeMap<(PetIdentity, SessionId), BTreeMap<String, WorkingContext>>,
    identities: BTreeMap<PetIdentity, IdentityView>,
    identity_versions: BTreeMap<PetIdentity, u64>,
    context_versions: BTreeMap<(PetIdentity, SessionId, String), (u64, u64)>,
    cache: Vec<CacheEntry>,
    cache_capacity: usize,
}

impl Default for ReadState {
    fn default() -> Self {
        Self {
            epoch: Arc::new(()),
            configuration: 0,
            policies: BTreeMap::new(),
            contexts: BTreeMap::new(),
            identities: BTreeMap::new(),
            identity_versions: BTreeMap::new(),
            context_versions: BTreeMap::new(),
            cache: Vec::new(),
            cache_capacity: 64,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum EntryKey {
    Record(MemoryId),
    Context(String),
}

#[derive(Clone, PartialEq, Eq)]
struct Dependency {
    key: EntryKey,
    revision: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct Dependencies {
    policy_version: u64,
    entries: Vec<Dependency>,
    sources: BTreeSet<SourceRef>,
}

#[derive(Clone)]
struct CacheEntry {
    identity: PetIdentity,
    session: SessionId,
    purpose: ReadPurpose,
    query: RetrievalQuery,
    dependencies: Dependencies,
    view: EntityView,
}

impl MemoryService {
    pub fn set_read_policy(
        &mut self,
        identity: &PetIdentity,
        policy: ReadPolicy,
    ) -> Result<(), MemoryError> {
        if policy.version == 0 || policy.sources.len() > MAX_RECORDS {
            return Err(MemoryError::InvalidInput("invalid read policy"));
        }
        if self
            .reads
            .policies
            .get(identity)
            .is_some_and(|current| policy.version <= current.version)
        {
            return Err(MemoryError::PolicyVersionConflict);
        }
        if !policy.sources.iter().all(|source| {
            policy::scope_readable(&source.key.scope, identity) && self.source_current(source)
        }) {
            return Err(MemoryError::InvalidEvidence);
        }
        if !self.reads.policies.contains_key(identity) && self.reads.policies.len() >= MAX_RECORDS {
            return Err(MemoryError::CapacityExceeded);
        }
        let configuration = self.next_configuration()?;
        self.reads.policies.insert(identity.clone(), policy);
        self.reads.configuration = configuration;
        Ok(())
    }

    pub fn put_working_context(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        context_id: impl Into<String>,
        revision: u64,
        draft: MemoryDraft,
        now_ms: u64,
    ) -> Result<(), MemoryError> {
        let context_id = context_id.into();
        validation::identifier(&context_id)?;
        if revision == 0
            || draft.category != Category::WorkingContext
            || draft.lifecycle != Lifecycle::ShortTerm
        {
            return Err(MemoryError::InvalidInput("invalid session working context"));
        }
        policy::validate_draft(&draft, identity, &self.store, now_ms)?;
        let key = (identity.clone(), session.clone());
        let version_key = (identity.clone(), session.clone(), context_id.clone());
        let current = self
            .reads
            .contexts
            .get(&key)
            .and_then(|items| items.get(&context_id));
        if let Some((previous_revision, previous_time)) =
            self.reads.context_versions.get(&version_key)
        {
            if revision <= *previous_revision {
                return Err(MemoryError::ContextVersionConflict);
            }
            if now_ms < *previous_time {
                return Err(MemoryError::TimeWentBackwards);
            }
        } else if self.reads.context_versions.len() >= MAX_RECORDS {
            return Err(MemoryError::CapacityExceeded);
        }
        if let Some(current) = current {
            policy::validate_sensitivity_change(current.draft.sensitivity, draft.sensitivity)?;
        }
        if current.is_some_and(|current| {
            !current
                .draft
                .dependencies
                .iter()
                .all(|dependency| draft.dependencies.contains(dependency))
        }) {
            return Err(MemoryError::InvalidEvidence);
        }
        let configuration = self.next_configuration()?;
        self.reads
            .context_versions
            .insert(version_key, (revision, now_ms));
        self.reads.contexts.entry(key).or_default().insert(
            context_id.clone(),
            WorkingContext {
                id: context_id,
                revision,
                draft,
                updated_at_ms: now_ms,
            },
        );
        self.reads.configuration = configuration;
        Ok(())
    }

    pub fn set_identity_view(
        &mut self,
        identity: &PetIdentity,
        version: u64,
        text: impl Into<String>,
        sources: Vec<SourceRef>,
    ) -> Result<(), MemoryError> {
        let text = text.into();
        validation::text(&text)?;
        if version == 0 || sources.is_empty() || sources.len() > MAX_SOURCES {
            return Err(MemoryError::InvalidInput("invalid identity view"));
        }
        if self
            .reads
            .identity_versions
            .get(identity)
            .is_some_and(|current| version <= *current)
        {
            return Err(MemoryError::IdentityVersionConflict);
        }
        let mut unique = BTreeSet::new();
        if !sources.iter().all(|source| {
            unique.insert(source)
                && source.key.scope == MemoryScope::Private(identity.clone())
                && self.source_current(source)
                && self.store.sources[&source.key].epistemic == Epistemic::AuthoritativeState
        }) {
            return Err(MemoryError::InvalidEvidence);
        }
        if !self.reads.identity_versions.contains_key(identity)
            && self.reads.identity_versions.len() >= MAX_RECORDS
        {
            return Err(MemoryError::CapacityExceeded);
        }
        let configuration = self.next_configuration()?;
        self.reads
            .identity_versions
            .insert(identity.clone(), version);
        self.reads.identities.insert(
            identity.clone(),
            IdentityView {
                version,
                text,
                sources,
            },
        );
        self.reads.configuration = configuration;
        Ok(())
    }

    pub fn set_entity_cache_capacity(&mut self, capacity: usize) -> Result<(), MemoryError> {
        if capacity > 256 {
            return Err(MemoryError::InvalidInput(
                "entity cache capacity exceeds limit",
            ));
        }
        let configuration = self.next_configuration()?;
        self.reads.cache.clear();
        self.reads.cache_capacity = capacity;
        self.reads.configuration = configuration;
        Ok(())
    }

    pub(crate) fn next_configuration(&self) -> Result<u64, MemoryError> {
        self.reads
            .configuration
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)
    }

    pub(crate) fn task_configuration_current(
        &self,
        identity: &PetIdentity,
        stamp: &crate::management::TaskStamp,
    ) -> bool {
        self.reads.configuration == stamp.configuration
            && self
                .reads
                .policies
                .get(identity)
                .is_some_and(|policy| policy.offline && policy.version == stamp.policy)
    }

    pub fn revoke_access(
        &mut self,
        identity: &PetIdentity,
        policy_version: u64,
    ) -> Result<(), MemoryError> {
        self.set_read_policy(identity, ReadPolicy::deny_all(policy_version))?;
        self.reads
            .contexts
            .retain(|(owner, _), _| owner != identity);
        self.reads.identities.remove(identity);
        self.reads.cache.retain(|entry| &entry.identity != identity);
        Ok(())
    }

    pub fn remove_working_context(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        context_id: &str,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<u64, MemoryError> {
        let key = (identity.clone(), session.clone());
        let current = self
            .reads
            .contexts
            .get(&key)
            .and_then(|items| items.get(context_id))
            .ok_or(MemoryError::NotFound)?;
        if current.revision != expected_revision {
            return Err(MemoryError::ContextVersionConflict);
        }
        if now_ms < current.updated_at_ms {
            return Err(MemoryError::TimeWentBackwards);
        }
        let revision = expected_revision
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        let configuration = self.next_configuration()?;
        self.reads
            .contexts
            .get_mut(&key)
            .expect("validated session")
            .remove(context_id);
        self.reads.context_versions.insert(
            (identity.clone(), session.clone(), context_id.into()),
            (revision, now_ms),
        );
        self.reads
            .cache
            .retain(|entry| entry.identity != *identity || entry.session != *session);
        self.reads.configuration = configuration;
        Ok(revision)
    }

    pub fn remove_identity_view(
        &mut self,
        identity: &PetIdentity,
        expected_version: u64,
    ) -> Result<u64, MemoryError> {
        let current = self
            .reads
            .identities
            .get(identity)
            .ok_or(MemoryError::NotFound)?;
        if current.version != expected_version {
            return Err(MemoryError::IdentityVersionConflict);
        }
        let version = expected_version
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        let configuration = self.next_configuration()?;
        self.reads.identities.remove(identity);
        self.reads
            .identity_versions
            .insert(identity.clone(), version);
        self.reads.configuration = configuration;
        Ok(version)
    }

    pub fn for_session(
        &mut self,
        identity: &PetIdentity,
        session: &SessionId,
        purpose: ReadPurpose,
    ) -> MemoryReader<'_> {
        MemoryReader {
            service: self,
            identity: identity.clone(),
            session: session.clone(),
            purpose,
        }
    }

    fn source_current(&self, source: &SourceRef) -> bool {
        self.store
            .sources
            .get(&source.key)
            .is_some_and(|current| !current.revoked && current.revision == source.revision)
    }
}

pub struct MemoryReader<'service> {
    service: &'service mut MemoryService,
    identity: PetIdentity,
    session: SessionId,
    purpose: ReadPurpose,
}

impl MemoryReader<'_> {
    pub fn begin_task(
        &self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<crate::MemoryTask, MemoryError> {
        let result = self.retrieve(query, now_ms)?;
        let identity_view = self.identity().ok();
        let mut request = Vec::new();
        for number in [
            now_ms,
            query.search.text.len() as u64,
            query.search.limit as u64,
            query.char_budget as u64,
        ] {
            request.extend_from_slice(&number.to_le_bytes());
        }
        request.extend_from_slice(query.search.text.as_bytes());
        request.push(match query.search.scope {
            SearchScope::Private => 0,
            SearchScope::Shared => 1,
            SearchScope::All => 2,
        });
        request.push(match query.layer {
            RetrievalLayer::WorkingContext => 0,
            RetrievalLayer::LongTerm => 1,
            RetrievalLayer::All => 2,
            RetrievalLayer::FormalMaterial => 3,
        });
        request.extend_from_slice(
            &identity_view
                .as_ref()
                .map_or(0, IdentityView::version)
                .to_le_bytes(),
        );
        request.extend_from_slice(&(result.items().len() as u64).to_le_bytes());
        for item in result.items() {
            match item {
                RetrievalItem::LongTerm(record) => {
                    request.push(0);
                    request.extend_from_slice(&record.id().0.to_le_bytes());
                    request.extend_from_slice(&record.revision().to_le_bytes());
                }
                RetrievalItem::WorkingContext(context) => {
                    request.push(1);
                    request.extend_from_slice(&(context.id.len() as u64).to_le_bytes());
                    request.extend_from_slice(context.id.as_bytes());
                    request.extend_from_slice(&context.revision.to_le_bytes());
                }
            }
        }
        Ok(crate::MemoryTask {
            epoch: self.service.reads.epoch.clone(),
            identity: self.identity.clone(),
            purpose: self.purpose,
            stamp: crate::management::TaskStamp {
                generation: self.service.store.generation,
                configuration: self.service.reads.configuration,
                policy: self.authorize()?.version,
                session: self.session.clone(),
                request,
            },
            query: query.clone(),
            result,
            identity_view,
            issued_at_ms: now_ms,
        })
    }

    pub fn retrieve(
        &self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<RetrievalResult, MemoryError> {
        let (candidates, _) = self.authorized_candidates(query, now_ms)?;
        Ok(select(candidates, query))
    }

    pub fn identity(&self) -> Result<IdentityView, MemoryError> {
        let policy = self.authorize()?;
        self.service
            .reads
            .identities
            .get(&self.identity)
            .filter(|view| {
                view.sources.iter().all(|source| {
                    policy.sources.contains(source) && self.service.source_current(source)
                })
            })
            .cloned()
            .ok_or(MemoryError::NotFound)
    }

    pub fn entity(
        &mut self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<EntityRead, MemoryError> {
        let (candidates, dependencies) = self.authorized_candidates(query, now_ms)?;
        if query.search.text.trim().is_empty() {
            return Err(MemoryError::InvalidInput("entity label must not be empty"));
        }
        if query.layer == RetrievalLayer::FormalMaterial {
            return Ok(EntityRead::Entries(select(candidates, query)));
        }
        let position = self.service.reads.cache.iter().position(|entry| {
            entry.identity == self.identity
                && entry.session == self.session
                && entry.purpose == self.purpose
                && entry.query == *query
        });
        if let Some(position) = position {
            let entry = &self.service.reads.cache[position];
            if entry.dependencies == dependencies {
                return Ok(EntityRead::View {
                    view: entry.view.clone(),
                    cache: CacheStatus::Hit,
                });
            }
        }
        let result = select(candidates, query);
        if self.service.reads.cache_capacity == 0
            || (position.is_none()
                && self.service.reads.cache.len() >= self.service.reads.cache_capacity)
        {
            return Ok(EntityRead::Entries(result));
        }
        let view = EntityView {
            label: query.search.text.clone(),
            result,
        };
        let entry = CacheEntry {
            identity: self.identity.clone(),
            session: self.session.clone(),
            purpose: self.purpose,
            query: query.clone(),
            dependencies,
            view: view.clone(),
        };
        if let Some(position) = position {
            self.service.reads.cache[position] = entry;
        } else {
            self.service.reads.cache.push(entry);
        }
        Ok(EntityRead::View {
            view,
            cache: CacheStatus::Built,
        })
    }

    fn authorize(&self) -> Result<&ReadPolicy, MemoryError> {
        self.service
            .reads
            .policies
            .get(&self.identity)
            .filter(|policy| policy.offline && self.purpose == ReadPurpose::OfflineReasoning)
            .ok_or(MemoryError::AccessDenied)
    }

    fn authorized_candidates(
        &self,
        query: &RetrievalQuery,
        now_ms: u64,
    ) -> Result<(Vec<RetrievalItem>, Dependencies), MemoryError> {
        let policy = self.authorize()?;
        policy::validate_query(&query.search)?;
        if query.char_budget > MAX_RESULTS * MAX_TEXT_CHARS {
            return Err(MemoryError::InvalidInput("retrieval budget exceeds limit"));
        }
        let mut candidates = Vec::new();
        if query.layer != RetrievalLayer::WorkingContext {
            candidates.extend(
                self.service
                    .store
                    .records
                    .values()
                    .filter(|record| {
                        record.lifecycle() == Lifecycle::LongTerm
                            && if query.layer == RetrievalLayer::FormalMaterial {
                                matches!(record.scope(), MemoryScope::Shared(_))
                                    && record.protected
                                    && matches!(
                                        record.category(),
                                        Category::ToolDefinition | Category::Protocol
                                    )
                                    && record.epistemic() == Epistemic::Reference
                            } else {
                                !record.protected
                                    && !matches!(
                                        record.category(),
                                        Category::WorkingContext
                                            | Category::Identity
                                            | Category::ToolDefinition
                                            | Category::Protocol
                                    )
                            }
                            && policy::readable(record, &self.identity, &self.service.store, now_ms)
                            && policy::matches_scope(record, &query.search)
                            && record
                                .sources()
                                .iter()
                                .all(|source| policy.sources.contains(source))
                    })
                    .cloned()
                    .map(RetrievalItem::LongTerm),
            );
        }
        if matches!(
            query.layer,
            RetrievalLayer::WorkingContext | RetrievalLayer::All
        ) && query.search.scope != SearchScope::Shared
            && let Some(contexts) = self
                .service
                .reads
                .contexts
                .get(&(self.identity.clone(), self.session.clone()))
        {
            candidates.extend(
                contexts
                    .values()
                    .filter(|context| {
                        context.draft.sensitivity != Sensitivity::Restricted
                            && context
                                .draft
                                .expires_at_ms
                                .is_none_or(|expiration| now_ms < expiration)
                            && policy::sources_valid(
                                &context.draft,
                                &MemoryScope::Private(self.identity.clone()),
                                &self.service.store,
                            )
                            && policy::dependencies_valid(
                                &context.draft,
                                &MemoryScope::Private(self.identity.clone()),
                                &self.service.store,
                                now_ms,
                            )
                            && context
                                .draft
                                .sources
                                .iter()
                                .all(|source| policy.sources.contains(source))
                    })
                    .cloned()
                    .map(RetrievalItem::WorkingContext),
            );
        }
        let entries = candidates
            .iter()
            .map(|item| {
                let (key, revision) = match item {
                    RetrievalItem::LongTerm(record) => {
                        (EntryKey::Record(record.id()), record.revision())
                    }
                    RetrievalItem::WorkingContext(context) => {
                        (EntryKey::Context(context.id.clone()), context.revision)
                    }
                };
                Dependency { key, revision }
            })
            .collect();
        let sources = candidates
            .iter()
            .flat_map(|item| item.sources().iter().cloned())
            .collect();
        Ok((
            candidates,
            Dependencies {
                policy_version: policy.version,
                entries,
                sources,
            },
        ))
    }
}

fn select(candidates: Vec<RetrievalItem>, query: &RetrievalQuery) -> RetrievalResult {
    let needle = query.search.text.to_lowercase();
    let mut ranked: Vec<_> = candidates
        .into_iter()
        .filter_map(|item| {
            let text = item.text().to_lowercase();
            if !text.contains(&needle) {
                return None;
            }
            let relevance = if needle.is_empty() {
                0
            } else {
                text.matches(&needle).count()
            };
            Some((item, relevance))
        })
        .collect();
    ranked.sort_by(|(left, left_score), (right, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| right.updated_at_ms().cmp(&left.updated_at_ms()))
            .then_with(|| match (left, right) {
                (RetrievalItem::LongTerm(left), RetrievalItem::LongTerm(right)) => {
                    left.id().cmp(&right.id())
                }
                (RetrievalItem::WorkingContext(left), RetrievalItem::WorkingContext(right)) => {
                    left.id.cmp(&right.id)
                }
                (RetrievalItem::LongTerm(_), RetrievalItem::WorkingContext(_)) => {
                    std::cmp::Ordering::Less
                }
                (RetrievalItem::WorkingContext(_), RetrievalItem::LongTerm(_)) => {
                    std::cmp::Ordering::Greater
                }
            })
    });
    let mut result = RetrievalResult {
        items: Vec::new(),
        used_chars: 0,
    };
    for (item, _) in ranked {
        let cost = item.text().chars().count();
        if cost > query.char_budget - result.used_chars {
            continue;
        }
        result.used_chars += cost;
        result.items.push(item);
        if result.items.len() == query.search.limit {
            break;
        }
    }
    result
}
