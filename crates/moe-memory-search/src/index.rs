use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Weak};

use moe_memory_core::{
    CandidateRef, CandidateTicket, EmbeddingProfile, MemoryId, MemoryScope, RecordVersion,
    RetrievalItem, SearchScope,
};
use sha2::{Digest, Sha256};

use crate::{
    ChunkConfig, EncodedQuery, RetrievalHost, SearchError, TextChunk,
    cache::FileCache,
    chunk::{ChunkView, chunk_views},
    vector::UnitVector,
};

/// Trusted host binding, not a request-supplied tenant or a read capability.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StorageDomain(pub(crate) String);

impl StorageDomain {
    pub fn new(value: impl Into<String>) -> Result<Self, SearchError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(SearchError::InvalidConfig("invalid storage domain"));
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for StorageDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StorageDomain([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Hybrid,
    KeywordOnly,
    PartialIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchReason {
    PartialIndex,
    QueueFull,
    ProfileMismatch,
    CacheUnavailable,
    RerankUnavailable,
    ModelUnavailable,
    TransmissionDenied,
}

/// All counts describe only this ticket's currently authorized candidate set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexStatus {
    pub candidates: usize,
    pub indexed_records: usize,
    pub missing_chunks: usize,
    pub queued_records: usize,
    pub reasons: Vec<SearchReason>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RecordKey(pub(crate) MemoryScope, pub(crate) MemoryId);

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct IndexKey {
    pub(crate) domain: StorageDomain,
    pub(crate) scope: MemoryScope,
    pub(crate) parent: RecordVersion,
    pub(crate) config: ChunkConfig,
    pub(crate) ordinal: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) profile: EmbeddingProfile,
}

impl IndexKey {
    pub(crate) fn record_key(&self) -> RecordKey {
        RecordKey(self.scope.clone(), self.parent.id())
    }
}

pub(crate) struct StoredVector {
    pub(crate) input: Arc<RetrievalItem>,
    pub(crate) vector: UnitVector,
}

pub(crate) struct IndexSlice {
    pub(crate) key: IndexKey,
    pub(crate) reference: CandidateRef,
    pub(crate) input: Arc<RetrievalItem>,
    pub(crate) chunk: TextChunk,
}

fn index_key(
    domain: &StorageDomain,
    profile: &EmbeddingProfile,
    config: &ChunkConfig,
    input: &RetrievalItem,
    fingerprint: [u8; 32],
    chunk: ChunkView<'_>,
) -> Option<IndexKey> {
    let RetrievalItem::LongTerm(record) = input else {
        return None;
    };
    Some(IndexKey {
        domain: domain.clone(),
        scope: record.scope().clone(),
        parent: record.version_ref(),
        config: config.clone(),
        ordinal: chunk.ordinal,
        start: chunk.start_char,
        end: chunk.end_char,
        fingerprint,
        profile: profile.clone(),
    })
}

fn same_chunk(view: ChunkView<'_>, chunk: &TextChunk) -> bool {
    view.ordinal == chunk.ordinal()
        && view.start_char == chunk.start_char()
        && view.end_char == chunk.end_char()
        && view.text() == chunk.text()
}

/// Sealed by an index from current core-authorized inputs; not constructible from IDs.
pub struct IndexBatch {
    pub(crate) lease: Arc<()>,
    pub(crate) domain: StorageDomain,
    pub(crate) generation: u64,
    pub(crate) profile: EmbeddingProfile,
    pub(crate) config: ChunkConfig,
    pub(crate) slices: Vec<IndexSlice>,
}

impl IndexBatch {
    pub fn len(&self) -> usize {
        self.slices.len()
    }
    pub fn is_empty(&self) -> bool {
        self.slices.is_empty()
    }
    pub(crate) fn validate(&self, ticket: &CandidateTicket) -> Result<(), SearchError> {
        if &self.profile != ticket.plan().profile() {
            return Err(SearchError::ProfileMismatch);
        }
        if self.slices.is_empty() || self.slices.len() > 32 {
            return Err(SearchError::InvalidInput("invalid index batch"));
        }
        for slice in &self.slices {
            if ticket.candidate(&slice.reference) != Some(slice.input.as_ref()) {
                return Err(SearchError::StaleIndexWork);
            }
            let fingerprint: [u8; 32] = Sha256::digest(slice.input.text().as_bytes()).into();
            let mut expected_key = None;
            for chunk in chunk_views(slice.input.text(), &self.config) {
                let chunk = chunk?;
                if chunk.ordinal == slice.chunk.ordinal() {
                    if !same_chunk(chunk, &slice.chunk) {
                        return Err(SearchError::StaleIndexWork);
                    }
                    expected_key = index_key(
                        &self.domain,
                        &self.profile,
                        &self.config,
                        slice.input.as_ref(),
                        fingerprint,
                        chunk,
                    );
                }
            }
            if expected_key.as_ref() != Some(&slice.key) {
                return Err(SearchError::StaleIndexWork);
            }
        }
        Ok(())
    }
}

/// Produced only by the authorized embedding adapter. No bare-vector insertion API exists.
pub struct CompletedIndexBatch {
    pub(crate) batch: IndexBatch,
    pub(crate) vectors: Vec<UnitVector>,
}

impl fmt::Debug for IndexBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IndexBatch([redacted])")
    }
}
impl fmt::Debug for CompletedIndexBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CompletedIndexBatch([redacted])")
    }
}

/// Disposable derived state only. Construction and all ranking are network-free.
pub struct SearchIndex {
    pub(crate) domain: StorageDomain,
    pub(crate) profile: EmbeddingProfile,
    pub(crate) config: ChunkConfig,
    pub(crate) entries: BTreeMap<IndexKey, StoredVector>,
    queue: BTreeMap<RecordKey, Arc<RetrievalItem>>,
    queue_capacity: usize,
    batch_limit: usize,
    pub(crate) vector_capacity: usize,
    generation: u64,
    in_flight: Weak<()>,
    pub(crate) cache: Option<FileCache>,
    pub(crate) notices: Vec<SearchReason>,
    pub(crate) calibration: Option<crate::HybridConfig>,
}

impl fmt::Debug for SearchIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SearchIndex([redacted])")
    }
}

impl SearchIndex {
    pub fn new(
        domain: StorageDomain,
        profile: EmbeddingProfile,
        config: ChunkConfig,
        queue_capacity: usize,
        batch_limit: usize,
        vector_capacity: usize,
    ) -> Result<Self, SearchError> {
        if !(1..=4096).contains(&queue_capacity)
            || !(1..=32).contains(&batch_limit)
            || !(1..=65536).contains(&vector_capacity)
        {
            return Err(SearchError::InvalidConfig("invalid index capacities"));
        }
        Ok(Self {
            domain,
            profile,
            config,
            entries: BTreeMap::new(),
            queue: BTreeMap::new(),
            queue_capacity,
            batch_limit,
            vector_capacity,
            generation: 1,
            in_flight: Weak::new(),
            cache: None,
            notices: Vec::new(),
            calibration: None,
        })
    }

    /// A trusted configuration change invalidates in-flight work and old vectors.
    pub fn rebuild<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        profile: EmbeddingProfile,
        config: ChunkConfig,
    ) -> Result<(), SearchError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(SearchError::InvalidConfig("index generation exhausted"))?;
        host.authority().cancel_current_tasks()?;
        self.generation = generation;
        self.profile = profile;
        self.config = config;
        self.entries.clear();
        self.queue.clear();
        self.in_flight = Weak::new();
        self.calibration = None;
        self.notices.push(SearchReason::ProfileMismatch);
        Ok(())
    }

    pub fn chunk_config(&self) -> &ChunkConfig {
        &self.config
    }

    pub(crate) fn bind_calibration<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        config: &crate::HybridConfig,
    ) -> Result<(), SearchError> {
        if self
            .calibration
            .as_ref()
            .is_some_and(|current| current != config)
        {
            host.authority().cancel_current_tasks()?;
            self.calibration = Some(config.clone());
            return Err(moe_memory_core::MemoryError::TaskCancelled.into());
        }
        self.calibration = Some(config.clone());
        Ok(())
    }

    pub(crate) fn authorize<Host: RetrievalHost>(
        &self,
        host: &mut Host,
        ticket: &CandidateTicket,
    ) -> Result<(), SearchError> {
        let now = host.now_ms();
        host.authority().validate_candidate_ticket(ticket, now)?;
        if ticket.plan().profile() != &self.profile {
            return Err(SearchError::ProfileMismatch);
        }
        Ok(())
    }

    pub(crate) fn visit_expected<'ticket>(
        &self,
        ticket: &'ticket CandidateTicket,
        mut visit: impl FnMut(
            IndexKey,
            CandidateRef,
            &'ticket RetrievalItem,
            ChunkView<'ticket>,
        ) -> Result<bool, SearchError>,
    ) -> Result<bool, SearchError> {
        for candidate in ticket.candidates() {
            let RetrievalItem::LongTerm(record) = candidate.item() else {
                continue;
            };
            let fingerprint: [u8; 32] = Sha256::digest(record.text().as_bytes()).into();
            let reference = candidate.candidate_ref();
            for chunk in chunk_views(record.text(), &self.config) {
                let chunk = chunk?;
                let Some(key) = index_key(
                    &self.domain,
                    &self.profile,
                    &self.config,
                    candidate.item(),
                    fingerprint,
                    chunk,
                ) else {
                    continue;
                };
                if !visit(key, reference.clone(), candidate.item(), chunk)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn covered_by_ticket(ticket: &CandidateTicket, input: &RetrievalItem) -> bool {
        let RetrievalItem::LongTerm(record) = input else {
            return false;
        };
        let in_scope = match record.scope() {
            MemoryScope::Private(owner) => {
                owner == ticket.identity() && ticket.query().scope() != SearchScope::Shared
            }
            MemoryScope::Shared(namespace) => {
                namespace == ticket.identity().namespace()
                    && ticket.query().scope() != SearchScope::Private
            }
        };
        in_scope
            && match ticket.query().layer() {
                moe_memory_core::RetrievalLayer::WorkingContext => false,
                moe_memory_core::RetrievalLayer::FormalMaterial => record.is_protected(),
                moe_memory_core::RetrievalLayer::LongTerm
                | moe_memory_core::RetrievalLayer::All => !record.is_protected(),
            }
    }

    /// Missing notifications are repaired only within the ticket's scope and retrieval layer.
    /// Queue merging is record-level; no authoritative commit is performed here.
    pub fn reconcile<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        ticket: &CandidateTicket,
    ) -> Result<IndexStatus, SearchError> {
        self.authorize(host, ticket)?;
        let mut records = BTreeMap::new();
        let mut queue_inputs = BTreeMap::new();
        let mut queue_order = Vec::new();
        let mut matching_keys = BTreeSet::new();
        let mut missing = BTreeSet::new();
        let mut present = BTreeSet::new();
        let mut missing_chunks = 0;
        let mut queue_full = false;
        let mut status = IndexStatus {
            candidates: ticket.candidates().len(),
            ..IndexStatus::default()
        };
        self.visit_expected(ticket, |key, _, input, _| {
            let record = key.record_key();
            records.entry(record.clone()).or_insert(input);
            if self
                .entries
                .get(&key)
                .is_some_and(|stored| stored.input.as_ref() == input)
            {
                matching_keys.insert(key);
                present.insert(record);
            } else {
                missing_chunks += 1;
                if missing.insert(record.clone()) {
                    queue_order.push(record.clone());
                    queue_inputs.insert(record, input);
                }
            }
            Ok(true)
        })?;
        self.entries.retain(|key, stored| {
            !Self::covered_by_ticket(ticket, stored.input.as_ref()) || matching_keys.contains(key)
        });
        self.queue.retain(|key, input| {
            !Self::covered_by_ticket(ticket, input.as_ref())
                || records
                    .get(key)
                    .is_some_and(|current| *current == input.as_ref())
        });
        status.reasons = std::mem::take(&mut self.notices);
        for record in queue_order {
            if self.queue.contains_key(&record) {
                continue;
            }
            if self.queue.len() < self.queue_capacity {
                if let Some(input) = queue_inputs.get(&record) {
                    self.queue.insert(record, Arc::new((**input).clone()));
                }
            } else {
                queue_full = true;
            }
        }
        status.missing_chunks = missing_chunks;
        status.indexed_records = present.len();
        status.queued_records = missing
            .iter()
            .filter(|key| self.queue.contains_key(*key))
            .count();
        if queue_full {
            status.reasons.push(SearchReason::QueueFull);
        }
        if status.missing_chunks > 0 {
            status.reasons.push(SearchReason::PartialIndex);
        }
        Ok(status)
    }

    /// The caller runs at most one batch at a time; a batch has at most 32 slices.
    pub fn take_batch<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        ticket: &CandidateTicket,
    ) -> Result<Option<IndexBatch>, SearchError> {
        self.reconcile(host, ticket)?;
        if self.in_flight.upgrade().is_some() {
            return Err(SearchError::InvalidInput("index batch already in flight"));
        }
        let mut slices = Vec::with_capacity(self.batch_limit);
        self.visit_expected(ticket, |key, reference, input, chunk| {
            let record = key.record_key();
            let Some(queued) = self
                .queue
                .get(&record)
                .filter(|queued| queued.as_ref() == input)
            else {
                return Ok(true);
            };
            if !self.entries.contains_key(&key) {
                slices.push(IndexSlice {
                    key,
                    reference,
                    input: Arc::clone(queued),
                    chunk: chunk.to_owned(),
                });
            }
            Ok(slices.len() < self.batch_limit)
        })?;
        if slices.is_empty() {
            return Ok(None);
        }
        let lease = Arc::new(());
        self.in_flight = Arc::downgrade(&lease);
        Ok(Some(IndexBatch {
            lease,
            domain: self.domain.clone(),
            generation: self.generation,
            profile: self.profile.clone(),
            config: self.config.clone(),
            slices,
        }))
    }

    /// Late, revoked, wrong-profile and foreign-ticket results cannot change active state.
    pub fn commit<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        ticket: &CandidateTicket,
        completed: CompletedIndexBatch,
    ) -> Result<(), SearchError> {
        self.authorize(host, ticket)?;
        let batch = completed.batch;
        batch.validate(ticket)?;
        if batch.domain != self.domain
            || batch.generation != self.generation
            || batch.config != self.config
            || batch.slices.len() != completed.vectors.len()
            || !self.in_flight.ptr_eq(&Arc::downgrade(&batch.lease))
        {
            return Err(SearchError::StaleIndexWork);
        }
        let added = batch
            .slices
            .iter()
            .filter(|slice| !self.entries.contains_key(&slice.key))
            .count();
        if added > self.vector_capacity.saturating_sub(self.entries.len()) {
            return Err(SearchError::InvalidInput("index vector capacity exceeded"));
        }
        let batch_keys: BTreeSet<_> = batch.slices.iter().map(|slice| slice.key.clone()).collect();
        let mut remaining = BTreeSet::new();
        self.visit_expected(ticket, |key, _, _, _| {
            if !self.entries.contains_key(&key) && !batch_keys.contains(&key) {
                remaining.insert(key.record_key());
            }
            Ok(true)
        })?;
        for (slice, vector) in batch.slices.into_iter().zip(completed.vectors) {
            self.entries.insert(
                slice.key,
                StoredVector {
                    input: slice.input,
                    vector,
                },
            );
        }
        self.queue.retain(|key, input| {
            !Self::covered_by_ticket(ticket, input.as_ref()) || remaining.contains(key)
        });
        Ok(())
    }

    pub(crate) fn vector_rank(
        &self,
        ticket: &CandidateTicket,
        query: &EncodedQuery,
        minimum: f64,
    ) -> Result<Vec<(CandidateRef, f64)>, SearchError> {
        if query.profile != self.profile
            || query.text != ticket.query().text()
            || query.text.is_empty()
        {
            return Err(SearchError::ProfileMismatch);
        }
        let mut records: BTreeMap<CandidateRef, f64> = BTreeMap::new();
        self.visit_expected(ticket, |key, reference, input, _| {
            if let Some(stored) = self
                .entries
                .get(&key)
                .filter(|stored| stored.input.as_ref() == input)
            {
                let score = query.vector.cosine(&stored.vector);
                if score >= minimum {
                    records
                        .entry(reference)
                        .and_modify(|current| *current = current.max(score))
                        .or_insert(score);
                }
            }
            Ok(true)
        })?;
        let mut ranked: Vec<_> = records.into_iter().collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(ranked)
    }

    pub(crate) fn rerank_chunks(
        &self,
        ticket: &CandidateTicket,
        entries: &[CandidateRef],
        query: Option<&EncodedQuery>,
    ) -> Result<Vec<String>, SearchError> {
        self.visit_expected(ticket, |_, _, _, _| Ok(true))?;
        entries
            .iter()
            .map(|reference| {
                let item = ticket
                    .candidate(reference)
                    .ok_or(SearchError::InvalidInput("foreign rerank reference"))?;
                let fingerprint: [u8; 32] = Sha256::digest(item.text().as_bytes()).into();
                let working_context = matches!(item, RetrievalItem::WorkingContext(_));
                let mut best = None;
                for chunk in chunk_views(item.text(), &self.config) {
                    let chunk = chunk?;
                    let semantic = match index_key(
                        &self.domain,
                        &self.profile,
                        &self.config,
                        item,
                        fingerprint,
                        chunk,
                    )
                    .and_then(|key| self.entries.get(&key))
                    .filter(|stored| stored.input.as_ref() == item)
                    {
                        Some(stored) => query
                            .map(|query| query.vector.cosine(&stored.vector))
                            .unwrap_or(-1.0),
                        None => -1.0,
                    };
                    let exact = matches!(item, RetrievalItem::LongTerm(_))
                        && !ticket.query().text().is_empty()
                        && chunk.text().contains(ticket.query().text());
                    let score = (
                        exact,
                        crate::lexical::matching_terms(ticket.query().text(), chunk.text()),
                        semantic,
                    );
                    if best.as_ref().is_none_or(|(current, _)| {
                        score > *current || (working_context && score == *current)
                    }) {
                        best = Some((score, chunk.text().to_owned()));
                    }
                }
                match best {
                    Some((_, text)) => Ok(text),
                    None => Err(SearchError::InputTooLong),
                }
            })
            .collect()
    }

    /// Explicit opt-in. The existing protected directory is host-selected.
    pub fn enable_file_cache(
        &mut self,
        protected_directory: &Path,
        max_bytes: usize,
    ) -> Result<(), SearchError> {
        self.cache = Some(FileCache::new(protected_directory, max_bytes)?);
        Ok(())
    }
}

#[cfg(test)]
mod queue_regressions {
    use moe_memory_core::{
        CandidateTicket, Category, EmbeddingProfile, Epistemic, MemoryDraft, MemoryScope,
        MemoryService, Namespace, PetIdentity, PublicationPolicy, ReadPolicy, ReadPurpose,
        RetrievalLayer, RetrievalPlan, RetrievalQuery, SessionId,
    };

    use super::{CompletedIndexBatch, SearchError, SearchIndex, StorageDomain};
    use crate::{ChunkConfig, SearchHost, vector::UnitVector};

    struct Host(MemoryService);

    impl SearchHost for Host {
        fn memory(&mut self) -> &mut MemoryService {
            &mut self.0
        }

        fn now_ms(&mut self) -> u64 {
            100
        }
    }

    fn ticket(
        host: &mut Host,
        owner: &PetIdentity,
        profile: &EmbeddingProfile,
        layer: RetrievalLayer,
    ) -> CandidateTicket {
        host.0
            .for_session(
                owner,
                &SessionId::new("queue-session").unwrap(),
                ReadPurpose::OfflineReasoning,
            )
            .begin_candidates(
                &RetrievalQuery::new("").with_layer(layer),
                RetrievalPlan::new(profile.clone()),
                100,
            )
            .unwrap()
    }

    #[test]
    fn layer_switches_and_cross_layer_commits_preserve_exact_queued_snapshots() {
        let namespace = Namespace::new("queue-regressions", "user").unwrap();
        let owner = PetIdentity::new(namespace.clone(), "alice").unwrap();
        let mut host = Host(MemoryService::new());
        let ordinary_source = host
            .0
            .register_source(
                MemoryScope::Private(owner.clone()),
                "ordinary",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();
        for text in ["ordinary first", "ordinary second"] {
            host.0
                .for_pet(&owner)
                .create(
                    MemoryDraft::new(
                        text,
                        Category::Episode,
                        Epistemic::UserStatement,
                        vec![ordinary_source.clone()],
                    ),
                    100,
                )
                .unwrap();
        }
        let formal_source = host
            .0
            .register_source(
                MemoryScope::Shared(namespace.clone()),
                "formal",
                1,
                Epistemic::Reference,
            )
            .unwrap();
        host.0
            .set_formal_publication_policy(
                &namespace,
                PublicationPolicy::allow_sources(1, vec![formal_source.clone()]),
            )
            .unwrap();
        for text in ["formal first", "formal second"] {
            let command = host
                .0
                .prepare_formal_publication(
                    &namespace,
                    MemoryDraft::new(
                        text,
                        Category::Protocol,
                        Epistemic::Reference,
                        vec![formal_source.clone()],
                    ),
                    100,
                )
                .unwrap();
            command.execute(&mut host.0).unwrap();
        }
        host.0
            .set_read_policy(
                &owner,
                ReadPolicy::offline(1, vec![ordinary_source, formal_source]),
            )
            .unwrap();
        let profile = EmbeddingProfile::new("fixture", "queue-model", 2).unwrap();
        let mut index = SearchIndex::new(
            StorageDomain::new("queue-root").unwrap(),
            profile.clone(),
            ChunkConfig::new(1, 512, 16, 1536).unwrap(),
            16,
            1,
            16,
        )
        .unwrap();
        let ordinary = ticket(&mut host, &owner, &profile, RetrievalLayer::LongTerm);
        let working = ticket(&mut host, &owner, &profile, RetrievalLayer::WorkingContext);
        let formal = ticket(&mut host, &owner, &profile, RetrievalLayer::FormalMaterial);
        index.reconcile(&mut host, &ordinary).unwrap();
        let ordinary_snapshot = index.queue.clone();
        assert_eq!(ordinary_snapshot.len(), 2);
        index.reconcile(&mut host, &working).unwrap();
        assert!(index.queue == ordinary_snapshot);
        index.reconcile(&mut host, &formal).unwrap();
        assert_eq!(index.queue.len(), 4);
        assert!(
            ordinary_snapshot
                .iter()
                .all(|(key, input)| index.queue.get(key) == Some(input))
        );
        let mixed_snapshot = index.queue.clone();
        index.reconcile(&mut host, &working).unwrap();
        assert!(index.queue == mixed_snapshot);
        for current in [&formal, &ordinary] {
            let before = index.queue.clone();
            let batch = index.take_batch(&mut host, current).unwrap().unwrap();
            assert_eq!(batch.len(), 1);
            assert!(index.queue == before);
            let batch_key = batch.slices[0].key.clone();
            let batch_input = std::sync::Arc::clone(&batch.slices[0].input);
            assert!(std::sync::Arc::ptr_eq(
                index.queue.get(&batch_key.record_key()).unwrap(),
                &batch_input
            ));
            let mut expected = before;
            assert!(
                expected.remove(&batch.slices[0].key.record_key())
                    == Some(batch.slices[0].input.clone())
            );
            index
                .commit(
                    &mut host,
                    current,
                    CompletedIndexBatch {
                        batch,
                        vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()],
                    },
                )
                .unwrap();
            assert!(std::sync::Arc::ptr_eq(
                &index.entries.get(&batch_key).unwrap().input,
                &batch_input
            ));
            assert!(index.queue == expected);
            index.reconcile(&mut host, &working).unwrap();
            assert!(index.queue == expected);
        }
        assert_eq!(index.queue.len(), 2);
        assert_eq!(index.entries.len(), 2);
        let remaining = index.queue.clone();
        for layer in [
            RetrievalLayer::All,
            RetrievalLayer::FormalMaterial,
            RetrievalLayer::LongTerm,
        ] {
            let current = ticket(&mut host, &owner, &profile, layer);
            index.reconcile(&mut host, &current).unwrap();
            assert!(index.queue == remaining);
        }
    }

    #[test]
    fn invalid_late_candidate_keeps_a_full_queue_unchanged() {
        let namespace = Namespace::new("queue-atomicity", "user").unwrap();
        let owner = PetIdentity::new(namespace, "alice").unwrap();
        let mut host = Host(MemoryService::new());
        let source = host
            .0
            .register_source(
                MemoryScope::Private(owner.clone()),
                "synthetic",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();
        host.0
            .for_pet(&owner)
            .create(
                MemoryDraft::new(
                    "short",
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![source.clone()],
                ),
                100,
            )
            .unwrap();
        host.0
            .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
            .unwrap();
        let profile = EmbeddingProfile::new("fixture", "atomicity-model", 2).unwrap();
        let mut index = SearchIndex::new(
            StorageDomain::new("atomicity-root").unwrap(),
            profile.clone(),
            ChunkConfig::new(1, 2048, 2047, 16384).unwrap(),
            1,
            1,
            1,
        )
        .unwrap();
        let valid = ticket(&mut host, &owner, &profile, RetrievalLayer::LongTerm);
        assert_eq!(
            index.reconcile(&mut host, &valid).unwrap().queued_records,
            1
        );
        let queued = index.queue.clone();

        host.0
            .for_pet(&owner)
            .create(
                MemoryDraft::new(
                    "中".repeat(4096),
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![source],
                ),
                100,
            )
            .unwrap();
        let expanded = ticket(&mut host, &owner, &profile, RetrievalLayer::LongTerm);
        assert_eq!(expanded.candidates().len(), 2);
        assert!(matches!(
            index.reconcile(&mut host, &expanded),
            Err(SearchError::InputTooLong)
        ));
        assert!(index.queue == queued);
    }
}
