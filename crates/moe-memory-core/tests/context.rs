use moe_memory_core::{
    Category, CommandResult, ContextBlockKind, ContextOrigin, Epistemic, Lifecycle, MemoryDraft,
    MemoryError, MemoryId, MemoryScope, MemoryService, MemoryTask, Namespace, PetIdentity,
    PublicationPolicy, ReadPolicy, ReadPurpose, RequestContext, RetrievalLayer, RetrievalQuery,
    Sensitivity, SessionId, SourceRef,
};

fn owner(name: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new("home", "user").unwrap(), name).unwrap()
}

fn session() -> SessionId {
    SessionId::new("current").unwrap()
}

fn draft(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Episode,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
}

fn fixture() -> (MemoryService, SourceRef, MemoryId) {
    let mut service = MemoryService::new();
    let source = service
        .register_source(
            MemoryScope::Private(owner("alice")),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let id = service
        .for_pet(&owner("alice"))
        .create(draft("tea before breakfast", &source), 100)
        .unwrap()
        .id();
    service
        .for_pet(&owner("alice"))
        .create(draft("tea after lunch", &source), 100)
        .unwrap();
    service
        .set_read_policy(
            &owner("alice"),
            ReadPolicy::offline(1, vec![source.clone()]),
        )
        .unwrap();
    (service, source, id)
}

fn task(service: &mut MemoryService, now: u64) -> MemoryTask {
    service
        .for_session(&owner("alice"), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), now)
        .unwrap()
}

fn context(service: &mut MemoryService) -> RequestContext {
    let task = task(service, 100);
    service.assemble_context(&task, 100, 100).unwrap()
}

fn identity(service: &mut MemoryService, source: &SourceRef) {
    let authority = service
        .register_source(
            MemoryScope::Private(owner("alice")),
            "host-identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    service
        .set_identity_view(&owner("alice"), 1, "我是小萌", vec![authority.clone()])
        .unwrap();
    service
        .set_read_policy(
            &owner("alice"),
            ReadPolicy::offline(2, vec![source.clone(), authority]),
        )
        .unwrap();
}

#[test]
fn assembly_is_read_only_and_reports_whole_item_omissions() {
    let (mut service, source, id) = fixture();
    identity(&mut service, &source);
    let task = task(&mut service, 100);
    let generation = service.generation();
    let context = service.assemble_context(&task, 24, 100).unwrap();
    let view = service.context_view(&context, 100).unwrap();
    assert_eq!(view.identity().unwrap().text(), "我是小萌");
    assert_eq!(view.blocks().len(), 1);
    assert_eq!(view.blocks()[0].text(), "tea before breakfast");
    assert_eq!(view.usage().used_chars, 24);
    assert_eq!(view.usage().retrieved_items, 2);
    assert_eq!(view.usage().omitted_items, 1);
    assert_eq!(view.usage().revision, 1);
    assert_eq!(service.generation(), generation);
    assert_eq!(
        service
            .for_pet(&owner("alice"))
            .get(id, 100)
            .unwrap()
            .revision(),
        1
    );
    assert_eq!(task.result().items().len(), 2);
}

#[test]
fn identity_is_never_truncated_or_silently_dropped() {
    let (mut service, source, _) = fixture();
    identity(&mut service, &source);
    let task = task(&mut service, 100);
    assert!(matches!(
        service.assemble_context(&task, 3, 100),
        Err(MemoryError::ContextBudgetExceeded {
            required: 4,
            available: 3
        })
    ));
    let context = service.assemble_context(&task, 4, 100).unwrap();
    let view = service.context_view(&context, 100).unwrap();
    assert_eq!(view.identity().unwrap().text(), "我是小萌");
    assert!(view.blocks().is_empty());
    assert_eq!(view.usage().omitted_items, 2);
}

#[test]
fn empty_context_accepts_zero_budget_but_budget_is_bounded() {
    let (mut service, _, _) = fixture();
    let task = service
        .for_session(&owner("alice"), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("unmatched"), 100)
        .unwrap();
    let context = service.assemble_context(&task, 0, 100).unwrap();
    assert_eq!(
        service
            .context_view(&context, 100)
            .unwrap()
            .usage()
            .used_chars,
        0
    );
    assert!(matches!(
        service.assemble_context(&task, usize::MAX, 100),
        Err(MemoryError::InvalidInput(_))
    ));
}

#[test]
fn compression_preserves_exact_provenance_and_original_records() {
    let (mut service, source, id) = fixture();
    let task = task(&mut service, 100);
    let originals = task.result().clone();
    let mut context = service.assemble_context(&task, 100, 100).unwrap();
    let before = service.context_view(&context, 100).unwrap().clone();
    let compaction = service.begin_compaction(&context, 0..2, 100).unwrap();
    let expected: Vec<_> = compaction
        .inputs()
        .iter()
        .flat_map(|block| block.evidence().to_vec())
        .collect();
    assert_eq!(compaction.revision(), 1);
    service
        .apply_compaction(&mut context, &compaction, "tea routine", 110)
        .unwrap();
    let view = service.context_view(&context, 110).unwrap();
    assert_eq!(view.blocks().len(), 1);
    assert_eq!(view.blocks()[0].kind(), ContextBlockKind::UnverifiedSummary);
    assert_eq!(view.blocks()[0].evidence(), expected);
    assert!(
        expected
            .iter()
            .all(|evidence| evidence.sources() == [source.clone()])
    );
    assert_eq!(view.usage().used_chars, 11);
    assert_eq!(view.usage().revision, 2);
    assert_eq!(view.usage().omitted_items, before.usage().omitted_items);
    assert_eq!(task.result(), &originals);
    assert_eq!(
        service
            .for_pet(&owner("alice"))
            .get(id, 110)
            .unwrap()
            .text(),
        "tea before breakfast"
    );
}

#[test]
fn invalid_or_nonshrinking_summary_is_atomic_and_retryable() {
    let (mut service, _, _) = fixture();
    let mut context = context(&mut service);
    let before = service.context_view(&context, 100).unwrap().clone();
    let compaction = service.begin_compaction(&context, 0..1, 100).unwrap();
    for summary in [
        String::new(),
        "   ".to_owned(),
        "bad\0text".to_owned(),
        "tea before breakfast".to_owned(),
        "a".repeat(4097),
    ] {
        assert!(matches!(
            service.apply_compaction(&mut context, &compaction, summary, 100),
            Err(MemoryError::InvalidInput(_))
        ));
        assert_eq!(service.context_view(&context, 100).unwrap(), &before);
    }
    service
        .apply_compaction(&mut context, &compaction, "tea", 100)
        .unwrap();
}

#[test]
fn empty_reversed_and_out_of_bounds_ranges_are_rejected() {
    let (mut service, _, _) = fixture();
    let context = context(&mut service);
    for range in [
        0..0,
        2..3,
        std::ops::Range { start: 2, end: 1 },
        0..usize::MAX,
    ] {
        assert!(matches!(
            service.begin_compaction(&context, range, 100),
            Err(MemoryError::InvalidInput(_))
        ));
    }
}

#[test]
fn competing_summaries_use_projection_version_cas() {
    let (mut service, _, _) = fixture();
    let mut context = context(&mut service);
    let first = service.begin_compaction(&context, 0..1, 100).unwrap();
    let second = service.begin_compaction(&context, 1..2, 100).unwrap();
    service
        .apply_compaction(&mut context, &second, "tea", 100)
        .unwrap();
    let accepted = service.context_view(&context, 100).unwrap().clone();
    assert_eq!(
        service.apply_compaction(&mut context, &first, "stale", 100),
        Err(MemoryError::ContextVersionConflict)
    );
    assert_eq!(
        service.apply_compaction(&mut context, &second, "repeat", 100),
        Err(MemoryError::ContextVersionConflict)
    );
    assert_eq!(service.context_view(&context, 100).unwrap(), &accepted);
}

#[test]
fn ticket_cannot_be_used_on_an_identical_different_projection() {
    let (mut service, _, _) = fixture();
    let first = context(&mut service);
    let mut second = context(&mut service);
    let ticket = service.begin_compaction(&first, 0..2, 100).unwrap();
    assert_eq!(
        service.apply_compaction(&mut second, &ticket, "tea", 100),
        Err(MemoryError::TaskCancelled)
    );
    assert_eq!(
        service.context_view(&second, 100).unwrap().usage().revision,
        1
    );
}

#[test]
fn summaries_are_not_fresh_evidence_for_more_summaries() {
    let (mut service, _, _) = fixture();
    let mut context = context(&mut service);
    let ticket = service.begin_compaction(&context, 0..1, 100).unwrap();
    service
        .apply_compaction(&mut context, &ticket, "claim", 100)
        .unwrap();
    assert!(matches!(
        service.begin_compaction(&context, 0..1, 100),
        Err(MemoryError::InvalidState)
    ));
    assert!(service.begin_compaction(&context, 1..2, 100).is_ok());
}

#[test]
fn stale_input_rejects_view_compaction_and_late_result() {
    let (mut service, source, id) = fixture();
    let mut context = context(&mut service);
    let ticket = service.begin_compaction(&context, 0..2, 100).unwrap();
    service
        .for_pet(&owner("alice"))
        .update(id, 1, draft("tea corrected", &source), 110)
        .unwrap();
    assert!(matches!(
        service.context_view(&context, 110),
        Err(MemoryError::TaskCancelled)
    ));
    assert!(matches!(
        service.begin_compaction(&context, 0..1, 110),
        Err(MemoryError::TaskCancelled)
    ));
    assert_eq!(
        service.apply_compaction(&mut context, &ticket, "old", 110),
        Err(MemoryError::TaskCancelled)
    );
    let fresh = task(&mut service, 110);
    let fresh = service.assemble_context(&fresh, 100, 110).unwrap();
    assert!(
        service
            .context_view(&fresh, 110)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.text() == "tea corrected")
    );
}

#[test]
fn cancellation_policy_changes_and_revocation_invalidate_projections() {
    for change in [0, 1, 2] {
        let (mut service, source, _) = fixture();
        let mut context = context(&mut service);
        let ticket = service.begin_compaction(&context, 0..2, 100).unwrap();
        match change {
            0 => {
                service.cancel_tasks(service.generation()).unwrap();
            }
            1 => {
                service
                    .set_read_policy(&owner("alice"), ReadPolicy::deny_all(2))
                    .unwrap();
            }
            _ => {
                service.revoke_source(&source).unwrap();
            }
        }
        assert!(matches!(
            service.context_view(&context, 110),
            Err(MemoryError::TaskCancelled)
        ));
        assert_eq!(
            service.apply_compaction(&mut context, &ticket, "late", 110),
            Err(MemoryError::TaskCancelled)
        );
    }
}

#[test]
fn expiry_and_time_before_issuance_reject_context_use() {
    let (mut service, source, id) = fixture();
    service
        .for_pet(&owner("alice"))
        .update(
            id,
            1,
            draft("tea expiring", &source).with_expiration(120),
            100,
        )
        .unwrap();
    let task = task(&mut service, 100);
    let mut context = service.assemble_context(&task, 100, 105).unwrap();
    assert!(matches!(
        service.context_view(&context, 104),
        Err(MemoryError::TaskCancelled)
    ));
    let ticket = service.begin_compaction(&context, 0..2, 110).unwrap();
    assert_eq!(
        service.apply_compaction(&mut context, &ticket, "early", 109),
        Err(MemoryError::TaskCancelled)
    );
    assert!(matches!(
        service.context_view(&context, 120),
        Err(MemoryError::TaskCancelled)
    ));
    assert_eq!(
        service.apply_compaction(&mut context, &ticket, "expired", 120),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn different_service_and_different_pet_never_rebind_a_projection() {
    let (mut service, source, _) = fixture();
    let task = task(&mut service, 100);
    let mut context = service.assemble_context(&task, 100, 100).unwrap();
    let ticket = service.begin_compaction(&context, 0..2, 100).unwrap();
    let (mut other, _, _) = fixture();
    assert!(matches!(
        other.assemble_context(&task, 100, 100),
        Err(MemoryError::TaskCancelled)
    ));
    assert!(matches!(
        other.context_view(&context, 100),
        Err(MemoryError::TaskCancelled)
    ));
    assert_eq!(
        other.apply_compaction(&mut context, &ticket, "cross", 100),
        Err(MemoryError::TaskCancelled)
    );
    assert_eq!(
        service.set_read_policy(&owner("bob"), ReadPolicy::offline(1, vec![source])),
        Err(MemoryError::InvalidEvidence)
    );
    let bob_source = service
        .register_source(
            MemoryScope::Private(owner("bob")),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    service
        .set_read_policy(&owner("bob"), ReadPolicy::offline(1, vec![bob_source]))
        .unwrap();
    let bob_task = service
        .for_session(&owner("bob"), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let bob_context = service.assemble_context(&bob_task, 100, 100).unwrap();
    assert!(
        service
            .context_view(&bob_context, 100)
            .unwrap()
            .blocks()
            .is_empty()
    );
}

#[test]
fn unauthorized_purposes_cannot_create_input_tasks() {
    let (mut service, _, _) = fixture();
    for purpose in [
        ReadPurpose::Unknown,
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        assert!(matches!(
            service
                .for_session(&owner("alice"), &session(), purpose)
                .begin_task(&RetrievalQuery::new("tea"), 100),
            Err(MemoryError::AccessDenied)
        ));
    }
}

#[test]
fn working_provenance_retains_session_version_and_no_persistence_promotion() {
    let (mut service, source, _) = fixture();
    service
        .put_working_context(
            &owner("alice"),
            &session(),
            "temporary-secret",
            1,
            MemoryDraft::new(
                "tea work private",
                Category::WorkingContext,
                Epistemic::Inference,
                vec![source.clone()],
            )
            .with_lifecycle(Lifecycle::ShortTerm)
            .with_sensitivity(Sensitivity::Private),
            100,
        )
        .unwrap();
    let task = service
        .for_session(&owner("alice"), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(
            &RetrievalQuery::new("tea").with_layer(RetrievalLayer::WorkingContext),
            100,
        )
        .unwrap();
    let mut context = service.assemble_context(&task, 100, 100).unwrap();
    let ticket = service.begin_compaction(&context, 0..1, 100).unwrap();
    service
        .apply_compaction(&mut context, &ticket, "tea", 100)
        .unwrap();
    let evidence = &service.context_view(&context, 100).unwrap().blocks()[0].evidence()[0];
    assert_eq!(
        evidence.origin(),
        &ContextOrigin::WorkingContext {
            session: session(),
            id: "temporary-secret".into(),
            revision: 1
        }
    );
    assert_eq!(evidence.epistemic(), Epistemic::Inference);
    assert_eq!(evidence.lifecycle(), Lifecycle::ShortTerm);
    assert_eq!(evidence.sensitivity(), Sensitivity::Private);
    assert!(matches!(
        task.propose_create(draft("not promoted", &source), 100),
        Err(MemoryError::InvalidInput(_))
    ));
    service
        .put_working_context(
            &owner("alice"),
            &session(),
            "temporary-secret",
            2,
            MemoryDraft::new(
                "tea work changed",
                Category::WorkingContext,
                Epistemic::Inference,
                vec![source],
            )
            .with_lifecycle(Lifecycle::ShortTerm)
            .with_sensitivity(Sensitivity::Private),
            110,
        )
        .unwrap();
    assert!(matches!(
        service.context_view(&context, 110),
        Err(MemoryError::TaskCancelled)
    ));
}

#[test]
fn omitted_inputs_still_fence_and_constrain_derived_writes() {
    let (mut service, source, id) = fixture();
    let task = task(&mut service, 100);
    let context = service.assemble_context(&task, 0, 100).unwrap();
    let candidate = draft("derived", &source);
    let command = task.propose_create(candidate, 100).unwrap();
    let receipt = match command.execute(&mut service).unwrap() {
        CommandResult::Write(receipt) => receipt,
        _ => panic!("write expected"),
    };
    let created = service
        .for_pet(&owner("alice"))
        .get(receipt.id(), 100)
        .unwrap();
    assert_eq!(created.dependencies().len(), 2);
    assert!(
        created
            .dependencies()
            .iter()
            .any(|dependency| dependency.id() == id)
    );
    assert!(matches!(
        service.context_view(&context, 100),
        Err(MemoryError::TaskCancelled)
    ));
}

#[test]
fn formal_material_is_pinned_and_cannot_be_summarized() {
    for category in [Category::Protocol, Category::ToolDefinition] {
        let mut service = MemoryService::new();
        let namespace = owner("alice").namespace().clone();
        let source = service
            .register_source(
                MemoryScope::Shared(namespace.clone()),
                "reference",
                1,
                Epistemic::Reference,
            )
            .unwrap();
        service
            .set_formal_publication_policy(
                &namespace,
                PublicationPolicy::allow_sources(1, vec![source.clone()]),
            )
            .unwrap();
        let command = service
            .prepare_formal_publication(
                &namespace,
                MemoryDraft::new(
                    "formal material",
                    category,
                    Epistemic::Reference,
                    vec![source.clone()],
                ),
                100,
            )
            .unwrap();
        command.execute(&mut service).unwrap();
        service
            .set_read_policy(&owner("alice"), ReadPolicy::offline(1, vec![source]))
            .unwrap();
        let task = service
            .for_session(&owner("alice"), &session(), ReadPurpose::OfflineReasoning)
            .begin_task(
                &RetrievalQuery::new("").with_layer(RetrievalLayer::FormalMaterial),
                100,
            )
            .unwrap();
        assert!(matches!(
            service.assemble_context(&task, 14, 100),
            Err(MemoryError::ContextBudgetExceeded {
                required: 15,
                available: 14
            })
        ));
        let context = service.assemble_context(&task, 15, 100).unwrap();
        assert_eq!(
            service.context_view(&context, 100).unwrap().blocks()[0].kind(),
            ContextBlockKind::ProtectedMaterial
        );
        assert!(matches!(
            service.begin_compaction(&context, 0..1, 100),
            Err(MemoryError::ProtectedContent)
        ));
    }
}

#[test]
fn debug_does_not_expose_text_or_working_identifiers() {
    let (mut service, source, _) = fixture();
    identity(&mut service, &source);
    service
        .put_working_context(
            &owner("alice"),
            &session(),
            "secret-working-id",
            1,
            MemoryDraft::new(
                "tea private note",
                Category::WorkingContext,
                Epistemic::UserStatement,
                vec![source],
            )
            .with_lifecycle(Lifecycle::ShortTerm),
            100,
        )
        .unwrap();
    let context = context(&mut service);
    let ticket = service.begin_compaction(&context, 0..3, 100).unwrap();
    let debug = format!(
        "{:?} {:?} {:?} {:?}",
        context,
        ticket,
        service.context_view(&context, 100).unwrap(),
        ticket.inputs()
    );
    for secret in [
        "tea before breakfast",
        "tea after lunch",
        "tea private note",
        "secret-working-id",
        "我是小萌",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn successful_compaction_advances_the_projection_time_fence() {
    let (mut service, _, _) = fixture();
    let mut context = context(&mut service);
    let ticket = service.begin_compaction(&context, 0..1, 100).unwrap();
    service
        .apply_compaction(&mut context, &ticket, "tea", 120)
        .unwrap();
    assert!(matches!(
        service.context_view(&context, 119),
        Err(MemoryError::TaskCancelled)
    ));
    assert!(matches!(
        service.begin_compaction(&context, 1..2, 119),
        Err(MemoryError::TaskCancelled)
    ));
    assert!(service.begin_compaction(&context, 1..2, 120).is_ok());
}

#[test]
fn unicode_compression_keeps_character_budget_and_mixed_evidence() {
    let (mut service, first_source, first_id) = fixture();
    let second_source = service
        .register_source(
            MemoryScope::Private(owner("alice")),
            "second-message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    service
        .for_pet(&owner("alice"))
        .update(
            first_id,
            1,
            MemoryDraft::new(
                "早餐后喜欢喝茶🍵",
                Category::UserProfile,
                Epistemic::Inference,
                vec![second_source.clone()],
            )
            .with_sensitivity(Sensitivity::Private),
            110,
        )
        .unwrap();
    service
        .set_read_policy(
            &owner("alice"),
            ReadPolicy::offline(2, vec![first_source.clone(), second_source.clone()]),
        )
        .unwrap();
    let task = service
        .for_session(&owner("alice"), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new(""), 110)
        .unwrap();
    let mut context = service.assemble_context(&task, 100, 110).unwrap();
    let ticket = service.begin_compaction(&context, 0..2, 110).unwrap();
    service
        .apply_compaction(&mut context, &ticket, "喝茶🍵", 110)
        .unwrap();
    let view = service.context_view(&context, 110).unwrap();
    assert_eq!(view.usage().used_chars, 3);
    let evidence = view.blocks()[0].evidence();
    assert_eq!(evidence[0].sources(), [second_source]);
    assert_eq!(evidence[0].category(), Category::UserProfile);
    assert_eq!(evidence[0].epistemic(), Epistemic::Inference);
    assert_eq!(evidence[0].sensitivity(), Sensitivity::Private);
    assert_eq!(evidence[1].sources(), [first_source]);
    assert_eq!(evidence[1].epistemic(), Epistemic::UserStatement);
    assert_eq!(evidence[1].sensitivity(), Sensitivity::Normal);
}

#[test]
fn identity_replacement_invalidates_an_old_summary_ticket() {
    let (mut service, source, _) = fixture();
    identity(&mut service, &source);
    let mut context = context(&mut service);
    let ticket = service.begin_compaction(&context, 0..2, 100).unwrap();
    let authority = service
        .context_view(&context, 100)
        .unwrap()
        .identity()
        .unwrap()
        .sources()
        .to_vec();
    service
        .set_identity_view(&owner("alice"), 2, "新的受保护身份", authority)
        .unwrap();
    assert!(matches!(
        service.context_view(&context, 110),
        Err(MemoryError::TaskCancelled)
    ));
    assert_eq!(
        service.apply_compaction(&mut context, &ticket, "old identity", 110),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn hiding_private_inputs_with_budget_or_summary_does_not_enable_publication() {
    let (mut service, source, _) = fixture();
    let task = task(&mut service, 100);
    let empty = service.assemble_context(&task, 0, 100).unwrap();
    assert!(
        service
            .context_view(&empty, 100)
            .unwrap()
            .blocks()
            .is_empty()
    );
    let mut compressed = service.assemble_context(&task, 100, 100).unwrap();
    let ticket = service.begin_compaction(&compressed, 0..2, 100).unwrap();
    service
        .apply_compaction(&mut compressed, &ticket, "public knowledge", 100)
        .unwrap();
    assert_eq!(
        task.propose_shared(draft("apparently public", &source), 100)
            .unwrap_err(),
        MemoryError::PublicationDenied
    );
}
