use moe_memory_core::{
    CacheStatus, Category, EntityRead, Epistemic, Lifecycle, MemoryDraft, MemoryError, MemoryScope,
    MemoryService, Namespace, PetIdentity, ReadPolicy, ReadPurpose, RetrievalItem, RetrievalLayer,
    RetrievalQuery, RetrievalResult, SearchScope, Sensitivity, SessionId, SourceRef,
};

fn pet(workspace: &str, user: &str, instance: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new(workspace, user).unwrap(), instance).unwrap()
}

fn session(name: &str) -> SessionId {
    SessionId::new(name).unwrap()
}

fn evidence(service: &mut MemoryService, owner: &PetIdentity, name: &str) -> SourceRef {
    service
        .register_source(
            MemoryScope::Private(owner.clone()),
            name,
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
}

fn draft(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Episode,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
}

fn context(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::WorkingContext,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
    .with_lifecycle(Lifecycle::ShortTerm)
}

fn authority(service: &mut MemoryService, owner: &PetIdentity) -> SourceRef {
    service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "host-identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap()
}

fn fixture() -> (MemoryService, PetIdentity, SourceRef) {
    let mut service = MemoryService::new();
    let owner = pet("home", "user", "alice");
    let source = evidence(&mut service, &owner, "message");
    service
        .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    (service, owner, source)
}

fn retrieve(
    service: &mut MemoryService,
    owner: &PetIdentity,
    query: &RetrievalQuery,
    now: u64,
) -> RetrievalResult {
    service
        .for_session(owner, &session("current"), ReadPurpose::OfflineReasoning)
        .retrieve(query, now)
        .unwrap()
}

fn entity(
    service: &mut MemoryService,
    owner: &PetIdentity,
    query: &RetrievalQuery,
    now: u64,
) -> EntityRead {
    service
        .for_session(owner, &session("current"), ReadPurpose::OfflineReasoning)
        .entity(query, now)
        .unwrap()
}

#[test]
fn working_context_checks_category_lifecycle_versions_and_time() {
    let (mut service, owner, source) = fixture();
    assert!(
        service
            .put_working_context(
                &owner,
                &session("current"),
                "context",
                1,
                draft("not working", &source),
                100
            )
            .is_err()
    );
    assert!(
        service
            .put_working_context(
                &owner,
                &session("current"),
                "context",
                0,
                context("tea", &source),
                100
            )
            .is_err()
    );
    service
        .put_working_context(
            &owner,
            &session("current"),
            "context",
            1,
            context("tea", &source),
            100,
        )
        .unwrap();
    assert_eq!(
        service.put_working_context(
            &owner,
            &session("current"),
            "context",
            1,
            context("replace", &source),
            200
        ),
        Err(MemoryError::ContextVersionConflict)
    );
    assert_eq!(
        service.put_working_context(
            &owner,
            &session("current"),
            "context",
            2,
            context("replace", &source),
            99
        ),
        Err(MemoryError::TimeWentBackwards)
    );
}

#[test]
fn working_context_rechecks_sensitivity_expiry_and_source_versions() {
    let (mut service, owner, source) = fixture();
    service
        .put_working_context(
            &owner,
            &session("current"),
            "restricted",
            1,
            context("tea restricted", &source).with_sensitivity(Sensitivity::Restricted),
            100,
        )
        .unwrap();
    service
        .put_working_context(
            &owner,
            &session("current"),
            "temporary",
            1,
            context("tea expires", &source).with_expiration(200),
            100,
        )
        .unwrap();
    assert_eq!(
        retrieve(&mut service, &owner, &RetrievalQuery::new("tea"), 199)
            .items()
            .len(),
        1
    );
    assert!(
        retrieve(&mut service, &owner, &RetrievalQuery::new("tea"), 200)
            .items()
            .is_empty()
    );
    service
        .put_working_context(
            &owner,
            &session("current"),
            "valid",
            1,
            context("tea valid", &source),
            200,
        )
        .unwrap();
    service
        .register_source(
            MemoryScope::Private(owner.clone()),
            source.id(),
            2,
            Epistemic::UserStatement,
        )
        .unwrap();
    assert!(
        retrieve(&mut service, &owner, &RetrievalQuery::new("tea"), 200)
            .items()
            .is_empty()
    );
}

#[test]
fn working_context_sensitivity_transitions_preserve_state_on_rejection() {
    use Sensitivity::{Normal, Private, Restricted};

    for (current, next, allowed) in [
        (Normal, Normal, true),
        (Normal, Private, true),
        (Normal, Restricted, true),
        (Private, Normal, false),
        (Private, Private, true),
        (Private, Restricted, true),
        (Restricted, Normal, false),
        (Restricted, Private, false),
        (Restricted, Restricted, true),
    ] {
        let (mut service, owner, source) = fixture();
        service
            .put_working_context(
                &owner,
                &session("current"),
                "context",
                1,
                context("tea original", &source).with_sensitivity(current),
                100,
            )
            .unwrap();
        let query = RetrievalQuery::new("tea");
        let before = retrieve(&mut service, &owner, &query, 100);
        entity(&mut service, &owner, &query, 100);
        let task = service
            .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
            .begin_task(&query, 100)
            .unwrap();
        let generation = service.generation();
        let result = service.put_working_context(
            &owner,
            &session("current"),
            "context",
            2,
            context("tea updated", &source).with_sensitivity(next),
            200,
        );
        assert_eq!(service.generation(), generation);
        if allowed {
            assert_eq!(result, Ok(()), "{current:?} -> {next:?}");
            assert_eq!(
                service.validate_task(&task, 200),
                Err(MemoryError::TaskCancelled)
            );
            let after = retrieve(&mut service, &owner, &query, 200);
            if next == Restricted {
                assert!(after.items().is_empty());
            } else {
                assert_eq!(after.items().len(), 1);
                assert_eq!(after.items()[0].text(), "tea updated");
                assert_eq!(after.items()[0].sensitivity(), next);
                let RetrievalItem::WorkingContext(context) = &after.items()[0] else {
                    panic!("expected working context");
                };
                assert_eq!(context.revision(), 2);
            }
        } else {
            assert_eq!(
                result,
                Err(MemoryError::ProtectedContent),
                "{current:?} -> {next:?}"
            );
            assert_eq!(retrieve(&mut service, &owner, &query, 200), before);
            assert_eq!(service.validate_task(&task, 200), Ok(()));
            assert_eq!(
                entity(&mut service, &owner, &query, 200).cache_status(),
                CacheStatus::Hit
            );
            service
                .put_working_context(
                    &owner,
                    &session("current"),
                    "context",
                    2,
                    context("tea retry", &source).with_sensitivity(current),
                    150,
                )
                .unwrap();
            assert_eq!(
                service.validate_task(&task, 200),
                Err(MemoryError::TaskCancelled)
            );
        }
    }
}

#[test]
fn working_context_with_private_dependency_can_tighten_sensitivity() {
    let (mut service, owner, source) = fixture();
    let parent = service
        .for_pet(&owner)
        .create(
            draft("tea private parent", &source).with_sensitivity(Sensitivity::Private),
            100,
        )
        .unwrap();
    let dependency = service
        .for_pet(&owner)
        .get(parent.id(), 100)
        .unwrap()
        .version_ref();
    let original = context("tea private context", &source)
        .with_sensitivity(Sensitivity::Private)
        .with_dependencies(vec![dependency]);
    service
        .put_working_context(&owner, &session("current"), "context", 1, original, 100)
        .unwrap();
    let query = RetrievalQuery::new("tea").with_layer(RetrievalLayer::WorkingContext);
    assert_eq!(entity(&mut service, &owner, &query, 100).items().len(), 1);
    let task = service
        .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
        .begin_task(&query, 100)
        .unwrap();
    let tightened = context("tea private context", &source)
        .with_sensitivity(Sensitivity::Restricted)
        .with_dependencies(vec![dependency]);
    service
        .put_working_context(&owner, &session("current"), "context", 2, tightened, 200)
        .unwrap();
    assert_eq!(
        service.validate_task(&task, 200),
        Err(MemoryError::TaskCancelled)
    );
    assert!(
        retrieve(&mut service, &owner, &query, 200)
            .items()
            .is_empty()
    );
    let current = entity(&mut service, &owner, &query, 200);
    assert_eq!(current.cache_status(), CacheStatus::Built);
    assert!(current.items().is_empty());
    assert_eq!(
        service
            .for_pet(&owner)
            .get(parent.id(), 200)
            .unwrap()
            .sensitivity(),
        Sensitivity::Private
    );
}

#[test]
fn identity_is_host_protected_and_not_synthesized_from_ordinary_records() {
    let (mut service, owner, source) = fixture();
    let authority = authority(&mut service, &owner);
    service
        .set_identity_view(&owner, 1, "trusted anchor", vec![authority.clone()])
        .unwrap();
    service
        .set_read_policy(
            &owner,
            ReadPolicy::offline(2, vec![source.clone(), authority]),
        )
        .unwrap();
    assert_eq!(
        service.for_pet(&owner).create(
            MemoryDraft::new(
                "override",
                Category::Identity,
                Epistemic::UserStatement,
                vec![source.clone()]
            ),
            100
        ),
        Err(MemoryError::ProtectedContent)
    );
    service
        .for_pet(&owner)
        .create(draft("trusted anchor ordinary claim", &source), 100)
        .unwrap();
    let view = service
        .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
        .identity()
        .unwrap();
    assert_eq!(view.text(), "trusted anchor");
    assert_eq!(view.version(), 1);
    let result = entity(
        &mut service,
        &owner,
        &RetrievalQuery::new("trusted anchor"),
        100,
    );
    assert_eq!(result.items().len(), 1);
    assert_eq!(result.items()[0].epistemic(), Epistemic::UserStatement);
    assert_ne!(result.items()[0].text(), view.text());
}

#[test]
fn identity_requires_private_authority_unique_evidence_and_advancing_versions() {
    let (mut service, owner, ordinary) = fixture();
    assert_eq!(
        service.set_identity_view(&owner, 1, "anchor", vec![ordinary]),
        Err(MemoryError::InvalidEvidence)
    );
    let shared = service
        .register_source(
            MemoryScope::Shared(owner.namespace().clone()),
            "shared-authority",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    assert_eq!(
        service.set_identity_view(&owner, 1, "anchor", vec![shared]),
        Err(MemoryError::InvalidEvidence)
    );
    let source = authority(&mut service, &owner);
    assert_eq!(
        service.set_identity_view(&owner, 1, "anchor", vec![source.clone(), source.clone()]),
        Err(MemoryError::InvalidEvidence)
    );
    service
        .set_identity_view(&owner, 1, "anchor", vec![source.clone()])
        .unwrap();
    assert_eq!(
        service.set_identity_view(&owner, 1, "replace", vec![source.clone()]),
        Err(MemoryError::IdentityVersionConflict)
    );
    service
        .set_identity_view(&owner, 2, "replace", vec![source])
        .unwrap();
}

#[test]
fn identity_rechecks_grants_source_versions_and_owner() {
    let (mut service, owner, _) = fixture();
    let source = authority(&mut service, &owner);
    service
        .set_identity_view(&owner, 1, "anchor", vec![source.clone()])
        .unwrap();
    assert_eq!(
        service
            .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
            .identity(),
        Err(MemoryError::NotFound)
    );
    service
        .set_read_policy(&owner, ReadPolicy::offline(2, vec![source.clone()]))
        .unwrap();
    assert!(
        service
            .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
            .identity()
            .is_ok()
    );
    let other = pet("home", "user", "bob");
    service
        .set_read_policy(&other, ReadPolicy::offline(1, vec![]))
        .unwrap();
    assert_eq!(
        service
            .for_session(&other, &session("current"), ReadPurpose::OfflineReasoning)
            .identity(),
        Err(MemoryError::NotFound)
    );
    service
        .register_source(
            MemoryScope::Private(owner.clone()),
            source.id(),
            2,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    assert_eq!(
        service
            .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
            .identity(),
        Err(MemoryError::NotFound)
    );
}

#[test]
fn cache_hits_reauthorize_and_depend_on_record_revisions() {
    let (mut service, owner, source) = fixture();
    let created = service
        .for_pet(&owner)
        .create(draft("tea old", &source), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    let old = entity(&mut service, &owner, &query, 100);
    assert_eq!(old.cache_status(), CacheStatus::Built);
    assert_eq!(
        entity(&mut service, &owner, &query, 101).cache_status(),
        CacheStatus::Hit
    );
    service
        .for_pet(&owner)
        .update(created.id(), 1, draft("tea corrected", &source), 200)
        .unwrap();
    let current = entity(&mut service, &owner, &query, 200);
    assert_eq!(current.cache_status(), CacheStatus::Built);
    assert_eq!(current.items()[0].text(), "tea corrected");
    assert_eq!(old.items()[0].text(), "tea old");
}

#[test]
fn cache_detects_new_records_not_only_previous_read_set() {
    let (mut service, owner, source) = fixture();
    let query = RetrievalQuery::new("tea").with_limit(1);
    assert!(entity(&mut service, &owner, &query, 100).items().is_empty());
    service
        .for_pet(&owner)
        .create(draft("tea new", &source), 100)
        .unwrap();
    let result = entity(&mut service, &owner, &query, 100);
    assert_eq!(result.cache_status(), CacheStatus::Built);
    assert_eq!(result.items()[0].text(), "tea new");
}

#[test]
fn deletion_invalidates_cached_entity_without_resurrection() {
    let (mut service, owner, source) = fixture();
    let created = service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    entity(&mut service, &owner, &query, 100);
    service
        .for_pet(&owner)
        .delete(created.id(), 1, 200)
        .unwrap();
    let result = entity(&mut service, &owner, &query, 200);
    assert_eq!(result.cache_status(), CacheStatus::Built);
    assert!(result.items().is_empty());
}

#[test]
fn expiration_invalidates_cache_at_exact_boundary_without_mutation() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea", &source).with_expiration(200), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    entity(&mut service, &owner, &query, 100);
    assert_eq!(
        entity(&mut service, &owner, &query, 199).cache_status(),
        CacheStatus::Hit
    );
    let result = entity(&mut service, &owner, &query, 200);
    assert_eq!(result.cache_status(), CacheStatus::Built);
    assert!(result.items().is_empty());
}

#[test]
fn policy_version_changes_rebuild_even_when_entries_are_unchanged() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    entity(&mut service, &owner, &query, 100);
    service
        .set_read_policy(&owner, ReadPolicy::offline(2, vec![source]))
        .unwrap();
    assert_eq!(
        entity(&mut service, &owner, &query, 100).cache_status(),
        CacheStatus::Built
    );
}

#[test]
fn grant_revocation_and_deny_all_never_return_stale_cached_content() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    entity(&mut service, &owner, &query, 100);
    service
        .set_read_policy(&owner, ReadPolicy::offline(2, vec![]))
        .unwrap();
    let result = entity(&mut service, &owner, &query, 100);
    assert_eq!(result.cache_status(), CacheStatus::Built);
    assert!(result.items().is_empty());
    service
        .set_read_policy(&owner, ReadPolicy::deny_all(3))
        .unwrap();
    assert_eq!(
        service
            .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
            .entity(&query, 100),
        Err(MemoryError::AccessDenied)
    );
}

#[test]
fn source_version_changes_invalidate_entity_cache() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    entity(&mut service, &owner, &query, 100);
    service
        .register_source(
            MemoryScope::Private(owner.clone()),
            source.id(),
            2,
            Epistemic::UserStatement,
        )
        .unwrap();
    let result = entity(&mut service, &owner, &query, 100);
    assert_eq!(result.cache_status(), CacheStatus::Built);
    assert!(result.items().is_empty());
}

#[test]
fn working_context_versions_and_session_binding_invalidate_or_partition_cache() {
    let (mut service, owner, source) = fixture();
    let query = RetrievalQuery::new("tea");
    service
        .put_working_context(
            &owner,
            &session("current"),
            "context",
            1,
            context("tea old", &source),
            100,
        )
        .unwrap();
    entity(&mut service, &owner, &query, 100);
    service
        .put_working_context(
            &owner,
            &session("current"),
            "context",
            2,
            context("tea new", &source),
            200,
        )
        .unwrap();
    let result = entity(&mut service, &owner, &query, 200);
    assert_eq!(result.cache_status(), CacheStatus::Built);
    assert_eq!(result.items()[0].text(), "tea new");
    let other = service
        .for_session(&owner, &session("other"), ReadPurpose::OfflineReasoning)
        .entity(&query, 200)
        .unwrap();
    assert_eq!(other.cache_status(), CacheStatus::Built);
    assert!(other.items().is_empty());
}

#[test]
fn cache_key_includes_query_scope_layer_count_and_budget() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    let queries = [
        RetrievalQuery::new("tea"),
        RetrievalQuery::new("TEA"),
        RetrievalQuery::new("tea").with_scope(SearchScope::Private),
        RetrievalQuery::new("tea").with_layer(RetrievalLayer::LongTerm),
        RetrievalQuery::new("tea").with_limit(1),
        RetrievalQuery::new("tea").with_char_budget(1),
    ];
    for query in queries {
        assert_eq!(
            entity(&mut service, &owner, &query, 100).cache_status(),
            CacheStatus::Built
        );
        assert_eq!(
            entity(&mut service, &owner, &query, 100).cache_status(),
            CacheStatus::Hit
        );
    }
}

#[test]
fn unavailable_or_full_cache_falls_back_to_identically_authorized_budgeted_entries() {
    let (mut service, owner, source) = fixture();
    let denied = evidence(&mut service, &owner, "denied");
    service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    service
        .for_pet(&owner)
        .create(draft("tea tea", &denied), 200)
        .unwrap();
    let query = RetrievalQuery::new("tea").with_char_budget(3).with_limit(1);
    let expected = retrieve(&mut service, &owner, &query, 200);
    service.set_entity_cache_capacity(0).unwrap();
    let result = entity(&mut service, &owner, &query, 200);
    assert!(matches!(result, EntityRead::Entries(_)));
    assert_eq!(result.cache_status(), CacheStatus::Fallback);
    assert_eq!(result.items(), expected.items());
    assert_eq!(result.used_chars(), expected.used_chars());
    service.set_entity_cache_capacity(1).unwrap();
    entity(&mut service, &owner, &RetrievalQuery::new("unused"), 200);
    assert_eq!(
        entity(&mut service, &owner, &query, 200).items(),
        expected.items()
    );
    assert_eq!(
        entity(&mut service, &owner, &query, 200).cache_status(),
        CacheStatus::Fallback
    );
    assert!(service.set_entity_cache_capacity(257).is_err());
    service
        .set_read_policy(&owner, ReadPolicy::deny_all(2))
        .unwrap();
    assert_eq!(
        service
            .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
            .entity(&query, 200),
        Err(MemoryError::AccessDenied)
    );
}

#[test]
fn invalid_or_old_policies_do_not_replace_existing_grants() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    assert_eq!(
        service.set_read_policy(&owner, ReadPolicy::deny_all(1)),
        Err(MemoryError::PolicyVersionConflict)
    );
    assert!(
        service
            .set_read_policy(&owner, ReadPolicy::deny_all(0))
            .is_err()
    );
    assert_eq!(
        retrieve(&mut service, &owner, &RetrievalQuery::new("tea"), 100)
            .items()
            .len(),
        1
    );
}

#[test]
fn debug_output_redacts_query_entity_identity_and_working_body() {
    let (mut service, owner, source) = fixture();
    let secret = "sensitive-unique-body";
    service
        .put_working_context(
            &owner,
            &session("current"),
            secret,
            1,
            context(secret, &source),
            100,
        )
        .unwrap();
    let query = RetrievalQuery::new(secret);
    assert!(!format!("{query:?}").contains(secret));
    assert!(!format!("{:?}", entity(&mut service, &owner, &query, 100)).contains(secret));
    let authority = authority(&mut service, &owner);
    service
        .set_identity_view(&owner, 1, secret, vec![authority.clone()])
        .unwrap();
    service
        .set_read_policy(&owner, ReadPolicy::offline(2, vec![authority]))
        .unwrap();
    let view = service
        .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
        .identity()
        .unwrap();
    assert!(!format!("{view:?}").contains(secret));
}

#[test]
fn absent_policy_denies_all_read_surfaces() {
    let owner = pet("home", "user", "alice");
    let mut service = MemoryService::new();
    let source = evidence(&mut service, &owner, "message");
    service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    let mut reader =
        service.for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning);
    assert_eq!(
        reader.retrieve(&RetrievalQuery::new("tea"), 100),
        Err(MemoryError::AccessDenied)
    );
    assert_eq!(
        reader.entity(&RetrievalQuery::new("tea"), 100),
        Err(MemoryError::AccessDenied)
    );
    assert_eq!(reader.identity(), Err(MemoryError::AccessDenied));
}

#[test]
fn unknown_viewing_and_transmission_purposes_cannot_reuse_offline_cache() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea", &source), 100)
        .unwrap();
    entity(&mut service, &owner, &RetrievalQuery::new("tea"), 100);
    for purpose in [
        ReadPurpose::default(),
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        let mut reader = service.for_session(&owner, &session("current"), purpose);
        assert_eq!(
            reader.retrieve(&RetrievalQuery::new(""), 100),
            Err(MemoryError::AccessDenied)
        );
        assert_eq!(
            reader.entity(&RetrievalQuery::new("tea"), 100),
            Err(MemoryError::AccessDenied)
        );
        assert_eq!(reader.identity(), Err(MemoryError::AccessDenied));
    }
}

#[test]
fn authorization_happens_before_relevance_limit_and_budget() {
    let (mut service, owner, allowed) = fixture();
    let denied = evidence(&mut service, &owner, "unapproved");
    service
        .for_pet(&owner)
        .create(draft("tea", &allowed), 100)
        .unwrap();
    service
        .for_pet(&owner)
        .create(draft("tea tea tea", &denied), 300)
        .unwrap();
    service
        .for_pet(&owner)
        .create(
            draft("tea tea", &allowed).with_sensitivity(Sensitivity::Restricted),
            400,
        )
        .unwrap();
    let other = pet("home", "user", "bob");
    let other_source = evidence(&mut service, &other, "message");
    service
        .for_pet(&other)
        .create(draft("tea tea tea tea", &other_source), 500)
        .unwrap();
    let query = RetrievalQuery::new("tea").with_limit(1).with_char_budget(3);
    let result = retrieve(&mut service, &owner, &query, 500);
    assert_eq!(result.items().len(), 1);
    assert_eq!(result.items()[0].text(), "tea");
    assert_eq!(result.used_chars(), 3);
    assert_eq!(
        entity(&mut service, &owner, &query, 500).items(),
        result.items()
    );
}

#[test]
fn every_source_must_be_granted_and_valid_not_just_one() {
    let (mut service, owner, allowed) = fixture();
    let denied = evidence(&mut service, &owner, "unapproved");
    let combined = MemoryDraft::new(
        "tea",
        Category::Episode,
        Epistemic::Inference,
        vec![allowed.clone(), denied.clone()],
    );
    service.for_pet(&owner).create(combined, 100).unwrap();
    assert!(
        retrieve(&mut service, &owner, &RetrievalQuery::new("tea"), 100)
            .items()
            .is_empty()
    );
    service
        .set_read_policy(
            &owner,
            ReadPolicy::offline(2, vec![allowed, denied.clone()]),
        )
        .unwrap();
    assert_eq!(
        retrieve(&mut service, &owner, &RetrievalQuery::new("tea"), 100)
            .items()
            .len(),
        1
    );
    service
        .register_source(
            MemoryScope::Private(owner.clone()),
            denied.id(),
            2,
            Epistemic::UserStatement,
        )
        .unwrap();
    assert!(
        retrieve(&mut service, &owner, &RetrievalQuery::new("tea"), 100)
            .items()
            .is_empty()
    );
}

#[test]
fn budget_counts_unicode_scalars_skips_oversized_entries_and_accepts_zero() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("茶🐾", &source), 100)
        .unwrap();
    service
        .for_pet(&owner)
        .create(draft("oversized", &source), 200)
        .unwrap();
    let result = retrieve(
        &mut service,
        &owner,
        &RetrievalQuery::new("").with_char_budget(2),
        200,
    );
    assert_eq!(result.used_chars(), 2);
    assert_eq!(result.items()[0].text(), "茶🐾");
    let result = retrieve(
        &mut service,
        &owner,
        &RetrievalQuery::new("").with_char_budget(0),
        200,
    );
    assert!(result.items().is_empty());
    assert_eq!(result.used_chars(), 0);
}

#[test]
fn ranking_is_deterministic_and_applies_count_after_authorization() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea first", &source), 200)
        .unwrap();
    service
        .for_pet(&owner)
        .create(draft("tea second", &source), 200)
        .unwrap();
    service
        .for_pet(&owner)
        .create(draft("tea tea", &source), 100)
        .unwrap();
    let result = retrieve(
        &mut service,
        &owner,
        &RetrievalQuery::new("TEA").with_limit(2),
        200,
    );
    assert_eq!(
        result
            .items()
            .iter()
            .map(RetrievalItem::text)
            .collect::<Vec<_>>(),
        ["tea tea", "tea first"]
    );
}

#[test]
fn query_and_session_validation_are_bounded() {
    let (mut service, owner, _) = fixture();
    for query in [
        RetrievalQuery::new("\0"),
        RetrievalQuery::new("a".repeat(257)),
        RetrievalQuery::new("").with_limit(0),
        RetrievalQuery::new("").with_limit(101),
        RetrievalQuery::new("").with_char_budget(409_601),
    ] {
        assert!(matches!(
            service
                .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
                .retrieve(&query, 100),
            Err(MemoryError::InvalidInput(_))
        ));
    }
    assert!(
        service
            .for_session(&owner, &session("current"), ReadPurpose::OfflineReasoning)
            .entity(&RetrievalQuery::new(" "), 100)
            .is_err()
    );
    for invalid in ["", " leading", "newline\n"] {
        assert!(SessionId::new(invalid).is_err());
    }
}

#[test]
fn pet_user_and_workspace_isolation_applies_to_entries_and_cache() {
    let mut service = MemoryService::new();
    let owners = [
        pet("home", "user", "alice"),
        pet("home", "user", "bob"),
        pet("home", "other-user", "alice"),
        pet("other-home", "user", "alice"),
    ];
    for (index, owner) in owners.iter().enumerate() {
        let source = evidence(&mut service, owner, "same-source-name");
        service
            .set_read_policy(owner, ReadPolicy::offline(1, vec![source.clone()]))
            .unwrap();
        service
            .for_pet(owner)
            .create(draft(&format!("tea-{index}"), &source), 100)
            .unwrap();
        service
            .put_working_context(
                owner,
                &session("current"),
                "same-context",
                1,
                context(&format!("tea-context-{index}"), &source),
                100,
            )
            .unwrap();
    }
    for (index, owner) in owners.iter().enumerate() {
        let result = entity(&mut service, owner, &RetrievalQuery::new("tea"), 100);
        assert_eq!(result.cache_status(), CacheStatus::Built);
        assert_eq!(result.items().len(), 2);
        assert_eq!(result.items()[0].text(), format!("tea-{index}"));
        assert_eq!(result.items()[1].text(), format!("tea-context-{index}"));
        assert_eq!(
            entity(&mut service, owner, &RetrievalQuery::new("tea"), 101).cache_status(),
            CacheStatus::Hit
        );
    }
}

#[test]
fn policies_cannot_grant_other_pets_users_or_workspaces_sources() {
    let (mut service, owner, _) = fixture();
    for outsider in [
        pet("home", "user", "bob"),
        pet("home", "other", "alice"),
        pet("other", "user", "alice"),
    ] {
        let source = evidence(&mut service, &outsider, "message");
        assert_eq!(
            service.set_read_policy(&owner, ReadPolicy::offline(2, vec![source])),
            Err(MemoryError::InvalidEvidence)
        );
    }
}

#[test]
fn working_context_is_bound_to_current_session_and_separate_from_long_term() {
    let (mut service, owner, source) = fixture();
    service
        .for_pet(&owner)
        .create(draft("tea long term", &source), 100)
        .unwrap();
    service
        .for_pet(&owner)
        .create(
            draft("tea unbound short term", &source).with_lifecycle(Lifecycle::ShortTerm),
            100,
        )
        .unwrap();
    service
        .for_pet(&owner)
        .create(
            MemoryDraft::new(
                "tea unbound working",
                Category::WorkingContext,
                Epistemic::UserStatement,
                vec![source.clone()],
            ),
            100,
        )
        .unwrap();
    service
        .put_working_context(
            &owner,
            &session("current"),
            "context",
            1,
            context("tea current", &source),
            100,
        )
        .unwrap();
    service
        .put_working_context(
            &owner,
            &session("old"),
            "context",
            1,
            context("tea old", &source),
            100,
        )
        .unwrap();
    let result = retrieve(&mut service, &owner, &RetrievalQuery::new("tea"), 100);
    assert_eq!(
        result
            .items()
            .iter()
            .map(RetrievalItem::text)
            .collect::<Vec<_>>(),
        ["tea long term", "tea current"]
    );
    assert!(matches!(result.items()[0], RetrievalItem::LongTerm(_)));
    assert!(matches!(
        result.items()[1],
        RetrievalItem::WorkingContext(_)
    ));
    for layer in [RetrievalLayer::LongTerm, RetrievalLayer::WorkingContext] {
        assert_eq!(
            retrieve(
                &mut service,
                &owner,
                &RetrievalQuery::new("tea").with_layer(layer),
                100
            )
            .items()
            .len(),
            1
        );
    }
    assert!(
        retrieve(
            &mut service,
            &owner,
            &RetrievalQuery::new("tea")
                .with_layer(RetrievalLayer::WorkingContext)
                .with_scope(SearchScope::Shared),
            100
        )
        .items()
        .is_empty()
    );
    let other = service
        .for_session(&owner, &session("new"), ReadPurpose::OfflineReasoning)
        .retrieve(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    assert_eq!(other.items().len(), 1);
}
