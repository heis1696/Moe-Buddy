use moe_memory_core::{
    CacheStatus, Category, CommandResult, Epistemic, Lifecycle, MemoryCommand, MemoryDraft,
    MemoryError, MemoryId, MemoryScope, MemoryService, Namespace, PetIdentity, ReadPolicy,
    ReadPurpose, RecordState, RecordVersion, RetrievalQuery, Sensitivity, SessionId, SourceRef,
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

fn context(source: &SourceRef, dependencies: Vec<RecordVersion>) -> MemoryDraft {
    MemoryDraft::new(
        "tea context",
        Category::WorkingContext,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
    .with_lifecycle(Lifecycle::ShortTerm)
    .with_dependencies(dependencies)
}

fn fixture() -> (MemoryService, PetIdentity, SourceRef, MemoryId) {
    let mut service = MemoryService::new();
    let pet = owner("alice");
    let source = service
        .register_source(
            MemoryScope::Private(pet.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let id = service
        .for_pet(&pet)
        .create(draft("tea original", &source), 100)
        .unwrap()
        .id();
    service
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    (service, pet, source, id)
}

fn reference(service: &mut MemoryService, pet: &PetIdentity, id: MemoryId) -> RecordVersion {
    service.for_pet(pet).get(id, 100).unwrap().version_ref()
}

fn entity_count(service: &mut MemoryService, pet: &PetIdentity, now: u64) -> (usize, CacheStatus) {
    let view = service
        .for_session(pet, &session(), ReadPurpose::OfflineReasoning)
        .entity(&RetrievalQuery::new("tea"), now)
        .unwrap();
    (view.items().len(), view.cache_status())
}

#[test]
fn archive_restore_is_explicit_versioned_and_not_erasure() {
    let (mut service, pet, source, id) = fixture();
    let snapshot = service.for_pet(&pet).get(id, 100).unwrap();
    assert_eq!(
        entity_count(&mut service, &pet, 100),
        (1, CacheStatus::Built)
    );
    service.for_pet(&pet).archive(id, 1, 200).unwrap();
    let status = service.for_pet(&pet).status(id, 200).unwrap();
    assert_eq!(status.state(), RecordState::Archived);
    assert!(!status.is_available());
    assert!(!status.physical_erasure_performed());
    assert_eq!(entity_count(&mut service, &pet, 200).0, 0);
    assert_eq!(
        service
            .for_pet(&pet)
            .update(id, 2, draft("revive", &source), 200),
        Err(MemoryError::InvalidState)
    );
    let restored = service.for_pet(&pet).restore_archive(id, 2, 300).unwrap();
    assert_eq!(restored.revision(), 3);
    assert_eq!(entity_count(&mut service, &pet, 300).0, 1);
    assert_eq!(snapshot.text(), "tea original");
}

#[test]
fn expiration_is_a_read_boundary_not_archive_or_deletion() {
    let (mut service, pet, source, _) = fixture();
    let id = service
        .for_pet(&pet)
        .create(draft("tea temporary", &source).with_expiration(200), 100)
        .unwrap()
        .id();
    assert_eq!(
        service.for_pet(&pet).status(id, 199).unwrap().state(),
        RecordState::Active
    );
    assert_eq!(
        service.for_pet(&pet).status(id, 200).unwrap().state(),
        RecordState::Expired
    );
    assert_eq!(
        service
            .for_pet(&pet)
            .update(id, 1, draft("revive", &source), 200),
        Err(MemoryError::InvalidState)
    );
    assert_eq!(
        service.for_pet(&pet).restore_archive(id, 1, 200),
        Err(MemoryError::InvalidState)
    );
    let replacement = service
        .for_pet(&pet)
        .replace(id, 1, draft("tea fresh correction", &source), 200)
        .unwrap();
    assert_ne!(replacement.replacement().id(), id);
}

#[test]
fn an_expired_archive_cannot_be_restored_or_have_its_deadline_cleared() {
    let (mut service, pet, source, _) = fixture();
    let id = service
        .for_pet(&pet)
        .create(draft("tea temporary", &source).with_expiration(200), 100)
        .unwrap()
        .id();
    service.for_pet(&pet).archive(id, 1, 150).unwrap();
    let generation = service.generation();
    assert!(service.for_pet(&pet).restore_archive(id, 2, 200).is_err());
    assert_eq!(service.generation(), generation);
    assert_eq!(
        service.for_pet(&pet).status(id, 200).unwrap().state(),
        RecordState::Archived
    );
}

#[test]
fn atomic_replacement_retires_old_id_and_cannot_be_undone_by_crud() {
    let (mut service, pet, source, id) = fixture();
    let receipt = service
        .for_pet(&pet)
        .replace(id, 1, draft("tea corrected", &source), 200)
        .unwrap();
    assert_eq!(receipt.previous().revision(), 2);
    assert_eq!(
        service.for_pet(&pet).status(id, 200).unwrap().state(),
        RecordState::Superseded(receipt.replacement().id())
    );
    assert_eq!(
        service.for_pet(&pet).get(id, 200),
        Err(MemoryError::NotFound)
    );
    assert_eq!(
        service.for_pet(&pet).restore_archive(id, 2, 200),
        Err(MemoryError::InvalidState)
    );
    assert_eq!(
        service
            .for_pet(&pet)
            .update(id, 2, draft("late", &source), 200),
        Err(MemoryError::InvalidState)
    );
    assert_eq!(
        service
            .for_pet(&pet)
            .get(receipt.replacement().id(), 200)
            .unwrap()
            .text(),
        "tea corrected"
    );
}

#[test]
fn rejected_replacement_preserves_both_state_and_id_allocation() {
    let (mut service, pet, source, id) = fixture();
    let generation = service.generation();
    assert!(
        service
            .for_pet(&pet)
            .replace(id, 2, draft("wrong cas", &source), 200)
            .is_err()
    );
    assert!(
        service
            .for_pet(&pet)
            .replace(
                id,
                1,
                MemoryDraft::new(
                    "no sources",
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![]
                ),
                200
            )
            .is_err()
    );
    assert_eq!(service.generation(), generation);
    assert_eq!(service.for_pet(&pet).get(id, 200).unwrap().revision(), 1);
    let receipt = service
        .for_pet(&pet)
        .replace(id, 1, draft("valid", &source), 200)
        .unwrap();
    let control = fixture()
        .0
        .for_pet(&pet)
        .create(draft("valid", &source), 200)
        .unwrap();
    assert_eq!(receipt.replacement().id(), control.id());
}

#[test]
fn replacement_does_not_downgrade_sensitivity_or_gain_authority() {
    let (mut service, pet, source, _) = fixture();
    let id = service
        .for_pet(&pet)
        .create(
            draft("private", &source).with_sensitivity(Sensitivity::Private),
            100,
        )
        .unwrap()
        .id();
    assert_eq!(
        service
            .for_pet(&pet)
            .replace(id, 1, draft("public", &source), 200),
        Err(MemoryError::ProtectedContent)
    );
    assert_eq!(
        service.for_pet(&pet).replace(
            id,
            1,
            MemoryDraft::new(
                "anchor",
                Category::Identity,
                Epistemic::UserStatement,
                vec![source]
            ),
            200
        ),
        Err(MemoryError::ProtectedContent)
    );
}

#[test]
fn deleting_a_root_invalidates_transitive_records_contexts_and_entities_only() {
    let (mut service, pet, source, root) = fixture();
    let root_ref = reference(&mut service, &pet, root);
    let child = service
        .for_pet(&pet)
        .create(
            draft("tea child", &source).with_dependencies(vec![root_ref]),
            100,
        )
        .unwrap()
        .id();
    let child_ref = reference(&mut service, &pet, child);
    let grandchild = service
        .for_pet(&pet)
        .create(
            draft("tea grandchild", &source).with_dependencies(vec![child_ref]),
            100,
        )
        .unwrap()
        .id();
    service
        .put_working_context(
            &pet,
            &session(),
            "derived",
            1,
            context(&source, vec![child_ref]),
            100,
        )
        .unwrap();
    let independent = service
        .for_pet(&pet)
        .create(draft("tea independent", &source), 100)
        .unwrap()
        .id();
    assert_eq!(entity_count(&mut service, &pet, 100).0, 5);
    service.for_pet(&pet).delete(root, 1, 200).unwrap();
    for id in [child, grandchild] {
        let status = service.for_pet(&pet).status(id, 200).unwrap();
        assert_eq!(status.state(), RecordState::Active);
        assert!(!status.is_available());
    }
    assert_eq!(entity_count(&mut service, &pet, 200).0, 1);
    assert!(service.for_pet(&pet).get(independent, 200).is_ok());
    assert_eq!(
        service.for_pet(&pet).status(root, 200).unwrap().state(),
        RecordState::Deleted
    );
}

#[test]
fn correction_or_archive_restore_never_revives_previous_dependency_versions() {
    for archive in [false, true] {
        let (mut service, pet, source, root) = fixture();
        let dependency = reference(&mut service, &pet, root);
        let child = service
            .for_pet(&pet)
            .create(
                draft("tea child", &source).with_dependencies(vec![dependency]),
                100,
            )
            .unwrap()
            .id();
        if archive {
            service.for_pet(&pet).archive(root, 1, 200).unwrap();
            service.for_pet(&pet).restore_archive(root, 2, 300).unwrap();
        } else {
            service
                .for_pet(&pet)
                .update(root, 1, draft("tea fixed", &source), 200)
                .unwrap();
        }
        assert_eq!(
            service.for_pet(&pet).get(child, 300),
            Err(MemoryError::NotFound)
        );
        assert_eq!(
            service
                .for_pet(&pet)
                .update(child, 1, draft("detach", &source), 300),
            Err(MemoryError::InvalidEvidence)
        );
        assert_eq!(entity_count(&mut service, &pet, 300).0, 1);
    }
}

#[test]
fn dependencies_reject_cycles_duplicates_overflow_foreign_records_and_missing_provenance() {
    let (mut service, pet, source, root) = fixture();
    let dependency = reference(&mut service, &pet, root);
    let child = service
        .for_pet(&pet)
        .create(
            draft("child", &source).with_dependencies(vec![dependency]),
            100,
        )
        .unwrap()
        .id();
    let child_ref = reference(&mut service, &pet, child);
    assert_eq!(
        service.for_pet(&pet).update(
            root,
            1,
            draft("cycle", &source).with_dependencies(vec![child_ref]),
            200
        ),
        Err(MemoryError::InvalidEvidence)
    );
    assert_eq!(
        service.for_pet(&pet).replace(
            root,
            1,
            draft("cycle", &source).with_dependencies(vec![child_ref]),
            200
        ),
        Err(MemoryError::InvalidEvidence)
    );
    for dependencies in [vec![dependency, dependency], vec![dependency; 33]] {
        assert_eq!(
            service
                .for_pet(&pet)
                .create(draft("bad", &source).with_dependencies(dependencies), 100),
            Err(MemoryError::InvalidEvidence)
        );
    }
    assert!(RecordVersion::new(root, 0).is_err());
    let other = service
        .register_source(
            MemoryScope::Private(pet.clone()),
            "other",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    assert_eq!(
        service.for_pet(&pet).create(
            draft("missing origin", &other).with_dependencies(vec![dependency]),
            100
        ),
        Err(MemoryError::InvalidEvidence)
    );
    let bob = owner("bob");
    let bob_source = service
        .register_source(
            MemoryScope::Private(bob.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    assert_eq!(
        service.for_pet(&bob).create(
            draft("foreign", &bob_source).with_dependencies(vec![dependency]),
            100
        ),
        Err(MemoryError::InvalidEvidence)
    );
}

#[test]
fn dependencies_cannot_launder_private_sensitivity() {
    let (mut service, pet, source, _) = fixture();
    let id = service
        .for_pet(&pet)
        .create(
            draft("private", &source).with_sensitivity(Sensitivity::Private),
            100,
        )
        .unwrap()
        .id();
    let dependency = reference(&mut service, &pet, id);
    assert_eq!(
        service.for_pet(&pet).create(
            draft("normal", &source).with_dependencies(vec![dependency]),
            100
        ),
        Err(MemoryError::InvalidEvidence)
    );
    let child = service
        .for_pet(&pet)
        .create(
            draft("private child", &source)
                .with_sensitivity(Sensitivity::Private)
                .with_dependencies(vec![dependency]),
            100,
        )
        .unwrap();
    service
        .for_pet(&pet)
        .update(
            child.id(),
            child.revision(),
            draft("private child", &source)
                .with_sensitivity(Sensitivity::Restricted)
                .with_dependencies(vec![dependency]),
            200,
        )
        .unwrap();
    assert!(
        !service
            .for_pet(&pet)
            .status(child.id(), 200)
            .unwrap()
            .is_available()
    );
    assert_eq!(
        service.for_pet(&pet).get(id, 200).unwrap().sensitivity(),
        Sensitivity::Private
    );
}

#[test]
fn expiration_of_an_ancestor_invalidates_long_lived_derivations_and_contexts() {
    let (mut service, pet, source, _) = fixture();
    let root = service
        .for_pet(&pet)
        .create(draft("tea root", &source).with_expiration(200), 100)
        .unwrap()
        .id();
    let dependency = reference(&mut service, &pet, root);
    let child = service
        .for_pet(&pet)
        .create(
            draft("tea child", &source).with_dependencies(vec![dependency]),
            100,
        )
        .unwrap()
        .id();
    service
        .put_working_context(
            &pet,
            &session(),
            "derived",
            1,
            context(&source, vec![dependency]),
            100,
        )
        .unwrap();
    assert_eq!(entity_count(&mut service, &pet, 199).0, 4);
    assert_eq!(entity_count(&mut service, &pet, 200).0, 1);
    assert_eq!(
        service.for_pet(&pet).get(child, 200),
        Err(MemoryError::NotFound)
    );
}

#[test]
fn source_revocation_covers_reads_writes_contexts_and_cached_views() {
    let (mut service, pet, source, id) = fixture();
    service
        .put_working_context(
            &pet,
            &session(),
            "working",
            1,
            context(&source, vec![]),
            100,
        )
        .unwrap();
    assert_eq!(entity_count(&mut service, &pet, 100).0, 2);
    service.revoke_source(&source).unwrap();
    assert_eq!(entity_count(&mut service, &pet, 200).0, 0);
    assert_eq!(
        service.for_pet(&pet).get(id, 200),
        Err(MemoryError::NotFound)
    );
    assert_eq!(
        service.for_pet(&pet).create(draft("late", &source), 200),
        Err(MemoryError::InvalidEvidence)
    );
    assert!(
        service
            .put_working_context(
                &pet,
                &session(),
                "working",
                2,
                context(&source, vec![]),
                200
            )
            .is_err()
    );
    assert!(
        service
            .set_read_policy(&pet, ReadPolicy::offline(2, vec![source]))
            .is_err()
    );
}

#[test]
fn source_restoration_requires_new_revision_and_does_not_restore_old_content_or_grants() {
    let (mut service, pet, source, id) = fixture();
    service.revoke_source(&source).unwrap();
    assert!(
        service
            .register_source(
                MemoryScope::Private(pet.clone()),
                "message",
                2,
                Epistemic::UserStatement
            )
            .is_err()
    );
    assert!(service.restore_source(&source, 1).is_err());
    let restored = service.restore_source(&source, 2).unwrap();
    assert_eq!(
        service.for_pet(&pet).get(id, 200),
        Err(MemoryError::NotFound)
    );
    service
        .for_pet(&pet)
        .update(id, 1, draft("tea revalidated", &restored), 200)
        .unwrap();
    assert_eq!(entity_count(&mut service, &pet, 200).0, 0);
    service
        .set_read_policy(&pet, ReadPolicy::offline(2, vec![restored]))
        .unwrap();
    assert_eq!(entity_count(&mut service, &pet, 200).0, 1);
}

#[test]
fn source_revocation_is_exactly_scoped_and_stale_requests_have_no_effect() {
    let (mut service, pet, source, _) = fixture();
    let bob = owner("bob");
    let other_source = service
        .register_source(
            MemoryScope::Private(bob.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let id = service
        .for_pet(&bob)
        .create(draft("bob", &other_source), 100)
        .unwrap()
        .id();
    service.revoke_source(&source).unwrap();
    let generation = service.generation();
    assert!(service.revoke_source(&source).is_err());
    assert_eq!(service.generation(), generation);
    assert!(service.for_pet(&bob).get(id, 200).is_ok());
    assert!(service.for_pet(&pet).status(id, 200).is_err());
}

#[test]
fn protected_identity_revocation_and_restoration_require_fresh_authority_and_policy() {
    let (mut service, pet, source, _) = fixture();
    let authority = service
        .register_source(
            MemoryScope::Private(pet.clone()),
            "authority",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    service
        .set_read_policy(
            &pet,
            ReadPolicy::offline(2, vec![source.clone(), authority.clone()]),
        )
        .unwrap();
    service
        .set_identity_view(&pet, 1, "protected anchor", vec![authority.clone()])
        .unwrap();
    service.revoke_source(&authority).unwrap();
    assert_eq!(
        service
            .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
            .identity(),
        Err(MemoryError::NotFound)
    );
    let restored = service.restore_source(&authority, 2).unwrap();
    assert!(
        service
            .set_identity_view(&pet, 2, "anchor", vec![authority])
            .is_err()
    );
    service
        .set_identity_view(&pet, 2, "new anchor", vec![restored.clone()])
        .unwrap();
    assert!(
        service
            .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
            .identity()
            .is_err()
    );
    service
        .set_read_policy(&pet, ReadPolicy::offline(3, vec![source, restored]))
        .unwrap();
    assert_eq!(
        service
            .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
            .identity()
            .unwrap()
            .version(),
        2
    );
}

#[test]
fn access_revocation_clears_volatile_views_preserves_watermarks_and_denies_all_purposes() {
    let (mut service, pet, source, _) = fixture();
    let authority = service
        .register_source(
            MemoryScope::Private(pet.clone()),
            "authority",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    service
        .set_identity_view(&pet, 5, "anchor", vec![authority.clone()])
        .unwrap();
    service
        .put_working_context(
            &pet,
            &session(),
            "working",
            7,
            context(&source, vec![]),
            100,
        )
        .unwrap();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    service.revoke_access(&pet, 2).unwrap();
    assert_eq!(
        service.validate_task(&task, 100),
        Err(MemoryError::TaskCancelled)
    );
    for purpose in [
        ReadPurpose::OfflineReasoning,
        ReadPurpose::Unknown,
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        assert!(
            service
                .for_session(&pet, &session(), purpose)
                .begin_task(&RetrievalQuery::new("tea"), 100)
                .is_err()
        );
    }
    assert!(
        service
            .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
            .is_err()
    );
    service
        .set_read_policy(
            &pet,
            ReadPolicy::offline(3, vec![source.clone(), authority.clone()]),
        )
        .unwrap();
    assert_eq!(entity_count(&mut service, &pet, 100).0, 1);
    assert!(
        service
            .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
            .identity()
            .is_err()
    );
    assert!(
        service
            .put_working_context(
                &pet,
                &session(),
                "working",
                7,
                context(&source, vec![]),
                100
            )
            .is_err()
    );
    assert!(
        service
            .set_identity_view(&pet, 5, "old anchor", vec![authority])
            .is_err()
    );
}

#[test]
fn explicit_context_and_identity_removal_keep_version_tombstones() {
    let (mut service, pet, source, _) = fixture();
    service
        .put_working_context(
            &pet,
            &session(),
            "working",
            1,
            context(&source, vec![]),
            100,
        )
        .unwrap();
    assert_eq!(
        service
            .remove_working_context(&pet, &session(), "working", 1, 200)
            .unwrap(),
        2
    );
    for revision in [1, 2] {
        assert!(
            service
                .put_working_context(
                    &pet,
                    &session(),
                    "working",
                    revision,
                    context(&source, vec![]),
                    200
                )
                .is_err()
        );
    }
    assert!(
        service
            .put_working_context(
                &pet,
                &session(),
                "working",
                3,
                context(&source, vec![]),
                199
            )
            .is_err()
    );
    service
        .put_working_context(
            &pet,
            &session(),
            "working",
            3,
            context(&source, vec![]),
            200,
        )
        .unwrap();
    let authority = service
        .register_source(
            MemoryScope::Private(pet.clone()),
            "authority",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    service
        .set_identity_view(&pet, 1, "anchor", vec![authority.clone()])
        .unwrap();
    assert_eq!(service.remove_identity_view(&pet, 1).unwrap(), 2);
    assert!(
        service
            .set_identity_view(&pet, 2, "stale", vec![authority.clone()])
            .is_err()
    );
    service
        .set_identity_view(&pet, 3, "fresh", vec![authority])
        .unwrap();
}

#[test]
fn cancellation_generation_is_cas_checked_and_cancels_late_creation() {
    let (mut service, pet, source, _) = fixture();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let generation = service.generation();
    assert_eq!(
        service.cancel_tasks(generation - 1),
        Err(MemoryError::TaskCancelled)
    );
    assert_eq!(service.generation(), generation);
    assert_eq!(service.cancel_tasks(generation).unwrap(), generation + 1);
    assert_eq!(
        service.create_from_task(&task, draft("late resurrected copy", &source), 200),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn any_committed_mutation_cancels_read_tasks_but_failed_validation_does_not() {
    let (mut service, pet, source, root) = fixture();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    assert!(service.for_pet(&pet).delete(root, 99, 200).is_err());
    assert!(service.validate_task(&task, 200).is_ok());
    service.for_pet(&pet).delete(root, 1, 200).unwrap();
    assert_eq!(
        service.create_from_task(&task, draft("new id from stale bytes", &source), 200),
        Err(MemoryError::TaskCancelled)
    );
    assert!(
        service
            .for_pet(&pet)
            .search(&moe_memory_core::SearchQuery::new(""), 200)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn expiry_and_backward_time_cancel_tasks_without_a_storage_mutation() {
    let (mut service, pet, source, root) = fixture();
    service
        .for_pet(&pet)
        .update(
            root,
            1,
            draft("tea temporary", &source).with_expiration(200),
            100,
        )
        .unwrap();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let generation = service.generation();
    assert_eq!(
        service.validate_task(&task, 99),
        Err(MemoryError::TaskCancelled)
    );
    assert!(service.validate_task(&task, 199).is_ok());
    assert_eq!(
        service.validate_task(&task, 200),
        Err(MemoryError::TaskCancelled)
    );
    assert_eq!(service.generation(), generation);
}

#[test]
fn prepared_commands_cannot_bypass_runtime_policy_changes() {
    let (mut service, pet, source, _) = fixture();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let command = task
        .propose_create(draft("tea derived", &source), 200)
        .unwrap();
    service.revoke_access(&pet, 2).unwrap();
    assert_eq!(
        command.execute(&mut service),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn task_submission_tracks_the_actual_read_set_and_rejects_missing_origins() {
    let (mut service, pet, source, root) = fixture();
    let other = service
        .register_source(
            MemoryScope::Private(pet.clone()),
            "other",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    assert_eq!(
        service.create_from_task(&task, draft("washed", &other), 200),
        Err(MemoryError::InvalidEvidence)
    );
    let derived = service
        .create_from_task(&task, draft("tea derived", &source), 200)
        .unwrap();
    assert_eq!(
        service
            .for_pet(&pet)
            .get(derived.id(), 200)
            .unwrap()
            .dependencies()[0]
            .id(),
        root
    );
    service.for_pet(&pet).delete(root, 1, 300).unwrap();
    assert_eq!(
        service.for_pet(&pet).get(derived.id(), 300),
        Err(MemoryError::NotFound)
    );
}

#[test]
fn task_identity_evidence_is_also_required_and_debug_never_discloses_query_bytes() {
    let (mut service, pet, source, _) = fixture();
    let authority = service
        .register_source(
            MemoryScope::Private(pet.clone()),
            "authority",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap();
    service
        .set_identity_view(&pet, 1, "private anchor", vec![authority.clone()])
        .unwrap();
    service
        .set_read_policy(
            &pet,
            ReadPolicy::offline(2, vec![source.clone(), authority.clone()]),
        )
        .unwrap();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("secret-query"), 100)
        .unwrap();
    assert!(task.identity_view().is_some());
    assert_eq!(
        task.propose_create(draft("candidate", &source), 200),
        Err(MemoryError::InvalidEvidence)
    );
    let candidate = MemoryDraft::new(
        "candidate-secret",
        Category::Episode,
        Epistemic::Inference,
        vec![source, authority],
    );
    let command = task.propose_create(candidate, 200).unwrap();
    for debug in [format!("{task:?}"), format!("{command:?}")] {
        assert!(!debug.contains("secret-query"));
        assert!(!debug.contains("candidate-secret"));
        assert!(!debug.contains("private anchor"));
        assert!(!debug.contains("115, 101, 99, 114, 101, 116"));
    }
}

#[test]
fn task_fences_from_another_service_epoch_are_rejected_even_when_versions_match() {
    let (mut service, pet, source, _) = fixture();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let mut other = fixture().0;
    assert_eq!(other.generation(), task.generation());
    assert_eq!(
        other.create_from_task(&task, draft("wrong epoch", &source), 200),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn session_read_tasks_cannot_be_promoted_to_persistent_records() {
    let (mut service, pet, source, _) = fixture();
    service
        .put_working_context(
            &pet,
            &session(),
            "working",
            1,
            context(&source, vec![]),
            100,
        )
        .unwrap();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    assert!(
        service
            .create_from_task(&task, draft("session promotion", &source), 200)
            .is_err()
    );
    service
        .remove_working_context(&pet, &session(), "working", 1, 200)
        .unwrap();
    assert_eq!(
        service.validate_task(&task, 200),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn v2_commands_roundtrip_and_reject_truncated_or_cross_version_frames() {
    let (mut service, pet, source, id) = fixture();
    let dependency = reference(&mut service, &pet, id);
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let commands = vec![
        MemoryCommand::archive(pet.clone(), id, 1, 200),
        MemoryCommand::restore_archive(pet.clone(), id, 2, 300),
        MemoryCommand::replace(pet.clone(), id, 1, draft("replacement", &source), 200),
        MemoryCommand::revoke_source(source.clone()),
        MemoryCommand::restore_source(source.clone(), 2),
        MemoryCommand::cancel_tasks(service.generation()),
        MemoryCommand::create(
            pet.clone(),
            draft("derived", &source).with_dependencies(vec![dependency]),
            200,
        ),
        MemoryCommand::update(
            pet,
            id,
            1,
            draft("derived", &source).with_dependencies(vec![dependency]),
            200,
        ),
        task.propose_create(draft("derived", &source), 200).unwrap(),
    ];
    for command in commands {
        let bytes = command.to_bytes().unwrap();
        assert_eq!(bytes[0], 2);
        assert_eq!(MemoryCommand::from_bytes(&bytes).unwrap(), command);
        for length in 0..bytes.len() {
            assert!(MemoryCommand::from_bytes(&bytes[..length]).is_err());
        }
        let mut invalid = bytes.clone();
        invalid[0] = 1;
        assert!(MemoryCommand::from_bytes(&invalid).is_err());
        invalid[0] = 3;
        assert!(MemoryCommand::from_bytes(&invalid).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(MemoryCommand::from_bytes(&trailing).is_err());
    }
}

#[test]
fn task_request_identity_includes_query_budgets_sessions_and_read_set() {
    let (mut service, pet, source, _) = fixture();
    let mut payloads = Vec::new();
    for query in [
        RetrievalQuery::new("tea"),
        RetrievalQuery::new("te"),
        RetrievalQuery::new("tea").with_limit(1),
        RetrievalQuery::new("tea").with_char_budget(100),
    ] {
        let task = service
            .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
            .begin_task(&query, 100)
            .unwrap();
        payloads.push(
            task.propose_create(draft("same candidate", &source), 200)
                .unwrap()
                .to_bytes()
                .unwrap(),
        );
    }
    for left in 0..payloads.len() {
        for right in left + 1..payloads.len() {
            assert_ne!(payloads[left], payloads[right]);
        }
    }
}

#[test]
fn trusted_replay_restores_task_writes_without_reinstalling_authorization() {
    let pet = owner("alice");
    let mut service = MemoryService::new();
    let source_command = MemoryCommand::register_source(
        MemoryScope::Private(pet.clone()),
        "message",
        1,
        Epistemic::UserStatement,
    );
    let CommandResult::Source(source) = source_command.execute(&mut service).unwrap() else {
        panic!()
    };
    let create = MemoryCommand::create(pet.clone(), draft("tea", &source), 100);
    create.execute(&mut service).unwrap();
    service
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    let task = service
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let command = task
        .propose_create(draft("tea derived", &source), 200)
        .unwrap();
    let mut recovered = MemoryService::new();
    source_command.replay(&mut recovered).unwrap();
    create.replay(&mut recovered).unwrap();
    assert_eq!(
        command.execute(&mut recovered),
        Err(MemoryError::TaskCancelled)
    );
    command.replay(&mut recovered).unwrap();
    assert_eq!(
        recovered
            .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&RetrievalQuery::new("tea"), 200),
        Err(MemoryError::AccessDenied)
    );
    assert_eq!(
        recovered.validate_task(&task, 200),
        Err(MemoryError::TaskCancelled)
    );
}

#[test]
fn prepared_and_decoded_commands_cannot_reuse_live_authority_in_another_epoch() {
    let (mut issuer, pet, source, _) = fixture();
    let task = issuer
        .for_session(&pet, &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let command = task
        .propose_create(draft("tea derived", &source), 200)
        .unwrap();
    for grants in [vec![], vec![source.clone()]] {
        let mut other = MemoryService::new();
        let registered = other
            .register_source(
                MemoryScope::Private(pet.clone()),
                "message",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();
        other
            .for_pet(&pet)
            .create(draft("tea original", &registered), 100)
            .unwrap();
        other
            .set_read_policy(&pet, ReadPolicy::offline(1, grants))
            .unwrap();
        assert_eq!(issuer.generation(), other.generation());
        assert_eq!(command.execute(&mut other), Err(MemoryError::TaskCancelled));
    }
    let decoded = MemoryCommand::from_bytes(&command.to_bytes().unwrap()).unwrap();
    assert_eq!(decoded, command);
    assert_eq!(
        decoded.execute(&mut issuer),
        Err(MemoryError::TaskCancelled)
    );
    assert!(command.execute(&mut issuer).is_ok());
}
