use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_memory_core::{
    Category, Epistemic, Lifecycle, MemoryDraft, MemoryError, MemoryScope, Namespace, PetIdentity,
    SearchQuery, Sensitivity, SourceRef, WriteOperation,
};
use moe_memory_local::{Durability, LocalMemory, OperationId, PersistenceError};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let directory = std::env::temp_dir().join(format!(
                "moe-memory-m1-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&directory) {
                Ok(()) => return Self(directory),
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

fn identity(pet: &str) -> PetIdentity {
    PetIdentity::new(Namespace::new("home", "user").unwrap(), pet).unwrap()
}

fn operation(value: &str) -> OperationId {
    OperationId::new(value).unwrap()
}

fn source(memory: &mut LocalMemory, identity: &PetIdentity) -> SourceRef {
    memory
        .register_source(
            operation("source-1"),
            MemoryScope::Private(identity.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone()
}

fn draft(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::UserProfile,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
}

#[test]
fn restarts_restore_records_versions_sources_and_all_draft_fields() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let original = draft("用户喜欢红茶 🐾", &source)
        .with_lifecycle(Lifecycle::ShortTerm)
        .with_sensitivity(Sensitivity::Private)
        .with_expiration(500);
    let created = memory
        .for_pet(&pet)
        .create(operation("create"), original, 100)
        .unwrap();
    assert_eq!(created.sequence(), 2);
    assert_eq!(created.durability(), Durability::FileSynced);
    let updated = memory
        .for_pet(&pet)
        .update(
            operation("update"),
            created.value().id(),
            1,
            draft("用户喜欢绿茶 🐾", &source)
                .with_sensitivity(Sensitivity::Private)
                .with_lifecycle(Lifecycle::ShortTerm)
                .with_expiration(600),
            200,
        )
        .unwrap();
    let before = memory.for_pet(&pet).get(updated.value().id(), 200).unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(
        memory.for_pet(&pet).get(updated.value().id(), 200).unwrap(),
        before
    );
    assert!(memory.for_pet(&pet).get(updated.value().id(), 600).is_err());
    assert_eq!(
        memory
            .for_pet(&pet)
            .search(&SearchQuery::new("绿茶"), 200)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn duplicate_create_returns_original_receipt_without_a_second_append_after_restart() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let created = memory
        .for_pet(&pet)
        .create(operation("create"), draft("tea", &source), 100)
        .unwrap();
    let length = fs::metadata(scratch.path()).unwrap().len();
    assert_eq!(
        memory
            .for_pet(&pet)
            .create(operation("create"), draft("tea", &source), 100)
            .unwrap(),
        created
    );
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(
        memory
            .for_pet(&pet)
            .create(operation("create"), draft("tea", &source), 100)
            .unwrap(),
        created
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
    assert_eq!(
        memory
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 100)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn duplicate_update_does_not_roll_back_a_newer_revision() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let created = memory
        .for_pet(&pet)
        .create(operation("create"), draft("one", &source), 100)
        .unwrap();
    let id = created.value().id();
    let updated = memory
        .for_pet(&pet)
        .update(operation("update"), id, 1, draft("two", &source), 200)
        .unwrap();
    memory
        .for_pet(&pet)
        .update(
            operation("update-newer"),
            id,
            2,
            draft("three", &source),
            300,
        )
        .unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(
        memory
            .for_pet(&pet)
            .update(operation("update"), id, 1, draft("two", &source), 200)
            .unwrap(),
        updated
    );
    let current = memory.for_pet(&pet).get(id, 300).unwrap();
    assert_eq!(current.text(), "three");
    assert_eq!(current.revision(), 3);
}

#[test]
fn reused_operation_id_rejects_changed_payload_time_kind_and_actor() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let created = memory
        .for_pet(&pet)
        .create(operation("create"), draft("one", &source), 100)
        .unwrap();
    for (text, now) in [("different", 100), ("one", 101)] {
        assert!(matches!(
            memory
                .for_pet(&pet)
                .create(operation("create"), draft(text, &source), now),
            Err(PersistenceError::OperationConflict)
        ));
    }
    assert!(matches!(
        memory
            .for_pet(&identity("bob"))
            .create(operation("create"), draft("one", &source), 100),
        Err(PersistenceError::OperationConflict)
    ));
    assert!(matches!(
        memory
            .for_pet(&pet)
            .delete(operation("create"), created.value().id(), 1, 100),
        Err(PersistenceError::OperationConflict)
    ));
    assert!(matches!(
        memory.register_source(
            operation("create"),
            MemoryScope::Private(pet),
            "message",
            1,
            Epistemic::UserStatement
        ),
        Err(PersistenceError::OperationConflict)
    ));
}

#[test]
fn deletion_and_old_retries_never_resurrect_or_reuse_ids() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let created = memory
        .for_pet(&pet)
        .create(operation("create"), draft("old", &source), 100)
        .unwrap();
    let id = created.value().id();
    let deleted = memory
        .for_pet(&pet)
        .delete(operation("delete"), id, 1, 200)
        .unwrap();
    assert_eq!(deleted.value().operation(), WriteOperation::Deleted);
    assert_eq!(deleted.value().revision(), 2);
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(
        memory
            .for_pet(&pet)
            .delete(operation("delete"), id, 1, 200)
            .unwrap(),
        deleted
    );
    assert_eq!(
        memory
            .for_pet(&pet)
            .create(operation("create"), draft("old", &source), 100)
            .unwrap(),
        created
    );
    assert!(matches!(
        memory.for_pet(&pet).get(id, 300),
        Err(PersistenceError::Memory(MemoryError::NotFound))
    ));
    assert!(matches!(
        memory
            .for_pet(&pet)
            .update(operation("late"), id, 1, draft("revive", &source), 300),
        Err(PersistenceError::Memory(MemoryError::NotFound))
    ));
    let new = memory
        .for_pet(&pet)
        .create(operation("new"), draft("new", &source), 300)
        .unwrap();
    assert!(new.value().id() > id);
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(
        memory
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 300)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn cas_and_out_of_order_time_rejections_leave_the_journal_unchanged() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let id = memory
        .for_pet(&pet)
        .create(operation("create"), draft("one", &source), 100)
        .unwrap()
        .value()
        .id();
    memory
        .for_pet(&pet)
        .update(operation("update"), id, 1, draft("two", &source), 200)
        .unwrap();
    let length = fs::metadata(scratch.path()).unwrap().len();
    assert!(matches!(
        memory
            .for_pet(&pet)
            .update(operation("stale"), id, 1, draft("stale", &source), 300),
        Err(PersistenceError::Memory(MemoryError::RevisionConflict {
            actual: 2,
            ..
        }))
    ));
    assert!(matches!(
        memory
            .for_pet(&pet)
            .delete(operation("stale-delete"), id, 1, 300),
        Err(PersistenceError::Memory(
            MemoryError::RevisionConflict { .. }
        ))
    ));
    assert!(matches!(
        memory
            .for_pet(&pet)
            .update(operation("early"), id, 2, draft("early", &source), 199),
        Err(PersistenceError::Memory(MemoryError::TimeWentBackwards))
    ));
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(memory.for_pet(&pet).get(id, 300).unwrap().text(), "two");
}

#[test]
fn source_revisions_and_invalidation_survive_restart_and_reject_late_evidence() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let scope = MemoryScope::Private(pet.clone());
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let old = source(&mut memory, &pet);
    let id = memory
        .for_pet(&pet)
        .create(operation("create"), draft("one", &old), 100)
        .unwrap()
        .value()
        .id();
    let newer = memory
        .register_source(
            operation("source-2"),
            scope.clone(),
            "message",
            2,
            Epistemic::UserStatement,
        )
        .unwrap();
    assert!(
        memory
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 100)
            .unwrap()
            .is_empty()
    );
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    let length = fs::metadata(scratch.path()).unwrap().len();
    assert!(matches!(
        memory.register_source(
            operation("source-stale"),
            scope.clone(),
            "message",
            1,
            Epistemic::UserStatement
        ),
        Err(PersistenceError::Memory(MemoryError::SourceVersionConflict))
    ));
    assert!(matches!(
        memory.register_source(
            operation("source-conflict"),
            scope.clone(),
            "message",
            2,
            Epistemic::Observation
        ),
        Err(PersistenceError::Memory(MemoryError::SourceVersionConflict))
    ));
    assert!(matches!(
        memory
            .for_pet(&pet)
            .create(operation("late"), draft("late", &old), 200),
        Err(PersistenceError::Memory(MemoryError::InvalidEvidence))
    ));
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
    assert!(memory.for_pet(&pet).get(id, 200).is_err());
    assert_eq!(
        memory
            .register_source(
                operation("source-2"),
                scope,
                "message",
                2,
                Epistemic::UserStatement
            )
            .unwrap(),
        newer
    );
    memory
        .for_pet(&pet)
        .update(operation("repair"), id, 1, draft("two", newer.value()), 200)
        .unwrap();
    assert_eq!(memory.for_pet(&pet).get(id, 200).unwrap().text(), "two");
}

#[test]
fn failed_validation_does_not_consume_operation_id_or_memory_id() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    assert!(
        memory
            .for_pet(&pet)
            .create(operation("create"), draft("", &source), 100)
            .is_err()
    );
    let created = memory
        .for_pet(&pet)
        .create(operation("create"), draft("valid", &source), 100)
        .unwrap();
    assert_eq!(created.sequence(), 2);
    assert_eq!(format!("{:?}", created.value().id()), "MemoryId(1)");
}

#[test]
fn owner_and_namespace_filters_remain_in_force_after_restart() {
    let scratch = Scratch::new();
    let alice = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &alice);
    let id = memory
        .for_pet(&alice)
        .create(operation("create"), draft("private", &source), 100)
        .unwrap()
        .value()
        .id();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    for denied in [
        identity("bob"),
        PetIdentity::new(Namespace::new("elsewhere", "user").unwrap(), "alice").unwrap(),
        PetIdentity::new(Namespace::new("home", "other-user").unwrap(), "alice").unwrap(),
    ] {
        assert!(memory.for_pet(&denied).get(id, 100).is_err());
        assert!(
            memory
                .for_pet(&denied)
                .search(&SearchQuery::new(""), 100)
                .unwrap()
                .is_empty()
        );
        assert!(
            memory
                .for_pet(&denied)
                .delete(operation("denied"), id, 1, 100)
                .is_err()
        );
    }
}

#[test]
fn open_never_creates_and_create_never_overwrites() {
    let scratch = Scratch::new();
    assert!(LocalMemory::open(scratch.path()).is_err());
    assert!(!scratch.path().exists());
    drop(LocalMemory::create(scratch.path()).unwrap());
    let before = fs::read(scratch.path()).unwrap();
    assert!(LocalMemory::create(scratch.path()).is_err());
    assert_eq!(fs::read(scratch.path()).unwrap(), before);
    assert!(LocalMemory::create(scratch.0.join("missing").join("journal")).is_err());
}

#[test]
fn empty_invalid_and_unknown_version_files_fail_closed_and_are_not_rewritten() {
    let scratch = Scratch::new();
    for bytes in [
        Vec::new(),
        b"not a memory archive".to_vec(),
        b"MOEMEMJ\0\x02\0\0\0\0\0\0\0".to_vec(),
    ] {
        fs::write(scratch.path(), &bytes).unwrap();
        assert!(matches!(
            LocalMemory::open(scratch.path()),
            Err(PersistenceError::Corrupt { .. } | PersistenceError::UnsupportedVersion(_))
        ));
        assert_eq!(fs::read(scratch.path()).unwrap(), bytes);
    }
}

#[test]
fn corruption_truncation_and_trailing_garbage_never_fall_back_to_empty() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    memory
        .for_pet(&pet)
        .create(operation("create"), draft("tea", &source), 100)
        .unwrap();
    drop(memory);
    let valid = fs::read(scratch.path()).unwrap();
    for offset in [0, 16, 24, valid.len() / 2, valid.len() - 1] {
        let mut corrupted = valid.clone();
        corrupted[offset] ^= 0x80;
        fs::write(scratch.path(), &corrupted).unwrap();
        assert!(
            LocalMemory::open(scratch.path()).is_err(),
            "accepted corrupt offset {offset}"
        );
        assert_eq!(fs::read(scratch.path()).unwrap(), corrupted);
    }
    for length in [1, 15, 17, valid.len() - 1] {
        fs::write(scratch.path(), &valid[..length]).unwrap();
        assert!(
            LocalMemory::open(scratch.path()).is_err(),
            "accepted truncated length {length}"
        );
    }
    let mut garbage = valid.clone();
    garbage.push(1);
    fs::write(scratch.path(), garbage).unwrap();
    assert!(LocalMemory::open(scratch.path()).is_err());
    fs::write(scratch.path(), valid).unwrap();
    assert!(LocalMemory::open(scratch.path()).is_ok());
}

#[test]
fn writer_lock_excludes_another_open_and_is_released_on_drop() {
    let scratch = Scratch::new();
    let first = LocalMemory::create(scratch.path()).unwrap();
    assert!(matches!(
        LocalMemory::open(scratch.path()),
        Err(PersistenceError::Locked)
    ));
    drop(first);
    assert!(LocalMemory::open(scratch.path()).is_ok());
}

#[test]
fn writer_lock_excludes_another_process() {
    let scratch = Scratch::new();
    let _writer = LocalMemory::create(scratch.path()).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "writer_lock_child", "--nocapture"])
        .env("MOE_M1_LOCK_PATH", scratch.path())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn writer_lock_child() {
    if let Some(path) = std::env::var_os("MOE_M1_LOCK_PATH") {
        assert!(matches!(
            LocalMemory::open(path),
            Err(PersistenceError::Locked)
        ));
    }
}

#[test]
fn operation_ids_are_validated_and_debug_does_not_leak_memory_text() {
    for value in ["", " trimmed ", "line\nbreak"] {
        assert!(OperationId::new(value).is_err());
    }
    assert!(OperationId::new("x".repeat(129)).is_err());
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let receipt = memory
        .for_pet(&pet)
        .create(
            operation("create"),
            draft("secret-memory-text", &source),
            100,
        )
        .unwrap();
    assert!(!format!("{receipt:?}").contains("secret-memory-text"));
}

#[test]
fn process_exit_without_destructors_preserves_deletion_and_id_high_water_mark() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let old = memory
        .for_pet(&pet)
        .create(operation("create"), draft("old", &source), 100)
        .unwrap();
    drop(memory);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "restart_writer_child", "--nocapture"])
        .env("MOE_M1_RESTART_PATH", scratch.path())
        .status()
        .unwrap();
    assert!(status.success());
    let mut recovered = LocalMemory::open(scratch.path()).unwrap();
    let current = recovered
        .for_pet(&pet)
        .search(&SearchQuery::new(""), 300)
        .unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].text(), "new");
    assert!(current[0].id() > old.value().id());
    assert!(recovered.for_pet(&pet).get(old.value().id(), 300).is_err());
    assert_eq!(
        recovered
            .for_pet(&pet)
            .delete(operation("delete-child"), old.value().id(), 1, 200)
            .unwrap()
            .sequence(),
        3
    );
}

#[test]
fn restart_writer_child() {
    if let Some(path) = std::env::var_os("MOE_M1_RESTART_PATH") {
        let pet = identity("alice");
        let mut memory = LocalMemory::open(path).unwrap();
        let source = source(&mut memory, &pet);
        let old = memory
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 100)
            .unwrap();
        memory
            .for_pet(&pet)
            .delete(operation("delete-child"), old[0].id(), 1, 200)
            .unwrap();
        memory
            .for_pet(&pet)
            .create(operation("create-child"), draft("new", &source), 300)
            .unwrap();
        std::process::exit(0);
    }
}

#[test]
fn concurrent_cas_callers_are_serialized_and_only_one_wins() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let id = memory
        .for_pet(&pet)
        .create(operation("create"), draft("old", &source), 100)
        .unwrap()
        .value()
        .id();
    let memory = std::sync::Arc::new(std::sync::Mutex::new(memory));
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for name in ["first", "second"] {
            let memory = memory.clone();
            let pet = pet.clone();
            let source = source.clone();
            handles.push(scope.spawn(move || {
                memory.lock().unwrap().for_pet(&pet).update(
                    operation(name),
                    id,
                    1,
                    draft(name, &source),
                    200,
                )
            }));
        }
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                Err(PersistenceError::Memory(
                    MemoryError::RevisionConflict { .. }
                ))
            ))
            .count(),
        1
    );
    drop(memory);
    let mut recovered = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(recovered.for_pet(&pet).get(id, 200).unwrap().revision(), 2);
}

#[test]
fn changed_payload_fields_and_source_requests_conflict_after_restart() {
    let scratch = Scratch::new();
    let pet = identity("alice");
    let mut memory = LocalMemory::create(scratch.path()).unwrap();
    let source = source(&mut memory, &pet);
    let id = memory
        .for_pet(&pet)
        .create(operation("create"), draft("one", &source), 100)
        .unwrap()
        .value()
        .id();
    memory
        .for_pet(&pet)
        .update(operation("update"), id, 1, draft("two", &source), 200)
        .unwrap();
    drop(memory);
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    let length = fs::metadata(scratch.path()).unwrap().len();
    for changed in [
        draft("one", &source).with_sensitivity(Sensitivity::Private),
        draft("one", &source).with_lifecycle(Lifecycle::ShortTerm),
        draft("one", &source).with_expiration(500),
        MemoryDraft::new(
            "one",
            Category::Relationship,
            Epistemic::UserStatement,
            vec![source.clone()],
        ),
        MemoryDraft::new(
            "one",
            Category::UserProfile,
            Epistemic::Inference,
            vec![source.clone()],
        ),
    ] {
        assert!(matches!(
            memory
                .for_pet(&pet)
                .create(operation("create"), changed, 100),
            Err(PersistenceError::OperationConflict)
        ));
    }
    assert!(matches!(
        memory
            .for_pet(&pet)
            .update(operation("update"), id, 2, draft("two", &source), 200),
        Err(PersistenceError::OperationConflict)
    ));
    assert!(matches!(
        memory.register_source(
            operation("source-1"),
            MemoryScope::Private(pet.clone()),
            "message",
            2,
            Epistemic::UserStatement
        ),
        Err(PersistenceError::OperationConflict)
    ));
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), length);
    assert_eq!(memory.for_pet(&pet).get(id, 200).unwrap().text(), "two");
}
