use moe_memory_core::{
    CacheStatus, Category, CommandResult, Epistemic, Lifecycle, MemoryCommand, MemoryDraft,
    MemoryError, MemoryId, MemoryScope, MemoryService, MemoryTask, Namespace, PetIdentity,
    PublicationPolicy, ReadPolicy, ReadPurpose, RecordVersion, RetrievalLayer, RetrievalQuery,
    SearchQuery, SearchScope, Sensitivity, SessionId, SourceRef, WriteReceipt,
};

fn namespace() -> Namespace {
    Namespace::new("home", "user").unwrap()
}
fn pet(name: &str) -> PetIdentity {
    PetIdentity::new(namespace(), name).unwrap()
}
fn session() -> SessionId {
    SessionId::new("current").unwrap()
}
fn draft(text: &str, sources: Vec<SourceRef>) -> MemoryDraft {
    MemoryDraft::new(text, Category::Knowledge, Epistemic::Reference, sources)
}
fn formal(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Protocol,
        Epistemic::Reference,
        vec![source.clone()],
    )
}
fn fixture() -> (MemoryService, SourceRef) {
    let mut service = MemoryService::new();
    let source = service
        .register_source(
            MemoryScope::Shared(namespace()),
            "reference",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service
        .set_publication_policy(
            &pet("alice"),
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    service
        .set_formal_publication_policy(
            &namespace(),
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    service
        .set_read_policy(&pet("alice"), ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    (service, source)
}
fn task(service: &mut MemoryService, identity: &PetIdentity, query: &str, now: u64) -> MemoryTask {
    service
        .for_session(identity, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new(query), now)
        .unwrap()
}
fn write(command: &MemoryCommand, service: &mut MemoryService) -> WriteReceipt {
    match command.execute(service).unwrap() {
        CommandResult::Write(receipt) => receipt,
        _ => panic!("expected write receipt"),
    }
}
fn seed(service: &mut MemoryService, source: &SourceRef, text: &str) -> MemoryId {
    let candidate = task(service, &pet("alice"), "no-matching-seed", 100)
        .propose_shared(draft(text, vec![source.clone()]), 100)
        .unwrap();
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    write(&command, service).id()
}
fn shared_count(service: &mut MemoryService) -> usize {
    service
        .for_pet(&pet("alice"))
        .search(&SearchQuery::new("").with_scope(SearchScope::Shared), 200)
        .unwrap()
        .len()
}

#[test]
fn read_grants_do_not_grant_shared_write_and_automatic_rules_remain_disabled() {
    let (mut service, source) = fixture();
    service
        .set_publication_policy(&pet("alice"), PublicationPolicy::deny_all(2))
        .unwrap();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("unapproved", vec![source]), 100)
        .unwrap();
    let generation = service.generation();
    assert_eq!(
        service.prepare_shared_publication(&candidate, 100),
        Err(MemoryError::PublicationDenied)
    );
    assert_eq!(
        service.prepare_automatic_publication(&candidate),
        Err(MemoryError::AutomaticPublicationDisabled)
    );
    assert_eq!(service.generation(), generation);
    assert_eq!(shared_count(&mut service), 0);
}

#[test]
fn explicit_exact_candidate_review_publishes_only_shared_normal_long_term_content() {
    let (mut service, source) = fixture();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("reviewed experience", vec![source.clone()]), 100)
        .unwrap();
    assert_eq!(candidate.text(), "reviewed experience");
    assert_eq!(candidate.sources(), &[source]);
    assert!(candidate.dependencies().is_empty());
    assert!(candidate.task().result().items().is_empty());
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    assert_eq!(shared_count(&mut service), 0);
    let receipt = write(&command, &mut service);
    let record = service.for_pet(&pet("bob")).get(receipt.id(), 100).unwrap();
    assert_eq!(record.scope(), &MemoryScope::Shared(namespace()));
    assert_eq!(record.sensitivity(), Sensitivity::Normal);
    assert_eq!(record.lifecycle(), Lifecycle::LongTerm);
    assert!(!record.is_protected());
    assert_eq!(
        service.prepare_automatic_publication(&candidate),
        Err(MemoryError::AutomaticPublicationDisabled)
    );
}

#[test]
fn shared_readability_is_not_a_publish_grant_for_other_pets_or_namespaces() {
    let (mut service, source) = fixture();
    let id = seed(&mut service, &source, "root");
    service
        .set_read_policy(&pet("bob"), ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    let candidate = task(&mut service, &pet("bob"), "root", 100)
        .propose_shared(draft("copy", vec![source]), 100)
        .unwrap();
    assert_eq!(
        service.prepare_shared_publication(&candidate, 100),
        Err(MemoryError::PublicationDenied)
    );
    for other in [
        Namespace::new("away", "user").unwrap(),
        Namespace::new("home", "other").unwrap(),
    ] {
        let foreign = PetIdentity::new(other, "alice").unwrap();
        assert_eq!(
            service.for_pet(&foreign).get(id, 100),
            Err(MemoryError::NotFound)
        );
    }
}

#[test]
fn omitted_actual_sources_are_rejected_and_record_versions_are_added() {
    let (mut service, first) = fixture();
    let second = service
        .register_source(
            MemoryScope::Shared(namespace()),
            "second",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service
        .set_read_policy(
            &pet("alice"),
            ReadPolicy::offline(2, vec![first.clone(), second.clone()]),
        )
        .unwrap();
    service
        .set_publication_policy(
            &pet("alice"),
            PublicationPolicy::allow_sources(2, vec![first.clone(), second.clone()]),
        )
        .unwrap();
    let root = seed(&mut service, &first, "root");
    seed(&mut service, &second, "root second");
    let snapshot = task(&mut service, &pet("alice"), "root", 100);
    assert_eq!(snapshot.result().items().len(), 2);
    assert_eq!(
        snapshot
            .propose_shared(draft("launder", vec![first.clone()]), 100)
            .unwrap_err(),
        MemoryError::InvalidEvidence
    );
    let candidate = snapshot
        .propose_shared(draft("complete", vec![first, second]), 100)
        .unwrap();
    assert!(
        candidate
            .dependencies()
            .contains(&RecordVersion::new(root, 1).unwrap())
    );
    assert_eq!(candidate.dependencies().len(), 2);
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    write(&command, &mut service);
}

#[test]
fn partial_publish_grant_cannot_approve_multi_source_candidates() {
    let (mut service, first) = fixture();
    let second = service
        .register_source(
            MemoryScope::Shared(namespace()),
            "second",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("both", vec![first, second]), 100)
        .unwrap();
    assert_eq!(
        service.prepare_shared_publication(&candidate, 100),
        Err(MemoryError::PublicationDenied)
    );
}

#[test]
fn normal_private_record_cannot_be_laundered_even_with_only_shared_sources() {
    let (mut service, source) = fixture();
    service
        .for_pet(&pet("alice"))
        .create(draft("private root", vec![source.clone()]), 100)
        .unwrap();
    let snapshot = task(&mut service, &pet("alice"), "private root", 100);
    assert_eq!(snapshot.result().items().len(), 1);
    assert_eq!(
        snapshot
            .propose_shared(draft("apparently public", vec![source]), 100)
            .unwrap_err(),
        MemoryError::PublicationDenied
    );
}

#[test]
fn private_sources_and_private_or_restricted_candidates_never_cross_the_shared_boundary() {
    let (mut service, source) = fixture();
    let private = service
        .register_source(
            MemoryScope::Private(pet("alice")),
            "secret",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    let snapshot = task(&mut service, &pet("alice"), "", 100);
    assert_eq!(
        snapshot
            .propose_shared(draft("private source", vec![private]), 100)
            .unwrap_err(),
        MemoryError::PublicationDenied
    );
    for sensitivity in [Sensitivity::Private, Sensitivity::Restricted] {
        assert_eq!(
            snapshot
                .propose_shared(
                    draft("sensitive", vec![source.clone()]).with_sensitivity(sensitivity),
                    100
                )
                .unwrap_err(),
            MemoryError::PublicationDenied
        );
    }
}

#[test]
fn foreign_source_or_private_dependency_is_not_a_shareable_provenance() {
    let (mut service, source) = fixture();
    let private_id = service
        .for_pet(&pet("alice"))
        .create(draft("unselected private", vec![source.clone()]), 100)
        .unwrap()
        .id();
    let candidate = task(&mut service, &pet("alice"), "no-match", 100)
        .propose_shared(
            draft("bad dependency", vec![source])
                .with_dependencies(vec![RecordVersion::new(private_id, 1).unwrap()]),
            100,
        )
        .unwrap();
    assert_eq!(
        service.prepare_shared_publication(&candidate, 100),
        Err(MemoryError::InvalidEvidence)
    );
    let foreign = service
        .register_source(
            MemoryScope::Shared(Namespace::new("other", "user").unwrap()),
            "foreign",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    assert_eq!(
        task(&mut service, &pet("alice"), "no-match", 100)
            .propose_shared(draft("foreign", vec![foreign]), 100)
            .unwrap_err(),
        MemoryError::PublicationDenied
    );
}

#[test]
fn an_identity_anchor_is_part_of_actual_input_even_when_records_are_empty() {
    let (mut service, source) = fixture();
    let identity_source = service
        .register_source(
            MemoryScope::Private(pet("alice")),
            "identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    service
        .set_read_policy(
            &pet("alice"),
            ReadPolicy::offline(2, vec![source.clone(), identity_source.clone()]),
        )
        .unwrap();
    service
        .set_identity_view(&pet("alice"), 1, "private anchor", vec![identity_source])
        .unwrap();
    let snapshot = task(&mut service, &pet("alice"), "no-match", 100);
    assert!(snapshot.identity_view().is_some());
    assert_eq!(
        snapshot
            .propose_shared(draft("omit identity", vec![source]), 100)
            .unwrap_err(),
        MemoryError::PublicationDenied
    );
}

#[test]
fn session_context_cannot_be_promoted_by_shared_publication() {
    let (mut service, source) = fixture();
    service
        .put_working_context(
            &pet("alice"),
            &session(),
            "working",
            1,
            MemoryDraft::new(
                "temporary",
                Category::WorkingContext,
                Epistemic::Reference,
                vec![source.clone()],
            )
            .with_lifecycle(Lifecycle::ShortTerm),
            100,
        )
        .unwrap();
    let snapshot = task(&mut service, &pet("alice"), "temporary", 100);
    assert_eq!(
        snapshot
            .propose_shared(draft("promoted", vec![source.clone()]), 100)
            .unwrap_err(),
        MemoryError::PublicationDenied
    );
    let empty = task(&mut service, &pet("alice"), "no-match", 100);
    assert_eq!(
        empty
            .propose_shared(
                draft("short", vec![source]).with_lifecycle(Lifecycle::ShortTerm),
                100
            )
            .unwrap_err(),
        MemoryError::PublicationDenied
    );
}

#[test]
fn ordinary_candidates_cannot_claim_identity_authority_capabilities_or_protocol() {
    let (mut service, source) = fixture();
    let snapshot = task(&mut service, &pet("alice"), "", 100);
    for category in [
        Category::Identity,
        Category::UserProfile,
        Category::Episode,
        Category::ToolDefinition,
        Category::Protocol,
    ] {
        assert_eq!(
            snapshot
                .propose_shared(
                    MemoryDraft::new(
                        "claim",
                        category,
                        Epistemic::Reference,
                        vec![source.clone()]
                    ),
                    100
                )
                .unwrap_err(),
            MemoryError::PublicationDenied
        );
    }
    for epistemic in [Epistemic::AuthoritativeState, Epistemic::Fiction] {
        assert_eq!(
            snapshot
                .propose_shared(
                    MemoryDraft::new(
                        "claim",
                        Category::Knowledge,
                        epistemic,
                        vec![source.clone()]
                    ),
                    100
                )
                .unwrap_err(),
            MemoryError::PublicationDenied
        );
    }
}

#[test]
fn formal_policy_and_ordinary_publish_policy_are_independent() {
    let (mut service, source) = fixture();
    service
        .set_formal_publication_policy(&namespace(), PublicationPolicy::deny_all(2))
        .unwrap();
    assert_eq!(
        service.prepare_formal_publication(&namespace(), formal("protocol", &source), 100),
        Err(MemoryError::PublicationDenied)
    );
    let id = seed(&mut service, &source, "ordinary");
    assert!(service.for_pet(&pet("alice")).get(id, 100).is_ok());
    service
        .set_publication_policy(&pet("alice"), PublicationPolicy::deny_all(2))
        .unwrap();
    service
        .set_formal_publication_policy(
            &namespace(),
            PublicationPolicy::allow_sources(3, vec![source.clone()]),
        )
        .unwrap();
    let command = service
        .prepare_formal_publication(&namespace(), formal("protocol", &source), 100)
        .unwrap();
    write(&command, &mut service);
}

#[test]
fn formal_material_is_protected_reference_information_not_runtime_authority() {
    let (mut service, source) = fixture();
    for category in [Category::ToolDefinition, Category::Protocol] {
        let command = service
            .prepare_formal_publication(
                &namespace(),
                MemoryDraft::new(
                    "formal description",
                    category,
                    Epistemic::Reference,
                    vec![source.clone()],
                ),
                100,
            )
            .unwrap();
        let id = write(&command, &mut service).id();
        let record = service.for_pet(&pet("bob")).get(id, 100).unwrap();
        assert!(record.is_protected());
        assert_eq!(record.category(), category);
        assert_eq!(record.epistemic(), Epistemic::Reference);
        assert_eq!(
            service.for_pet(&pet("alice")).update(
                id,
                1,
                draft("rewrite", vec![source.clone()]),
                100
            ),
            Err(MemoryError::NotFound)
        );
        assert_eq!(
            service.for_pet(&pet("alice")).delete(id, 1, 100),
            Err(MemoryError::NotFound)
        );
        assert_eq!(
            service.for_pet(&pet("alice")).archive(id, 1, 100),
            Err(MemoryError::NotFound)
        );
        assert_eq!(
            service.for_pet(&pet("alice")).replace(
                id,
                1,
                draft("replace", vec![source.clone()]),
                100
            ),
            Err(MemoryError::NotFound)
        );
    }
    assert_eq!(
        service.set_identity_view(&pet("alice"), 1, "not private authority", vec![source]),
        Err(MemoryError::InvalidEvidence)
    );
}

#[test]
fn formal_input_cannot_promote_ordinary_or_private_derived_data() {
    let (mut service, source) = fixture();
    let id = seed(&mut service, &source, "ordinary");
    for invalid in [
        draft("ordinary", vec![source.clone()]),
        formal("dependent", &source).with_dependencies(vec![RecordVersion::new(id, 1).unwrap()]),
        formal("private", &source).with_sensitivity(Sensitivity::Private),
        formal("short", &source).with_lifecycle(Lifecycle::ShortTerm),
        MemoryDraft::new(
            "authority",
            Category::Protocol,
            Epistemic::AuthoritativeState,
            vec![source.clone()],
        ),
    ] {
        assert_eq!(
            service.prepare_formal_publication(&namespace(), invalid, 100),
            Err(MemoryError::PublicationDenied)
        );
    }
    assert_eq!(
        service.prepare_formal_revision(&namespace(), id, 1, formal("promotion", &source), 100),
        Err(MemoryError::NotFound)
    );
}

#[test]
fn formal_revision_requires_exact_scope_category_version_and_monotonic_time() {
    let (mut service, source) = fixture();
    let command = service
        .prepare_formal_publication(&namespace(), formal("v1", &source), 100)
        .unwrap();
    let id = write(&command, &mut service).id();
    assert_eq!(
        service.prepare_formal_revision(&namespace(), id, 0, formal("v2", &source), 200),
        Err(MemoryError::RevisionConflict {
            expected: 0,
            actual: 1
        })
    );
    assert_eq!(
        service.prepare_formal_revision(&namespace(), id, 1, formal("backwards", &source), 99),
        Err(MemoryError::TimeWentBackwards)
    );
    let changed_kind = MemoryDraft::new(
        "changed kind",
        Category::ToolDefinition,
        Epistemic::Reference,
        vec![source.clone()],
    );
    assert_eq!(
        service.prepare_formal_revision(&namespace(), id, 1, changed_kind, 200),
        Err(MemoryError::InvalidState)
    );
    let command = service
        .prepare_formal_revision(&namespace(), id, 1, formal("v2", &source), 200)
        .unwrap();
    assert_eq!(write(&command, &mut service).revision(), 2);
    assert_eq!(
        service.for_pet(&pet("alice")).get(id, 200).unwrap().text(),
        "v2"
    );
    assert_eq!(
        command.execute(&mut service),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn formal_documents_cannot_be_used_as_unreviewed_ordinary_derivation_evidence() {
    let (mut service, source) = fixture();
    let command = service
        .prepare_formal_publication(&namespace(), formal("formal root", &source), 100)
        .unwrap();
    write(&command, &mut service);
    let query = RetrievalQuery::new("formal root").with_layer(RetrievalLayer::FormalMaterial);
    let snapshot = service
        .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&query, 100)
        .unwrap();
    assert_eq!(snapshot.result().items().len(), 1);
    assert_eq!(
        snapshot
            .propose_shared(draft("laundered", vec![source]), 100)
            .unwrap_err(),
        MemoryError::PublicationDenied
    );
}

#[test]
fn publisher_policy_changes_cancel_candidates_and_approved_commands_without_writes() {
    let (mut service, source) = fixture();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("approved", vec![source]), 100)
        .unwrap();
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    let generation = service.generation();
    service
        .set_publication_policy(&pet("alice"), PublicationPolicy::deny_all(2))
        .unwrap();
    assert_eq!(service.generation(), generation);
    assert_eq!(
        service.prepare_shared_publication(&candidate, 100),
        Err(MemoryError::TaskCancelled)
    );
    assert_eq!(
        command.execute(&mut service),
        Err(MemoryError::PublicationDenied)
    );
}

#[test]
fn storage_changes_and_expiration_cancel_pending_publications() {
    let (mut service, source) = fixture();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("pending", vec![source.clone()]), 100)
        .unwrap();
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    service.cancel_tasks(service.generation()).unwrap();
    assert_eq!(
        command.execute(&mut service),
        Err(MemoryError::TaskCancelled)
    );
    let expiring = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(
            draft("root", vec![source.clone()]).with_expiration(150),
            100,
        )
        .unwrap();
    let command = service.prepare_shared_publication(&expiring, 100).unwrap();
    write(&command, &mut service);
    let snapshot = task(&mut service, &pet("alice"), "root", 100);
    let candidate = snapshot
        .propose_shared(draft("too late", vec![source]), 100)
        .unwrap();
    assert_eq!(
        service.prepare_shared_publication(&candidate, 150),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn source_restore_requires_fresh_exact_publish_grant_and_new_provenance() {
    let (mut service, source) = fixture();
    let id = seed(&mut service, &source, "root");
    service.revoke_source(&source).unwrap();
    let restored = service.restore_source(&source, 2).unwrap();
    assert_eq!(
        service.for_pet(&pet("alice")).get(id, 100),
        Err(MemoryError::NotFound)
    );
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("new source", vec![restored.clone()]), 100)
        .unwrap();
    assert_eq!(
        service.prepare_shared_publication(&candidate, 100),
        Err(MemoryError::PublicationDenied)
    );
    service
        .set_publication_policy(
            &pet("alice"),
            PublicationPolicy::allow_sources(2, vec![restored.clone()]),
        )
        .unwrap();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("new source", vec![restored]), 100)
        .unwrap();
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    write(&command, &mut service);
    assert_eq!(
        service.for_pet(&pet("alice")).get(id, 100),
        Err(MemoryError::NotFound)
    );
}

#[test]
fn shared_ancestor_expiration_invalidates_records_and_entity_cache() {
    let (mut service, source) = fixture();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(
            draft("root", vec![source.clone()]).with_expiration(150),
            100,
        )
        .unwrap();
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    write(&command, &mut service);
    let candidate = task(&mut service, &pet("alice"), "root", 100)
        .propose_shared(draft("child", vec![source]), 100)
        .unwrap();
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    let child = write(&command, &mut service).id();
    let query = RetrievalQuery::new("child");
    assert_eq!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap()
            .cache_status(),
        CacheStatus::Built
    );
    assert_eq!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap()
            .cache_status(),
        CacheStatus::Hit
    );
    assert_eq!(
        service.for_pet(&pet("alice")).get(child, 150),
        Err(MemoryError::NotFound)
    );
    assert!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 150)
            .unwrap()
            .items()
            .is_empty()
    );
}

#[test]
fn publishing_does_not_enable_user_inspection_unknown_or_model_transmission() {
    let (mut service, source) = fixture();
    seed(&mut service, &source, "root");
    for purpose in [
        ReadPurpose::Unknown,
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        assert_eq!(
            service
                .for_session(&pet("alice"), &session(), purpose)
                .retrieve(&RetrievalQuery::new("root"), 100),
            Err(MemoryError::AccessDenied)
        );
    }
}

#[test]
fn policy_watermarks_require_advance_and_reject_private_foreign_or_revoked_sources() {
    let (mut service, source) = fixture();
    assert_eq!(PublicationPolicy::deny_all(3).version(), 3);
    assert_eq!(
        service.set_publication_policy(&pet("alice"), PublicationPolicy::deny_all(1)),
        Err(MemoryError::PublicationVersionConflict)
    );
    assert_eq!(
        service.set_formal_publication_policy(&namespace(), PublicationPolicy::deny_all(1)),
        Err(MemoryError::PublicationVersionConflict)
    );
    assert!(
        service
            .set_publication_policy(&pet("bob"), PublicationPolicy::deny_all(0))
            .is_err()
    );
    let private = service
        .register_source(
            MemoryScope::Private(pet("alice")),
            "private",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    assert_eq!(
        service.set_publication_policy(
            &pet("alice"),
            PublicationPolicy::allow_sources(2, vec![private.clone()])
        ),
        Err(MemoryError::InvalidEvidence)
    );
    assert_eq!(
        service.set_formal_publication_policy(
            &namespace(),
            PublicationPolicy::allow_sources(2, vec![private])
        ),
        Err(MemoryError::InvalidEvidence)
    );
    let foreign = Namespace::new("foreign", "user").unwrap();
    assert_eq!(
        service.set_formal_publication_policy(
            &foreign,
            PublicationPolicy::allow_sources(1, vec![source.clone()])
        ),
        Err(MemoryError::InvalidEvidence)
    );
    service.revoke_source(&source).unwrap();
    assert_eq!(
        service.set_publication_policy(
            &pet("alice"),
            PublicationPolicy::allow_sources(2, vec![source])
        ),
        Err(MemoryError::InvalidEvidence)
    );
}

#[test]
fn decoded_publications_have_wire_equality_but_no_live_approval() {
    let (mut service, source) = fixture();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("shared secret text", vec![source.clone()]), 100)
        .unwrap();
    let shared = service.prepare_shared_publication(&candidate, 100).unwrap();
    let formal = service
        .prepare_formal_publication(&namespace(), formal("formal secret text", &source), 100)
        .unwrap();
    for command in [shared, formal] {
        let bytes = command.to_bytes().unwrap();
        assert_eq!(bytes[0], 2);
        let decoded = MemoryCommand::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, command);
        assert_eq!(decoded.to_bytes().unwrap(), bytes);
        assert_eq!(
            decoded.execute(&mut service),
            Err(MemoryError::PublicationDenied)
        );
        assert!(!format!("{command:?}").contains("secret text"));
    }
    assert!(!format!("{candidate:?}").contains("secret text"));
}

#[test]
fn replay_recovers_committed_shared_and_formal_writes_but_not_permissions() {
    let (mut service, source) = fixture();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("root", vec![source.clone()]), 100)
        .unwrap();
    let shared = service.prepare_shared_publication(&candidate, 100).unwrap();
    let shared_id = write(&shared, &mut service).id();
    let install = service
        .prepare_formal_publication(&namespace(), formal("v1", &source), 100)
        .unwrap();
    let formal_id = write(&install, &mut service).id();
    let revision = service
        .prepare_formal_revision(&namespace(), formal_id, 1, formal("v2", &source), 200)
        .unwrap();
    write(&revision, &mut service);
    let mut recovered = MemoryService::new();
    recovered
        .register_source(
            MemoryScope::Shared(namespace()),
            "reference",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    for command in [&shared, &install, &revision] {
        MemoryCommand::from_bytes(&command.to_bytes().unwrap())
            .unwrap()
            .replay(&mut recovered)
            .unwrap();
    }
    assert_eq!(
        recovered
            .for_pet(&pet("alice"))
            .get(shared_id, 200)
            .unwrap()
            .text(),
        "root"
    );
    assert_eq!(
        recovered
            .for_pet(&pet("alice"))
            .get(formal_id, 200)
            .unwrap()
            .revision(),
        2
    );
    assert_eq!(
        recovered
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&RetrievalQuery::new(""), 200),
        Err(MemoryError::AccessDenied)
    );
    assert_eq!(
        recovered.prepare_formal_publication(&namespace(), formal("new", &source), 200),
        Err(MemoryError::PublicationDenied)
    );
}

#[test]
fn prepared_publications_from_another_epoch_cannot_execute_even_at_identical_versions() {
    let (mut first, source) = fixture();
    let candidate = task(&mut first, &pet("alice"), "", 100)
        .propose_shared(draft("root", vec![source.clone()]), 100)
        .unwrap();
    let shared = first.prepare_shared_publication(&candidate, 100).unwrap();
    let formal = first
        .prepare_formal_publication(&namespace(), formal("protocol", &source), 100)
        .unwrap();
    let (mut second, _) = fixture();
    assert_eq!(second.generation(), first.generation());
    assert_eq!(
        shared.execute(&mut second),
        Err(MemoryError::PublicationDenied)
    );
    assert_eq!(
        formal.execute(&mut second),
        Err(MemoryError::PublicationDenied)
    );
    assert_eq!(
        second.prepare_shared_publication(&candidate, 100),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn raw_candidate_dependency_difference_survives_automatic_edge_completion() {
    let (mut service, source) = fixture();
    let id = seed(&mut service, &source, "root");
    let snapshot = task(&mut service, &pet("alice"), "root", 100);
    let raw = draft("child", vec![source]);
    let omitted = snapshot.propose_shared(raw.clone(), 100).unwrap();
    let explicit = snapshot
        .propose_shared(
            raw.with_dependencies(vec![RecordVersion::new(id, 1).unwrap()]),
            100,
        )
        .unwrap();
    assert_eq!(omitted.dependencies(), explicit.dependencies());
    let first = service.prepare_shared_publication(&omitted, 100).unwrap();
    let second = service.prepare_shared_publication(&explicit, 100).unwrap();
    assert_ne!(first, second);
    assert_ne!(first.to_bytes().unwrap(), second.to_bytes().unwrap());
}

#[test]
fn publication_codec_rejects_unknown_version_tag_truncation_trailing_data_and_zero_policy() {
    let (mut service, source) = fixture();
    let candidate = task(&mut service, &pet("alice"), "", 100)
        .propose_shared(draft("root", vec![source.clone()]), 100)
        .unwrap();
    let shared = service.prepare_shared_publication(&candidate, 100).unwrap();
    let install = service
        .prepare_formal_publication(&namespace(), formal("v1", &source), 100)
        .unwrap();
    let id = write(&install, &mut service).id();
    let revise = service
        .prepare_formal_revision(&namespace(), id, 1, formal("v2", &source), 200)
        .unwrap();
    for (command, tag) in [(shared, 11), (install, 12), (revise, 13)] {
        let bytes = command.to_bytes().unwrap();
        assert_eq!(bytes[1], tag);
        for end in 0..bytes.len() {
            assert!(MemoryCommand::from_bytes(&bytes[..end]).is_err());
        }
        let mut invalid = bytes.clone();
        invalid.push(0);
        assert!(MemoryCommand::from_bytes(&invalid).is_err());
        let mut invalid = bytes.clone();
        invalid[0] = 1;
        assert!(MemoryCommand::from_bytes(&invalid).is_err());
        let mut invalid = bytes.clone();
        invalid[1] = 14;
        assert!(MemoryCommand::from_bytes(&invalid).is_err());
        let mut invalid = bytes;
        let length = invalid.len();
        invalid[length - 8..].fill(0);
        assert!(MemoryCommand::from_bytes(&invalid).is_err());
    }
}

#[test]
fn formal_read_layer_preserves_purpose_scope_source_budget_and_no_entity_promotion() {
    let (mut service, source) = fixture();
    let command = service
        .prepare_formal_publication(&namespace(), formal("formal document", &source), 100)
        .unwrap();
    let id = write(&command, &mut service).id();
    let query = RetrievalQuery::new("formal").with_layer(RetrievalLayer::FormalMaterial);
    assert!(
        task(&mut service, &pet("alice"), "formal", 100)
            .result()
            .items()
            .is_empty()
    );
    assert_eq!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query, 100)
            .unwrap()
            .items()
            .len(),
        1
    );
    assert!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query.clone().with_char_budget(1), 100)
            .unwrap()
            .items()
            .is_empty()
    );
    assert!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query.clone().with_scope(SearchScope::Private), 100)
            .unwrap()
            .items()
            .is_empty()
    );
    assert_eq!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap()
            .cache_status(),
        CacheStatus::Fallback
    );
    for purpose in [
        ReadPurpose::Unknown,
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        assert_eq!(
            service
                .for_session(&pet("alice"), &session(), purpose)
                .retrieve(&query, 100),
            Err(MemoryError::AccessDenied)
        );
    }
    service
        .set_read_policy(&pet("alice"), ReadPolicy::offline(2, Vec::new()))
        .unwrap();
    assert!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query, 100)
            .unwrap()
            .items()
            .is_empty()
    );
    assert!(service.for_pet(&pet("alice")).get(id, 100).is_ok());
}

#[test]
fn formal_read_tasks_cannot_persist_an_ordinary_copy_or_claim_the_same_wire_read_layer() {
    let (mut service, source) = fixture();
    let command = service
        .prepare_formal_publication(&namespace(), formal("formal root", &source), 100)
        .unwrap();
    write(&command, &mut service);
    let query = RetrievalQuery::new("formal root").with_layer(RetrievalLayer::FormalMaterial);
    let snapshot = service
        .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&query, 100)
        .unwrap();
    service.validate_task(&snapshot, 100).unwrap();
    assert_eq!(
        service.create_from_task(&snapshot, draft("ordinary copy", vec![source]), 100),
        Err(MemoryError::InvalidEvidence)
    );
}

#[test]
fn formal_expiration_is_not_a_license_to_clear_expiration_via_revision() {
    let (mut service, source) = fixture();
    let command = service
        .prepare_formal_publication(
            &namespace(),
            formal("expiring", &source).with_expiration(150),
            100,
        )
        .unwrap();
    let id = write(&command, &mut service).id();
    assert_eq!(
        service.for_pet(&pet("alice")).get(id, 150),
        Err(MemoryError::NotFound)
    );
    assert_eq!(
        service.prepare_formal_revision(&namespace(), id, 1, formal("revived", &source), 150),
        Err(MemoryError::InvalidState)
    );
}

#[test]
fn source_version_change_invalidates_shared_descendants_session_context_and_cached_entities() {
    let (mut service, source) = fixture();
    let root = seed(&mut service, &source, "root");
    let snapshot = task(&mut service, &pet("alice"), "root", 100);
    let candidate = snapshot
        .propose_shared(draft("child", vec![source.clone()]), 100)
        .unwrap();
    let command = service.prepare_shared_publication(&candidate, 100).unwrap();
    let child = write(&command, &mut service).id();
    service
        .put_working_context(
            &pet("alice"),
            &session(),
            "derived",
            1,
            MemoryDraft::new(
                "session child",
                Category::WorkingContext,
                Epistemic::Reference,
                vec![source.clone()],
            )
            .with_lifecycle(Lifecycle::ShortTerm)
            .with_dependencies(vec![RecordVersion::new(child, 1).unwrap()]),
            100,
        )
        .unwrap();
    let query = RetrievalQuery::new("child");
    assert_eq!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap()
            .items()
            .len(),
        2
    );
    service
        .register_source(
            MemoryScope::Shared(namespace()),
            source.id(),
            2,
            Epistemic::Reference,
        )
        .unwrap();
    assert_eq!(
        service.for_pet(&pet("alice")).get(root, 100),
        Err(MemoryError::NotFound)
    );
    assert_eq!(
        service.for_pet(&pet("alice")).get(child, 100),
        Err(MemoryError::NotFound)
    );
    assert!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap()
            .items()
            .is_empty()
    );
}

#[test]
fn duplicate_overflow_stale_and_missing_publication_evidence_fail_without_mutating() {
    let (mut service, source) = fixture();
    let snapshot = task(&mut service, &pet("alice"), "", 100);
    assert!(
        snapshot
            .propose_shared(draft("none", Vec::new()), 100)
            .is_err()
    );
    assert!(
        snapshot
            .propose_shared(draft("overflow", vec![source.clone(); 33]), 100)
            .is_err()
    );
    let duplicate = snapshot
        .propose_shared(
            draft("duplicate", vec![source.clone(), source.clone()]),
            100,
        )
        .unwrap();
    assert_eq!(
        service.prepare_shared_publication(&duplicate, 100),
        Err(MemoryError::InvalidEvidence)
    );
    assert_eq!(
        snapshot
            .propose_shared(draft("backwards", vec![source.clone()]), 99)
            .unwrap_err(),
        MemoryError::TimeWentBackwards
    );
    assert!(
        snapshot
            .propose_shared(
                draft("too long", vec![source.clone()]).with_expiration(100),
                100
            )
            .is_err()
    );
    let command = service
        .prepare_formal_publication(&namespace(), formal("formal", &source), 100)
        .unwrap();
    service.revoke_source(&source).unwrap();
    assert_eq!(
        command.execute(&mut service),
        Err(MemoryError::TaskCancelled)
    );
    assert_eq!(shared_count(&mut service), 0);
}

#[test]
fn formal_source_restore_and_policy_reauthentication_require_explicit_new_versions() {
    let (mut service, source) = fixture();
    let install = service
        .prepare_formal_publication(&namespace(), formal("old", &source), 100)
        .unwrap();
    let id = write(&install, &mut service).id();
    let pending = service
        .prepare_formal_revision(&namespace(), id, 1, formal("pending", &source), 200)
        .unwrap();
    service
        .set_formal_publication_policy(&namespace(), PublicationPolicy::deny_all(2))
        .unwrap();
    assert_eq!(
        pending.execute(&mut service),
        Err(MemoryError::PublicationDenied)
    );
    service.revoke_source(&source).unwrap();
    let restored = service.restore_source(&source, 2).unwrap();
    assert_eq!(
        service.prepare_formal_revision(&namespace(), id, 1, formal("new", &restored), 200),
        Err(MemoryError::PublicationDenied)
    );
    service
        .set_formal_publication_policy(
            &namespace(),
            PublicationPolicy::allow_sources(3, vec![restored.clone()]),
        )
        .unwrap();
    let command = service
        .prepare_formal_revision(&namespace(), id, 1, formal("new", &restored), 200)
        .unwrap();
    write(&command, &mut service);
    let query = RetrievalQuery::new("new").with_layer(RetrievalLayer::FormalMaterial);
    assert!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query, 200)
            .unwrap()
            .items()
            .is_empty()
    );
    service
        .set_read_policy(&pet("alice"), ReadPolicy::offline(2, vec![restored]))
        .unwrap();
    assert_eq!(
        service
            .for_session(&pet("alice"), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query, 200)
            .unwrap()
            .items()
            .len(),
        1
    );
}

#[test]
fn review_time_is_monotonic_and_original_proposal_time_remains_in_the_request() {
    let (mut service, source) = fixture();
    let snapshot = task(&mut service, &pet("alice"), "", 100);
    let first = snapshot
        .propose_shared(draft("same", vec![source.clone()]), 100)
        .unwrap();
    let second = snapshot
        .propose_shared(draft("same", vec![source]), 101)
        .unwrap();
    assert_eq!(first.proposed_at_ms(), 100);
    assert_eq!(
        service.prepare_shared_publication(&second, 100),
        Err(MemoryError::TimeWentBackwards)
    );
    let first = service.prepare_shared_publication(&first, 200).unwrap();
    let second = service.prepare_shared_publication(&second, 200).unwrap();
    assert_ne!(first, second);
    let id = write(&first, &mut service).id();
    assert_eq!(
        service
            .for_pet(&pet("alice"))
            .get(id, 200)
            .unwrap()
            .created_at_ms(),
        200
    );
}
