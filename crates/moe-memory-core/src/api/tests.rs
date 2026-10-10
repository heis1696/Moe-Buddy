use super::*;
use crate::{Category, Namespace, SearchScope, Sensitivity};

fn identity(workspace: &str, user: &str, pet: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new(workspace, user).unwrap(), pet).unwrap()
}

fn fixture(service: &mut MemoryService, scope: MemoryScope, protected: bool) -> MemoryId {
    let source = service
        .register_source(scope.clone(), "fixture", 1, Epistemic::Reference)
        .unwrap();
    let id = service.store.allocate_id().unwrap();
    service.store.records.insert(
        id,
        MemoryRecord {
            id,
            revision: 1,
            scope,
            draft: MemoryDraft::new(
                "shared reference",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source],
            ),
            created_at_ms: 100,
            updated_at_ms: 100,
            protected,
            state: crate::RecordState::Active,
        },
    );
    id
}

#[test]
fn derived_dependencies_cannot_extract_hidden_shared_or_protected_content() {
    for variant in 0..6 {
        let alice = identity("home", "user", "alice");
        let mut service = MemoryService::new();
        let id = fixture(
            &mut service,
            MemoryScope::Shared(alice.namespace().clone()),
            false,
        );
        let record = service.store.records.get_mut(&id).unwrap();
        match variant {
            0 => record.draft.sensitivity = Sensitivity::Private,
            1 => record.protected = true,
            2 => record.draft.lifecycle = crate::Lifecycle::ShortTerm,
            3 => record.draft.category = crate::Category::WorkingContext,
            4 => record.draft.category = crate::Category::Identity,
            _ => record.draft.category = crate::Category::Protocol,
        }
        let source = record.sources()[0].clone();
        let dependency = record.version_ref();
        let draft = MemoryDraft::new(
            "derived",
            crate::Category::Knowledge,
            Epistemic::Reference,
            vec![source],
        )
        .with_sensitivity(Sensitivity::Private)
        .with_dependencies(vec![dependency]);
        assert_eq!(
            service.for_pet(&alice).create(draft, 100),
            Err(MemoryError::InvalidEvidence)
        );
    }
}

#[test]
fn management_generation_exhaustion_never_partially_mutates_state() {
    let alice = identity("home", "user", "alice");
    let mut service = MemoryService::new();
    let id = fixture(&mut service, MemoryScope::Private(alice.clone()), false);
    let source = service.store.records[&id].sources()[0].clone();
    let draft = service.store.records[&id].draft.clone();
    service.store.generation = u64::MAX;
    assert_eq!(
        service.for_pet(&alice).create(draft.clone(), 200),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(
        service.for_pet(&alice).update(id, 1, draft.clone(), 200),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(
        service.for_pet(&alice).replace(id, 1, draft, 200),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(
        service.for_pet(&alice).archive(id, 1, 200),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(
        service.for_pet(&alice).delete(id, 1, 200),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(
        service.revoke_source(&source),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(
        service.cancel_tasks(u64::MAX),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(service.store.records.len(), 1);
    assert!(service.store.tombstones.is_empty());
    assert!(!service.store.sources[&source.key].revoked);
    assert_eq!(service.for_pet(&alice).get(id, 200).unwrap().revision(), 1);
}

#[test]
fn tombstone_capacity_refuses_deletion_without_evicting_history_or_live_data() {
    let alice = identity("home", "user", "alice");
    let mut service = MemoryService::new();
    let id = fixture(&mut service, MemoryScope::Private(alice.clone()), false);
    for counter in 0..crate::validation::MAX_TOMBSTONES {
        let tombstone_id = MemoryId(counter as u64 + 100);
        service.store.tombstones.insert(
            tombstone_id,
            (alice.clone(), crate::MemoryStatus::deleted(tombstone_id, 2)),
        );
    }
    let generation = service.generation();
    assert_eq!(
        service.for_pet(&alice).delete(id, 1, 200),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(service.generation(), generation);
    assert_eq!(
        service.store.tombstones.len(),
        crate::validation::MAX_TOMBSTONES
    );
    assert!(service.for_pet(&alice).get(id, 200).is_ok());
}

#[test]
fn shared_fixture_is_readable_by_both_pets_but_not_other_users_or_workspaces() {
    let alice = identity("home", "user", "alice");
    let bob = identity("home", "user", "bob");
    let outsider = identity("other-home", "user", "alice");
    let other_user = identity("home", "other-user", "alice");
    let mut service = MemoryService::new();
    let id = fixture(
        &mut service,
        MemoryScope::Shared(alice.namespace().clone()),
        false,
    );
    for allowed in [&alice, &bob] {
        let client = service.for_pet(allowed);
        assert!(client.get(id, 100).is_ok());
        assert_eq!(
            client
                .search(&SearchQuery::new("").with_scope(SearchScope::Shared), 100)
                .unwrap()
                .len(),
            1
        );
        assert!(
            client
                .search(&SearchQuery::new("").with_scope(SearchScope::Private), 100)
                .unwrap()
                .is_empty()
        );
    }
    for denied in [&outsider, &other_user] {
        let client = service.for_pet(denied);
        assert_eq!(client.get(id, 100), Err(MemoryError::NotFound));
        assert!(
            client
                .search(&SearchQuery::new(""), 100)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn shared_read_access_never_grants_update_or_delete_permission() {
    let alice = identity("home", "user", "alice");
    let mut service = MemoryService::new();
    let id = fixture(
        &mut service,
        MemoryScope::Shared(alice.namespace().clone()),
        false,
    );
    let evidence = service
        .register_source(
            MemoryScope::Private(alice.clone()),
            "event",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let candidate = MemoryDraft::new(
        "overwrite",
        Category::Knowledge,
        Epistemic::UserStatement,
        vec![evidence],
    );
    let mut client = service.for_pet(&alice);
    assert_eq!(
        client.update(id, 1, candidate, 200),
        Err(MemoryError::NotFound)
    );
    assert_eq!(client.delete(id, 1, 200), Err(MemoryError::NotFound));
    assert_eq!(client.get(id, 200).unwrap().text(), "shared reference");
}

#[test]
fn private_provenance_cannot_be_laundered_through_a_shared_fixture() {
    let alice = identity("home", "user", "alice");
    let bob = identity("home", "user", "bob");
    let mut service = MemoryService::new();
    let id = fixture(
        &mut service,
        MemoryScope::Shared(alice.namespace().clone()),
        false,
    );
    let private_source = service
        .register_source(
            MemoryScope::Private(alice.clone()),
            "private-event",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service.store.records.get_mut(&id).unwrap().draft.sources = vec![private_source];
    for identity in [&alice, &bob] {
        let client = service.for_pet(identity);
        assert_eq!(client.get(id, 100), Err(MemoryError::NotFound));
        assert!(
            client
                .search(&SearchQuery::new(""), 100)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn non_normal_shared_fixtures_fail_closed() {
    let alice = identity("home", "user", "alice");
    let mut service = MemoryService::new();
    let id = fixture(
        &mut service,
        MemoryScope::Shared(alice.namespace().clone()),
        false,
    );
    service
        .store
        .records
        .get_mut(&id)
        .unwrap()
        .draft
        .sensitivity = Sensitivity::Private;
    assert_eq!(
        service.for_pet(&alice).get(id, 100),
        Err(MemoryError::NotFound)
    );
}

#[test]
fn protected_records_cannot_be_downgraded_by_choosing_an_ordinary_category() {
    let alice = identity("home", "user", "alice");
    let mut service = MemoryService::new();
    let id = fixture(&mut service, MemoryScope::Private(alice.clone()), true);
    let evidence = service
        .register_source(
            MemoryScope::Private(alice.clone()),
            "event",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let candidate = MemoryDraft::new(
        "downgrade",
        Category::Knowledge,
        Epistemic::UserStatement,
        vec![evidence],
    );
    let mut client = service.for_pet(&alice);
    assert!(client.get(id, 100).unwrap().is_protected());
    assert_eq!(
        client.update(id, 1, candidate, 200),
        Err(MemoryError::ProtectedContent)
    );
    assert_eq!(
        client.delete(id, 1, 200),
        Err(MemoryError::ProtectedContent)
    );
}

#[test]
fn revision_overflow_does_not_modify_or_delete_the_record() {
    let alice = identity("home", "user", "alice");
    let mut service = MemoryService::new();
    let id = fixture(&mut service, MemoryScope::Private(alice.clone()), false);
    service.store.records.get_mut(&id).unwrap().revision = u64::MAX;
    let evidence = service
        .register_source(
            MemoryScope::Private(alice.clone()),
            "event",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let candidate = MemoryDraft::new(
        "overflow",
        Category::Episode,
        Epistemic::UserStatement,
        vec![evidence],
    );
    let mut client = service.for_pet(&alice);
    assert_eq!(
        client.update(id, u64::MAX, candidate, 200),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(
        client.delete(id, u64::MAX, 200),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(client.get(id, 200).unwrap().revision(), u64::MAX);
}

#[test]
fn memory_capacity_failure_does_not_evict_existing_records() {
    let alice = identity("home", "user", "alice");
    let mut service = MemoryService::new();
    let first_id = fixture(&mut service, MemoryScope::Private(alice.clone()), false);
    let template = service.store.records.get(&first_id).unwrap().clone();
    for _ in 1..MAX_RECORDS {
        let id = service.store.allocate_id().unwrap();
        let mut record = template.clone();
        record.id = id;
        service.store.records.insert(id, record);
    }
    let evidence = service
        .register_source(
            MemoryScope::Private(alice.clone()),
            "event",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let candidate = MemoryDraft::new(
        "too much",
        Category::Episode,
        Epistemic::UserStatement,
        vec![evidence],
    );
    assert_eq!(
        service.for_pet(&alice).create(candidate, 100),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(service.store.records.len(), MAX_RECORDS);
    assert!(service.for_pet(&alice).get(first_id, 100).is_ok());
}
