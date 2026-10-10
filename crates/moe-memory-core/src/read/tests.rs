use super::*;
use crate::{MemoryRecord, Namespace};

#[test]
fn shared_views_require_explicit_namespace_evidence_grants_for_each_pet() {
    let namespace = Namespace::new("home", "user").unwrap();
    let alice = PetIdentity::new(namespace.clone(), "alice").unwrap();
    let bob = PetIdentity::new(namespace.clone(), "bob").unwrap();
    let mut service = MemoryService::new();
    let source = service
        .register_source(
            MemoryScope::Shared(namespace.clone()),
            "reference",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    let id = service.store.allocate_id().unwrap();
    service.store.records.insert(
        id,
        MemoryRecord {
            id,
            revision: 1,
            scope: MemoryScope::Shared(namespace),
            draft: MemoryDraft::new(
                "tea reference",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source.clone()],
            ),
            created_at_ms: 100,
            updated_at_ms: 100,
            protected: false,
            state: crate::RecordState::Active,
        },
    );
    service
        .set_read_policy(&alice, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    service
        .set_read_policy(&bob, ReadPolicy::offline(1, vec![]))
        .unwrap();
    let session = SessionId::new("current").unwrap();
    let query = RetrievalQuery::new("tea").with_scope(SearchScope::Shared);
    assert_eq!(
        service
            .for_session(&alice, &session, ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap()
            .items()
            .len(),
        1
    );
    assert!(
        service
            .for_session(&bob, &session, ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap()
            .items()
            .is_empty()
    );
    service
        .set_read_policy(&bob, ReadPolicy::offline(2, vec![source.clone()]))
        .unwrap();
    assert_eq!(
        service
            .for_session(&bob, &session, ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap()
            .items()
            .len(),
        1
    );
    for namespace in [
        Namespace::new("other", "user").unwrap(),
        Namespace::new("home", "other").unwrap(),
    ] {
        let outsider = PetIdentity::new(namespace, "alice").unwrap();
        assert_eq!(
            service.set_read_policy(&outsider, ReadPolicy::offline(1, vec![source.clone()])),
            Err(MemoryError::InvalidEvidence)
        );
    }
}

#[test]
fn protected_records_never_become_entity_evidence_even_with_granted_sources() {
    let owner = PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap();
    let mut service = MemoryService::new();
    let source = service
        .register_source(
            MemoryScope::Private(owner.clone()),
            "reference",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service
        .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    let id = service.store.allocate_id().unwrap();
    service.store.records.insert(
        id,
        MemoryRecord {
            id,
            revision: 1,
            scope: MemoryScope::Private(owner.clone()),
            draft: MemoryDraft::new(
                "tea protected",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source],
            ),
            created_at_ms: 100,
            updated_at_ms: 100,
            protected: true,
            state: crate::RecordState::Active,
        },
    );
    let result = service
        .for_session(
            &owner,
            &SessionId::new("current").unwrap(),
            ReadPurpose::OfflineReasoning,
        )
        .entity(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    assert!(result.items().is_empty());
}
