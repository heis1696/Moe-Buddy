use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use moe_memory_core::{
    CandidateTicket, Category, EmbeddingProfile, Epistemic, MemoryContextAccess, MemoryDraft,
    MemoryError, MemoryScope, MemoryService, Namespace, PetIdentity, ReadPolicy, ReadPurpose,
    RetrievalLayer, RetrievalPlan, RetrievalQuery, SearchScope, SessionId, SourceRef,
};
use moe_memory_local::{LocalMemory, OperationId, PersistenceError};

use crate::hybrid::{fallback_eligible, transmission_denied};
use crate::{
    CallControl, Cancellation, ChunkConfig, CompletedIndexBatch, EncodedQuery, HybridConfig,
    LocalSearchHost, RerankMode, SearchError, SearchHost, SearchIndex, SearchMemory, SearchMode,
    SearchReason, StorageDomain, hybrid_rank, rerank_or_fallback, vector::UnitVector,
};

struct UnhealthyHost {
    validations: usize,
    recoveries: usize,
}

impl crate::RetrievalHost for UnhealthyHost {
    fn authority(&mut self) -> &mut dyn crate::SearchAuthority {
        self
    }

    fn now_ms(&mut self) -> u64 {
        100
    }
}

impl crate::SearchAuthority for UnhealthyHost {
    fn validate_candidate_ticket(
        &mut self,
        _: &CandidateTicket,
        _: u64,
    ) -> Result<(), SearchError> {
        self.validations += 1;
        if self.validations == 1 {
            Ok(())
        } else {
            Err(SearchError::Persistence(PersistenceError::RecoveryRequired))
        }
    }

    fn recover_failed_rerank(
        &mut self,
        _: &mut CandidateTicket,
        _: u64,
    ) -> Result<CandidateTicket, SearchError> {
        self.recoveries += 1;
        Err(SearchError::Persistence(PersistenceError::RecoveryRequired))
    }

    fn prepare_transmission(
        &mut self,
        _: &CandidateTicket,
        _: moe_memory_core::TransmissionUsage,
        _: &str,
        _: &[moe_memory_core::CandidateRef],
        _: u64,
    ) -> Result<moe_memory_core::TransmissionTicket, SearchError> {
        unreachable!()
    }

    fn record_transmission(
        &mut self,
        _: &mut CandidateTicket,
        _: &moe_memory_core::TransmissionTicket,
        _: u64,
    ) -> Result<(), SearchError> {
        unreachable!()
    }

    fn fail_transmission(
        &mut self,
        _: &mut CandidateTicket,
        _: &moe_memory_core::TransmissionTicket,
        _: u64,
    ) -> Result<(), SearchError> {
        unreachable!()
    }

    fn cancel_current_tasks(&mut self) -> Result<(), SearchError> {
        unreachable!()
    }
}

#[tokio::test]
async fn health_failure_cannot_enter_failed_rerank_recovery() {
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let config = HybridConfig::new(
        fixture.profile.clone(),
        1,
        0.8,
        true,
        RerankMode::AllowFallback,
        20,
    )
    .unwrap();
    let ranking = hybrid_rank(&mut index, &mut fixture.host, &ticket, None, &config).unwrap();
    let mut host = UnhealthyHost {
        validations: 0,
        recoveries: 0,
    };
    let control = CallControl::new(Cancellation::default(), Duration::from_secs(1)).unwrap();
    let failure = rerank_or_fallback(&index, &mut host, ticket, ranking, None, None, &control)
        .await
        .unwrap_err();
    assert!(matches!(
        failure.error(),
        SearchError::Persistence(PersistenceError::RecoveryRequired)
    ));
    assert_eq!(host.recoveries, 0);
}

struct Host {
    service: MemoryService,
    now: u64,
}
impl SearchHost for Host {
    fn memory(&mut self) -> &mut MemoryService {
        &mut self.service
    }
    fn now_ms(&mut self) -> u64 {
        self.now
    }
}

struct Fixture {
    host: Host,
    owner: PetIdentity,
    source: SourceRef,
    profile: EmbeddingProfile,
}
impl Fixture {
    fn new(texts: &[&str]) -> Self {
        let mut service = MemoryService::new();
        let owner =
            PetIdentity::new(Namespace::new("index-tests", "user").unwrap(), "alice").unwrap();
        let source = service
            .register_source(
                MemoryScope::Private(owner.clone()),
                "synthetic",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();
        for text in texts {
            service
                .for_pet(&owner)
                .create(draft(text, &source), 100)
                .unwrap();
        }
        service
            .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
            .unwrap();
        Self {
            host: Host { service, now: 100 },
            owner,
            source,
            profile: EmbeddingProfile::new("fixture", "model", 2).unwrap(),
        }
    }
    fn ticket(&mut self, query: RetrievalQuery) -> CandidateTicket {
        self.host
            .service
            .for_session(
                &self.owner,
                &SessionId::new("session").unwrap(),
                ReadPurpose::OfflineReasoning,
            )
            .begin_candidates(
                &query,
                RetrievalPlan::new(self.profile.clone()),
                self.host.now,
            )
            .unwrap()
    }
    fn index(&self) -> SearchIndex {
        SearchIndex::new(
            StorageDomain::new("trusted-root-a").unwrap(),
            self.profile.clone(),
            chunks(),
            256,
            32,
            4096,
        )
        .unwrap()
    }
    fn config(&self, threshold: f64) -> HybridConfig {
        HybridConfig::new(
            self.profile.clone(),
            1,
            threshold,
            true,
            RerankMode::Disabled,
            20,
        )
        .unwrap()
    }
}

fn draft(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Episode,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
}
fn chunks() -> ChunkConfig {
    ChunkConfig::new(1, 512, 16, 1536).unwrap()
}
fn encoded(ticket: &CandidateTicket, values: Vec<f64>) -> EncodedQuery {
    EncodedQuery {
        profile: ticket.plan().profile().clone(),
        text: ticket.query().text().to_owned(),
        vector: UnitVector::new(values, 2).unwrap(),
    }
}

fn fill(
    index: &mut SearchIndex,
    fixture: &mut Fixture,
    ticket: &CandidateTicket,
    vector: impl Fn(&str) -> Vec<f64>,
) {
    while let Some(batch) = index.take_batch(&mut fixture.host, ticket).unwrap() {
        let vectors = batch
            .slices
            .iter()
            .map(|slice| UnitVector::new(vector(slice.chunk.text()), 2).unwrap())
            .collect();
        index
            .commit(
                &mut fixture.host,
                ticket,
                CompletedIndexBatch { batch, vectors },
            )
            .unwrap();
    }
}

#[test]
fn complete_authorized_candidates_not_keyword_top100_feed_both_routes() {
    let mut fixture = Fixture::new(&["茉莉花茶"]);
    for position in 0..150 {
        fixture
            .host
            .service
            .for_pet(&fixture.owner)
            .create(
                draft(&format!("unrelated {position}"), &fixture.source),
                100,
            )
            .unwrap();
    }
    let ticket = fixture.ticket(RetrievalQuery::new("偏爱的饮品").with_limit(1));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |text| {
        if text == "茉莉花茶" {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        }
    });
    let config = fixture.config(0.9);
    let result = hybrid_rank(
        &mut index,
        &mut fixture.host,
        &ticket,
        Some(&encoded(&ticket, vec![1.0, 0.0])),
        &config,
    )
    .unwrap();
    assert_eq!(result.status().candidates, 151);
    assert_eq!(result.lexical_hits(), 0);
    assert_eq!(result.vector_hits(), 1);
    assert_eq!(
        ticket.candidate(&result.ranking()[0].0).unwrap().text(),
        "茉莉花茶"
    );
}

#[test]
fn foreign_pet_sources_never_enter_statistics_queue_or_budget() {
    let mut fixture = Fixture::new(&["tea"]);
    let other = PetIdentity::new(fixture.owner.namespace().clone(), "bob").unwrap();
    let source = fixture
        .host
        .service
        .register_source(
            MemoryScope::Private(other.clone()),
            "other",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    fixture
        .host
        .service
        .for_pet(&other)
        .create(draft("tea tea tea", &source), 100)
        .unwrap();
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let config = fixture.config(0.9);
    let result = hybrid_rank(&mut index, &mut fixture.host, &ticket, None, &config).unwrap();
    assert_eq!(result.status().candidates, 1);
    assert_eq!(result.status().queued_records, 1);
    assert_eq!(result.lexical_hits(), 1);
    assert_eq!(result.used_chars(), 3);
}

#[test]
fn rrf_uses_ranks_not_raw_bm25_plus_cosine() {
    let mut fixture = Fixture::new(&["tea", "tea tea", "drink"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |text| {
        if text == "tea" {
            vec![0.0, 1.0]
        } else {
            vec![1.0, 0.0]
        }
    });
    let config = fixture.config(0.1);
    let result = hybrid_rank(
        &mut index,
        &mut fixture.host,
        &ticket,
        Some(&encoded(&ticket, vec![1.0, 0.0])),
        &config,
    )
    .unwrap();
    assert_eq!(
        ticket.candidate(&result.ranking()[0].0).unwrap().text(),
        "tea tea"
    );
    assert!((result.ranking()[0].1 - (1.0 / 61.0 + 1.0 / 61.0)).abs() < 1e-12);
}

#[test]
fn multiple_slices_take_maximum_and_one_record_slot_with_full_body() {
    let mut fixture = Fixture::new(&["first sentence. tea final sentence.", "tea"]);
    let mut index = fixture.index();
    index
        .rebuild(
            &mut fixture.host,
            fixture.profile.clone(),
            ChunkConfig::new(2, 12, 2, 36).unwrap(),
        )
        .unwrap();
    let ticket = fixture.ticket(RetrievalQuery::new("drink").with_limit(2));
    fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
    let config = fixture.config(0.8);
    let result = hybrid_rank(
        &mut index,
        &mut fixture.host,
        &ticket,
        Some(&encoded(&ticket, vec![1.0, 0.0])),
        &config,
    )
    .unwrap();
    assert_eq!(result.vector_hits(), 2);
    assert_eq!(result.ranking().len(), 2);
    let task = fixture
        .host
        .service
        .for_session(
            &fixture.owner,
            &SessionId::new("session").unwrap(),
            ReadPurpose::OfflineReasoning,
        )
        .complete_retrieval(&ticket, result.ranking(), 100)
        .unwrap();
    assert!(
        task.result()
            .items()
            .iter()
            .any(|item| item.text() == "first sentence. tea final sentence.")
    );
}

#[test]
fn threshold_returns_empty_instead_of_filling_unrelated_memories() {
    let mut fixture = Fixture::new(&["车辆维修手册"]);
    let ticket = fixture.ticket(RetrievalQuery::new("饮品"));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |_| vec![0.0, 1.0]);
    let config = fixture.config(0.8);
    let result = hybrid_rank(
        &mut index,
        &mut fixture.host,
        &ticket,
        Some(&encoded(&ticket, vec![1.0, 0.0])),
        &config,
    )
    .unwrap();
    assert!(result.ranking().is_empty());
    assert_eq!(result.mode(), SearchMode::Hybrid);
}

#[test]
fn missing_index_is_visible_and_keyword_route_still_covers_full_set() {
    let mut fixture = Fixture::new(&["tea", "coffee"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let config = fixture.config(0.8);
    let result = hybrid_rank(
        &mut index,
        &mut fixture.host,
        &ticket,
        Some(&encoded(&ticket, vec![1.0, 0.0])),
        &config,
    )
    .unwrap();
    assert_eq!(result.mode(), SearchMode::PartialIndex);
    assert_eq!(result.status().missing_chunks, 2);
    assert!(
        result
            .status()
            .reasons
            .contains(&SearchReason::PartialIndex)
    );
    assert_eq!(result.ranking().len(), 1);
}

#[test]
fn bounded_queue_merges_records_and_allows_only_one_batch_in_flight() {
    let mut fixture = Fixture::new(&["tea", "coffee"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = SearchIndex::new(
        StorageDomain::new("root").unwrap(),
        fixture.profile.clone(),
        chunks(),
        1,
        1,
        10,
    )
    .unwrap();
    let first = index.reconcile(&mut fixture.host, &ticket).unwrap();
    let second = index.reconcile(&mut fixture.host, &ticket).unwrap();
    assert_eq!(first.queued_records, 1);
    assert_eq!(second.queued_records, 1);
    assert!(second.reasons.contains(&SearchReason::QueueFull));
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    assert_eq!(batch.len(), 1);
    assert!(index.take_batch(&mut fixture.host, &ticket).is_err());
    drop(batch);
    assert!(
        index
            .take_batch(&mut fixture.host, &ticket)
            .unwrap()
            .is_some()
    );
}

#[test]
fn model_or_chunk_rebuild_rejects_late_work_even_when_dimension_matches() {
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    index
        .rebuild(
            &mut fixture.host,
            fixture.profile.clone(),
            ChunkConfig::new(2, 500, 16, 1500).unwrap(),
        )
        .unwrap();
    assert_eq!(
        index.commit(
            &mut fixture.host,
            &ticket,
            CompletedIndexBatch {
                batch,
                vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()]
            }
        ),
        Err(SearchError::Core(MemoryError::TaskCancelled))
    );
    fixture.profile = EmbeddingProfile::new("fixture", "other-model", 2).unwrap();
    let changed = fixture.ticket(RetrievalQuery::new("tea"));
    assert_eq!(
        index.reconcile(&mut fixture.host, &changed),
        Err(SearchError::ProfileMismatch)
    );
    index
        .rebuild(&mut fixture.host, fixture.profile.clone(), chunks())
        .unwrap();
    let changed = fixture.ticket(RetrievalQuery::new("tea"));
    let status = index.reconcile(&mut fixture.host, &changed).unwrap();
    assert_eq!(status.missing_chunks, 1);
    assert!(status.reasons.contains(&SearchReason::ProfileMismatch));
}

#[test]
fn authority_update_without_notification_invalidates_old_vectors_and_late_result() {
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    let record = ticket.candidates()[0].candidate_ref().record().unwrap();
    fixture
        .host
        .service
        .for_pet(&fixture.owner)
        .update(
            record.id(),
            record.revision(),
            draft("coffee", &fixture.source),
            101,
        )
        .unwrap();
    assert!(matches!(
        index.commit(
            &mut fixture.host,
            &ticket,
            CompletedIndexBatch {
                batch,
                vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()]
            }
        ),
        Err(SearchError::Core(MemoryError::TaskCancelled))
    ));
    fixture.host.now = 101;
    let current = fixture.ticket(RetrievalQuery::new("tea"));
    let config = fixture.config(0.8);
    let result = hybrid_rank(&mut index, &mut fixture.host, &current, None, &config).unwrap();
    assert!(result.ranking().is_empty());
    assert_eq!(result.status().missing_chunks, 1);
}

#[test]
fn revoked_source_is_not_a_model_fallback_and_never_receives_late_vectors() {
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    fixture.host.service.revoke_source(&fixture.source).unwrap();
    assert!(matches!(
        index.commit(
            &mut fixture.host,
            &ticket,
            CompletedIndexBatch {
                batch,
                vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()]
            }
        ),
        Err(SearchError::Core(_))
    ));
    let config = fixture.config(0.8);
    assert!(matches!(
        hybrid_rank(&mut index, &mut fixture.host, &ticket, None, &config),
        Err(SearchError::Core(_))
    ));
}

#[test]
fn empty_query_keeps_existing_list_order_and_never_requires_a_vector() {
    let mut fixture = Fixture::new(&["tea", "coffee"]);
    let query = RetrievalQuery::new("").with_limit(1);
    let expected = fixture
        .host
        .service
        .for_session(
            &fixture.owner,
            &SessionId::new("session").unwrap(),
            ReadPurpose::OfflineReasoning,
        )
        .retrieve(&query, 100)
        .unwrap();
    let ticket = fixture.ticket(query);
    let mut index = fixture.index();
    let config = HybridConfig::new(
        fixture.profile.clone(),
        1,
        0.8,
        false,
        RerankMode::Disabled,
        20,
    )
    .unwrap();
    let result = hybrid_rank(&mut index, &mut fixture.host, &ticket, None, &config).unwrap();
    assert_eq!(
        ticket.candidate(&result.ranking()[0].0).unwrap(),
        &expected.items()[0]
    );
    assert_eq!(result.mode(), SearchMode::KeywordOnly);
}

#[test]
fn strict_vector_mode_and_wrong_profile_do_not_claim_keyword_success() {
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let config = HybridConfig::new(
        fixture.profile.clone(),
        1,
        0.8,
        false,
        RerankMode::Disabled,
        20,
    )
    .unwrap();
    assert!(hybrid_rank(&mut index, &mut fixture.host, &ticket, None, &config).is_err());
    assert!(
        hybrid_rank(
            &mut index,
            &mut fixture.host,
            &ticket,
            Some(&encoded(&ticket, vec![1.0, 0.0])),
            &config
        )
        .is_err()
    );
    let wrong = EncodedQuery {
        profile: EmbeddingProfile::new("fixture", "wrong-model", 2).unwrap(),
        text: "tea".into(),
        vector: UnitVector::new(vec![1.0, 0.0], 2).unwrap(),
    };
    let config = fixture.config(0.8);
    assert!(matches!(
        hybrid_rank(
            &mut index,
            &mut fixture.host,
            &ticket,
            Some(&wrong),
            &config
        ),
        Err(SearchError::ProfileMismatch)
    ));
}

static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);
struct TemporaryDirectory(PathBuf);
impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "moe-index-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        for file in [
            "moe-memory-vectors.cache",
            "moe-memory-vectors.cache.pending",
            "journal.guard",
            "memory.journal",
        ] {
            let _ = fs::remove_file(self.0.join(file));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

#[test]
fn optional_cache_roundtrip_rebinds_only_current_inputs_without_plaintext() {
    let directory = TemporaryDirectory::new();
    let mut fixture = Fixture::new(&["秘密茉莉花茶"]);
    let ticket = fixture.ticket(RetrievalQuery::new("饮品"));
    let mut index = fixture.index();
    assert_eq!(
        index.save_file_cache(&mut fixture.host, &ticket),
        Err(SearchError::CacheUnavailable)
    );
    fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
    index.enable_file_cache(&directory.0, 1_000_000).unwrap();
    index.save_file_cache(&mut fixture.host, &ticket).unwrap();
    let bytes = fs::read(directory.0.join("moe-memory-vectors.cache")).unwrap();
    for text in ["秘密茉莉花茶", "饮品", "session", "synthetic"] {
        assert!(
            !bytes
                .windows(text.len())
                .any(|window| window == text.as_bytes())
        );
    }
    let current = fixture.ticket(RetrievalQuery::new("饮品"));
    let mut restored = fixture.index();
    restored.enable_file_cache(&directory.0, 1_000_000).unwrap();
    assert_eq!(
        restored
            .load_file_cache(&mut fixture.host, &current)
            .unwrap()
            .missing_chunks,
        0
    );
    let config = fixture.config(0.8);
    let result = hybrid_rank(
        &mut restored,
        &mut fixture.host,
        &current,
        Some(&encoded(&current, vec![1.0, 0.0])),
        &config,
    )
    .unwrap();
    assert_eq!(result.vector_hits(), 1);
    assert!(current.candidate(&result.ranking()[0].0).is_some());
    assert!(ticket.candidate(&result.ranking()[0].0).is_none());
}

#[test]
fn corrupt_missing_cache_and_clear_never_touch_authoritative_marker() {
    let directory = TemporaryDirectory::new();
    fs::write(
        directory.0.join("journal.guard"),
        b"authoritative-synthetic-marker",
    )
    .unwrap();
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    index.enable_file_cache(&directory.0, 4096).unwrap();
    assert!(
        index
            .load_file_cache(&mut fixture.host, &ticket)
            .unwrap()
            .reasons
            .contains(&SearchReason::CacheUnavailable)
    );
    fs::write(
        directory.0.join("moe-memory-vectors.cache"),
        b"bad-snapshot",
    )
    .unwrap();
    let status = index.load_file_cache(&mut fixture.host, &ticket).unwrap();
    assert!(status.reasons.contains(&SearchReason::CacheUnavailable));
    let config = fixture.config(0.8);
    assert_eq!(
        hybrid_rank(&mut index, &mut fixture.host, &ticket, None, &config)
            .unwrap()
            .lexical_hits(),
        1
    );
    index.clear_file_cache().unwrap();
    assert_eq!(
        fs::read(directory.0.join("journal.guard")).unwrap(),
        b"authoritative-synthetic-marker"
    );
    assert_eq!(
        fixture
            .host
            .service
            .for_session(
                &fixture.owner,
                &SessionId::new("session").unwrap(),
                ReadPurpose::OfflineReasoning
            )
            .retrieve(&RetrievalQuery::new("tea"), 100)
            .unwrap()
            .items()
            .len(),
        1
    );
}

#[test]
fn cache_rejects_cross_domain_and_same_dimension_different_model() {
    let directory = TemporaryDirectory::new();
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea").with_scope(SearchScope::Private));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
    index.enable_file_cache(&directory.0, 1_000_000).unwrap();
    index.save_file_cache(&mut fixture.host, &ticket).unwrap();
    let mut other = SearchIndex::new(
        StorageDomain::new("trusted-root-b").unwrap(),
        fixture.profile.clone(),
        chunks(),
        1,
        1,
        10,
    )
    .unwrap();
    other.enable_file_cache(&directory.0, 1_000_000).unwrap();
    let status = other.load_file_cache(&mut fixture.host, &ticket).unwrap();
    assert!(status.reasons.contains(&SearchReason::CacheUnavailable));
    assert_eq!(status.missing_chunks, 1);
    fixture.profile = EmbeddingProfile::new("fixture", "new-model", 2).unwrap();
    index
        .rebuild(&mut fixture.host, fixture.profile.clone(), chunks())
        .unwrap();
    let changed = fixture.ticket(RetrievalQuery::new("tea"));
    let status = index.load_file_cache(&mut fixture.host, &changed).unwrap();
    assert!(status.reasons.contains(&SearchReason::ProfileMismatch));
    assert_eq!(status.missing_chunks, 1);
}

#[test]
fn cache_load_after_authority_update_does_not_reuse_old_body_version() {
    let directory = TemporaryDirectory::new();
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
    index.enable_file_cache(&directory.0, 1_000_000).unwrap();
    index.save_file_cache(&mut fixture.host, &ticket).unwrap();
    let record = ticket.candidates()[0].candidate_ref().record().unwrap();
    fixture
        .host
        .service
        .for_pet(&fixture.owner)
        .update(
            record.id(),
            record.revision(),
            draft("coffee", &fixture.source),
            101,
        )
        .unwrap();
    fixture.host.now = 101;
    let current = fixture.ticket(RetrievalQuery::new("tea"));
    let status = index.load_file_cache(&mut fixture.host, &current).unwrap();
    assert_eq!(status.missing_chunks, 1);
    let config = fixture.config(0.8);
    let result = hybrid_rank(
        &mut index,
        &mut fixture.host,
        &current,
        Some(&encoded(&current, vec![1.0, 0.0])),
        &config,
    )
    .unwrap();
    assert!(result.ranking().is_empty());
}

#[test]
fn vector_capacity_failure_is_visible_without_changing_authoritative_records() {
    let mut fixture = Fixture::new(&["tea", "coffee"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = SearchIndex::new(
        StorageDomain::new("root").unwrap(),
        fixture.profile.clone(),
        chunks(),
        2,
        32,
        1,
    )
    .unwrap();
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    assert!(
        index
            .commit(
                &mut fixture.host,
                &ticket,
                CompletedIndexBatch {
                    batch,
                    vectors: vec![
                        UnitVector::new(vec![1.0, 0.0], 2).unwrap(),
                        UnitVector::new(vec![0.0, 1.0], 2).unwrap()
                    ]
                }
            )
            .is_err()
    );
    let status = index.reconcile(&mut fixture.host, &ticket).unwrap();
    assert_eq!(status.missing_chunks, 2);
    assert_eq!(status.candidates, 2);
}

#[test]
fn cache_of_same_numeric_id_in_another_pet_cannot_supply_a_vector() {
    let directory = TemporaryDirectory::new();
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("drink"));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
    index.enable_file_cache(&directory.0, 1_000_000).unwrap();
    index.save_file_cache(&mut fixture.host, &ticket).unwrap();
    let mut other = Fixture::new(&[]);
    other.owner = PetIdentity::new(fixture.owner.namespace().clone(), "bob").unwrap();
    other.source = other
        .host
        .service
        .register_source(
            MemoryScope::Private(other.owner.clone()),
            "bob",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    other
        .host
        .service
        .for_pet(&other.owner)
        .create(draft("tea", &other.source), 100)
        .unwrap();
    other
        .host
        .service
        .set_read_policy(
            &other.owner,
            ReadPolicy::offline(1, vec![other.source.clone()]),
        )
        .unwrap();
    let current = other.ticket(RetrievalQuery::new("drink"));
    assert_eq!(
        ticket.candidates()[0]
            .candidate_ref()
            .record()
            .unwrap()
            .id(),
        current.candidates()[0]
            .candidate_ref()
            .record()
            .unwrap()
            .id()
    );
    let mut restored = other.index();
    restored.enable_file_cache(&directory.0, 1_000_000).unwrap();
    assert_eq!(
        restored
            .load_file_cache(&mut other.host, &current)
            .unwrap()
            .missing_chunks,
        1
    );
}

fn replace_cache_payload(directory: &TemporaryDirectory, value: &serde_json::Value) {
    use sha2::{Digest, Sha256};
    let body = serde_json::to_vec(value).unwrap();
    let mut bytes = b"MOEIDX\0\x01".to_vec();
    bytes.extend_from_slice(&(body.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&Sha256::digest(&body));
    fs::write(directory.0.join("moe-memory-vectors.cache"), bytes).unwrap();
}

#[test]
fn cache_validates_checksum_duplicate_keys_dimensions_norm_and_file_limit() {
    let directory = TemporaryDirectory::new();
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
    index.enable_file_cache(&directory.0, 4096).unwrap();
    index.save_file_cache(&mut fixture.host, &ticket).unwrap();
    let path = directory.0.join("moe-memory-vectors.cache");
    let original = fs::read(&path).unwrap();
    let original_value: serde_json::Value =
        serde_json::from_slice(&original[16..original.len() - 32]).unwrap();
    for invalid in [
        serde_json::json!([1.0]),
        serde_json::json!([0.0, 0.0]),
        serde_json::json!([2.0, 0.0]),
        serde_json::json!([null, 1.0]),
        serde_json::json!([1e308, 1e308]),
    ] {
        let mut value = original_value.clone();
        value["entries"][0]["vector"] = invalid;
        replace_cache_payload(&directory, &value);
        let mut restored = fixture.index();
        restored.enable_file_cache(&directory.0, 4096).unwrap();
        let status = restored
            .load_file_cache(&mut fixture.host, &ticket)
            .unwrap();
        assert!(status.reasons.contains(&SearchReason::CacheUnavailable));
        assert_eq!(status.missing_chunks, 1);
    }
    let mut duplicate = original_value.clone();
    duplicate["entries"]
        .as_array_mut()
        .unwrap()
        .push(original_value["entries"][0].clone());
    replace_cache_payload(&directory, &duplicate);
    assert!(
        index
            .load_file_cache(&mut fixture.host, &ticket)
            .unwrap()
            .reasons
            .contains(&SearchReason::CacheUnavailable)
    );
    let mut bad_checksum = original.clone();
    bad_checksum[16] ^= 1;
    fs::write(&path, bad_checksum).unwrap();
    assert!(
        index
            .load_file_cache(&mut fixture.host, &ticket)
            .unwrap()
            .reasons
            .contains(&SearchReason::CacheUnavailable)
    );
    fs::write(&path, vec![0; 4097]).unwrap();
    assert!(
        index
            .load_file_cache(&mut fixture.host, &ticket)
            .unwrap()
            .reasons
            .contains(&SearchReason::CacheUnavailable)
    );
}

#[test]
fn failed_snapshot_replacement_preserves_old_cache_and_never_removes_foreign_pending() {
    let directory = TemporaryDirectory::new();
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
    index.enable_file_cache(&directory.0, 4096).unwrap();
    index.save_file_cache(&mut fixture.host, &ticket).unwrap();
    let path = directory.0.join("moe-memory-vectors.cache");
    let original = fs::read(&path).unwrap();
    fs::write(
        directory.0.join("moe-memory-vectors.cache.pending"),
        b"another-writer",
    )
    .unwrap();
    assert_eq!(
        index.save_file_cache(&mut fixture.host, &ticket),
        Err(SearchError::CacheUnavailable)
    );
    assert_eq!(fs::read(path).unwrap(), original);
    assert_eq!(
        fs::read(directory.0.join("moe-memory-vectors.cache.pending")).unwrap(),
        b"another-writer"
    );
}

#[test]
fn expiry_at_equality_hides_record_then_removes_active_vectors_and_jobs() {
    let mut fixture = Fixture::new(&[]);
    fixture
        .host
        .service
        .for_pet(&fixture.owner)
        .create(draft("tea", &fixture.source).with_expiration(101), 100)
        .unwrap();
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
    fixture.host.now = 101;
    let current = fixture.ticket(RetrievalQuery::new("tea"));
    let status = index.reconcile(&mut fixture.host, &current).unwrap();
    assert_eq!(status.candidates, 0);
    assert_eq!(status.missing_chunks, 0);
    assert!(index.entries.is_empty());
}

#[test]
fn calibration_change_cancels_previous_tasks_then_requires_a_fresh_ticket() {
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let config = fixture.config(0.7);
    let ranking = hybrid_rank(&mut index, &mut fixture.host, &ticket, None, &config).unwrap();
    let task = fixture
        .host
        .service
        .for_session(
            &fixture.owner,
            ticket.session(),
            ReadPurpose::OfflineReasoning,
        )
        .complete_retrieval(&ticket, ranking.ranking(), 100)
        .unwrap();
    let active = fixture.ticket(RetrievalQuery::new("tea"));
    let changed = HybridConfig::new(
        fixture.profile.clone(),
        2,
        0.8,
        true,
        RerankMode::Disabled,
        20,
    )
    .unwrap();
    assert!(matches!(
        hybrid_rank(&mut index, &mut fixture.host, &active, None, &changed),
        Err(SearchError::Core(MemoryError::TaskCancelled))
    ));
    assert_eq!(
        fixture.host.service.validate_task(&task, 100),
        Err(MemoryError::TaskCancelled)
    );
    let current = fixture.ticket(RetrievalQuery::new("tea"));
    assert_eq!(
        hybrid_rank(&mut index, &mut fixture.host, &current, None, &changed)
            .unwrap()
            .lexical_hits(),
        1
    );
}

#[test]
fn working_and_persistent_records_share_one_budget_without_truncation() {
    let mut fixture = Fixture::new(&["tea long"]);
    let session = SessionId::new("session").unwrap();
    fixture
        .host
        .service
        .put_working_context(
            &fixture.owner,
            &session,
            "active",
            1,
            MemoryDraft::new(
                "tea",
                Category::WorkingContext,
                Epistemic::UserStatement,
                vec![fixture.source.clone()],
            )
            .with_lifecycle(moe_memory_core::Lifecycle::ShortTerm),
            100,
        )
        .unwrap();
    let ticket = fixture.ticket(RetrievalQuery::new("tea").with_limit(2).with_char_budget(3));
    let mut index = fixture.index();
    let config = fixture.config(0.8);
    let ranking = hybrid_rank(&mut index, &mut fixture.host, &ticket, None, &config).unwrap();
    assert_eq!(ranking.used_chars(), 3);
    assert_eq!(ranking.omitted_for_budget(), 1);
    assert_eq!(ranking.status().missing_chunks, 1);
    let task = fixture
        .host
        .service
        .for_session(&fixture.owner, &session, ReadPurpose::OfflineReasoning)
        .complete_retrieval(&ticket, ranking.ranking(), 100)
        .unwrap();
    assert_eq!(task.result().items().len(), 1);
    assert_eq!(task.result().items()[0].text(), "tea");
}

struct Journal {
    directory: TemporaryDirectory,
    local: LocalMemory,
    owner: PetIdentity,
    source: SourceRef,
    session: SessionId,
    profile: EmbeddingProfile,
}

impl Journal {
    fn new(persona: Option<&str>, texts: &[(&str, Option<u64>)]) -> Self {
        let directory = TemporaryDirectory::new();
        let mut local = LocalMemory::create(directory.0.join("memory.journal")).unwrap();
        let owner =
            PetIdentity::new(Namespace::new("facade-tests", "user").unwrap(), "alice").unwrap();
        let source = local
            .register_source(
                OperationId::new("register-source").unwrap(),
                MemoryScope::Private(owner.clone()),
                "synthetic",
                1,
                Epistemic::UserStatement,
            )
            .unwrap()
            .value()
            .clone();
        let authority = local
            .register_source(
                OperationId::new("register-authority").unwrap(),
                MemoryScope::Private(owner.clone()),
                "synthetic-persona",
                1,
                Epistemic::AuthoritativeState,
            )
            .unwrap()
            .value()
            .clone();
        for (position, (text, expiration)) in texts.iter().enumerate() {
            let mut record = draft(text, &source);
            if let Some(expiration) = expiration {
                record = record.with_expiration(*expiration);
            }
            local
                .for_pet(&owner)
                .create(
                    OperationId::new(format!("create-{position}")).unwrap(),
                    record,
                    100,
                )
                .unwrap();
        }
        if let Some(persona) = persona {
            local
                .set_identity_view(&owner, 1, persona, vec![authority.clone()])
                .unwrap();
        }
        local
            .set_read_policy(
                &owner,
                ReadPolicy::offline(1, vec![source.clone(), authority]),
            )
            .unwrap();
        Self {
            directory,
            local,
            owner,
            source,
            session: SessionId::new("session").unwrap(),
            profile: EmbeddingProfile::new("fixture", "model", 2).unwrap(),
        }
    }
}

/// Arms one sealed keyword-only hybrid ranking through the LocalMemory bridge.
/// Manual vectors stay in the dedicated vector test below.
async fn arm_keyword_ranking(
    local: &mut LocalMemory,
    profile: &EmbeddingProfile,
    owner: &PetIdentity,
    session: &SessionId,
    query: &RetrievalQuery,
) -> crate::RankedCandidates {
    let ticket = local
        .for_session(owner, session, ReadPurpose::OfflineReasoning)
        .begin_candidates(query, RetrievalPlan::new(profile.clone()), 100)
        .unwrap();
    let mut index = SearchIndex::new(
        StorageDomain::new("facade-tests-root").unwrap(),
        profile.clone(),
        chunks(),
        256,
        32,
        4096,
    )
    .unwrap();
    let config =
        HybridConfig::new(profile.clone(), 1, 0.8, true, RerankMode::Disabled, 20).unwrap();
    let mut host = LocalSearchHost::new(local, 100);
    let ranking = hybrid_rank(&mut index, &mut host, &ticket, None, &config).unwrap();
    let control = CallControl::new(Cancellation::default(), Duration::from_secs(1)).unwrap();
    rerank_or_fallback(&index, &mut host, ticket, ranking, None, None, &control)
        .await
        .unwrap()
}

#[test]
fn search_error_equality_keeps_original_variants_and_persistence_never_degrades() {
    use std::io;
    assert_eq!(
        SearchError::Core(MemoryError::TaskCancelled),
        SearchError::Core(MemoryError::TaskCancelled)
    );
    assert_ne!(
        SearchError::Core(MemoryError::TaskCancelled),
        SearchError::Persistence(PersistenceError::Memory(MemoryError::TaskCancelled))
    );
    assert_eq!(
        SearchError::Persistence(PersistenceError::Io(io::Error::from(
            io::ErrorKind::NotFound
        ))),
        SearchError::Persistence(PersistenceError::Io(io::Error::from(
            io::ErrorKind::NotFound
        )))
    );
    assert_eq!(
        SearchError::Persistence(PersistenceError::RecoveryRequired),
        SearchError::Persistence(PersistenceError::RecoveryRequired)
    );
    assert_ne!(
        SearchError::Persistence(PersistenceError::RecoveryRequired),
        SearchError::Persistence(PersistenceError::OperationConflict)
    );
    assert_ne!(SearchError::HttpStatus(429), SearchError::HttpStatus(500));
    for error in [
        SearchError::Persistence(PersistenceError::RecoveryRequired),
        SearchError::Persistence(PersistenceError::Io(io::Error::from(
            io::ErrorKind::PermissionDenied,
        ))),
        SearchError::Core(MemoryError::TaskCancelled),
        SearchError::Core(MemoryError::CapacityExceeded),
    ] {
        assert!(!fallback_eligible(&error), "{error:?} must never degrade");
        assert!(
            !transmission_denied(&error),
            "{error:?} is not pre-send denial"
        );
    }
    assert!(transmission_denied(&SearchError::Core(
        MemoryError::TransmissionDenied
    )));
    assert!(transmission_denied(&SearchError::Persistence(
        PersistenceError::Memory(MemoryError::TransmissionDenied)
    )));
}

#[tokio::test]
async fn facade_delivers_sealed_hybrid_ranking_once_into_protected_context() {
    let mut journal = Journal::new(
        Some("synthetic protected persona"),
        &[("秘密茉莉花茶偏好", None)],
    );
    for position in 0..8 {
        journal
            .local
            .for_pet(&journal.owner)
            .create(
                OperationId::new(format!("filler-{position}")).unwrap(),
                draft(&format!("unrelated {position}"), &journal.source),
                100,
            )
            .unwrap();
    }
    let query = RetrievalQuery::new("偏爱的饮品").with_limit(1);
    let ticket = journal
        .local
        .for_session(
            &journal.owner,
            &journal.session,
            ReadPurpose::OfflineReasoning,
        )
        .begin_candidates(&query, RetrievalPlan::new(journal.profile.clone()), 100)
        .unwrap();
    let mut index = SearchIndex::new(
        StorageDomain::new("facade-vector-root").unwrap(),
        journal.profile.clone(),
        chunks(),
        256,
        32,
        4096,
    )
    .unwrap();
    {
        let mut host = LocalSearchHost::new(&mut journal.local, 100);
        while let Some(batch) = index.take_batch(&mut host, &ticket).unwrap() {
            let vectors = batch
                .slices
                .iter()
                .map(|slice| {
                    UnitVector::new(
                        if slice.chunk.text() == "秘密茉莉花茶偏好" {
                            vec![1.0, 0.0]
                        } else {
                            vec![0.0, 1.0]
                        },
                        2,
                    )
                    .unwrap()
                })
                .collect();
            index
                .commit(&mut host, &ticket, CompletedIndexBatch { batch, vectors })
                .unwrap();
        }
    }
    let config = HybridConfig::new(
        journal.profile.clone(),
        1,
        0.9,
        true,
        RerankMode::Disabled,
        20,
    )
    .unwrap();
    let encoded_query = EncodedQuery {
        profile: journal.profile.clone(),
        text: query.text().to_owned(),
        vector: UnitVector::new(vec![1.0, 0.0], 2).unwrap(),
    };
    let control = CallControl::new(Cancellation::default(), Duration::from_secs(1)).unwrap();
    let mut host = LocalSearchHost::new(&mut journal.local, 100);
    let ranking = hybrid_rank(
        &mut index,
        &mut host,
        &ticket,
        Some(&encoded_query),
        &config,
    )
    .unwrap();
    let ranked = rerank_or_fallback(
        &index,
        &mut host,
        ticket,
        ranking,
        Some(&encoded_query),
        None,
        &control,
    )
    .await
    .unwrap();
    let _ = host;

    let Journal {
        local,
        owner,
        session,
        ..
    } = journal;
    let mut facade = SearchMemory::with_ranked(local, ranked);
    let other_session = SessionId::new("other-session").unwrap();
    {
        let mut reader = facade.for_session(&owner, &other_session, ReadPurpose::OfflineReasoning);
        assert!(matches!(
            reader.begin_task(&query, 100),
            Err(SearchError::InvalidInput(
                "request does not match the prepared hybrid retrieval"
            ))
        ));
        assert_eq!(
            reader.retrieve(&query, 100),
            Err(SearchError::InvalidInput(
                "hybrid mode delivers only the sealed ranking"
            ))
        );
    }
    let mut reader = facade.for_session(&owner, &session, ReadPurpose::OfflineReasoning);
    let search_task = reader.begin_task(&query, 100).unwrap();
    let diagnostic = search_task.diagnostic().unwrap();
    assert_eq!(diagnostic.mode(), SearchMode::Hybrid);
    assert_eq!(diagnostic.vector_hits(), 1);
    assert_eq!(diagnostic.status().candidates, 9);
    let diagnostic_text = format!("{diagnostic:?}");
    assert!(!diagnostic_text.contains("秘密茉莉花茶偏好"));
    assert!(!diagnostic_text.contains("偏爱的饮品"));
    let task = search_task.into_task();
    assert_eq!(task.result().items()[0].text(), "秘密茉莉花茶偏好");
    assert_eq!(
        task.identity_view().unwrap().text(),
        "synthetic protected persona"
    );
    assert!(matches!(
        reader.begin_task(&query, 100),
        Err(SearchError::InvalidInput(
            "prepared hybrid retrieval was already consumed"
        ))
    ));
    assert!(matches!(
        reader.retrieve(&query, 100),
        Err(SearchError::InvalidInput(
            "hybrid mode delivers only the sealed ranking"
        ))
    ));
    drop(reader);
    let context = facade.assemble_context(&task, 4096, 100).unwrap();
    assert!(facade.context_view(&context, 100).is_ok());
    let view = facade.context_view(&context, 100).unwrap();
    assert_eq!(
        view.identity().unwrap().text(),
        "synthetic protected persona"
    );
    assert!(
        view.blocks()
            .iter()
            .any(|block| block.text() == "秘密茉莉花茶偏好")
    );
}

#[test]
fn facade_keyword_default_matches_local_memory_reader() {
    let mut journal = Journal::new(
        Some("synthetic protected persona"),
        &[("tea before breakfast", None)],
    );
    let query = RetrievalQuery::new("tea");
    let expected = journal
        .local
        .for_session(
            &journal.owner,
            &journal.session,
            ReadPurpose::OfflineReasoning,
        )
        .retrieve(&query, 100)
        .unwrap();
    let Journal {
        local,
        owner,
        session,
        ..
    } = journal;
    let mut facade = SearchMemory::new(local);
    let mut reader = facade.for_session(&owner, &session, ReadPurpose::OfflineReasoning);
    let search_task = reader.begin_task(&query, 100).unwrap();
    assert!(search_task.diagnostic().is_none());
    assert_eq!(
        search_task.task().result().items().len(),
        expected.items().len()
    );
    assert_eq!(
        search_task.task().result().items()[0].text(),
        "tea before breakfast"
    );
    assert_eq!(
        reader.retrieve(&query, 100).unwrap().items()[0].text(),
        "tea before breakfast"
    );
    assert_eq!(
        reader.identity().unwrap().text(),
        "synthetic protected persona"
    );
    let task = facade
        .begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100)
        .unwrap();
    let context = facade.assemble_context(task.task(), 2048, 100).unwrap();
    let view = facade.context_view(&context, 100).unwrap();
    assert_eq!(
        view.identity().unwrap().text(),
        "synthetic protected persona"
    );
    assert!(facade.into_memory().generation().is_ok());
}

#[tokio::test]
async fn facade_rejects_every_dimension_mismatch_without_keyword_fallback() {
    let mut journal = Journal::new(None, &[("tea", None), ("coffee", None)]);
    let query = RetrievalQuery::new("tea").with_limit(2);
    let ranked = arm_keyword_ranking(
        &mut journal.local,
        &journal.profile,
        &journal.owner,
        &journal.session,
        &query,
    )
    .await;
    let Journal {
        local,
        owner,
        session,
        ..
    } = journal;
    let mut facade = SearchMemory::with_ranked(local, ranked);
    let bob = PetIdentity::new(owner.namespace().clone(), "bob").unwrap();
    let other_session = SessionId::new("other").unwrap();
    for (identity, session, purpose, query) in [
        (
            PetIdentity::new(
                Namespace::new("other-workspace", "user").unwrap(),
                owner.instance_id(),
            )
            .unwrap(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            query.clone(),
        ),
        (
            PetIdentity::new(
                Namespace::new("facade-tests", "other-user").unwrap(),
                owner.instance_id(),
            )
            .unwrap(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            query.clone(),
        ),
        (
            owner.clone(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            RetrievalQuery::new("coffee").with_limit(2),
        ),
        (
            owner.clone(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            query.clone().with_scope(SearchScope::Private),
        ),
        (
            owner.clone(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            query.clone().with_scope(SearchScope::Shared),
        ),
        (
            owner.clone(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            RetrievalQuery::new("tea"),
        ),
        (
            owner.clone(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            query.clone().with_char_budget(128),
        ),
        (
            owner.clone(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            query
                .clone()
                .with_layer(moe_memory_core::RetrievalLayer::LongTerm),
        ),
        (
            owner.clone(),
            session.clone(),
            ReadPurpose::UserInspection,
            query.clone(),
        ),
        (
            owner.clone(),
            other_session.clone(),
            ReadPurpose::OfflineReasoning,
            query.clone(),
        ),
        (
            bob.clone(),
            session.clone(),
            ReadPurpose::OfflineReasoning,
            query.clone(),
        ),
    ] {
        assert!(
            matches!(
                facade.begin_task(&identity, &session, purpose, &query, 100),
                Err(SearchError::InvalidInput(
                    "request does not match the prepared hybrid retrieval"
                ))
            ),
            "mismatched request must never fall back to keywords"
        );
    }
    let task = facade
        .begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100)
        .unwrap();
    assert_eq!(task.task().result().items().len(), 1);
    assert_eq!(task.task().result().items()[0].text(), "tea");
    assert!(matches!(
        facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100),
        Err(SearchError::InvalidInput(
            "prepared hybrid retrieval was already consumed"
        ))
    ));
}

#[tokio::test]
async fn revoked_source_and_cancelled_tasks_never_degrade_armed_facade() {
    let mut journal = Journal::new(None, &[("tea", None)]);
    let query = RetrievalQuery::new("tea");
    let ranked = arm_keyword_ranking(
        &mut journal.local,
        &journal.profile,
        &journal.owner,
        &journal.session,
        &query,
    )
    .await;
    journal
        .local
        .revoke_source(OperationId::new("revoke").unwrap(), &journal.source)
        .unwrap();
    let Journal {
        local,
        owner,
        session,
        ..
    } = journal;
    let mut facade = SearchMemory::with_ranked(local, ranked);
    assert!(matches!(
        facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100),
        Err(SearchError::Persistence(PersistenceError::Memory(
            MemoryError::TaskCancelled
        )))
    ));
    assert!(matches!(
        facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100),
        Err(SearchError::InvalidInput(
            "prepared hybrid retrieval was already consumed"
        ))
    ));

    let mut journal = Journal::new(None, &[("tea", None)]);
    let query = RetrievalQuery::new("tea");
    let ranked = arm_keyword_ranking(
        &mut journal.local,
        &journal.profile,
        &journal.owner,
        &journal.session,
        &query,
    )
    .await;
    let generation = journal.local.generation().unwrap();
    journal
        .local
        .cancel_tasks(OperationId::new("cancel").unwrap(), generation)
        .unwrap();
    let Journal {
        local,
        owner,
        session,
        ..
    } = journal;
    let mut facade = SearchMemory::with_ranked(local, ranked);
    assert!(matches!(
        facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100),
        Err(SearchError::Persistence(PersistenceError::Memory(
            MemoryError::TaskCancelled
        )))
    ));
    assert!(matches!(
        facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100),
        Err(SearchError::InvalidInput(
            "prepared hybrid retrieval was already consumed"
        ))
    ));
}

#[tokio::test]
async fn expiry_and_reopen_keep_armed_facade_delivery_closed() {
    let mut journal = Journal::new(None, &[("tea", Some(101))]);
    let query = RetrievalQuery::new("tea");
    let ranked = arm_keyword_ranking(
        &mut journal.local,
        &journal.profile,
        &journal.owner,
        &journal.session,
        &query,
    )
    .await;
    let Journal {
        local,
        owner,
        session,
        ..
    } = journal;
    let mut facade = SearchMemory::with_ranked(local, ranked);
    assert!(matches!(
        facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 101),
        Err(SearchError::Persistence(PersistenceError::Memory(
            MemoryError::TaskCancelled
        )))
    ));

    let mut journal = Journal::new(None, &[("tea", None)]);
    let query = RetrievalQuery::new("tea");
    let ranked = arm_keyword_ranking(
        &mut journal.local,
        &journal.profile,
        &journal.owner,
        &journal.session,
        &query,
    )
    .await;
    let Journal {
        directory,
        local,
        owner,
        session,
        ..
    } = journal;
    drop(local);
    let reopened = LocalMemory::open(directory.0.join("memory.journal")).unwrap();
    let mut facade = SearchMemory::with_ranked(reopened, ranked);
    assert!(matches!(
        facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100),
        Err(SearchError::Persistence(PersistenceError::Memory(
            MemoryError::TaskCancelled
        )))
    ));
}

#[tokio::test]
async fn protected_budget_shortfall_stays_a_facade_error() {
    let persona = "synthetic persona ".repeat(20);
    let mut journal = Journal::new(Some(&persona), &[("tea", None)]);
    let query = RetrievalQuery::new("tea");
    let ranked = arm_keyword_ranking(
        &mut journal.local,
        &journal.profile,
        &journal.owner,
        &journal.session,
        &query,
    )
    .await;
    let Journal {
        local,
        owner,
        session,
        ..
    } = journal;
    let mut facade = SearchMemory::with_ranked(local, ranked);
    let task = facade
        .begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100)
        .unwrap();
    assert!(matches!(
        facade.assemble_context(task.task(), 64, 100),
        Err(SearchError::Persistence(PersistenceError::Memory(
            MemoryError::ContextBudgetExceeded { .. }
        )))
    ));
    let context = facade.assemble_context(task.task(), 4096, 100).unwrap();
    assert!(facade.context_view(&context, 100).is_ok());
}

#[test]
fn batch_lease_is_deleted_by_completion_and_committed_work_leaves_the_queue() {
    let mut fixture = Fixture::new(&["tea", "coffee"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    assert_eq!(batch.len(), 2);
    assert!(matches!(
        index.take_batch(&mut fixture.host, &ticket),
        Err(SearchError::InvalidInput("index batch already in flight"))
    ));
    let vectors = (0..batch.len())
        .map(|_| UnitVector::new(vec![1.0, 0.0], 2).unwrap())
        .collect();
    index
        .commit(
            &mut fixture.host,
            &ticket,
            CompletedIndexBatch { batch, vectors },
        )
        .unwrap();
    assert!(
        index
            .take_batch(&mut fixture.host, &ticket)
            .unwrap()
            .is_none()
    );
    let status = index.reconcile(&mut fixture.host, &ticket).unwrap();
    assert_eq!(status.missing_chunks, 0);
    assert_eq!(status.indexed_records, 2);
    assert_eq!(status.queued_records, 0);
    assert_eq!(index.entries.len(), 2);
}

#[test]
fn cancelled_tasks_reject_the_leased_batch_and_never_activate_its_vectors() {
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    fixture
        .host
        .service
        .cancel_tasks(fixture.host.service.generation())
        .unwrap();
    assert_eq!(
        index.commit(
            &mut fixture.host,
            &ticket,
            CompletedIndexBatch {
                batch,
                vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()],
            },
        ),
        Err(SearchError::Core(MemoryError::TaskCancelled))
    );
    assert!(index.entries.is_empty());
    assert!(matches!(
        index.take_batch(&mut fixture.host, &ticket),
        Err(SearchError::Core(MemoryError::TaskCancelled))
    ));
    let fresh = fixture.ticket(RetrievalQuery::new("tea"));
    let batch = index
        .take_batch(&mut fixture.host, &fresh)
        .unwrap()
        .unwrap();
    index
        .commit(
            &mut fixture.host,
            &fresh,
            CompletedIndexBatch {
                batch,
                vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()],
            },
        )
        .unwrap();
    assert_eq!(index.entries.len(), 1);
}

#[test]
fn foreign_identity_cannot_commit_or_activate_a_leased_batch() {
    let mut fixture = Fixture::new(&["tea"]);
    let bob = PetIdentity::new(fixture.owner.namespace().clone(), "bob").unwrap();
    let bob_source = fixture
        .host
        .service
        .register_source(
            MemoryScope::Private(bob.clone()),
            "bob",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    fixture
        .host
        .service
        .for_pet(&bob)
        .create(draft("tea", &bob_source), 100)
        .unwrap();
    fixture
        .host
        .service
        .set_read_policy(&bob, ReadPolicy::offline(1, vec![bob_source.clone()]))
        .unwrap();
    let ticket = fixture.ticket(RetrievalQuery::new("tea"));
    let mut index = fixture.index();
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    let bob_ticket = fixture
        .host
        .service
        .for_session(
            &bob,
            &SessionId::new("session").unwrap(),
            ReadPurpose::OfflineReasoning,
        )
        .begin_candidates(
            &RetrievalQuery::new("tea"),
            RetrievalPlan::new(fixture.profile.clone()),
            fixture.host.now,
        )
        .unwrap();
    assert_eq!(
        index.commit(
            &mut fixture.host,
            &bob_ticket,
            CompletedIndexBatch {
                batch,
                vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()],
            },
        ),
        Err(SearchError::StaleIndexWork)
    );
    assert!(index.entries.is_empty());
    let bob_status = index.reconcile(&mut fixture.host, &bob_ticket).unwrap();
    assert_eq!(bob_status.missing_chunks, 1);
    assert_eq!(bob_status.indexed_records, 0);
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    index
        .commit(
            &mut fixture.host,
            &ticket,
            CompletedIndexBatch {
                batch,
                vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()],
            },
        )
        .unwrap();
    assert_eq!(index.entries.len(), 1);
    let alice_status = index.reconcile(&mut fixture.host, &ticket).unwrap();
    assert_eq!(alice_status.indexed_records, 1);
}

#[test]
fn reopened_index_rejects_old_completed_batches_and_keeps_new_vectors() {
    let mut fixture = Fixture::new(&["tea"]);
    let ticket = fixture.ticket(RetrievalQuery::new("drink"));
    let mut index = fixture.index();
    let batch = index
        .take_batch(&mut fixture.host, &ticket)
        .unwrap()
        .unwrap();
    let stale_vectors = (0..batch.len())
        .map(|_| UnitVector::new(vec![1.0, 0.0], 2).unwrap())
        .collect();
    drop(index);
    let mut reopened = fixture.index();
    assert_eq!(
        reopened.commit(
            &mut fixture.host,
            &ticket,
            CompletedIndexBatch {
                batch,
                vectors: stale_vectors,
            },
        ),
        Err(SearchError::StaleIndexWork)
    );
    assert!(reopened.entries.is_empty());
    fill(&mut reopened, &mut fixture, &ticket, |_| vec![0.0, 1.0]);
    assert_eq!(reopened.entries.len(), 1);
    let config = fixture.config(0.8);
    let hit = hybrid_rank(
        &mut reopened,
        &mut fixture.host,
        &ticket,
        Some(&encoded(&ticket, vec![0.0, 1.0])),
        &config,
    )
    .unwrap();
    assert_eq!(hit.vector_hits(), 1);
    let stale = hybrid_rank(
        &mut reopened,
        &mut fixture.host,
        &ticket,
        Some(&encoded(&ticket, vec![1.0, 0.0])),
        &config,
    )
    .unwrap();
    assert!(stale.ranking().is_empty());
    assert_eq!(stale.mode(), SearchMode::Hybrid);
}

#[test]
fn working_context_queries_preserve_long_term_vectors_and_queued_records() {
    let mut fixture = Fixture::new(&["tea", "coffee"]);
    let mut index = SearchIndex::new(
        StorageDomain::new("trusted-root-a").unwrap(),
        fixture.profile.clone(),
        chunks(),
        256,
        1,
        4096,
    )
    .unwrap();
    let long_term = fixture.ticket(RetrievalQuery::new("").with_layer(RetrievalLayer::LongTerm));
    let batch = index
        .take_batch(&mut fixture.host, &long_term)
        .unwrap()
        .unwrap();
    index
        .commit(
            &mut fixture.host,
            &long_term,
            CompletedIndexBatch {
                batch,
                vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()],
            },
        )
        .unwrap();
    assert_eq!(index.entries.len(), 1);
    let working =
        fixture.ticket(RetrievalQuery::new("").with_layer(RetrievalLayer::WorkingContext));
    let status = index.reconcile(&mut fixture.host, &working).unwrap();
    assert_eq!(status.candidates, 0);
    assert_eq!(status.indexed_records, 0);
    assert_eq!(status.missing_chunks, 0);
    assert!(
        index
            .take_batch(&mut fixture.host, &working)
            .unwrap()
            .is_none()
    );
    assert_eq!(index.entries.len(), 1);
    fill(&mut index, &mut fixture, &long_term, |_| vec![1.0, 0.0]);
    let status = index.reconcile(&mut fixture.host, &long_term).unwrap();
    assert_eq!(status.indexed_records, 2);
    assert_eq!(status.missing_chunks, 0);
}

#[test]
fn formal_and_ordinary_layers_preserve_each_others_vectors_and_pending_work() {
    let mut fixture = Fixture::new(&["tea", "coffee"]);
    let namespace = fixture.owner.namespace().clone();
    let source = fixture
        .host
        .service
        .register_source(
            MemoryScope::Shared(namespace.clone()),
            "formal",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    fixture
        .host
        .service
        .set_formal_publication_policy(
            &namespace,
            moe_memory_core::PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    for text in ["formal protocol", "formal reference"] {
        let command = fixture
            .host
            .service
            .prepare_formal_publication(
                &namespace,
                MemoryDraft::new(
                    text,
                    Category::Protocol,
                    Epistemic::Reference,
                    vec![source.clone()],
                ),
                100,
            )
            .unwrap();
        command.execute(&mut fixture.host.service).unwrap();
    }
    fixture
        .host
        .service
        .set_read_policy(
            &fixture.owner,
            ReadPolicy::offline(2, vec![fixture.source.clone(), source]),
        )
        .unwrap();
    let mut index = SearchIndex::new(
        StorageDomain::new("trusted-root-a").unwrap(),
        fixture.profile.clone(),
        chunks(),
        256,
        1,
        4096,
    )
    .unwrap();
    let ordinary = fixture.ticket(RetrievalQuery::new("").with_layer(RetrievalLayer::LongTerm));
    let formal = fixture.ticket(RetrievalQuery::new("").with_layer(RetrievalLayer::FormalMaterial));
    for ticket in [&ordinary, &formal] {
        let batch = index
            .take_batch(&mut fixture.host, ticket)
            .unwrap()
            .unwrap();
        index
            .commit(
                &mut fixture.host,
                ticket,
                CompletedIndexBatch {
                    batch,
                    vectors: vec![UnitVector::new(vec![1.0, 0.0], 2).unwrap()],
                },
            )
            .unwrap();
    }
    assert_eq!(index.entries.len(), 2);
    fill(&mut index, &mut fixture, &ordinary, |_| vec![1.0, 0.0]);
    assert_eq!(index.entries.len(), 3);
    fill(&mut index, &mut fixture, &formal, |_| vec![1.0, 0.0]);
    assert_eq!(index.entries.len(), 4);
    for layer in [
        RetrievalLayer::All,
        RetrievalLayer::FormalMaterial,
        RetrievalLayer::LongTerm,
    ] {
        let ticket = fixture.ticket(RetrievalQuery::new("tea").with_layer(layer));
        let config = fixture.config(0.8);
        let ranked = hybrid_rank(
            &mut index,
            &mut fixture.host,
            &ticket,
            Some(&encoded(&ticket, vec![1.0, 0.0])),
            &config,
        )
        .unwrap();
        assert_eq!(ranked.status().indexed_records, 2);
        assert_eq!(ranked.status().missing_chunks, 0);
        assert_eq!(ranked.ranking().len(), 2);
        assert_eq!(index.entries.len(), 4);
        assert!(ranked.ranking().iter().all(|(reference, _)| {
            let moe_memory_core::RetrievalItem::LongTerm(record) =
                ticket.candidate(reference).unwrap()
            else {
                return false;
            };
            record.is_protected() == (layer == RetrievalLayer::FormalMaterial)
        }));
    }
}

#[test]
fn preserved_out_of_layer_vectors_are_still_removed_by_authoritative_reconciliation() {
    for mutation in ["update", "delete", "revoke"] {
        let mut fixture = Fixture::new(&["tea"]);
        let mut index = fixture.index();
        let ticket = fixture.ticket(RetrievalQuery::new("tea"));
        let record = ticket.candidates()[0].candidate_ref().record().unwrap();
        fill(&mut index, &mut fixture, &ticket, |_| vec![1.0, 0.0]);
        match mutation {
            "update" => {
                fixture
                    .host
                    .service
                    .for_pet(&fixture.owner)
                    .update(
                        record.id(),
                        record.revision(),
                        draft("coffee", &fixture.source),
                        101,
                    )
                    .unwrap();
            }
            "delete" => {
                fixture
                    .host
                    .service
                    .for_pet(&fixture.owner)
                    .delete(record.id(), record.revision(), 101)
                    .unwrap();
            }
            "revoke" => {
                fixture.host.service.revoke_source(&fixture.source).unwrap();
            }
            _ => unreachable!(),
        }
        fixture.host.now = 101;
        let working =
            fixture.ticket(RetrievalQuery::new("").with_layer(RetrievalLayer::WorkingContext));
        let config = fixture.config(0.8);
        let ranked = hybrid_rank(
            &mut index,
            &mut fixture.host,
            &working,
            Some(&encoded(&working, vec![1.0, 0.0])),
            &config,
        )
        .unwrap();
        assert!(ranked.ranking().is_empty());
        assert_eq!(index.entries.len(), 1);
        let current =
            fixture.ticket(RetrievalQuery::new("tea").with_layer(RetrievalLayer::LongTerm));
        let status = index.reconcile(&mut fixture.host, &current).unwrap();
        assert!(index.entries.is_empty());
        assert_eq!(status.missing_chunks, usize::from(mutation == "update"));
        if mutation == "update" {
            fill(&mut index, &mut fixture, &current, |_| vec![0.0, 1.0]);
            assert_eq!(index.entries.len(), 1);
            assert!(
                index
                    .entries
                    .values()
                    .all(|stored| stored.input.text() == "coffee")
            );
        }
    }
}
