use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_memory_core::{
    Category, Epistemic, Lifecycle, MemoryDraft, MemoryError, MemoryScope, Namespace, PetIdentity,
    ReadPolicy, ReadPurpose, RecordState, RetrievalQuery, SessionId, SourceRef,
};
use moe_memory_local::{CleanupState, LocalMemory, OperationId, PersistenceError};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "moe-memory-m3-{}-{}",
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
fn owner() -> PetIdentity {
    PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap()
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
fn fixture(scratch: &Scratch) -> (LocalMemory, SourceRef, moe_memory_core::MemoryId) {
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
    let id = memory
        .for_pet(&owner())
        .create(operation("create"), draft("tea original", &source), 100)
        .unwrap()
        .value()
        .id();
    memory
        .set_read_policy(&owner(), ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    (memory, source, id)
}
fn denied(memory: &mut LocalMemory) {
    for purpose in [
        ReadPurpose::OfflineReasoning,
        ReadPurpose::Unknown,
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        assert!(matches!(
            memory
                .for_session(&owner(), &session(), purpose)
                .retrieve(&RetrievalQuery::new("tea"), 300),
            Err(PersistenceError::Memory(MemoryError::AccessDenied))
        ));
    }
}

#[test]
fn archive_restore_replace_and_delete_survive_restarts_with_exact_receipts() {
    let scratch = Scratch::new();
    let (mut memory, source, id) = fixture(&scratch);
    let archive = memory
        .for_pet(&owner())
        .archive(operation("archive"), id, 1, 150)
        .unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    denied(&mut memory);
    assert_eq!(
        memory.for_pet(&owner()).status(id, 200).unwrap().state(),
        RecordState::Archived
    );
    assert_eq!(
        memory
            .for_pet(&owner())
            .archive(operation("archive"), id, 1, 150)
            .unwrap(),
        archive
    );
    memory
        .for_pet(&owner())
        .restore_archive(operation("restore"), id, 2, 200)
        .unwrap();
    let replacement = memory
        .for_pet(&owner())
        .replace(
            operation("replace"),
            id,
            3,
            draft("tea correction", &source),
            250,
        )
        .unwrap();
    let new_id = replacement.value().replacement().id();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    let length = fs::metadata(scratch.path()).unwrap().len();
    assert_eq!(
        memory
            .for_pet(&owner())
            .replace(
                operation("replace"),
                id,
                3,
                draft("tea correction", &source),
                250
            )
            .unwrap(),
        replacement
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
    assert_eq!(
        memory.for_pet(&owner()).status(id, 300).unwrap().state(),
        RecordState::Superseded(new_id)
    );
    memory
        .for_pet(&owner())
        .delete(operation("delete"), new_id, 1, 300)
        .unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(
        memory
            .for_pet(&owner())
            .status(new_id, 300)
            .unwrap()
            .state(),
        RecordState::Deleted
    );
    assert!(memory.for_pet(&owner()).get(new_id, 300).is_err());
}

#[test]
fn source_revocation_and_explicit_restore_are_durable_without_automatic_reauthorization() {
    let scratch = Scratch::new();
    let (mut memory, source, id) = fixture(&scratch);
    let revoked = memory.revoke_source(operation("revoke"), &source).unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    denied(&mut memory);
    assert_eq!(
        memory.revoke_source(operation("revoke"), &source).unwrap(),
        revoked
    );
    assert!(
        memory
            .set_read_policy(&owner(), ReadPolicy::offline(1, vec![source.clone()]))
            .is_err()
    );
    assert!(
        memory
            .register_source(
                operation("implicit-restore"),
                MemoryScope::Private(owner()),
                "message",
                2,
                Epistemic::UserStatement
            )
            .is_err()
    );
    let restored = memory
        .restore_source(operation("source-restore"), &source, 2)
        .unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert!(memory.for_pet(&owner()).get(id, 300).is_err());
    assert_eq!(
        memory
            .restore_source(operation("source-restore"), &source, 2)
            .unwrap(),
        restored
    );
    memory
        .for_pet(&owner())
        .update(
            operation("revalidate"),
            id,
            1,
            draft("tea fresh evidence", restored.value()),
            300,
        )
        .unwrap();
    denied(&mut memory);
    memory
        .set_read_policy(
            &owner(),
            ReadPolicy::offline(1, vec![restored.value().clone()]),
        )
        .unwrap();
    assert_eq!(
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&RetrievalQuery::new("tea"), 300)
            .unwrap()
            .items()
            .len(),
        1
    );
}

#[test]
fn cancelled_generations_and_runtime_epochs_block_late_tasks_after_reopen() {
    let scratch = Scratch::new();
    let (mut memory, source, _) = fixture(&scratch);
    let task = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let generation = memory.generation().unwrap();
    let cancellation = memory
        .cancel_tasks(operation("cancel"), generation)
        .unwrap();
    assert!(matches!(
        memory.create_from_task(operation("late"), &task, draft("tea stale", &source), 200),
        Err(PersistenceError::Memory(MemoryError::TaskCancelled))
    ));
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(memory.generation().unwrap(), generation + 1);
    assert_eq!(
        memory
            .cancel_tasks(operation("cancel"), generation)
            .unwrap(),
        cancellation
    );
    memory
        .set_read_policy(&owner(), ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    assert!(matches!(
        memory.create_from_task(operation("late"), &task, draft("tea stale", &source), 200),
        Err(PersistenceError::Memory(MemoryError::TaskCancelled))
    ));
    let fresh = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 200)
        .unwrap();
    memory
        .create_from_task(operation("fresh"), &fresh, draft("tea fresh", &source), 300)
        .unwrap();
}

#[test]
fn successful_task_retry_confirms_history_without_reexecuting_or_requiring_old_grants() {
    let scratch = Scratch::new();
    let (mut memory, source, _) = fixture(&scratch);
    let task = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let candidate = draft("tea derived", &source);
    let created = memory
        .create_from_task(operation("task"), &task, candidate.clone(), 200)
        .unwrap();
    assert_eq!(
        memory
            .create_from_task(operation("task"), &task, candidate.clone(), 200)
            .unwrap(),
        created
    );
    memory
        .for_pet(&owner())
        .delete(operation("delete"), created.value().id(), 1, 300)
        .unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    denied(&mut memory);
    let length = fs::metadata(scratch.path()).unwrap().len();
    assert_eq!(
        memory
            .create_from_task(operation("task"), &task, candidate, 200)
            .unwrap(),
        created
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
    assert!(
        memory
            .for_pet(&owner())
            .get(created.value().id(), 300)
            .is_err()
    );
    assert!(matches!(
        memory.create_from_task(operation("task"), &task, draft("different", &source), 200),
        Err(PersistenceError::OperationConflict)
    ));
}

#[test]
fn task_operation_id_includes_the_full_read_request_not_just_candidate_text() {
    let scratch = Scratch::new();
    let (mut memory, source, _) = fixture(&scratch);
    let first = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let second = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("te"), 100)
        .unwrap();
    memory
        .create_from_task(operation("task"), &first, draft("same", &source), 200)
        .unwrap();
    assert!(matches!(
        memory.create_from_task(operation("task"), &second, draft("same", &source), 200),
        Err(PersistenceError::OperationConflict)
    ));
}

#[test]
fn persistent_dependency_closure_cannot_be_revived_by_old_create_retries() {
    let scratch = Scratch::new();
    let (mut memory, source, root) = fixture(&scratch);
    let dependency = memory
        .for_pet(&owner())
        .get(root, 100)
        .unwrap()
        .version_ref();
    let candidate = draft("tea child", &source).with_dependencies(vec![dependency]);
    let child = memory
        .for_pet(&owner())
        .create(operation("child"), candidate.clone(), 100)
        .unwrap();
    memory
        .for_pet(&owner())
        .archive(operation("archive"), root, 1, 150)
        .unwrap();
    memory
        .for_pet(&owner())
        .restore_archive(operation("restore"), root, 2, 200)
        .unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(
        memory
            .for_pet(&owner())
            .create(operation("child"), candidate, 100)
            .unwrap(),
        child
    );
    assert!(
        !memory
            .for_pet(&owner())
            .status(child.value().id(), 300)
            .unwrap()
            .is_available()
    );
    assert!(
        memory
            .for_pet(&owner())
            .get(child.value().id(), 300)
            .is_err()
    );
}

#[test]
fn management_payload_conflicts_and_cas_rejections_never_append_or_consume_ids() {
    let scratch = Scratch::new();
    let (mut memory, _, id) = fixture(&scratch);
    let length = fs::metadata(scratch.path()).unwrap().len();
    assert!(
        memory
            .for_pet(&owner())
            .archive(operation("manage"), id, 2, 200)
            .is_err()
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
    memory
        .for_pet(&owner())
        .archive(operation("manage"), id, 1, 200)
        .unwrap();
    let length = fs::metadata(scratch.path()).unwrap().len();
    assert!(matches!(
        memory
            .for_pet(&owner())
            .archive(operation("manage"), id, 1, 201),
        Err(PersistenceError::OperationConflict)
    ));
    assert!(matches!(
        memory
            .for_pet(&owner())
            .restore_archive(operation("manage"), id, 2, 300),
        Err(PersistenceError::OperationConflict)
    ));
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
}

#[test]
fn runtime_revocation_and_removal_do_not_persist_permissions_or_session_data() {
    let scratch = Scratch::new();
    let (mut memory, source, _) = fixture(&scratch);
    let context = MemoryDraft::new(
        "tea volatile",
        Category::WorkingContext,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
    .with_lifecycle(Lifecycle::ShortTerm);
    memory
        .put_working_context(&owner(), &session(), "context", 1, context, 100)
        .unwrap();
    let length = fs::metadata(scratch.path()).unwrap().len();
    memory
        .remove_working_context(&owner(), &session(), "context", 1, 200)
        .unwrap();
    memory.revoke_access(&owner(), 2).unwrap();
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    denied(&mut memory);
    memory
        .set_read_policy(&owner(), ReadPolicy::offline(1, vec![source]))
        .unwrap();
    assert_eq!(
        memory
            .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&RetrievalQuery::new("tea"), 300)
            .unwrap()
            .items()
            .len(),
        1
    );
}

#[test]
fn deletion_boundary_reports_retained_journal_and_undecided_unmanaged_copies() {
    let scratch = Scratch::new();
    let (mut memory, _, id) = fixture(&scratch);
    memory
        .for_pet(&owner())
        .delete(operation("delete"), id, 1, 200)
        .unwrap();
    let report = memory.deletion_boundary().unwrap();
    assert_eq!(report.physical_erasure(), CleanupState::NotPerformed);
    assert_eq!(report.journal(), CleanupState::HistoryRetained);
    assert_eq!(report.backups(), CleanupState::PolicyUndetermined);
    assert_eq!(report.external_copies(), CleanupState::Unmanaged);
    assert_eq!(report.delivered_snapshots(), CleanupState::NotRecallable);
    drop(memory);
    let bytes = fs::read(scratch.path()).unwrap();
    assert!(
        bytes
            .windows(b"tea original".len())
            .any(|window| window == b"tea original")
    );
}

#[test]
fn task_operation_id_preserves_original_dependencies_before_automatic_closure() {
    let scratch = Scratch::new();
    let (mut memory, source, root) = fixture(&scratch);
    let dependency = memory
        .for_pet(&owner())
        .get(root, 100)
        .unwrap()
        .version_ref();
    let task = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    let original = draft("same candidate", &source);
    let created = memory
        .create_from_task(operation("task"), &task, original.clone(), 200)
        .unwrap();
    assert_eq!(
        memory
            .create_from_task(operation("task"), &task, original, 200)
            .unwrap(),
        created
    );
    let supplied_dependency = draft("same candidate", &source).with_dependencies(vec![dependency]);
    assert!(matches!(
        memory.create_from_task(operation("task"), &task, supplied_dependency, 200),
        Err(PersistenceError::OperationConflict)
    ));
}
