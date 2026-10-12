use moe_memory_core::{
    Category, CommandResult, EmbeddingProfile, Epistemic, Lifecycle, MemoryDraft, MemoryError,
    MemoryScope, MemoryService, Namespace, PetIdentity, PublicationPolicy, ReadPolicy, ReadPurpose,
    RetrievalPlan, RetrievalQuery, SearchScope, SessionId, SourceRef, TransmissionPolicy,
    TransmissionUsage,
};

const RERANK_ENDPOINT: &str = "https://rerank.example.test/v1";

fn namespace() -> Namespace {
    Namespace::new("home", "user").unwrap()
}

fn pet(name: &str) -> PetIdentity {
    PetIdentity::new(namespace(), name).unwrap()
}

fn session() -> SessionId {
    SessionId::new("current").unwrap()
}

fn plan() -> RetrievalPlan {
    RetrievalPlan::new(EmbeddingProfile::new("fixture", "fixture-embedding", 8).unwrap())
        .with_rerank(moe_memory_core::RerankProfile::new("fixture", "fixture-rerank").unwrap())
}

fn draft(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Episode,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
}

fn private_fixture(name: &str, text: &str) -> (MemoryService, PetIdentity, SourceRef) {
    let mut service = MemoryService::new();
    let owner = pet(name);
    let source = service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    service
        .for_pet(&owner)
        .create(draft(text, &source), 100)
        .unwrap();
    service
        .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    (service, owner, source)
}

fn reader<'service>(
    service: &'service mut MemoryService,
    owner: &PetIdentity,
) -> moe_memory_core::MemoryReader<'service> {
    service.for_session(owner, &session(), ReadPurpose::OfflineReasoning)
}

#[test]
fn candidate_tickets_scan_the_full_authorized_set_beyond_result_limits() {
    let (mut service, owner, source) = private_fixture("alice", "seed");
    for index in 0..150 {
        service
            .for_pet(&owner)
            .create(draft(&format!("note {index}"), &source), 100)
            .unwrap();
    }
    let query = RetrievalQuery::new("");
    assert_eq!(query.text(), "");
    assert_eq!(query.scope(), SearchScope::All);
    assert_eq!(query.layer(), moe_memory_core::RetrievalLayer::All);
    assert_eq!(query.limit(), 20);
    assert_eq!(query.char_budget(), 16_384);
    assert_eq!(
        reader(&mut service, &owner)
            .retrieve(&query, 100)
            .unwrap()
            .items()
            .len(),
        20
    );
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&query, plan(), 100)
        .unwrap();
    assert_eq!(ticket.candidates().len(), 151);
}

#[test]
fn semantic_selection_enters_protected_context_without_query_substring() {
    let (mut service, owner, _source) = private_fixture("alice", "主人喜欢喝茉莉花茶");
    let query = RetrievalQuery::new("茶饮偏好");
    assert_eq!(
        reader(&mut service, &owner)
            .begin_task(&query, 100)
            .unwrap()
            .result()
            .items()
            .len(),
        0
    );
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&query, plan(), 100)
        .unwrap();
    assert_eq!(ticket.candidates().len(), 1);
    let reference = ticket.candidates()[0].candidate_ref();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[(reference, 0.9)], 100)
        .unwrap();
    assert_eq!(task.result().items().len(), 1);
    assert_eq!(task.retrieval_plan(), Some(&plan()));
    assert!(task.transmissions().is_empty());
    let context = service.assemble_context(&task, 512, 100).unwrap();
    let view = service.context_view(&context, 200).unwrap();
    assert_eq!(view.blocks().len(), 1);
    assert_eq!(view.blocks()[0].text(), "主人喜欢喝茉莉花茶");
    assert_eq!(view.blocks()[0].evidence().len(), 1);
}

#[test]
fn foreign_services_pets_and_duplicate_references_are_rejected() {
    let (mut service, alice, _source) = private_fixture("alice", "alice note");
    let bob = pet("bob");
    let bob_source = service
        .register_source(
            MemoryScope::Private(bob.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    service
        .for_pet(&bob)
        .create(draft("bob note", &bob_source), 100)
        .unwrap();
    service
        .set_read_policy(&bob, ReadPolicy::offline(1, vec![bob_source.clone()]))
        .unwrap();
    let (mut other_service, _other_alice, _other_source) = private_fixture("alice", "elsewhere");

    let ticket = reader(&mut service, &alice)
        .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
        .unwrap();
    let bob_ticket = reader(&mut service, &bob)
        .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
        .unwrap();
    let own_ref = ticket.candidates()[0].candidate_ref();
    let bob_ref = bob_ticket.candidates()[0].candidate_ref();

    assert_eq!(
        reader(&mut other_service, &alice)
            .complete_retrieval(&ticket, &[(own_ref.clone(), 1.0)], 100)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert_eq!(
        reader(&mut service, &bob)
            .complete_retrieval(&ticket, &[(own_ref.clone(), 1.0)], 100)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert_eq!(
        reader(&mut service, &alice)
            .complete_retrieval(&ticket, &[(bob_ref.clone(), 1.0)], 100)
            .unwrap_err(),
        MemoryError::InvalidEvidence
    );
    assert_eq!(
        reader(&mut service, &alice)
            .complete_retrieval(
                &ticket,
                &[(own_ref.clone(), 1.0), (own_ref.clone(), 0.5)],
                100
            )
            .unwrap_err(),
        MemoryError::InvalidEvidence
    );
    assert_eq!(
        reader(&mut service, &alice)
            .complete_retrieval(&ticket, &[], 99)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
}

#[test]
fn non_finite_scores_are_rejected() {
    let (mut service, owner, _source) = private_fixture("alice", "tea note");
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    let reference = ticket.candidates()[0].candidate_ref();
    for score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            reader(&mut service, &owner)
                .complete_retrieval(&ticket, &[(reference.clone(), score)], 100)
                .unwrap_err(),
            MemoryError::InvalidInput("relevance score must be finite")
        );
    }
}

#[test]
fn ranking_order_budget_and_limit_bound_the_delivery() {
    let (mut service, owner, source) = private_fixture("alice", "seed");
    for text in ["a", "bbb", "ccccc"] {
        service
            .for_pet(&owner)
            .create(draft(text, &source), 100)
            .unwrap();
    }
    let query = RetrievalQuery::new("").with_limit(2).with_char_budget(4);
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&query, plan(), 100)
        .unwrap();
    let reference = |text: &str| {
        ticket
            .candidates()
            .iter()
            .find(|item| item.text() == text)
            .unwrap()
            .candidate_ref()
    };
    let ranking = [
        (reference("ccccc"), 0.99),
        (reference("a"), 0.9),
        (reference("bbb"), 0.8),
    ];
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &ranking, 100)
        .unwrap();
    let texts: Vec<_> = task
        .result()
        .items()
        .iter()
        .map(|item| item.text())
        .collect();
    assert_eq!(texts, ["a", "bbb"]);
    assert_eq!(task.result().used_chars(), 4);
}

#[test]
fn fences_cancel_tickets_and_hybrid_tasks() {
    fn cancel(service: &mut MemoryService, _owner: &PetIdentity, _source: &SourceRef) {
        service.cancel_tasks(service.generation()).unwrap();
    }
    fn correct(service: &mut MemoryService, owner: &PetIdentity, source: &SourceRef) {
        let id = service
            .for_pet(owner)
            .search(&moe_memory_core::SearchQuery::new("note"), 100)
            .unwrap()[0]
            .id();
        service
            .for_pet(owner)
            .update(id, 1, draft("corrected note", source), 200)
            .unwrap();
    }
    fn revoke(service: &mut MemoryService, _owner: &PetIdentity, source: &SourceRef) {
        service.revoke_source(source).unwrap();
    }
    fn repolicy(service: &mut MemoryService, owner: &PetIdentity, source: &SourceRef) {
        service
            .set_read_policy(owner, ReadPolicy::offline(2, vec![source.clone()]))
            .unwrap();
    }
    fn transmit(service: &mut MemoryService, owner: &PetIdentity, _source: &SourceRef) {
        service
            .set_transmission_policy(
                owner,
                TransmissionPolicy::deny_all(1, TransmissionUsage::Rerank),
            )
            .unwrap();
    }
    let mutations: [fn(&mut MemoryService, &PetIdentity, &SourceRef); 5] =
        [cancel, correct, revoke, repolicy, transmit];
    for mutation in mutations {
        let (mut service, owner, source) = private_fixture("alice", "tea note");
        let ticket = reader(&mut service, &owner)
            .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
            .unwrap();
        let reference = ticket.candidates()[0].candidate_ref();
        mutation(&mut service, &owner, &source);
        assert_eq!(
            reader(&mut service, &owner)
                .complete_retrieval(&ticket, &[(reference.clone(), 1.0)], 300)
                .unwrap_err(),
            MemoryError::TaskCancelled
        );

        let (mut service, owner, source) = private_fixture("alice", "tea note");
        let ticket = reader(&mut service, &owner)
            .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
            .unwrap();
        let reference = ticket.candidates()[0].candidate_ref();
        let task = reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[(reference, 1.0)], 100)
            .unwrap();
        service.validate_task(&task, 150).unwrap();
        mutation(&mut service, &owner, &source);
        assert_eq!(
            service.validate_task(&task, 300).unwrap_err(),
            MemoryError::TaskCancelled
        );
    }
}

#[test]
fn expiry_boundary_cancels_delivery() {
    let (mut service, owner, source) = private_fixture("alice", "temporary note");
    service
        .for_pet(&owner)
        .create(
            MemoryDraft::new(
                "fleeting note",
                Category::Episode,
                Epistemic::UserStatement,
                vec![source],
            )
            .with_expiration(500),
            100,
        )
        .unwrap();
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
        .unwrap();
    assert_eq!(ticket.candidates().len(), 2);
    let reference = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "fleeting note")
        .unwrap()
        .candidate_ref();
    assert!(
        reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[(reference.clone(), 1.0)], 499)
            .is_ok()
    );
    assert_eq!(
        reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[(reference, 1.0)], 500)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
}

#[test]
fn existing_transmission_purposes_stay_denied_without_licenses() {
    let (mut service, owner, _source) = private_fixture("alice", "tea note");
    let query = RetrievalQuery::new("tea");
    for purpose in [
        ReadPurpose::Unknown,
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        assert_eq!(
            service
                .for_session(&owner, &session(), purpose)
                .begin_candidates(&query, plan(), 100)
                .unwrap_err(),
            MemoryError::AccessDenied
        );
    }
}

fn grant(
    service: &mut MemoryService,
    owner: &PetIdentity,
    usage: TransmissionUsage,
    sources: Vec<SourceRef>,
) {
    service
        .set_transmission_policy(
            owner,
            TransmissionPolicy::new(1, usage, RERANK_ENDPOINT, sources)
                .unwrap()
                .with_scope(MemoryScope::Private(owner.clone()))
                .with_scope(MemoryScope::Shared(namespace()))
                .transmit_query(),
        )
        .unwrap();
}

fn bind(
    service: &mut MemoryService,
    ticket: &mut moe_memory_core::CandidateTicket,
    source: &SourceRef,
    entries: &[moe_memory_core::CandidateRef],
) {
    service
        .bind_query_provenance(ticket, vec![source.clone()], entries, 100)
        .unwrap();
}

fn failed_rerank_ticket(
    service: &mut MemoryService,
    owner: &PetIdentity,
    source: &SourceRef,
) -> moe_memory_core::CandidateTicket {
    grant(
        service,
        owner,
        TransmissionUsage::Rerank,
        vec![source.clone()],
    );
    let mut ticket = reader(service, owner)
        .begin_candidates(&RetrievalQuery::new(""), plan(), 100)
        .unwrap();
    bind(service, &mut ticket, source, &[]);
    let entry = ticket.candidates()[0].candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::Rerank,
            RERANK_ENDPOINT,
            &[entry],
            100,
        )
        .unwrap();
    service.fail_transmission(&mut ticket, &sent, 110).unwrap();
    ticket
}

#[test]
fn nonrerank_failure_cannot_gain_recovery_from_a_later_pending_rerank_receipt() {
    let (mut service, owner, source) = private_fixture("alice", "tea");
    for usage in [TransmissionUsage::EntryEmbedding, TransmissionUsage::Rerank] {
        grant(&mut service, &owner, usage, vec![source.clone()]);
    }
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    bind(&mut service, &mut ticket, &source, &[]);
    let reference = ticket.candidates()[0].candidate_ref();
    let entry = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            std::slice::from_ref(&reference),
            100,
        )
        .unwrap();
    let rerank = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::Rerank,
            RERANK_ENDPOINT,
            std::slice::from_ref(&reference),
            100,
        )
        .unwrap();
    service.fail_transmission(&mut ticket, &entry, 110).unwrap();
    assert_eq!(
        service.record_transmission(&mut ticket, &rerank, 110),
        Err(MemoryError::TaskCancelled)
    );
    assert_eq!(ticket.transmissions().len(), 2);
    assert_eq!(
        ticket.transmissions().last().unwrap().usage(),
        TransmissionUsage::Rerank
    );
    assert!(service.recover_failed_rerank(&mut ticket, 110).is_err());
}

#[test]
fn failed_rerank_recovery_preserves_evidence_and_cannot_resend() {
    let (mut service, owner, private_source) = private_fixture("alice", "private preference");
    let shared_source = service
        .register_source(
            MemoryScope::Shared(namespace()),
            "manual",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service
        .set_read_policy(
            &owner,
            ReadPolicy::offline(2, vec![private_source.clone(), shared_source.clone()]),
        )
        .unwrap();
    publish_seed(&mut service, &owner, &shared_source);
    grant(
        &mut service,
        &owner,
        TransmissionUsage::Rerank,
        vec![private_source.clone(), shared_source.clone()],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new(""), plan(), 100)
        .unwrap();
    let private_reference = ticket
        .candidates()
        .iter()
        .find(|candidate| candidate.text() == "private preference")
        .unwrap()
        .candidate_ref();
    let shared_reference = ticket
        .candidates()
        .iter()
        .find(|candidate| candidate.text() == "shared manual")
        .unwrap()
        .candidate_ref();
    bind(
        &mut service,
        &mut ticket,
        &private_source,
        std::slice::from_ref(&private_reference),
    );
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::Rerank,
            RERANK_ENDPOINT,
            &[private_reference.clone(), shared_reference.clone()],
            100,
        )
        .unwrap();
    service.fail_transmission(&mut ticket, &sent, 110).unwrap();
    let failed_evidence = ticket.transmissions()[0].clone();
    let recovered = service.recover_failed_rerank(&mut ticket, 120).unwrap();
    assert_ne!(recovered.runtime(), ticket.runtime());
    assert_eq!(ticket.transmissions()[0], failed_evidence);
    assert_eq!(
        service.validate_candidate_ticket(&ticket, 120).unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert_eq!(
        reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[], 120)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert!(service.recover_failed_rerank(&mut ticket, 120).is_err());
    service.validate_candidate_ticket(&recovered, 120).unwrap();
    for usage in [
        TransmissionUsage::QueryEmbedding,
        TransmissionUsage::EntryEmbedding,
        TransmissionUsage::Rerank,
    ] {
        assert_eq!(
            service
                .prepare_transmission(&recovered, usage, RERANK_ENDPOINT, &[], 120)
                .unwrap_err(),
            MemoryError::TransmissionDenied
        );
    }
    let recovered_private = recovered
        .candidates()
        .iter()
        .find(|candidate| candidate.text() == "private preference")
        .unwrap()
        .candidate_ref();
    let recovered_shared = recovered
        .candidates()
        .iter()
        .find(|candidate| candidate.text() == "shared manual")
        .unwrap()
        .candidate_ref();
    assert_ne!(recovered_private, private_reference);
    assert_ne!(recovered_shared, shared_reference);
    assert_eq!(
        recovered.query_proof().entries(),
        std::slice::from_ref(&recovered_private)
    );
    let recovered_evidence = recovered.transmissions().remove(0);
    assert_eq!(recovered_evidence.usage(), failed_evidence.usage());
    assert_eq!(recovered_evidence.endpoint(), failed_evidence.endpoint());
    assert_eq!(
        recovered_evidence.sent_sources(),
        failed_evidence.sent_sources()
    );
    assert_eq!(
        recovered_evidence.sent_scopes(),
        failed_evidence.sent_scopes()
    );
    assert_eq!(
        recovered_evidence.authorized_at_ms(),
        failed_evidence.authorized_at_ms()
    );
    assert_eq!(
        recovered_evidence.recorded_at_ms(),
        failed_evidence.recorded_at_ms()
    );
    assert_eq!(recovered_evidence.sent_entries().len(), 2);
    assert!(
        recovered_evidence
            .sent_entries()
            .contains(&recovered_private)
    );
    assert!(
        recovered_evidence
            .sent_entries()
            .contains(&recovered_shared)
    );
    let task = reader(&mut service, &owner)
        .complete_retrieval(&recovered, &[(recovered_shared, 1.0)], 120)
        .unwrap();
    service.validate_task(&task, 120).unwrap();
    assert_eq!(task.transmissions()[0].sent_entries().len(), 2);
    assert_eq!(task.transmissions()[0].sent_scopes().len(), 2);
    assert_eq!(
        task.propose_shared(
            MemoryDraft::new(
                "must remain private",
                Category::Knowledge,
                Epistemic::Reference,
                vec![shared_source.clone()],
            ),
            120,
        )
        .unwrap_err(),
        MemoryError::PublicationDenied
    );
    assert_eq!(
        task.propose_create(draft("incomplete dependency", &shared_source), 120)
            .unwrap_err(),
        MemoryError::InvalidEvidence
    );
}

#[test]
fn rerank_recovery_rejects_pending_nonrerank_and_stale_tickets() {
    let (mut pending_service, owner, source) = private_fixture("alice", "pending input");
    grant(
        &mut pending_service,
        &owner,
        TransmissionUsage::Rerank,
        vec![source.clone()],
    );
    let mut pending = reader(&mut pending_service, &owner)
        .begin_candidates(&RetrievalQuery::new(""), plan(), 100)
        .unwrap();
    bind(&mut pending_service, &mut pending, &source, &[]);
    let pending_entry = pending.candidates()[0].candidate_ref();
    let _pending_receipt = pending_service
        .prepare_transmission(
            &pending,
            TransmissionUsage::Rerank,
            RERANK_ENDPOINT,
            &[pending_entry],
            100,
        )
        .unwrap();
    assert!(
        pending_service
            .recover_failed_rerank(&mut pending, 110)
            .is_err()
    );
    assert!(
        pending_service
            .validate_candidate_ticket(&pending, 110)
            .is_err()
    );

    let (mut other_service, other_owner, other_source) =
        private_fixture("alice", "entry embedding input");
    grant(
        &mut other_service,
        &other_owner,
        TransmissionUsage::EntryEmbedding,
        vec![other_source],
    );
    let mut entry_ticket = reader(&mut other_service, &other_owner)
        .begin_candidates(&RetrievalQuery::new(""), plan(), 100)
        .unwrap();
    let entry = entry_ticket.candidates()[0].candidate_ref();
    let receipt = other_service
        .prepare_transmission(
            &entry_ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            &[entry],
            100,
        )
        .unwrap();
    other_service
        .fail_transmission(&mut entry_ticket, &receipt, 110)
        .unwrap();
    assert!(
        other_service
            .recover_failed_rerank(&mut entry_ticket, 120)
            .is_err()
    );
}

#[test]
fn rerank_recovery_rejects_revoked_updated_and_expired_snapshots() {
    let (mut revoked_service, owner, source) = private_fixture("alice", "revoked input");
    let mut revoked_ticket = failed_rerank_ticket(&mut revoked_service, &owner, &source);
    revoked_service
        .set_read_policy(&owner, ReadPolicy::deny_all(2))
        .unwrap();
    assert!(
        revoked_service
            .recover_failed_rerank(&mut revoked_ticket, 120)
            .is_err()
    );

    let (mut updated_service, updated_owner, updated_source) =
        private_fixture("alice", "original input");
    let mut updated_ticket =
        failed_rerank_ticket(&mut updated_service, &updated_owner, &updated_source);
    let version = updated_ticket.candidates()[0]
        .candidate_ref()
        .record()
        .unwrap();
    updated_service
        .for_pet(&updated_owner)
        .update(
            version.id(),
            version.revision(),
            draft("updated input", &updated_source),
            120,
        )
        .unwrap();
    assert!(
        updated_service
            .recover_failed_rerank(&mut updated_ticket, 120)
            .is_err()
    );

    let (mut expired_service, expired_owner, expired_source) =
        private_fixture("alice", "stable input");
    expired_service
        .for_pet(&expired_owner)
        .create(
            draft("expires at boundary", &expired_source).with_expiration(150),
            100,
        )
        .unwrap();
    expired_service
        .set_read_policy(
            &expired_owner,
            ReadPolicy::offline(2, vec![expired_source.clone()]),
        )
        .unwrap();
    grant(
        &mut expired_service,
        &expired_owner,
        TransmissionUsage::Rerank,
        vec![expired_source.clone()],
    );
    let mut expired_ticket = reader(&mut expired_service, &expired_owner)
        .begin_candidates(&RetrievalQuery::new("expires"), plan(), 100)
        .unwrap();
    bind(
        &mut expired_service,
        &mut expired_ticket,
        &expired_source,
        &[],
    );
    let expired_entry = expired_ticket.candidates()[0].candidate_ref();
    let expired_receipt = expired_service
        .prepare_transmission(
            &expired_ticket,
            TransmissionUsage::Rerank,
            RERANK_ENDPOINT,
            &[expired_entry],
            100,
        )
        .unwrap();
    expired_service
        .fail_transmission(&mut expired_ticket, &expired_receipt, 110)
        .unwrap();
    assert!(
        expired_service
            .recover_failed_rerank(&mut expired_ticket, 150)
            .is_err()
    );
}

#[test]
fn query_permissions_require_both_provenance_and_explicit_scope() {
    for usage in [TransmissionUsage::QueryEmbedding, TransmissionUsage::Rerank] {
        let (mut service, owner, source) = private_fixture("alice", "tea note");
        grant(&mut service, &owner, usage, vec![source.clone()]);
        let mut ticket = reader(&mut service, &owner)
            .begin_candidates(&RetrievalQuery::new("private query"), plan(), 100)
            .unwrap();
        let entries = if usage == TransmissionUsage::Rerank {
            vec![ticket.candidates()[0].candidate_ref()]
        } else {
            vec![]
        };
        assert!(!ticket.query_proof().is_bound());
        assert_eq!(
            service
                .prepare_transmission(&ticket, usage, RERANK_ENDPOINT, &entries, 100)
                .unwrap_err(),
            MemoryError::TransmissionDenied
        );
        bind(&mut service, &mut ticket, &source, &[]);
        assert_eq!(
            ticket.query_proof().data_scope(),
            &MemoryScope::Private(owner.clone())
        );
        assert_eq!(ticket.query_proof().sources(), &[source]);
        let sent = service
            .prepare_transmission(&ticket, usage, RERANK_ENDPOINT, &entries, 100)
            .unwrap();
        assert!(sent.include_query());
        service
            .record_transmission(&mut ticket, &sent, 110)
            .unwrap();
        let task = reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[], 110)
            .unwrap();
        assert_eq!(task.transmissions().len(), 1);
    }
}

#[test]
fn shared_search_cannot_authorize_private_query_or_infer_scopes_from_sources() {
    let (mut service, owner, source) = private_fixture("alice", "tea note");
    service
        .set_transmission_policy(
            &owner,
            TransmissionPolicy::new(
                1,
                TransmissionUsage::QueryEmbedding,
                RERANK_ENDPOINT,
                vec![source.clone()],
            )
            .unwrap()
            .with_scope(MemoryScope::Shared(namespace()))
            .transmit_query(),
        )
        .unwrap();
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(
            &RetrievalQuery::new("private query").with_scope(SearchScope::Shared),
            plan(),
            100,
        )
        .unwrap();
    assert!(ticket.candidates().is_empty());
    bind(&mut service, &mut ticket, &source, &[]);
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::QueryEmbedding,
                RERANK_ENDPOINT,
                &[],
                100
            )
            .unwrap_err(),
        MemoryError::TransmissionDenied
    );
    service
        .set_transmission_policy(
            &owner,
            TransmissionPolicy::new(
                2,
                TransmissionUsage::QueryEmbedding,
                RERANK_ENDPOINT,
                vec![source.clone()],
            )
            .unwrap()
            .transmit_query(),
        )
        .unwrap();
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("private query"), plan(), 100)
        .unwrap();
    bind(&mut service, &mut ticket, &source, &[]);
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::QueryEmbedding,
                RERANK_ENDPOINT,
                &[],
                100
            )
            .unwrap_err(),
        MemoryError::TransmissionDenied
    );
}

#[test]
fn query_provenance_rejects_missing_foreign_duplicate_and_incomplete_sources() {
    let (mut service, owner, source) = private_fixture("alice", "tea note");
    let foreign = service
        .register_source(
            MemoryScope::Private(pet("bob")),
            "foreign",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    for sources in [vec![], vec![foreign], vec![source.clone(), source.clone()]] {
        assert_eq!(
            service
                .bind_query_provenance(&mut ticket, sources, &[], 100)
                .unwrap_err(),
            MemoryError::InvalidEvidence
        );
    }
    let entry = ticket.candidates()[0].candidate_ref();
    bind(&mut service, &mut ticket, &source, &[entry]);
    assert_eq!(
        service
            .bind_query_provenance(&mut ticket, vec![source], &[], 100)
            .unwrap_err(),
        MemoryError::InvalidState
    );
}

#[test]
fn transmission_usages_endpoint_and_query_flag_are_independent() {
    let (mut service, owner, source) = private_fixture("alice", "tea note");
    service
        .set_transmission_policy(
            &owner,
            TransmissionPolicy::new(
                1,
                TransmissionUsage::QueryEmbedding,
                RERANK_ENDPOINT,
                vec![source.clone()],
            )
            .unwrap()
            .with_scope(MemoryScope::Private(owner.clone())),
        )
        .unwrap();
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    bind(&mut service, &mut ticket, &source, &[]);
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::QueryEmbedding,
                RERANK_ENDPOINT,
                &[],
                100
            )
            .unwrap_err(),
        MemoryError::TransmissionDenied
    );
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source.clone()],
    );
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    let entry = ticket.candidates()[0].candidate_ref();
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::Rerank,
                RERANK_ENDPOINT,
                std::slice::from_ref(&entry),
                100
            )
            .unwrap_err(),
        MemoryError::TransmissionDenied
    );
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::EntryEmbedding,
                "https://other.example.test",
                std::slice::from_ref(&entry),
                100
            )
            .unwrap_err(),
        MemoryError::TransmissionDenied
    );
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                &[],
                100
            )
            .unwrap_err(),
        MemoryError::InvalidInput("transmission entries do not match usage")
    );
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            &[entry],
            100,
        )
        .unwrap();
    assert!(!sent.include_query());
    assert_eq!(sent.sent_sources(), &[source]);
}

#[test]
fn receipt_binding_rejects_foreign_session_query_and_cloned_service_runtime_collisions() {
    let (mut service, owner, source) = private_fixture("alice", "tea note");
    grant(
        &mut service,
        &owner,
        TransmissionUsage::QueryEmbedding,
        vec![source.clone()],
    );
    let mut twin = service.clone();
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    let mut collision = reader(&mut twin, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    assert_eq!(ticket.runtime(), collision.runtime());
    bind(&mut service, &mut ticket, &source, &[]);
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::QueryEmbedding,
            RERANK_ENDPOINT,
            &[],
            100,
        )
        .unwrap();
    assert_eq!(
        twin.record_transmission(&mut collision, &sent, 110)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    let other_session = SessionId::new("other").unwrap();
    let mut other = service
        .for_session(&owner, &other_session, ReadPurpose::OfflineReasoning)
        .begin_candidates(
            &RetrievalQuery::new("another query").with_scope(SearchScope::Shared),
            plan(),
            100,
        )
        .unwrap();
    assert_eq!(
        service
            .record_transmission(&mut other, &sent, 110)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert!(other.transmissions().is_empty());
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    assert_eq!(
        service
            .record_transmission(&mut ticket, &sent, 110)
            .unwrap_err(),
        MemoryError::InvalidEvidence
    );
}

#[test]
fn pending_sends_block_delivery_and_completed_tickets_cannot_gain_new_sends() {
    let (mut service, owner, source) = private_fixture("alice", "tea note");
    grant(
        &mut service,
        &owner,
        TransmissionUsage::QueryEmbedding,
        vec![source.clone()],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    bind(&mut service, &mut ticket, &source, &[]);
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::QueryEmbedding,
            RERANK_ENDPOINT,
            &[],
            100,
        )
        .unwrap();
    assert_eq!(
        reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[], 110)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[], 110)
        .unwrap();
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::QueryEmbedding,
                RERANK_ENDPOINT,
                &[],
                120
            )
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert_eq!(
        service
            .bind_query_provenance(&mut ticket, vec![source], &[], 120)
            .unwrap_err(),
        MemoryError::InvalidState
    );
    service.validate_task(&task, 120).unwrap();
}

#[test]
fn receipt_failure_keeps_audit_and_permanently_invalidates_ticket() {
    for backwards in [false, true] {
        let (mut service, owner, source) = private_fixture("alice", "tea note");
        grant(
            &mut service,
            &owner,
            TransmissionUsage::QueryEmbedding,
            vec![source.clone()],
        );
        let mut ticket = reader(&mut service, &owner)
            .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
            .unwrap();
        bind(&mut service, &mut ticket, &source, &[]);
        let sent = service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::QueryEmbedding,
                RERANK_ENDPOINT,
                &[],
                100,
            )
            .unwrap();
        if !backwards {
            service
                .set_transmission_policy(
                    &owner,
                    TransmissionPolicy::deny_all(2, TransmissionUsage::QueryEmbedding),
                )
                .unwrap();
        }
        assert_eq!(
            service
                .record_transmission(&mut ticket, &sent, if backwards { 99 } else { 110 })
                .unwrap_err(),
            MemoryError::TaskCancelled
        );
        assert_eq!(ticket.transmissions().len(), 1);
        assert_eq!(
            reader(&mut service, &owner)
                .complete_retrieval(&ticket, &[], 100)
                .unwrap_err(),
            MemoryError::TaskCancelled
        );
    }
}

#[test]
fn actual_send_evidence_is_bounded_before_dispatch() {
    let (mut service, owner, source) = private_fixture("alice", "tea note");
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source.clone()],
    );
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    let entry = ticket.candidates()[0].candidate_ref();
    for _ in 0..8 {
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                std::slice::from_ref(&entry),
                100,
            )
            .unwrap();
    }
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                &[entry],
                100
            )
            .unwrap_err(),
        MemoryError::CapacityExceeded
    );
}

#[test]
fn all_sent_entries_and_query_dependencies_expire_at_equality() {
    for query_dependency in [false, true] {
        let (mut service, owner, source) = private_fixture("alice", "stable note");
        service
            .for_pet(&owner)
            .create(draft("fleeting note", &source).with_expiration(500), 100)
            .unwrap();
        grant(
            &mut service,
            &owner,
            TransmissionUsage::Rerank,
            vec![source.clone()],
        );
        let mut ticket = reader(&mut service, &owner)
            .begin_candidates(&RetrievalQuery::new("note").with_limit(1), plan(), 100)
            .unwrap();
        let fleeting = ticket
            .candidates()
            .iter()
            .find(|item| item.text() == "fleeting note")
            .unwrap()
            .candidate_ref();
        let stable = ticket
            .candidates()
            .iter()
            .find(|item| item.text() == "stable note")
            .unwrap()
            .candidate_ref();
        bind(
            &mut service,
            &mut ticket,
            &source,
            if query_dependency {
                std::slice::from_ref(&fleeting)
            } else {
                &[]
            },
        );
        let sends = if query_dependency {
            vec![stable.clone()]
        } else {
            vec![fleeting.clone(), stable.clone()]
        };
        assert_eq!(
            service
                .prepare_transmission(
                    &ticket,
                    TransmissionUsage::Rerank,
                    RERANK_ENDPOINT,
                    &sends,
                    500
                )
                .unwrap_err(),
            MemoryError::TaskCancelled
        );
        let sent = service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::Rerank,
                RERANK_ENDPOINT,
                &sends,
                499,
            )
            .unwrap();
        service
            .record_transmission(&mut ticket, &sent, 499)
            .unwrap();
        let task = reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[(stable.clone(), 1.0)], 499)
            .unwrap();
        assert_eq!(
            service.validate_task(&task, 500).unwrap_err(),
            MemoryError::TaskCancelled
        );
        assert_eq!(
            reader(&mut service, &owner)
                .complete_retrieval(&ticket, &[(stable, 1.0)], 500)
                .unwrap_err(),
            MemoryError::TaskCancelled
        );
    }
}

#[test]
fn expiry_during_transmission_preserves_audit_and_blocks_even_empty_delivery() {
    let (mut service, owner, source) = private_fixture("alice", "stable note");
    service
        .for_pet(&owner)
        .create(draft("fleeting note", &source).with_expiration(500), 100)
        .unwrap();
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
        .unwrap();
    let fleeting = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "fleeting note")
        .unwrap()
        .candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            &[fleeting],
            499,
        )
        .unwrap();
    assert_eq!(
        service
            .record_transmission(&mut ticket, &sent, 500)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert_eq!(ticket.transmissions().len(), 1);
    assert_eq!(
        reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[], 500)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
}

fn publish_seed(service: &mut MemoryService, owner: &PetIdentity, source: &SourceRef) {
    service
        .set_publication_policy(
            owner,
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    let task = reader(service, owner)
        .begin_task(
            &RetrievalQuery::new("").with_scope(SearchScope::Shared),
            100,
        )
        .unwrap();
    let candidate = task
        .propose_shared(
            MemoryDraft::new(
                "shared manual",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source.clone()],
            ),
            100,
        )
        .unwrap();
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    assert!(matches!(
        command.execute(service).unwrap(),
        CommandResult::Write(_)
    ));
}

#[test]
fn private_entry_scope_survives_shared_sources_and_top_k() {
    let mut service = MemoryService::new();
    let owner = pet("alice");
    let source = service
        .register_source(
            MemoryScope::Shared(namespace()),
            "manual",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service
        .for_pet(&owner)
        .create(
            MemoryDraft::new(
                "private preference",
                Category::Episode,
                Epistemic::Reference,
                vec![source.clone()],
            ),
            100,
        )
        .unwrap();
    service
        .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    publish_seed(&mut service, &owner, &source);
    service
        .set_transmission_policy(
            &owner,
            TransmissionPolicy::new(
                1,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                vec![source.clone()],
            )
            .unwrap()
            .with_scope(MemoryScope::Shared(namespace())),
        )
        .unwrap();
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new(""), plan(), 100)
        .unwrap();
    let private = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "private preference")
        .unwrap()
        .candidate_ref();
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                std::slice::from_ref(&private),
                100
            )
            .unwrap_err(),
        MemoryError::TransmissionDenied
    );
    service
        .set_transmission_policy(
            &owner,
            TransmissionPolicy::new(
                2,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                vec![source.clone()],
            )
            .unwrap()
            .with_scope(MemoryScope::Private(owner.clone()))
            .with_scope(MemoryScope::Shared(namespace())),
        )
        .unwrap();
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new(""), plan(), 100)
        .unwrap();
    let shared = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "shared manual")
        .unwrap()
        .candidate_ref();
    let private = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "private preference")
        .unwrap()
        .candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            &[private, shared.clone()],
            100,
        )
        .unwrap();
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[(shared, 1.0)], 110)
        .unwrap();
    assert_eq!(task.transmissions()[0].sent_scopes().len(), 2);
    assert_eq!(
        task.propose_shared(
            MemoryDraft::new(
                "must not publish",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source]
            ),
            110
        )
        .unwrap_err(),
        MemoryError::PublicationDenied
    );
}

#[test]
fn shared_entry_only_transmission_can_still_publish_shared_derivatives() {
    let mut service = MemoryService::new();
    let owner = pet("alice");
    let source = service
        .register_source(
            MemoryScope::Shared(namespace()),
            "manual",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service
        .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    publish_seed(&mut service, &owner, &source);
    service
        .set_transmission_policy(
            &owner,
            TransmissionPolicy::new(
                1,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                vec![source.clone()],
            )
            .unwrap()
            .with_scope(MemoryScope::Shared(namespace())),
        )
        .unwrap();
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(
            &RetrievalQuery::new("").with_scope(SearchScope::Shared),
            plan(),
            100,
        )
        .unwrap();
    let shared = ticket.candidates()[0].candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            std::slice::from_ref(&shared),
            100,
        )
        .unwrap();
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[(shared, 1.0)], 110)
        .unwrap();
    let candidate = task
        .propose_shared(
            MemoryDraft::new(
                "shared derivative",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source],
            ),
            110,
        )
        .unwrap();
    service
        .prepare_shared_publication(&candidate, 110)
        .unwrap()
        .execute(&mut service)
        .unwrap();
}

#[test]
fn private_query_blocks_shared_publication_even_without_private_candidates() {
    let mut service = MemoryService::new();
    let owner = pet("alice");
    let source = service
        .register_source(
            MemoryScope::Shared(namespace()),
            "manual",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service
        .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    publish_seed(&mut service, &owner, &source);
    grant(
        &mut service,
        &owner,
        TransmissionUsage::Rerank,
        vec![source.clone()],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(
            &RetrievalQuery::new("private query").with_scope(SearchScope::Shared),
            plan(),
            100,
        )
        .unwrap();
    bind(&mut service, &mut ticket, &source, &[]);
    let shared = ticket.candidates()[0].candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::Rerank,
            RERANK_ENDPOINT,
            std::slice::from_ref(&shared),
            100,
        )
        .unwrap();
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[(shared, 1.0)], 110)
        .unwrap();
    assert_eq!(
        task.propose_shared(
            MemoryDraft::new(
                "must not publish",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source]
            ),
            110
        )
        .unwrap_err(),
        MemoryError::PublicationDenied
    );
}

#[test]
fn all_actual_send_sources_are_required_for_derived_creation() {
    let (mut service, owner, source) = private_fixture("alice", "A note");
    let second = service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "second",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    service
        .for_pet(&owner)
        .create(draft("B note", &second), 100)
        .unwrap();
    service
        .set_read_policy(
            &owner,
            ReadPolicy::offline(2, vec![source.clone(), second.clone()]),
        )
        .unwrap();
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source.clone(), second.clone()],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
        .unwrap();
    let entries: Vec<_> = ticket
        .candidates()
        .iter()
        .map(|item| item.candidate_ref())
        .collect();
    let selected = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "B note")
        .unwrap()
        .candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            &entries,
            100,
        )
        .unwrap();
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[(selected, 1.0)], 110)
        .unwrap();
    assert_eq!(
        service
            .create_from_task(&task, draft("bad derivative", &second), 110)
            .unwrap_err(),
        MemoryError::InvalidEvidence
    );
    service
        .create_from_task(
            &task,
            MemoryDraft::new(
                "good derivative",
                Category::Episode,
                Epistemic::Inference,
                vec![source, second],
            ),
            110,
        )
        .unwrap();
}

#[test]
fn unselected_sends_and_query_records_are_persistent_dependencies_even_with_same_source() {
    for query_dependency in [false, true] {
        let (mut service, owner, source) = private_fixture("alice", "A note");
        service
            .for_pet(&owner)
            .create(draft("B note", &source), 100)
            .unwrap();
        grant(
            &mut service,
            &owner,
            TransmissionUsage::Rerank,
            vec![source.clone()],
        );
        let mut ticket = reader(&mut service, &owner)
            .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
            .unwrap();
        let first = ticket
            .candidates()
            .iter()
            .find(|item| item.text() == "A note")
            .unwrap()
            .candidate_ref();
        let second = ticket
            .candidates()
            .iter()
            .find(|item| item.text() == "B note")
            .unwrap()
            .candidate_ref();
        bind(
            &mut service,
            &mut ticket,
            &source,
            if query_dependency {
                std::slice::from_ref(&first)
            } else {
                &[]
            },
        );
        let sends = if query_dependency {
            vec![second.clone()]
        } else {
            vec![first.clone(), second.clone()]
        };
        let sent = service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::Rerank,
                RERANK_ENDPOINT,
                &sends,
                100,
            )
            .unwrap();
        service
            .record_transmission(&mut ticket, &sent, 110)
            .unwrap();
        let task = reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[(second, 1.0)], 110)
            .unwrap();
        let created = service
            .create_from_task(&task, draft("derived note", &source), 110)
            .unwrap();
        let version = first.record().unwrap();
        service
            .for_pet(&owner)
            .delete(version.id(), version.revision(), 120)
            .unwrap();
        assert!(
            !service
                .for_pet(&owner)
                .status(created.id(), 120)
                .unwrap()
                .is_available()
        );
    }
}

#[test]
fn sent_working_context_cannot_be_persisted_even_when_final_result_is_empty() {
    let (mut service, owner, source) = private_fixture("alice", "note");
    service
        .put_working_context(
            &owner,
            &session(),
            "scratch",
            1,
            MemoryDraft::new(
                "working note",
                Category::WorkingContext,
                Epistemic::UserStatement,
                vec![source.clone()],
            )
            .with_lifecycle(Lifecycle::ShortTerm),
            100,
        )
        .unwrap();
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source.clone()],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
        .unwrap();
    let entry = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "working note")
        .unwrap()
        .candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            &[entry],
            100,
        )
        .unwrap();
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[], 110)
        .unwrap();
    assert_eq!(
        service
            .create_from_task(&task, draft("bad derivative", &source), 110)
            .unwrap_err(),
        MemoryError::InvalidInput("session-derived output cannot be persisted")
    );
}

#[test]
fn query_sources_and_provenance_remain_part_of_persistent_evidence() {
    let (mut service, owner, source) = private_fixture("alice", "note");
    let query_source = service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "query",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    service
        .set_read_policy(
            &owner,
            ReadPolicy::offline(2, vec![source.clone(), query_source.clone()]),
        )
        .unwrap();
    grant(
        &mut service,
        &owner,
        TransmissionUsage::QueryEmbedding,
        vec![query_source.clone()],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("private query"), plan(), 100)
        .unwrap();
    bind(&mut service, &mut ticket, &query_source, &[]);
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::QueryEmbedding,
            RERANK_ENDPOINT,
            &[],
            100,
        )
        .unwrap();
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[], 110)
        .unwrap();
    assert_eq!(
        service
            .create_from_task(&task, draft("bad derivative", &source), 110)
            .unwrap_err(),
        MemoryError::InvalidEvidence
    );
    let created = service
        .create_from_task(&task, draft("derived note", &query_source), 110)
        .unwrap();
    service.revoke_source(&query_source).unwrap();
    assert!(
        !service
            .for_pet(&owner)
            .status(created.id(), 120)
            .unwrap()
            .is_available()
    );
}

fn wire_flow(send_first: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (mut service, owner, source) = private_fixture("alice", "A note");
    service
        .for_pet(&owner)
        .create(draft("B note", &source), 100)
        .unwrap();
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source.clone()],
    );
    let keyword = reader(&mut service, &owner)
        .begin_task(&RetrievalQuery::new("note"), 110)
        .unwrap();
    let keyword_request = keyword.request_bytes().to_vec();
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("note"), plan(), 100)
        .unwrap();
    let first = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "A note")
        .unwrap()
        .candidate_ref();
    let second = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "B note")
        .unwrap()
        .candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            &[if send_first { first } else { second.clone() }],
            100,
        )
        .unwrap();
    service
        .record_transmission(&mut ticket, &sent, 110)
        .unwrap();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[(second, 1.0)], 110)
        .unwrap();
    let request = task.request_bytes().to_vec();
    let wire = task
        .propose_create(draft("derived note", &source), 110)
        .unwrap()
        .to_bytes()
        .unwrap();
    let restored = moe_memory_core::MemoryCommand::from_bytes(&wire).unwrap();
    assert_eq!(
        restored.execute(&mut service).unwrap_err(),
        MemoryError::TaskCancelled
    );
    (keyword_request, request, wire)
}

#[test]
fn actual_sends_distinguish_hybrid_requests_without_changing_keyword_layout_or_live_proofs() {
    let first = wire_flow(true);
    let second = wire_flow(false);
    assert_eq!(first, wire_flow(true));
    assert_eq!(first.0, second.0);
    assert_ne!(first.1, second.1);
    assert_ne!(first.2, second.2);
    assert_eq!(&first.0[..8], &110_u64.to_le_bytes());
    assert_eq!(&first.0[8..16], &4_u64.to_le_bytes());
    assert!(first.1.starts_with(b"MOE-HYBRID-TASK\0\x01"));
}

#[test]
fn profile_and_policy_input_limits_are_checked() {
    assert!(EmbeddingProfile::new("fixture", "model", 0).is_err());
    assert!(EmbeddingProfile::new("fixture", "model", 4097).is_err());
    assert!(EmbeddingProfile::new("", "model", 8).is_err());
    assert!(moe_memory_core::RerankProfile::new("fixture", "").is_err());
    assert!(
        TransmissionPolicy::new(0, TransmissionUsage::Rerank, RERANK_ENDPOINT, vec![]).is_err()
    );
    assert!(
        TransmissionPolicy::new(1, TransmissionUsage::Rerank, "bad\nendpoint", vec![]).is_err()
    );
    let (mut service, owner, source) = private_fixture("alice", "note");
    let foreign_scope = TransmissionPolicy::new(
        1,
        TransmissionUsage::EntryEmbedding,
        RERANK_ENDPOINT,
        vec![source],
    )
    .unwrap()
    .with_scope(MemoryScope::Private(pet("bob")));
    assert_eq!(
        service
            .set_transmission_policy(&owner, foreign_scope)
            .unwrap_err(),
        MemoryError::InvalidEvidence
    );
}

#[test]
fn hybrid_identity_stays_protected_when_memory_is_semantically_selected() {
    let (mut service, owner, source) = private_fixture("alice", "tea note");
    let authority = service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    service
        .set_identity_view(&owner, 1, "trusted identity", vec![authority.clone()])
        .unwrap();
    service
        .set_read_policy(
            &owner,
            ReadPolicy::offline(2, vec![source, authority.clone()]),
        )
        .unwrap();
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("preference"), plan(), 100)
        .unwrap();
    let entry = ticket.candidates()[0].candidate_ref();
    let task = reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[(entry, 1.0)], 100)
        .unwrap();
    assert!(matches!(
        service.assemble_context(&task, 1, 100),
        Err(MemoryError::ContextBudgetExceeded { .. })
    ));
    let context = service.assemble_context(&task, 512, 100).unwrap();
    assert_eq!(
        service
            .context_view(&context, 100)
            .unwrap()
            .identity()
            .unwrap()
            .text(),
        "trusted identity"
    );
    service.revoke_source(&authority).unwrap();
    assert_eq!(
        service.validate_task(&task, 100).unwrap_err(),
        MemoryError::TaskCancelled
    );
}

#[test]
fn disabled_rerank_plan_cannot_authorize_a_rerank_call() {
    let (mut service, owner, source) = private_fixture("alice", "tea note");
    grant(
        &mut service,
        &owner,
        TransmissionUsage::Rerank,
        vec![source.clone()],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(
            &RetrievalQuery::new("tea"),
            RetrievalPlan::new(EmbeddingProfile::new("fixture", "fixture-embedding", 8).unwrap()),
            100,
        )
        .unwrap();
    bind(&mut service, &mut ticket, &source, &[]);
    let entry = ticket.candidates()[0].candidate_ref();
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::Rerank,
                RERANK_ENDPOINT,
                &[entry],
                100
            )
            .unwrap_err(),
        MemoryError::TransmissionDenied
    );
}

#[test]
fn expired_dependencies_prevent_sending_an_otherwise_live_record() {
    let (mut service, owner, source) = private_fixture("alice", "stable note");
    let ancestor = service
        .for_pet(&owner)
        .create(draft("ancestor", &source).with_expiration(500), 100)
        .unwrap();
    let version = service
        .for_pet(&owner)
        .get(ancestor.id(), 100)
        .unwrap()
        .version_ref();
    service
        .for_pet(&owner)
        .create(
            draft("live child", &source).with_dependencies(vec![version]),
            100,
        )
        .unwrap();
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source],
    );
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("child"), plan(), 100)
        .unwrap();
    let entry = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "live child")
        .unwrap()
        .candidate_ref();
    assert_eq!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                &[entry],
                500
            )
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
}

#[test]
fn colliding_record_versions_and_unbound_snapshots_cannot_cross_ticket_boundaries() {
    let (mut service, owner, source) = private_fixture("alice", "same text");
    let (mut foreign, foreign_owner, _) = private_fixture("alice", "same text");
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source],
    );
    let ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("same"), plan(), 100)
        .unwrap();
    let foreign_ticket = reader(&mut foreign, &foreign_owner)
        .begin_candidates(&RetrievalQuery::new("same"), plan(), 100)
        .unwrap();
    let another = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("same"), plan(), 100)
        .unwrap();
    let own = ticket.candidates()[0].candidate_ref();
    let foreign_ref = foreign_ticket.candidates()[0].candidate_ref();
    assert_eq!(own.record(), foreign_ref.record());
    let raw = ticket.candidates()[0].item().candidate_ref();
    let another_ref = another.candidates()[0].candidate_ref();
    for reference in [foreign_ref, raw, another_ref] {
        assert_eq!(
            reader(&mut service, &owner)
                .complete_retrieval(&ticket, &[(reference.clone(), 1.0)], 100)
                .unwrap_err(),
            MemoryError::InvalidEvidence
        );
        assert_eq!(
            service
                .prepare_transmission(
                    &ticket,
                    TransmissionUsage::EntryEmbedding,
                    RERANK_ENDPOINT,
                    &[reference],
                    100
                )
                .unwrap_err(),
            MemoryError::InvalidEvidence
        );
    }
    reader(&mut service, &owner)
        .complete_retrieval(&ticket, &[(own, 1.0)], 100)
        .unwrap();
}

#[test]
fn failed_network_receipt_is_audited_and_permanently_closes_the_ticket() {
    let (mut service, owner, source) = private_fixture("alice", "network failure input");
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source],
    );
    let mut ticket = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("query"), plan(), 100)
        .unwrap();
    let entry = ticket.candidates()[0].candidate_ref();
    let sent = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            std::slice::from_ref(&entry),
            100,
        )
        .unwrap();
    service.fail_transmission(&mut ticket, &sent, 110).unwrap();
    assert_eq!(ticket.transmissions().len(), 1);
    assert!(
        reader(&mut service, &owner)
            .complete_retrieval(&ticket, &[], 110)
            .is_err()
    );
    assert!(
        service
            .prepare_transmission(
                &ticket,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                &[entry],
                110
            )
            .is_err()
    );
    assert!(
        service
            .record_transmission(&mut ticket, &sent, 110)
            .is_err()
    );
}

#[test]
fn foreign_failure_receipt_cannot_poison_another_ticket() {
    let (mut service, owner, source) = private_fixture("alice", "network failure input");
    grant(
        &mut service,
        &owner,
        TransmissionUsage::EntryEmbedding,
        vec![source],
    );
    let mut first = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("query"), plan(), 100)
        .unwrap();
    let mut second = reader(&mut service, &owner)
        .begin_candidates(&RetrievalQuery::new("query"), plan(), 100)
        .unwrap();
    let sent = service
        .prepare_transmission(
            &first,
            TransmissionUsage::EntryEmbedding,
            RERANK_ENDPOINT,
            &[first.candidates()[0].candidate_ref()],
            100,
        )
        .unwrap();
    assert!(service.fail_transmission(&mut second, &sent, 110).is_err());
    assert!(second.transmissions().is_empty());
    reader(&mut service, &owner)
        .complete_retrieval(&second, &[], 110)
        .unwrap();
    service.record_transmission(&mut first, &sent, 110).unwrap();
    reader(&mut service, &owner)
        .complete_retrieval(&first, &[], 110)
        .unwrap();
}

fn divergent_services() -> (MemoryService, MemoryService, PetIdentity, SourceRef) {
    let (mut original, owner, source) = private_fixture("alice", "branch original");
    let root = reader(&mut original, &owner)
        .begin_task(&RetrievalQuery::new("branch"), 100)
        .unwrap()
        .result()
        .items()[0]
        .candidate_ref()
        .record()
        .unwrap();
    original
        .for_pet(&owner)
        .create(draft("stable note", &source), 100)
        .unwrap();
    for usage in [
        TransmissionUsage::EntryEmbedding,
        TransmissionUsage::QueryEmbedding,
        TransmissionUsage::Rerank,
    ] {
        grant(&mut original, &owner, usage, vec![source.clone()]);
    }
    let mut fork = original.clone();
    original
        .for_pet(&owner)
        .update(root.id(), root.revision(), draft("branch A", &source), 200)
        .unwrap();
    fork.for_pet(&owner)
        .update(root.id(), root.revision(), draft("branch B", &source), 200)
        .unwrap();
    assert_eq!(original.generation(), fork.generation());
    (original, fork, owner, source)
}

#[test]
fn candidate_ticket_validation_checks_unselected_full_snapshots() {
    let (mut original, mut fork, owner, source) = divergent_services();
    let mut ticket = reader(&mut original, &owner)
        .begin_candidates(&RetrievalQuery::new("branch"), plan(), 200)
        .unwrap();
    let branch_reference = ticket
        .candidates()
        .iter()
        .find(|candidate| candidate.text() == "branch A")
        .unwrap()
        .candidate_ref();
    let stable_reference = ticket
        .candidates()
        .iter()
        .find(|candidate| candidate.text() == "stable note")
        .unwrap()
        .candidate_ref();
    original
        .bind_query_provenance(
            &mut ticket,
            vec![source],
            std::slice::from_ref(&stable_reference),
            200,
        )
        .unwrap();
    original.validate_candidate_ticket(&ticket, 200).unwrap();
    assert_eq!(original.generation(), fork.generation());
    let forked_branch = reader(&mut fork, &owner)
        .retrieve(&RetrievalQuery::new("branch"), 200)
        .unwrap()
        .items()
        .iter()
        .find(|candidate| candidate.text() == "branch B")
        .unwrap()
        .candidate_ref();
    assert_eq!(forked_branch.record(), branch_reference.record());
    assert_eq!(
        fork.validate_candidate_ticket(&ticket, 200).unwrap_err(),
        MemoryError::TaskCancelled
    );
}

#[test]
fn candidate_ticket_validation_rechecks_query_provenance_and_persona() {
    let mut source_service = MemoryService::new();
    let owner = pet("alice");
    let source = source_service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "query-origin",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    source_service
        .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    let mut revoked_fork = source_service.clone();
    source_service
        .cancel_tasks(source_service.generation())
        .unwrap();
    revoked_fork.revoke_source(&source).unwrap();
    let mut source_ticket = reader(&mut source_service, &owner)
        .begin_candidates(&RetrievalQuery::new("query"), plan(), 100)
        .unwrap();
    bind(&mut source_service, &mut source_ticket, &source, &[]);
    let fork_ticket = reader(&mut revoked_fork, &owner)
        .begin_candidates(&RetrievalQuery::new("query"), plan(), 100)
        .unwrap();
    assert_eq!(source_ticket.runtime(), fork_ticket.runtime());
    assert_eq!(source_service.generation(), revoked_fork.generation());
    source_service
        .validate_candidate_ticket(&source_ticket, 100)
        .unwrap();
    assert_eq!(
        revoked_fork
            .validate_candidate_ticket(&source_ticket, 100)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );

    let (mut identity_base, identity_owner, note_source) =
        private_fixture("alice", "identity source note");
    let authority = identity_base
        .register_source(
            MemoryScope::Private(identity_owner.clone()),
            "identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    identity_base
        .set_read_policy(
            &identity_owner,
            ReadPolicy::offline(2, vec![note_source, authority.clone()]),
        )
        .unwrap();
    let mut persona_original = identity_base.clone();
    let mut persona_fork = identity_base;
    persona_original
        .set_identity_view(
            &identity_owner,
            1,
            "persona version one A",
            vec![authority.clone()],
        )
        .unwrap();
    persona_fork
        .set_identity_view(&identity_owner, 1, "persona version one B", vec![authority])
        .unwrap();
    let fork_ticket = reader(&mut persona_fork, &identity_owner)
        .begin_candidates(&RetrievalQuery::new("identity"), plan(), 100)
        .unwrap();
    let identity_ticket = reader(&mut persona_original, &identity_owner)
        .begin_candidates(&RetrievalQuery::new("identity"), plan(), 100)
        .unwrap();
    assert_eq!(identity_ticket.runtime(), fork_ticket.runtime());
    assert_eq!(persona_original.generation(), persona_fork.generation());
    persona_original
        .validate_candidate_ticket(&identity_ticket, 100)
        .unwrap();
    assert_eq!(
        persona_fork
            .validate_candidate_ticket(&identity_ticket, 100)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
}

#[test]
fn candidate_completion_rejects_divergent_snapshots_but_accepts_identical_clones() {
    let (mut original, mut fork, owner, _) = divergent_services();
    let ticket = reader(&mut original, &owner)
        .begin_candidates(&RetrievalQuery::new("branch"), plan(), 200)
        .unwrap();
    let selected = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "branch A")
        .unwrap()
        .candidate_ref();
    let keyword = reader(&mut original, &owner)
        .begin_task(&RetrievalQuery::new("branch"), 200)
        .unwrap();
    assert_eq!(
        fork.validate_task(&keyword, 200).unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert_eq!(
        reader(&mut fork, &owner)
            .complete_retrieval(&ticket, &[(selected.clone(), 1.0)], 200)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    let mut identical = original.clone();
    identical.validate_task(&keyword, 200).unwrap();
    let task = reader(&mut identical, &owner)
        .complete_retrieval(&ticket, &[(selected, 1.0)], 200)
        .unwrap();
    assert_eq!(task.result().items()[0].text(), "branch A");
    original.validate_task(&task, 200).unwrap();
}

#[test]
fn query_provenance_binding_and_preparation_reject_different_record_snapshots() {
    let (mut original, mut fork, owner, source) = divergent_services();
    let mut ticket = reader(&mut original, &owner)
        .begin_candidates(&RetrievalQuery::new("branch"), plan(), 200)
        .unwrap();
    let query_input = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "branch A")
        .unwrap()
        .candidate_ref();
    assert_eq!(
        fork.bind_query_provenance(
            &mut ticket,
            vec![source.clone()],
            std::slice::from_ref(&query_input),
            200
        )
        .unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert!(!ticket.query_proof().is_bound());
    original
        .bind_query_provenance(&mut ticket, vec![source], &[query_input], 200)
        .unwrap();
    assert_eq!(
        fork.prepare_transmission(
            &ticket,
            TransmissionUsage::QueryEmbedding,
            RERANK_ENDPOINT,
            &[],
            200
        )
        .unwrap_err(),
        MemoryError::TaskCancelled
    );
    let sent = original
        .prepare_transmission(
            &ticket,
            TransmissionUsage::QueryEmbedding,
            RERANK_ENDPOINT,
            &[],
            200,
        )
        .unwrap();
    original
        .record_transmission(&mut ticket, &sent, 201)
        .unwrap();
}

#[test]
fn receipts_cannot_be_signed_by_a_divergent_clone_even_for_the_same_original_ticket() {
    for query_input in [false, true] {
        let (mut original, mut fork, owner, source) = divergent_services();
        let mut ticket = reader(&mut original, &owner)
            .begin_candidates(&RetrievalQuery::new("branch"), plan(), 200)
            .unwrap();
        let entry = ticket
            .candidates()
            .iter()
            .find(|item| item.text() == "branch A")
            .unwrap()
            .candidate_ref();
        let (usage, entries) = if query_input {
            original
                .bind_query_provenance(&mut ticket, vec![source], &[entry], 200)
                .unwrap();
            (TransmissionUsage::QueryEmbedding, vec![])
        } else {
            (TransmissionUsage::EntryEmbedding, vec![entry])
        };
        let sent = original
            .prepare_transmission(&ticket, usage, RERANK_ENDPOINT, &entries, 200)
            .unwrap();
        assert_eq!(
            fork.record_transmission(&mut ticket, &sent, 201)
                .unwrap_err(),
            MemoryError::TaskCancelled
        );
        assert_eq!(ticket.transmissions().len(), 1);
        assert_eq!(
            reader(&mut original, &owner)
                .complete_retrieval(&ticket, &[], 201)
                .unwrap_err(),
            MemoryError::TaskCancelled
        );
    }
}

#[test]
fn unselected_sent_snapshots_survive_into_empty_and_nonempty_task_validation_and_writes() {
    for select_stable in [false, true] {
        let (mut original, mut fork, owner, source) = divergent_services();
        let mut ticket = reader(&mut original, &owner)
            .begin_candidates(&RetrievalQuery::new("branch"), plan(), 200)
            .unwrap();
        let entry = ticket
            .candidates()
            .iter()
            .find(|item| item.text() == "branch A")
            .unwrap()
            .candidate_ref();
        let stable = ticket
            .candidates()
            .iter()
            .find(|item| item.text() == "stable note")
            .unwrap()
            .candidate_ref();
        let sent = original
            .prepare_transmission(
                &ticket,
                TransmissionUsage::EntryEmbedding,
                RERANK_ENDPOINT,
                &[entry],
                200,
            )
            .unwrap();
        original
            .record_transmission(&mut ticket, &sent, 201)
            .unwrap();
        let ranking = if select_stable {
            vec![(stable, 1.0)]
        } else {
            vec![]
        };
        assert_eq!(
            reader(&mut fork, &owner)
                .complete_retrieval(&ticket, &ranking, 201)
                .unwrap_err(),
            MemoryError::TaskCancelled
        );
        let task = reader(&mut original, &owner)
            .complete_retrieval(&ticket, &ranking, 201)
            .unwrap();
        original.validate_task(&task, 201).unwrap();
        assert_eq!(
            fork.validate_task(&task, 201).unwrap_err(),
            MemoryError::TaskCancelled
        );
        assert_eq!(
            fork.assemble_context(&task, 512, 201).unwrap_err(),
            MemoryError::TaskCancelled
        );
        let generation = fork.generation();
        assert_eq!(
            fork.create_from_task(&task, draft("derived note", &source), 201)
                .unwrap_err(),
            MemoryError::TaskCancelled
        );
        assert_eq!(fork.generation(), generation);
        let command = task
            .propose_create(draft("derived note", &source), 201)
            .unwrap();
        assert_eq!(
            command.execute(&mut fork).unwrap_err(),
            MemoryError::TaskCancelled
        );
    }
}

#[test]
fn actual_query_snapshots_survive_completion_and_cannot_attach_to_forked_dependencies() {
    let (mut original, mut fork, owner, source) = divergent_services();
    let mut ticket = reader(&mut original, &owner)
        .begin_candidates(&RetrievalQuery::new("branch"), plan(), 200)
        .unwrap();
    let entry = ticket
        .candidates()
        .iter()
        .find(|item| item.text() == "branch A")
        .unwrap()
        .candidate_ref();
    original
        .bind_query_provenance(&mut ticket, vec![source.clone()], &[entry], 200)
        .unwrap();
    let sent = original
        .prepare_transmission(
            &ticket,
            TransmissionUsage::QueryEmbedding,
            RERANK_ENDPOINT,
            &[],
            200,
        )
        .unwrap();
    original
        .record_transmission(&mut ticket, &sent, 201)
        .unwrap();
    let task = reader(&mut original, &owner)
        .complete_retrieval(&ticket, &[], 201)
        .unwrap();
    original.validate_task(&task, 201).unwrap();
    assert_eq!(
        fork.validate_task(&task, 201).unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert_eq!(
        fork.create_from_task(&task, draft("query derivative", &source), 201)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
}

#[test]
fn protected_identity_snapshots_cannot_be_substituted_at_candidate_completion_or_transmission() {
    let (mut original, owner, source) = private_fixture("alice", "tea note");
    let authority = original
        .register_source(
            MemoryScope::Private(owner.clone()),
            "identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    original
        .set_read_policy(
            &owner,
            ReadPolicy::offline(2, vec![source.clone(), authority.clone()]),
        )
        .unwrap();
    grant(
        &mut original,
        &owner,
        TransmissionUsage::QueryEmbedding,
        vec![source.clone()],
    );
    let mut fork = original.clone();
    original
        .set_identity_view(&owner, 1, "identity A", vec![authority.clone()])
        .unwrap();
    fork.set_identity_view(&owner, 1, "identity B", vec![authority])
        .unwrap();
    let mut ticket = reader(&mut original, &owner)
        .begin_candidates(&RetrievalQuery::new("tea"), plan(), 100)
        .unwrap();
    assert_eq!(
        reader(&mut fork, &owner)
            .complete_retrieval(&ticket, &[], 100)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    bind(&mut original, &mut ticket, &source, &[]);
    assert_eq!(
        fork.prepare_transmission(
            &ticket,
            TransmissionUsage::QueryEmbedding,
            RERANK_ENDPOINT,
            &[],
            100
        )
        .unwrap_err(),
        MemoryError::TaskCancelled
    );
    let mut identical = original.clone();
    let sent = identical
        .prepare_transmission(
            &ticket,
            TransmissionUsage::QueryEmbedding,
            RERANK_ENDPOINT,
            &[],
            100,
        )
        .unwrap();
    original
        .record_transmission(&mut ticket, &sent, 100)
        .unwrap();
}

struct RecoveryFixture {
    service: MemoryService,
    owner: PetIdentity,
    source: SourceRef,
    branch: moe_memory_core::WriteReceipt,
    stable: moe_memory_core::WriteReceipt,
    authority: SourceRef,
}

fn recovery_fixture() -> RecoveryFixture {
    let mut service = MemoryService::new();
    let owner = pet("alice");
    let source = service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let branch = service
        .for_pet(&owner)
        .create(draft("branch original", &source), 100)
        .unwrap();
    let stable = service
        .for_pet(&owner)
        .create(draft("stable note", &source), 100)
        .unwrap();
    let authority = service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    service
        .set_read_policy(
            &owner,
            ReadPolicy::offline(2, vec![source.clone(), authority.clone()]),
        )
        .unwrap();
    grant(
        &mut service,
        &owner,
        TransmissionUsage::Rerank,
        vec![source.clone()],
    );
    RecoveryFixture {
        service,
        owner,
        source,
        branch,
        stable,
        authority,
    }
}

fn reference_by_id(
    ticket: &moe_memory_core::CandidateTicket,
    id: moe_memory_core::MemoryId,
) -> moe_memory_core::CandidateRef {
    ticket
        .candidates()
        .iter()
        .find(|candidate| {
            candidate
                .item()
                .candidate_ref()
                .record()
                .is_some_and(|version| version.id() == id)
        })
        .unwrap()
        .candidate_ref()
}

fn failed_rerank_ticket_over(
    service: &mut MemoryService,
    owner: &PetIdentity,
    source: &SourceRef,
    query_dependency: Option<moe_memory_core::MemoryId>,
    sends: &[moe_memory_core::MemoryId],
) -> moe_memory_core::CandidateTicket {
    let mut ticket = reader(service, owner)
        .begin_candidates(&RetrievalQuery::new(""), plan(), 300)
        .unwrap();
    let entries = query_dependency
        .map(|id| vec![reference_by_id(&ticket, id)])
        .unwrap_or_default();
    service
        .bind_query_provenance(&mut ticket, vec![source.clone()], &entries, 300)
        .unwrap();
    let sent: Vec<_> = sends
        .iter()
        .map(|id| reference_by_id(&ticket, *id))
        .collect();
    let pending = service
        .prepare_transmission(
            &ticket,
            TransmissionUsage::Rerank,
            RERANK_ENDPOINT,
            &sent,
            300,
        )
        .unwrap();
    service
        .fail_transmission(&mut ticket, &pending, 310)
        .unwrap();
    ticket
}

#[test]
fn failed_rerank_recovery_rejects_divergent_candidate_query_send_and_persona_snapshots() {
    let RecoveryFixture {
        mut service,
        owner,
        source,
        branch,
        stable,
        ..
    } = recovery_fixture();
    let mut fork = service.clone();
    service
        .for_pet(&owner)
        .update(
            branch.id(),
            branch.revision(),
            draft("branch A", &source),
            300,
        )
        .unwrap();
    fork.for_pet(&owner)
        .update(
            branch.id(),
            branch.revision(),
            draft("branch B", &source),
            300,
        )
        .unwrap();
    assert_eq!(service.generation(), fork.generation());
    let mut original_ticket = failed_rerank_ticket_over(
        &mut service,
        &owner,
        &source,
        None,
        &[branch.id(), stable.id()],
    );
    let fork_ticket = failed_rerank_ticket_over(
        &mut fork,
        &owner,
        &source,
        None,
        &[branch.id(), stable.id()],
    );
    assert_eq!(original_ticket.runtime(), fork_ticket.runtime());
    assert_eq!(
        original_ticket.candidates().len(),
        fork_ticket.candidates().len()
    );
    assert_eq!(
        reference_by_id(&original_ticket, branch.id()).record(),
        reference_by_id(&fork_ticket, branch.id()).record()
    );
    assert_eq!(
        fork.recover_failed_rerank(&mut original_ticket, 320)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    service
        .recover_failed_rerank(&mut original_ticket, 320)
        .unwrap();

    let RecoveryFixture {
        mut service,
        owner,
        source,
        branch: _,
        stable,
        ..
    } = recovery_fixture();
    let mut fork = service.clone();
    service
        .for_pet(&owner)
        .update(
            stable.id(),
            stable.revision(),
            draft("stable note A", &source),
            300,
        )
        .unwrap();
    fork.for_pet(&owner)
        .update(
            stable.id(),
            stable.revision(),
            draft("stable note B", &source),
            300,
        )
        .unwrap();
    assert_eq!(service.generation(), fork.generation());
    let mut original_ticket = failed_rerank_ticket_over(
        &mut service,
        &owner,
        &source,
        Some(stable.id()),
        &[stable.id()],
    );
    let fork_ticket = failed_rerank_ticket_over(
        &mut fork,
        &owner,
        &source,
        Some(stable.id()),
        &[stable.id()],
    );
    assert_eq!(original_ticket.runtime(), fork_ticket.runtime());
    assert_eq!(
        fork.recover_failed_rerank(&mut original_ticket, 320)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    service
        .recover_failed_rerank(&mut original_ticket, 320)
        .unwrap();

    let RecoveryFixture {
        mut service,
        owner,
        source,
        branch,
        stable,
        ..
    } = recovery_fixture();
    let mut fork = service.clone();
    service
        .for_pet(&owner)
        .update(
            stable.id(),
            stable.revision(),
            draft("stable note A", &source),
            300,
        )
        .unwrap();
    fork.for_pet(&owner)
        .update(
            stable.id(),
            stable.revision(),
            draft("stable note B", &source),
            300,
        )
        .unwrap();
    assert_eq!(service.generation(), fork.generation());
    let mut original_ticket = failed_rerank_ticket_over(
        &mut service,
        &owner,
        &source,
        Some(branch.id()),
        &[branch.id(), stable.id()],
    );
    let fork_ticket = failed_rerank_ticket_over(
        &mut fork,
        &owner,
        &source,
        Some(branch.id()),
        &[branch.id(), stable.id()],
    );
    assert_eq!(original_ticket.runtime(), fork_ticket.runtime());
    assert_eq!(
        fork.recover_failed_rerank(&mut original_ticket, 320)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    let recovered = service
        .recover_failed_rerank(&mut original_ticket, 320)
        .unwrap();
    assert_eq!(recovered.transmissions()[0].sent_entries().len(), 2);
    let selected = reference_by_id(&recovered, branch.id());
    let task = reader(&mut service, &owner)
        .complete_retrieval(&recovered, &[(selected, 1.0)], 320)
        .unwrap();
    service.validate_task(&task, 320).unwrap();

    let RecoveryFixture {
        mut service,
        owner,
        source,
        branch,
        stable,
        authority,
    } = recovery_fixture();
    let mut fork = service.clone();
    service
        .set_identity_view(&owner, 1, "persona anchor A", vec![authority.clone()])
        .unwrap();
    fork.set_identity_view(&owner, 1, "persona anchor B", vec![authority])
        .unwrap();
    assert_eq!(service.generation(), fork.generation());
    let mut original_ticket = failed_rerank_ticket_over(
        &mut service,
        &owner,
        &source,
        None,
        &[branch.id(), stable.id()],
    );
    let mut fork_ticket = failed_rerank_ticket_over(
        &mut fork,
        &owner,
        &source,
        None,
        &[branch.id(), stable.id()],
    );
    assert_eq!(original_ticket.runtime(), fork_ticket.runtime());
    fork.recover_failed_rerank(&mut fork_ticket, 320).unwrap();
    assert_eq!(
        fork.recover_failed_rerank(&mut original_ticket, 320)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    service
        .recover_failed_rerank(&mut original_ticket, 320)
        .unwrap();
}

#[test]
fn unforked_clone_recovers_failed_rerank_once_and_can_only_close_out() {
    let RecoveryFixture {
        mut service,
        owner,
        source,
        branch,
        stable,
        ..
    } = recovery_fixture();
    let mut ticket = failed_rerank_ticket_over(
        &mut service,
        &owner,
        &source,
        Some(branch.id()),
        &[branch.id(), stable.id()],
    );
    let mut identical = service.clone();
    let recovered = identical.recover_failed_rerank(&mut ticket, 320).unwrap();
    assert_ne!(recovered.runtime(), ticket.runtime());
    assert_eq!(
        recovered.query_proof().entries(),
        std::slice::from_ref(&reference_by_id(&recovered, branch.id()))
    );
    assert_eq!(recovered.transmissions()[0].sent_entries().len(), 2);
    assert_eq!(
        service.recover_failed_rerank(&mut ticket, 320).unwrap_err(),
        MemoryError::TaskCancelled
    );
    assert_eq!(
        identical
            .recover_failed_rerank(&mut ticket, 320)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
    for usage in [
        TransmissionUsage::QueryEmbedding,
        TransmissionUsage::EntryEmbedding,
        TransmissionUsage::Rerank,
    ] {
        assert_eq!(
            identical
                .prepare_transmission(&recovered, usage, RERANK_ENDPOINT, &[], 320)
                .unwrap_err(),
            MemoryError::TransmissionDenied
        );
    }
    let selected = reference_by_id(&recovered, branch.id());
    let task = reader(&mut identical, &owner)
        .complete_retrieval(&recovered, &[(selected, 1.0)], 320)
        .unwrap();
    identical.validate_task(&task, 320).unwrap();
    assert_eq!(
        reader(&mut identical, &owner)
            .complete_retrieval(&recovered, &[], 320)
            .unwrap_err(),
        MemoryError::TaskCancelled
    );
}
