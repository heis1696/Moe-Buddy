use moe_memory_core::{
    CandidateTicket, Category, EmbeddingProfile, Epistemic, MemoryDraft, MemoryScope,
    MemoryService, Namespace, PetIdentity, ReadPolicy, ReadPurpose, RetrievalPlan, RetrievalQuery,
    SessionId, SourceRef,
};
use moe_memory_search::{
    ChunkConfig, SearchError, SearchHost, SearchIndex, SearchReason, StorageDomain,
};

struct Host {
    service: MemoryService,
}

impl SearchHost for Host {
    fn memory(&mut self) -> &mut MemoryService {
        &mut self.service
    }

    fn now_ms(&mut self) -> u64 {
        100
    }
}

struct Fixture {
    host: Host,
    owner: PetIdentity,
    source: SourceRef,
    profile: EmbeddingProfile,
}

impl Fixture {
    fn new(record_count: usize, char_count: usize) -> Self {
        let mut service = MemoryService::new();
        let owner =
            PetIdentity::new(Namespace::new("index-memory", "user").unwrap(), "alice").unwrap();
        let source = service
            .register_source(
                MemoryScope::Private(owner.clone()),
                "synthetic",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();
        for ordinal in 0..record_count {
            let text: String =
                std::iter::repeat_n(char::from_u32(0x4e00 + ordinal as u32).unwrap(), char_count)
                    .collect();
            service
                .for_pet(&owner)
                .create(
                    MemoryDraft::new(
                        text,
                        Category::Episode,
                        Epistemic::UserStatement,
                        vec![source.clone()],
                    ),
                    100,
                )
                .unwrap();
        }
        service
            .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
            .unwrap();
        Self {
            host: Host { service },
            owner,
            source,
            profile: EmbeddingProfile::new("fixture", "index-memory-model", 2).unwrap(),
        }
    }

    fn ticket(&mut self) -> CandidateTicket {
        self.ticket_for(String::new())
    }

    fn ticket_for(&mut self, text: String) -> CandidateTicket {
        self.host
            .service
            .for_session(
                &self.owner,
                &SessionId::new("index-memory-session").unwrap(),
                ReadPurpose::OfflineReasoning,
            )
            .begin_candidates(
                &RetrievalQuery::new(text)
                    .with_limit(32)
                    .with_char_budget(100_000),
                RetrievalPlan::new(self.profile.clone()),
                100,
            )
            .unwrap()
    }

    fn index(&self) -> SearchIndex {
        SearchIndex::new(
            StorageDomain::new("index-memory-root").unwrap(),
            self.profile.clone(),
            ChunkConfig::new(1, 3073, 3072, 16384).unwrap(),
            1,
            1,
            1,
        )
        .unwrap()
    }

    fn add_record(&mut self, char_count: usize) {
        let text = char::from_u32(0x4e30)
            .unwrap()
            .to_string()
            .repeat(char_count);
        self.host
            .service
            .for_pet(&self.owner)
            .create(
                MemoryDraft::new(
                    text,
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![self.source.clone()],
                ),
                100,
            )
            .unwrap();
    }
}

#[test]
fn high_overlap_reconcile_is_bounded_and_still_validates_late_candidates() {
    let mut fixture = Fixture::new(16, 4096);
    let authorized = fixture.ticket();
    assert_eq!(authorized.candidates().len(), 16);

    let mut index = fixture.index();
    let status = index.reconcile(&mut fixture.host, &authorized).unwrap();
    assert_eq!(status.missing_chunks, 16 * 1024);
    assert_eq!(status.queued_records, 1);
    assert!(status.reasons.contains(&SearchReason::QueueFull));
    let batch = index
        .take_batch(&mut fixture.host, &authorized)
        .unwrap()
        .unwrap();
    assert_eq!(batch.len(), 1);
    drop(batch);

    let mut strict_fixture = Fixture::new(1, 1024);
    let valid = strict_fixture.ticket();
    assert_eq!(valid.candidates().len(), 1);
    let mut strict_index = SearchIndex::new(
        StorageDomain::new("index-memory-root").unwrap(),
        strict_fixture.profile.clone(),
        ChunkConfig::new(2, 3072, 3071, 16384).unwrap(),
        1,
        1,
        1,
    )
    .unwrap();
    assert_eq!(
        strict_index
            .reconcile(&mut strict_fixture.host, &valid)
            .unwrap()
            .queued_records,
        1
    );
    strict_fixture.add_record(4096);
    let invalid = strict_fixture.ticket();
    assert_eq!(invalid.candidates().len(), 2);
    assert!(matches!(
        strict_index.reconcile(&mut strict_fixture.host, &invalid),
        Err(SearchError::InputTooLong)
    ));
}
