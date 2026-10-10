use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_memory_core::{
    CacheStatus, Category, Epistemic, Lifecycle, MemoryDraft, MemoryError, MemoryScope, Namespace,
    PetIdentity, ReadPolicy, ReadPurpose, RetrievalItem, RetrievalLayer, RetrievalQuery,
    Sensitivity, SessionId, SourceRef,
};
use moe_memory_local::{LocalMemory, OperationId, PersistenceError};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "moe-memory-m2-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("{error}"),
            }
        }
    }

    fn path(&self) -> PathBuf {
        self.0.join("memory.journal")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.path());
        let _ = fs::remove_dir(&self.0);
    }
}

fn operation(value: &str) -> OperationId {
    OperationId::new(value).unwrap()
}
fn session() -> SessionId {
    SessionId::new("current").unwrap()
}
fn owner() -> PetIdentity {
    PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap()
}

fn draft(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Episode,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
}

fn context(source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        "tea current work",
        Category::WorkingContext,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
    .with_lifecycle(Lifecycle::ShortTerm)
}

fn configure(memory: &mut LocalMemory, source: &SourceRef) {
    memory
        .set_read_policy(&owner(), ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
}

fn fixture(scratch: &Scratch) -> (LocalMemory, SourceRef) {
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = memory
        .register_source(
            operation("source"),
            MemoryScope::Private(owner()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    configure(&mut memory, &source);
    (memory, source)
}

#[test]
fn working_context_sensitivity_transitions_preserve_runtime_and_journal() {
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
        let scratch = Scratch::new();
        let (mut memory, source) = fixture(&scratch);
        memory
            .put_working_context(
                &owner(),
                &session(),
                "context",
                1,
                context(&source).with_sensitivity(current),
                100,
            )
            .unwrap();
        let query = RetrievalQuery::new("tea");
        let before = memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query, 100)
            .unwrap();
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 100)
            .unwrap();
        let task = memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .begin_task(&query, 100)
            .unwrap();
        let generation = memory.generation().unwrap();
        let length = fs::metadata(scratch.path()).unwrap().len();
        let result = memory.put_working_context(
            &owner(),
            &session(),
            "context",
            2,
            context(&source).with_sensitivity(next),
            200,
        );
        assert_eq!(memory.generation().unwrap(), generation);
        assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
        if allowed {
            assert!(result.is_ok(), "{current:?} -> {next:?}: {result:?}");
            assert!(matches!(
                memory.validate_task(&task, 200),
                Err(PersistenceError::Memory(MemoryError::TaskCancelled))
            ));
            let after = memory
                .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
                .retrieve(&query, 200)
                .unwrap();
            if next == Restricted {
                assert!(after.items().is_empty());
            } else {
                assert_eq!(after.items().len(), 1);
                assert_eq!(after.items()[0].sensitivity(), next);
                let RetrievalItem::WorkingContext(context) = &after.items()[0] else {
                    panic!("expected working context");
                };
                assert_eq!(context.revision(), 2);
            }
        } else {
            assert!(
                matches!(
                    result,
                    Err(PersistenceError::Memory(MemoryError::ProtectedContent))
                ),
                "{current:?} -> {next:?}"
            );
            assert_eq!(
                memory
                    .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
                    .retrieve(&query, 200)
                    .unwrap(),
                before
            );
            memory.validate_task(&task, 200).unwrap();
            assert_eq!(
                memory
                    .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
                    .entity(&query, 200)
                    .unwrap()
                    .cache_status(),
                CacheStatus::Hit
            );
            memory
                .put_working_context(
                    &owner(),
                    &session(),
                    "context",
                    2,
                    context(&source).with_sensitivity(current),
                    150,
                )
                .unwrap();
            assert!(matches!(
                memory.validate_task(&task, 200),
                Err(PersistenceError::Memory(MemoryError::TaskCancelled))
            ));
            assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
        }
    }
}

#[test]
fn restart_restores_records_but_not_grants_sessions_identity_or_cache() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(&scratch);
    let authority = memory
        .register_source(
            operation("authority"),
            MemoryScope::Private(owner()),
            "identity",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap()
        .value()
        .clone();
    memory
        .set_read_policy(
            &owner(),
            ReadPolicy::offline(2, vec![source.clone(), authority.clone()]),
        )
        .unwrap();
    memory
        .set_identity_view(&owner(), 1, "host anchor", vec![authority])
        .unwrap();
    memory
        .put_working_context(&owner(), &session(), "context", 1, context(&source), 100)
        .unwrap();
    memory
        .for_pet(&owner())
        .create(operation("create"), draft("tea long term", &source), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    let length = fs::metadata(scratch.path()).unwrap().len();
    let mut reader = memory.for_session(&owner(), &session(), ReadPurpose::OfflineReasoning);
    assert_eq!(reader.entity(&query, 100).unwrap().items().len(), 2);
    assert!(reader.identity().is_ok());
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert!(matches!(
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query, 100),
        Err(PersistenceError::Memory(MemoryError::AccessDenied))
    ));
    configure(&mut memory, &source);
    let mut reader = memory.for_session(&owner(), &session(), ReadPurpose::OfflineReasoning);
    let result = reader.entity(&query, 100).unwrap();
    assert_eq!(result.cache_status(), CacheStatus::Built);
    assert_eq!(result.items().len(), 1);
    assert_eq!(result.items()[0].text(), "tea long term");
    assert!(
        reader
            .retrieve(
                &query.clone().with_layer(RetrievalLayer::WorkingContext),
                100
            )
            .unwrap()
            .items()
            .is_empty()
    );
    assert!(matches!(
        reader.identity(),
        Err(PersistenceError::Memory(MemoryError::NotFound))
    ));
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
}

#[test]
fn committed_mutations_invalidate_cache_and_old_operation_retry_does_not_restore_it() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(&scratch);
    let original = draft("tea old", &source);
    let created = memory
        .for_pet(&owner())
        .create(operation("create"), original.clone(), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .entity(&query, 100)
        .unwrap();
    memory
        .for_pet(&owner())
        .update(
            operation("update"),
            created.value().id(),
            1,
            draft("tea new", &source),
            200,
        )
        .unwrap();
    assert_eq!(
        memory
            .for_pet(&owner())
            .create(operation("create"), original, 100)
            .unwrap(),
        created
    );
    let result = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .entity(&query, 200)
        .unwrap();
    assert_eq!(result.cache_status(), CacheStatus::Built);
    assert_eq!(result.items()[0].text(), "tea new");
    memory
        .for_pet(&owner())
        .delete(operation("delete"), created.value().id(), 2, 300)
        .unwrap();
    assert!(
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 300)
            .unwrap()
            .items()
            .is_empty()
    );
}

#[test]
fn rejected_cas_preserves_existing_cache_and_no_journal_append() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(&scratch);
    let created = memory
        .for_pet(&owner())
        .create(operation("create"), draft("tea old", &source), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .entity(&query, 100)
        .unwrap();
    let length = fs::metadata(scratch.path()).unwrap().len();
    assert!(matches!(
        memory.for_pet(&owner()).update(
            operation("bad-cas"),
            created.value().id(),
            9,
            draft("tea new", &source),
            200
        ),
        Err(PersistenceError::Memory(
            MemoryError::RevisionConflict { .. }
        ))
    ));
    assert_eq!(
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 200)
            .unwrap()
            .cache_status(),
        CacheStatus::Hit
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
}

#[test]
fn local_source_versions_reauthorize_and_require_new_exact_grants() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(&scratch);
    let created = memory
        .for_pet(&owner())
        .create(operation("create"), draft("tea old", &source), 100)
        .unwrap();
    let query = RetrievalQuery::new("tea");
    memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .entity(&query, 100)
        .unwrap();
    let current = memory
        .register_source(
            operation("source-v2"),
            MemoryScope::Private(owner()),
            source.id(),
            2,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    assert!(
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 200)
            .unwrap()
            .items()
            .is_empty()
    );
    memory
        .for_pet(&owner())
        .update(
            operation("update"),
            created.value().id(),
            1,
            draft("tea new", &current),
            200,
        )
        .unwrap();
    assert!(
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&query, 200)
            .unwrap()
            .items()
            .is_empty()
    );
    memory
        .set_read_policy(&owner(), ReadPolicy::offline(2, vec![current]))
        .unwrap();
    assert_eq!(
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .entity(&query, 200)
            .unwrap()
            .items()[0]
            .text(),
        "tea new"
    );
}

#[test]
fn local_fallback_and_undefined_purposes_do_not_expand_permissions() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(&scratch);
    memory
        .for_pet(&owner())
        .create(operation("create"), draft("tea", &source), 100)
        .unwrap();
    memory.set_entity_cache_capacity(0).unwrap();
    let query = RetrievalQuery::new("tea").with_limit(1).with_char_budget(3);
    let result = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .entity(&query, 100)
        .unwrap();
    assert_eq!(result.cache_status(), CacheStatus::Fallback);
    assert_eq!(result.used_chars(), 3);
    for purpose in [
        ReadPurpose::Unknown,
        ReadPurpose::ModelTransmission,
        ReadPurpose::UserInspection,
    ] {
        assert!(matches!(
            memory
                .for_session(&owner(), &session(), purpose)
                .entity(&query, 100),
            Err(PersistenceError::Memory(MemoryError::AccessDenied))
        ));
    }
}
