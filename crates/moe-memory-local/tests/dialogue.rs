use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_memory_core::{
    CacheStatus, Category, Epistemic, Lifecycle, MemoryCommand, MemoryDraft, MemoryError,
    MemoryScope, Namespace, PetIdentity, PublicationPolicy, ReadPolicy, ReadPurpose, RecordVersion,
    RetrievalLayer, RetrievalQuery, SearchScope, Sensitivity, SessionId, SourceRef,
};
use moe_memory_local::{
    ContextPath, ContextPreference, CorrectionDecision, DialogueCommit, DialogueError,
    DialogueFailure, DialogueReceipt, DialogueStage, Durability, HostDecision, LocalMemory,
    OfflineDialogue, OfflineFixture, OfflineProposal, OfflineTurn, OperationId,
};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let directory =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/dialogue-tests");
        fs::create_dir_all(&directory).unwrap();
        Self(directory.join(format!(
            "dialogue-{}-{}.log",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn pet() -> PetIdentity {
    PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap()
}
fn session() -> SessionId {
    SessionId::new("current").unwrap()
}
fn operation(name: &str) -> OperationId {
    OperationId::new(name).unwrap()
}
fn original(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::UserProfile,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
}
fn reference(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Knowledge,
        Epistemic::Reference,
        vec![source.clone()],
    )
}
fn dialogue(memory: &mut LocalMemory) -> OfflineDialogue<'_> {
    memory
        .offline_dialogue(&pet(), &session(), ReadPurpose::OfflineReasoning)
        .unwrap()
}
fn fixture(path: &Scratch) -> (LocalMemory, SourceRef, SourceRef) {
    let mut memory = LocalMemory::create(&path.0).unwrap();
    let private = memory
        .register_source(
            operation("private-source"),
            MemoryScope::Private(pet()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    let shared = memory
        .register_source(
            operation("shared-source"),
            MemoryScope::Shared(pet().namespace().clone()),
            "manual",
            1,
            Epistemic::Reference,
        )
        .unwrap()
        .value()
        .clone();
    memory
        .set_read_policy(
            &pet(),
            ReadPolicy::offline(1, vec![private.clone(), shared.clone()]),
        )
        .unwrap();
    memory
        .set_publication_policy(
            &pet(),
            PublicationPolicy::allow_sources(1, vec![shared.clone()]),
        )
        .unwrap();
    (memory, private, shared)
}
fn turn(memory: &mut LocalMemory, query: &str, now_ms: u64) -> OfflineTurn {
    dialogue(memory)
        .begin_turn(
            &RetrievalQuery::new(query),
            ContextPreference::Entries,
            now_ms,
        )
        .unwrap()
}
fn proposal(
    memory: &mut LocalMemory,
    query: &str,
    draft: MemoryDraft,
    now_ms: u64,
) -> OfflineProposal {
    let turn = turn(memory, query, now_ms);
    dialogue(memory)
        .generate(turn, OfflineFixture::Candidate(draft), now_ms)
        .unwrap()
}
fn private_commit(
    memory: &mut LocalMemory,
    proposal: &OfflineProposal,
    name: &str,
    now_ms: u64,
) -> Result<DialogueReceipt, DialogueError> {
    dialogue(memory).review_and_commit(proposal, operation(name), |_| HostDecision::CreatePrivate {
        now_ms,
    })
}
fn share(
    memory: &mut LocalMemory,
    proposal: &OfflineProposal,
    name: &str,
    now_ms: u64,
) -> Result<DialogueReceipt, DialogueError> {
    dialogue(memory).review_and_commit(proposal, operation(name), |_| HostDecision::PublishShared {
        now_ms,
    })
}
fn failure<Value: std::fmt::Debug>(
    actual: Result<Value, DialogueError>,
    stage: DialogueStage,
    expected: DialogueFailure,
) {
    let error = actual.unwrap_err();
    assert_eq!(error.stage(), stage);
    assert_eq!(error.failure(), &expected);
    assert!(error.to_string().contains("no new successful receipt"));
}

#[test]
fn end_to_end_host_identity_context_proposal_commit_correction_and_shared_read() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    let proposed = proposal(
        &mut memory,
        "no-memory",
        original("fixture prefers tea", &private),
        100,
    );
    assert_eq!(proposed.report().item_count, 0);
    let created = dialogue(&mut memory)
        .review_and_commit(&proposed, operation("create"), |review| {
            assert_eq!(review.candidate().text(), "fixture prefers tea");
            assert!(review.context().items().is_empty());
            assert!(review.identity_view().is_none());
            HostDecision::CreatePrivate { now_ms: 110 }
        })
        .unwrap();
    assert_eq!(created.committed().durability(), Durability::FileSynced);
    let id = created.committed().value().id();
    let old_turn = turn(&mut memory, "fixture", 120);
    assert_eq!(old_turn.context().items()[0].text(), "fixture prefers tea");
    let corrected = dialogue(&mut memory)
        .correct_original(
            operation("correction"),
            RecordVersion::new(id, 1).unwrap(),
            original("fixture prefers coffee", &private),
            130,
            |review| {
                assert_eq!(review.current().text(), "fixture prefers tea");
                assert_eq!(review.correction().text(), "fixture prefers coffee");
                CorrectionDecision::Commit { now_ms: 140 }
            },
        )
        .unwrap();
    assert_eq!(corrected.committed().value().revision(), 2);
    failure(
        dialogue(&mut memory).generate(old_turn, OfflineFixture::Unavailable, 150),
        DialogueStage::Generation,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
    assert_eq!(
        turn(&mut memory, "fixture", 150).context().items()[0].text(),
        "fixture prefers coffee"
    );
    let public = proposal(
        &mut memory,
        "empty-public-fixture",
        reference("public sample fact", &shared),
        160,
    );
    let published = share(&mut memory, &public, "publish", 170).unwrap();
    let bob = PetIdentity::new(pet().namespace().clone(), "bob").unwrap();
    memory
        .set_read_policy(&bob, ReadPolicy::offline(1, vec![shared]))
        .unwrap();
    let received = memory
        .offline_dialogue(&bob, &session(), ReadPurpose::OfflineReasoning)
        .unwrap()
        .begin_turn(&RetrievalQuery::new(""), ContextPreference::Entries, 180)
        .unwrap();
    assert_eq!(received.context().items().len(), 1);
    assert_eq!(received.context().items()[0].text(), "public sample fact");
    assert_eq!(published.committed().value().revision(), 1);
}

#[test]
fn forbidden_purposes_do_not_alias_offline_permission() {
    let path = Scratch::new();
    let (mut memory, _, _) = fixture(&path);
    for purpose in [
        ReadPurpose::Unknown,
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        let error = memory
            .offline_dialogue(&pet(), &session(), purpose)
            .err()
            .unwrap();
        assert_eq!(error.failure(), &DialogueFailure::PurposeDenied);
    }
}

#[test]
fn context_requires_current_read_grant_even_for_an_empty_query() {
    let path = Scratch::new();
    let mut memory = LocalMemory::create(&path.0).unwrap();
    failure(
        dialogue(&mut memory).begin_turn(
            &RetrievalQuery::new(""),
            ContextPreference::PreferEntity,
            100,
        ),
        DialogueStage::Context,
        DialogueFailure::Memory(MemoryError::AccessDenied),
    );
}

#[test]
fn unavailable_and_malformed_generation_never_write_or_claim_execution() {
    let path = Scratch::new();
    let (mut memory, _, _) = fixture(&path);
    let before = fs::metadata(&path.0).unwrap().len();
    let generation = memory.generation().unwrap();
    for (injected, expected) in [
        (
            OfflineFixture::Unavailable,
            DialogueFailure::ModelUnavailable,
        ),
        (
            OfflineFixture::Malformed,
            DialogueFailure::InvalidModelOutput,
        ),
    ] {
        let captured = turn(&mut memory, "", 100);
        failure(
            dialogue(&mut memory).generate(captured, injected, 110),
            DialogueStage::Generation,
            expected,
        );
    }
    assert_eq!(fs::metadata(&path.0).unwrap().len(), before);
    assert_eq!(memory.generation().unwrap(), generation);
}

#[test]
fn proposals_are_inert_and_a_real_host_review_may_reject() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let before = fs::metadata(&path.0).unwrap().len();
    let proposed = proposal(
        &mut memory,
        "",
        original("not yet committed", &private),
        100,
    );
    assert_eq!(fs::metadata(&path.0).unwrap().len(), before);
    failure(
        dialogue(&mut memory).review_and_commit(&proposed, operation("reject"), |review| {
            assert_eq!(review.candidate().text(), "not yet committed");
            HostDecision::Reject
        }),
        DialogueStage::Review,
        DialogueFailure::HostRejected,
    );
    assert_eq!(fs::metadata(&path.0).unwrap().len(), before);
}

#[test]
fn turns_and_proposals_cannot_be_rebound_to_other_pets_or_sessions() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let bob = PetIdentity::new(pet().namespace().clone(), "bob").unwrap();
    for (identity, bound_session) in [
        (bob, session()),
        (pet(), SessionId::new("another").unwrap()),
    ] {
        let captured = turn(&mut memory, "", 100);
        let mut other = memory
            .offline_dialogue(&identity, &bound_session, ReadPurpose::OfflineReasoning)
            .unwrap();
        failure(
            other.generate(captured, OfflineFixture::Unavailable, 100),
            DialogueStage::Generation,
            DialogueFailure::BindingMismatch,
        );
        let proposed = proposal(&mut memory, "", original("binding", &private), 100);
        let mut other = memory
            .offline_dialogue(&identity, &bound_session, ReadPurpose::OfflineReasoning)
            .unwrap();
        failure(
            other.review_and_commit(&proposed, operation("wrong-binding"), |_| {
                panic!("review must not run")
            }),
            DialogueStage::Review,
            DialogueFailure::BindingMismatch,
        );
    }
}

#[test]
fn entity_cache_disabled_full_and_unavailable_label_fall_back_to_authorized_entries() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    memory
        .for_pet(&pet())
        .create(operation("seed"), original("fixture person", &private), 90)
        .unwrap();
    memory.set_entity_cache_capacity(0).unwrap();
    let captured = dialogue(&mut memory)
        .begin_turn(
            &RetrievalQuery::new("fixture"),
            ContextPreference::PreferEntity,
            100,
        )
        .unwrap();
    assert_eq!(captured.report().path, ContextPath::EntriesFallback);
    assert_eq!(captured.context().items()[0].text(), "fixture person");
    memory.set_entity_cache_capacity(1).unwrap();
    let first = dialogue(&mut memory)
        .begin_turn(
            &RetrievalQuery::new("fixture"),
            ContextPreference::PreferEntity,
            100,
        )
        .unwrap();
    assert_eq!(first.report().path, ContextPath::Entity(CacheStatus::Built));
    let hit = dialogue(&mut memory)
        .begin_turn(
            &RetrievalQuery::new("fixture"),
            ContextPreference::PreferEntity,
            100,
        )
        .unwrap();
    assert_eq!(hit.report().path, ContextPath::Entity(CacheStatus::Hit));
    for query in ["person", ""] {
        let fallback = dialogue(&mut memory)
            .begin_turn(
                &RetrievalQuery::new(query),
                ContextPreference::PreferEntity,
                100,
            )
            .unwrap();
        assert_eq!(fallback.report().path, ContextPath::EntriesFallback);
        assert_eq!(fallback.context().items().len(), 1);
    }
}

#[test]
fn index_fallback_does_not_discard_actual_private_inputs_for_shared_review() {
    let path = Scratch::new();
    let (mut memory, _, shared) = fixture(&path);
    memory
        .for_pet(&pet())
        .create(
            operation("seed"),
            reference("private normal record", &shared),
            90,
        )
        .unwrap();
    memory.set_entity_cache_capacity(0).unwrap();
    let captured = dialogue(&mut memory)
        .begin_turn(
            &RetrievalQuery::new("private"),
            ContextPreference::PreferEntity,
            100,
        )
        .unwrap();
    assert_eq!(captured.report().path, ContextPath::EntriesFallback);
    let proposed = dialogue(&mut memory)
        .generate(
            captured,
            OfflineFixture::Candidate(reference("laundered", &shared)),
            100,
        )
        .unwrap();
    failure(
        share(&mut memory, &proposed, "launder", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::PublicationDenied),
    );
}

#[test]
fn candidate_sources_must_include_every_actual_read_source() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    memory
        .for_pet(&pet())
        .create(
            operation("seed"),
            original("private evidence", &private),
            90,
        )
        .unwrap();
    let proposed = proposal(
        &mut memory,
        "private",
        reference("omitted evidence", &shared),
        100,
    );
    failure(
        private_commit(&mut memory, &proposed, "missing", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::InvalidEvidence),
    );
}

#[test]
fn private_proposal_automatically_keeps_exact_read_dependencies() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let source = memory
        .for_pet(&pet())
        .create(operation("seed"), original("input evidence", &private), 90)
        .unwrap()
        .value()
        .id();
    let proposed = proposal(
        &mut memory,
        "input",
        original("derived output", &private),
        100,
    );
    let written = private_commit(&mut memory, &proposed, "derived", 110).unwrap();
    let stored = memory
        .for_pet(&pet())
        .get(written.committed().value().id(), 120)
        .unwrap();
    assert_eq!(
        stored.dependencies(),
        &[RecordVersion::new(source, 1).unwrap()]
    );
}

#[test]
fn shared_proposal_keeps_exact_shared_record_dependencies() {
    let path = Scratch::new();
    let (mut memory, _, shared) = fixture(&path);
    let first = proposal(&mut memory, "", reference("public input", &shared), 90);
    let first = share(&mut memory, &first, "first", 90).unwrap();
    let second = proposal(
        &mut memory,
        "public",
        reference("public derived", &shared),
        100,
    );
    let second = share(&mut memory, &second, "second", 110).unwrap();
    let stored = memory
        .for_pet(&pet())
        .get(second.committed().value().id(), 120)
        .unwrap();
    assert_eq!(
        stored.dependencies(),
        &[RecordVersion::new(first.committed().value().id(), 1).unwrap()]
    );
}

#[test]
fn late_review_rechecks_candidate_and_read_expiration() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    let proposed = proposal(
        &mut memory,
        "empty",
        reference("expires", &shared).with_expiration(120),
        100,
    );
    assert!(share(&mut memory, &proposed, "expired-candidate", 120).is_err());
    memory
        .for_pet(&pet())
        .create(
            operation("seed"),
            original("expires input", &private).with_expiration(140),
            100,
        )
        .unwrap();
    let proposed = proposal(&mut memory, "input", original("output", &private), 130);
    failure(
        private_commit(&mut memory, &proposed, "expired-read", 140),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
}

#[test]
fn generation_and_review_reject_backdated_host_time() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let captured = turn(&mut memory, "", 100);
    failure(
        dialogue(&mut memory).generate(
            captured,
            OfflineFixture::Candidate(original("old", &private)),
            99,
        ),
        DialogueStage::Generation,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
    let proposed = proposal(&mut memory, "", original("old", &private), 110);
    failure(
        private_commit(&mut memory, &proposed, "old", 109),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TimeWentBackwards),
    );
}

#[test]
fn any_storage_or_configuration_change_cancels_old_proposals() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    let proposed = proposal(&mut memory, "", original("cancel on write", &private), 100);
    memory
        .for_pet(&pet())
        .create(operation("unrelated"), original("unrelated", &private), 100)
        .unwrap();
    failure(
        private_commit(&mut memory, &proposed, "stale-write", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
    let proposed = proposal(&mut memory, "", reference("cancel on policy", &shared), 120);
    memory
        .set_publication_policy(&pet(), PublicationPolicy::allow_sources(2, vec![shared]))
        .unwrap();
    failure(
        share(&mut memory, &proposed, "stale-policy", 130),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
}

#[test]
fn cancel_and_revoke_cannot_be_hidden_by_fresh_offline_binding() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let proposed = proposal(&mut memory, "", original("cancel", &private), 100);
    memory
        .cancel_tasks(operation("cancel"), memory.generation().unwrap())
        .unwrap();
    failure(
        private_commit(&mut memory, &proposed, "after-cancel", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
    let proposed = proposal(&mut memory, "", original("revoked", &private), 120);
    memory.revoke_access(&pet(), 2).unwrap();
    failure(
        private_commit(&mut memory, &proposed, "after-revoke", 130),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
    failure(
        dialogue(&mut memory).begin_turn(&RetrievalQuery::new(""), ContextPreference::Entries, 130),
        DialogueStage::Context,
        DialogueFailure::Memory(MemoryError::AccessDenied),
    );
}

#[test]
fn identity_anchor_is_reviewable_but_never_shared_or_omitted_from_private_sources() {
    let path = Scratch::new();
    let (mut memory, _, shared) = fixture(&path);
    let authority = memory
        .register_source(
            operation("authority"),
            MemoryScope::Private(pet()),
            "identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap()
        .value()
        .clone();
    memory
        .set_identity_view(
            &pet(),
            1,
            "fixture identity secret",
            vec![authority.clone()],
        )
        .unwrap();
    memory
        .set_read_policy(
            &pet(),
            ReadPolicy::offline(2, vec![authority, shared.clone()]),
        )
        .unwrap();
    let proposed = proposal(
        &mut memory,
        "no-match",
        reference("identity derived", &shared),
        100,
    );
    assert!(proposed.report().identity_available);
    failure(
        dialogue(&mut memory).review_and_commit(&proposed, operation("private-anchor"), |review| {
            assert_eq!(
                review.identity_view().unwrap().text(),
                "fixture identity secret"
            );
            HostDecision::CreatePrivate { now_ms: 110 }
        }),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::InvalidEvidence),
    );
    failure(
        share(&mut memory, &proposed, "shared-anchor", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::PublicationDenied),
    );
    assert!(!format!("{proposed:?}").contains("fixture identity secret"));
}

#[test]
fn working_context_may_be_read_but_cannot_be_persisted_or_shared() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    memory
        .put_working_context(
            &pet(),
            &session(),
            "work",
            1,
            MemoryDraft::new(
                "work secret",
                Category::WorkingContext,
                Epistemic::UserStatement,
                vec![private.clone()],
            )
            .with_lifecycle(Lifecycle::ShortTerm),
            90,
        )
        .unwrap();
    let proposed = proposal(&mut memory, "work", original("work output", &private), 100);
    assert_eq!(proposed.report().item_count, 1);
    assert!(private_commit(&mut memory, &proposed, "work-private", 110).is_err());
    let proposed = proposal(&mut memory, "work", reference("work output", &shared), 100);
    failure(
        share(&mut memory, &proposed, "work-shared", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::PublicationDenied),
    );
}

#[test]
fn formal_context_stays_separate_and_cannot_become_an_ordinary_candidate() {
    let path = Scratch::new();
    let (mut memory, _, shared) = fixture(&path);
    memory
        .set_formal_publication_policy(
            pet().namespace(),
            PublicationPolicy::allow_sources(1, vec![shared.clone()]),
        )
        .unwrap();
    let formal = MemoryDraft::new(
        "formal instructions",
        Category::Protocol,
        Epistemic::Reference,
        vec![shared.clone()],
    );
    let command = memory
        .prepare_formal_publication(pet().namespace(), formal, 90)
        .unwrap();
    memory
        .commit_publication(operation("formal"), command)
        .unwrap();
    assert_eq!(turn(&mut memory, "formal", 100).report().item_count, 0);
    let captured = dialogue(&mut memory)
        .begin_turn(
            &RetrievalQuery::new("formal").with_layer(RetrievalLayer::FormalMaterial),
            ContextPreference::PreferEntity,
            100,
        )
        .unwrap();
    assert_eq!(captured.report().path, ContextPath::EntriesFallback);
    assert_eq!(captured.report().item_count, 1);
    let proposed = dialogue(&mut memory)
        .generate(
            captured,
            OfflineFixture::Candidate(reference("formal derived", &shared)),
            100,
        )
        .unwrap();
    assert!(private_commit(&mut memory, &proposed, "formal-private", 110).is_err());
    failure(
        share(&mut memory, &proposed, "formal-shared", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::PublicationDenied),
    );
}

#[test]
fn shared_grant_is_not_implied_by_read_permission() {
    let path = Scratch::new();
    let (mut memory, _, shared) = fixture(&path);
    memory
        .set_publication_policy(&pet(), PublicationPolicy::deny_all(2))
        .unwrap();
    let proposed = proposal(&mut memory, "", reference("not licensed", &shared), 100);
    failure(
        share(&mut memory, &proposed, "no-grant", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::PublicationDenied),
    );
}

#[test]
fn candidates_do_not_gain_authority_or_automatic_publication() {
    let path = Scratch::new();
    let (mut memory, _, shared) = fixture(&path);
    for (index, category) in [
        Category::Identity,
        Category::ToolDefinition,
        Category::Protocol,
    ]
    .into_iter()
    .enumerate()
    {
        let draft = MemoryDraft::new(
            "elevate",
            category,
            Epistemic::Reference,
            vec![shared.clone()],
        );
        let proposed = proposal(&mut memory, "", draft, 100);
        assert!(
            private_commit(&mut memory, &proposed, &format!("authority-{index}"), 110).is_err()
        );
        assert!(
            share(
                &mut memory,
                &proposed,
                &format!("share-authority-{index}"),
                110
            )
            .is_err()
        );
    }
}

#[test]
fn invalid_model_candidate_never_receives_a_committed_receipt() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let before = fs::metadata(&path.0).unwrap().len();
    let proposed = proposal(&mut memory, "", original("", &private), 100);
    let error = private_commit(&mut memory, &proposed, "invalid", 110).unwrap_err();
    assert_eq!(error.stage(), DialogueStage::Commit);
    assert!(matches!(
        error.failure(),
        DialogueFailure::Memory(MemoryError::InvalidInput(_))
    ));
    failure(
        memory.confirm_dialogue(error.request().unwrap()),
        DialogueStage::Confirmation,
        DialogueFailure::Memory(MemoryError::NotFound),
    );
    assert_eq!(fs::metadata(&path.0).unwrap().len(), before);
}

#[test]
fn original_correction_rejects_cas_conflict_backdating_and_host_rejection() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let id = memory
        .for_pet(&pet())
        .create(operation("seed"), original("current", &private), 90)
        .unwrap()
        .value()
        .id();
    failure(
        dialogue(&mut memory).correct_original(
            operation("cas"),
            RecordVersion::new(id, 2).unwrap(),
            original("next", &private),
            100,
            |_| panic!("no review for wrong CAS"),
        ),
        DialogueStage::Correction,
        DialogueFailure::Memory(MemoryError::RevisionConflict {
            expected: 2,
            actual: 1,
        }),
    );
    failure(
        dialogue(&mut memory).correct_original(
            operation("backdate"),
            RecordVersion::new(id, 1).unwrap(),
            original("next", &private),
            100,
            |_| CorrectionDecision::Commit { now_ms: 99 },
        ),
        DialogueStage::Correction,
        DialogueFailure::Memory(MemoryError::TimeWentBackwards),
    );
    failure(
        dialogue(&mut memory).correct_original(
            operation("reject"),
            RecordVersion::new(id, 1).unwrap(),
            original("next", &private),
            100,
            |_| CorrectionDecision::Reject,
        ),
        DialogueStage::Correction,
        DialogueFailure::HostRejected,
    );
    assert_eq!(memory.for_pet(&pet()).get(id, 110).unwrap().revision(), 1);
}

#[test]
fn original_correction_does_not_strip_model_derivation_or_dependencies() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    memory
        .for_pet(&pet())
        .create(operation("seed"), original("source", &private), 90)
        .unwrap();
    let proposed = proposal(&mut memory, "source", original("derived", &private), 100);
    let derived = private_commit(&mut memory, &proposed, "derived", 110).unwrap();
    failure(
        dialogue(&mut memory).correct_original(
            operation("strip"),
            RecordVersion::new(derived.committed().value().id(), 1).unwrap(),
            original("stripped", &private),
            120,
            |_| panic!("derived correction needs its own provenance"),
        ),
        DialogueStage::Correction,
        DialogueFailure::OriginalEvidenceRequired,
    );
}

#[test]
fn original_correction_cannot_downgrade_private_sensitivity_or_renew_expiry() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let id = memory
        .for_pet(&pet())
        .create(
            operation("seed"),
            original("private", &private)
                .with_sensitivity(Sensitivity::Private)
                .with_expiration(120),
            90,
        )
        .unwrap()
        .value()
        .id();
    failure(
        dialogue(&mut memory).correct_original(
            operation("downgrade"),
            RecordVersion::new(id, 1).unwrap(),
            original("public now", &private),
            100,
            |_| CorrectionDecision::Commit { now_ms: 110 },
        ),
        DialogueStage::Commit,
        DialogueFailure::Memory(MemoryError::ProtectedContent),
    );
    assert!(
        dialogue(&mut memory)
            .correct_original(
                operation("expired"),
                RecordVersion::new(id, 1).unwrap(),
                original("renew", &private).with_sensitivity(Sensitivity::Private),
                110,
                |_| CorrectionDecision::Commit { now_ms: 120 }
            )
            .is_err()
    );
}

#[test]
fn confirmation_is_historical_only_after_revocation_and_restart() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let proposed = proposal(&mut memory, "", original("historical", &private), 100);
    let committed = private_commit(&mut memory, &proposed, "historical", 110).unwrap();
    let id = committed.committed().value().id();
    memory
        .for_pet(&pet())
        .delete(operation("delete"), id, 1, 120)
        .unwrap();
    memory.revoke_access(&pet(), 2).unwrap();
    let before = fs::metadata(&path.0).unwrap().len();
    let generation = memory.generation().unwrap();
    assert_eq!(
        memory
            .confirm_dialogue(committed.request())
            .unwrap()
            .committed(),
        committed.committed()
    );
    assert_eq!(memory.generation().unwrap(), generation);
    drop(memory);
    let mut reopened = LocalMemory::open(&path.0).unwrap();
    assert_eq!(
        reopened
            .confirm_dialogue(committed.request())
            .unwrap()
            .committed(),
        committed.committed()
    );
    assert_eq!(fs::metadata(&path.0).unwrap().len(), before);
    assert!(reopened.for_pet(&pet()).get(id, 130).is_err());
    failure(
        dialogue(&mut reopened).begin_turn(
            &RetrievalQuery::new(""),
            ContextPreference::Entries,
            130,
        ),
        DialogueStage::Context,
        DialogueFailure::Memory(MemoryError::AccessDenied),
    );
}

#[test]
fn old_proposal_never_recovers_live_authority_after_restart() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    let proposed = proposal(&mut memory, "", original("old proof", &private), 100);
    drop(memory);
    let mut memory = LocalMemory::open(&path.0).unwrap();
    memory
        .set_read_policy(&pet(), ReadPolicy::offline(1, vec![private, shared]))
        .unwrap();
    failure(
        private_commit(&mut memory, &proposed, "old-proof", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
}

#[test]
fn operation_conflict_preserves_original_complete_request_and_confirmation() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let first = proposal(
        &mut memory,
        "empty-one",
        original("same text", &private),
        100,
    );
    let committed = private_commit(&mut memory, &first, "operation", 110).unwrap();
    let second = proposal(
        &mut memory,
        "empty-two",
        original("same text", &private),
        120,
    );
    let error = private_commit(&mut memory, &second, "operation", 130).unwrap_err();
    assert_eq!(error.failure(), &DialogueFailure::OperationConflict);
    failure(
        memory.confirm_dialogue(error.request().unwrap()),
        DialogueStage::Confirmation,
        DialogueFailure::OperationConflict,
    );
    assert_eq!(
        memory
            .confirm_dialogue(committed.request())
            .unwrap()
            .committed(),
        committed.committed()
    );
}

#[test]
fn debug_and_error_messages_do_not_echo_fixture_body_or_query() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let proposed = proposal(
        &mut memory,
        "secret-query",
        original("secret-body", &private),
        100,
    );
    assert!(!format!("{proposed:?}").contains("secret-query"));
    assert!(!format!("{proposed:?}").contains("secret-body"));
    let committed = private_commit(&mut memory, &proposed, "redaction", 110).unwrap();
    assert!(!format!("{committed:?}").contains("secret-body"));
    assert!(!format!("{:?}", committed.request()).contains("secret-query"));
}

#[test]
fn authorized_context_obeys_namespace_scope_and_character_budget() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    memory
        .for_pet(&pet())
        .create(
            operation("private"),
            original("private sample", &private),
            90,
        )
        .unwrap();
    let public = proposal(
        &mut memory,
        "empty",
        reference("public sample", &shared),
        100,
    );
    share(&mut memory, &public, "public", 100).unwrap();
    let query = RetrievalQuery::new("sample")
        .with_scope(SearchScope::Shared)
        .with_limit(1)
        .with_char_budget(13);
    let captured = dialogue(&mut memory)
        .begin_turn(&query, ContextPreference::Entries, 110)
        .unwrap();
    assert_eq!(captured.context().items().len(), 1);
    assert_eq!(captured.context().used_chars(), 13);
    assert_eq!(captured.context().items()[0].text(), "public sample");
    let stranger =
        PetIdentity::new(Namespace::new("other-workspace", "user").unwrap(), "alice").unwrap();
    assert!(
        memory
            .set_read_policy(&stranger, ReadPolicy::offline(1, vec![private, shared]))
            .is_err()
    );
    memory
        .set_read_policy(&stranger, ReadPolicy::offline(1, vec![]))
        .unwrap();
    let captured = memory
        .offline_dialogue(&stranger, &session(), ReadPurpose::OfflineReasoning)
        .unwrap()
        .begin_turn(&RetrievalQuery::new(""), ContextPreference::Entries, 110)
        .unwrap();
    assert!(captured.context().items().is_empty());
}

#[test]
fn revoked_source_requires_explicit_upgrade_and_does_not_revive_old_proposals() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    let proposed = proposal(&mut memory, "", reference("old source", &shared), 100);
    memory
        .revoke_source(operation("revoke-source"), &shared)
        .unwrap();
    failure(
        share(&mut memory, &proposed, "revoked", 110),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
    let restored = memory
        .restore_source(operation("restore-source"), &shared, 2)
        .unwrap()
        .value()
        .clone();
    let old_reference = proposal(&mut memory, "", reference("old reference", &shared), 120);
    assert!(share(&mut memory, &old_reference, "old-ref", 130).is_err());
    memory
        .set_read_policy(
            &pet(),
            ReadPolicy::offline(2, vec![private, restored.clone()]),
        )
        .unwrap();
    memory
        .set_publication_policy(
            &pet(),
            PublicationPolicy::allow_sources(2, vec![restored.clone()]),
        )
        .unwrap();
    let fresh = proposal(&mut memory, "", reference("new source", &restored), 140);
    share(&mut memory, &fresh, "fresh", 150).unwrap();
    failure(
        share(&mut memory, &proposed, "old-task", 160),
        DialogueStage::Review,
        DialogueFailure::Memory(MemoryError::TaskCancelled),
    );
}

#[test]
fn correction_and_shared_receipts_confirm_without_restoring_new_write_grants() {
    let path = Scratch::new();
    let (mut memory, private, shared) = fixture(&path);
    let id = memory
        .for_pet(&pet())
        .create(operation("seed"), original("original", &private), 90)
        .unwrap()
        .value()
        .id();
    let correction = dialogue(&mut memory)
        .correct_original(
            operation("correction"),
            RecordVersion::new(id, 1).unwrap(),
            original("corrected", &private),
            100,
            |_| CorrectionDecision::Commit { now_ms: 110 },
        )
        .unwrap();
    let proposed = proposal(&mut memory, "empty", reference("public", &shared), 120);
    let shared = share(&mut memory, &proposed, "shared", 130).unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(&path.0).unwrap();
    for receipt in [&correction, &shared] {
        assert_eq!(
            memory
                .confirm_dialogue(receipt.request())
                .unwrap()
                .committed(),
            receipt.committed()
        );
    }
    failure(
        dialogue(&mut memory).begin_turn(&RetrievalQuery::new(""), ContextPreference::Entries, 140),
        DialogueStage::Context,
        DialogueFailure::Memory(MemoryError::AccessDenied),
    );
}

#[test]
fn decoded_confirmation_requests_never_execute_even_for_host_crud_commands() {
    let path = Scratch::new();
    let (memory, private, _) = fixture(&path);
    let command = MemoryCommand::create(pet(), original("not in journal", &private), 100);
    let request =
        DialogueCommit::from_bytes(operation("unknown"), &command.to_bytes().unwrap()).unwrap();
    let before = memory.generation().unwrap();
    failure(
        memory.confirm_dialogue(&request),
        DialogueStage::Confirmation,
        DialogueFailure::Memory(MemoryError::NotFound),
    );
    assert_eq!(memory.generation().unwrap(), before);
    assert!(DialogueCommit::from_bytes(operation("bad"), &[255, 10]).is_err());
}

#[test]
fn decoded_success_confirmation_survives_restart_without_any_live_proof() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let proposed = proposal(&mut memory, "", original("wire historical", &private), 100);
    let committed = private_commit(&mut memory, &proposed, "wire", 110).unwrap();
    let wire = committed.request().to_bytes().unwrap();
    assert_eq!(&wire[..2], &[2, 10]);
    let decoded = DialogueCommit::from_bytes(operation("wire"), &wire).unwrap();
    assert_eq!(decoded.to_bytes().unwrap(), wire);
    drop(memory);
    let memory = LocalMemory::open(&path.0).unwrap();
    assert_eq!(
        memory.confirm_dialogue(&decoded).unwrap().committed(),
        committed.committed()
    );
}

#[test]
fn confirmation_of_non_write_metadata_returns_a_typed_error_not_a_panic() {
    let path = Scratch::new();
    let (memory, _, _) = fixture(&path);
    let command = MemoryCommand::register_source(
        MemoryScope::Private(pet()),
        "message",
        1,
        Epistemic::UserStatement,
    );
    let decoded =
        DialogueCommit::from_bytes(operation("private-source"), &command.to_bytes().unwrap())
            .unwrap();
    failure(
        memory.confirm_dialogue(&decoded),
        DialogueStage::Confirmation,
        DialogueFailure::Memory(MemoryError::InvalidInput("not a write receipt")),
    );
}

#[test]
fn host_review_can_inspect_all_original_candidate_metadata_without_mutation() {
    let path = Scratch::new();
    let (mut memory, private, _) = fixture(&path);
    let id = memory
        .for_pet(&pet())
        .create(operation("seed"), original("metadata input", &private), 90)
        .unwrap()
        .value()
        .id();
    let dependency = RecordVersion::new(id, 1).unwrap();
    let draft = original("metadata output", &private)
        .with_lifecycle(Lifecycle::LongTerm)
        .with_sensitivity(Sensitivity::Private)
        .with_expiration(200)
        .with_dependencies(vec![dependency]);
    let proposed = proposal(&mut memory, "input", draft, 100);
    dialogue(&mut memory)
        .review_and_commit(&proposed, operation("metadata"), |review| {
            let draft = review.candidate();
            assert_eq!(draft.text(), "metadata output");
            assert_eq!(draft.category(), Category::UserProfile);
            assert_eq!(draft.epistemic(), Epistemic::UserStatement);
            assert_eq!(draft.sources(), &[private]);
            assert_eq!(draft.lifecycle(), Lifecycle::LongTerm);
            assert_eq!(draft.sensitivity(), Sensitivity::Private);
            assert_eq!(draft.expires_at_ms(), Some(200));
            assert_eq!(draft.dependencies(), &[dependency]);
            assert_eq!(review.report().item_count, 1);
            HostDecision::CreatePrivate { now_ms: 110 }
        })
        .unwrap();
}
