use super::*;
use crate::{CommandResult, ReadPolicy, ReadPurpose, RetrievalQuery, SessionId};

fn fixture() -> (MemoryService, PetIdentity, SourceRef) {
    let identity = PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap();
    let mut service = MemoryService::new();
    let source = service
        .register_source(
            MemoryScope::Shared(identity.namespace().clone()),
            "source",
            1,
            Epistemic::Reference,
        )
        .unwrap();
    service
        .set_read_policy(&identity, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    service
        .set_publication_policy(
            &identity,
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    service
        .set_formal_publication_policy(
            identity.namespace(),
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    (service, identity, source)
}
fn formal(source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        "formal",
        Category::Protocol,
        Epistemic::Reference,
        vec![source.clone()],
    )
}
fn shared_command(
    service: &mut MemoryService,
    identity: &PetIdentity,
    source: &SourceRef,
) -> MemoryCommand {
    let task = service
        .for_session(
            identity,
            &SessionId::new("current").unwrap(),
            ReadPurpose::OfflineReasoning,
        )
        .begin_task(&RetrievalQuery::new("no-match"), 100)
        .unwrap();
    let candidate = task
        .propose_shared(
            MemoryDraft::new(
                "shared",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source.clone()],
            ),
            100,
        )
        .unwrap();
    service.prepare_shared_publication(&candidate, 100).unwrap()
}

#[test]
fn exhausted_runtime_configuration_does_not_replace_either_policy_or_watermark() {
    let (mut service, identity, _) = fixture();
    service.reads.configuration = u64::MAX;
    assert_eq!(
        service.set_publication_policy(&identity, PublicationPolicy::deny_all(2)),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(
        service.set_formal_publication_policy(identity.namespace(), PublicationPolicy::deny_all(2)),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(service.publications.publishers[&identity].version, 1);
    assert_eq!(
        service.publications.formal_authorities[identity.namespace()].version,
        1
    );
    assert_eq!(service.reads.configuration, u64::MAX);
}

#[test]
fn exhausted_storage_generation_rejects_publication_without_partial_record_insertion() {
    let (mut service, identity, source) = fixture();
    service.store.generation = u64::MAX;
    let shared = shared_command(&mut service, &identity, &source);
    let formal = service
        .prepare_formal_publication(identity.namespace(), formal(&source), 100)
        .unwrap();
    for command in [shared, formal] {
        assert_eq!(
            command.execute(&mut service),
            Err(MemoryError::CapacityExceeded)
        );
        assert!(service.store.records.is_empty());
        assert_eq!(service.store.generation, u64::MAX);
    }
}

#[test]
fn exhausted_formal_revision_leaves_text_and_generation_unchanged() {
    let (mut service, identity, source) = fixture();
    let command = service
        .prepare_formal_publication(identity.namespace(), formal(&source), 100)
        .unwrap();
    let CommandResult::Write(receipt) = command.execute(&mut service).unwrap() else {
        panic!("write expected")
    };
    service
        .store
        .records
        .get_mut(&receipt.id())
        .unwrap()
        .revision = u64::MAX;
    let command = service
        .prepare_formal_revision(
            identity.namespace(),
            receipt.id(),
            u64::MAX,
            formal(&source),
            200,
        )
        .unwrap();
    let before = service.store.records[&receipt.id()].clone();
    let generation = service.generation();
    assert_eq!(
        command.execute(&mut service),
        Err(MemoryError::CapacityExceeded)
    );
    assert_eq!(service.store.records[&receipt.id()], before);
    assert_eq!(service.generation(), generation);
}

#[test]
fn shared_and_formal_capacity_failures_do_not_allocate_or_advance_generation() {
    let (mut service, identity, source) = fixture();
    let command = shared_command(&mut service, &identity, &source);
    let CommandResult::Write(receipt) = command.execute(&mut service).unwrap() else {
        panic!("write expected")
    };
    let record = service.store.records[&receipt.id()].clone();
    for number in 2..=validation::MAX_RECORDS as u64 {
        let mut next = record.clone();
        next.id = MemoryId(number);
        service.store.records.insert(next.id, next);
    }
    let shared = shared_command(&mut service, &identity, &source);
    let formal = service
        .prepare_formal_publication(identity.namespace(), formal(&source), 100)
        .unwrap();
    let generation = service.generation();
    for command in [shared, formal] {
        assert_eq!(
            command.execute(&mut service),
            Err(MemoryError::CapacityExceeded)
        );
        assert_eq!(service.store.records.len(), validation::MAX_RECORDS);
        assert_eq!(service.generation(), generation);
    }
}
