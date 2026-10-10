use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_memory_core::{
    Category, ContextBlockKind, Epistemic, MemoryDraft, MemoryError, MemoryScope, Namespace,
    PetIdentity, ReadPolicy, ReadPurpose, RequestContext, RetrievalQuery, SessionId, SourceRef,
};
use moe_memory_local::{LocalMemory, OperationId, PersistenceError};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "moe-memory-context-{}-{}",
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

fn owner() -> PetIdentity {
    PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap()
}

fn session() -> SessionId {
    SessionId::new("current").unwrap()
}

fn operation(name: &str) -> OperationId {
    OperationId::new(name).unwrap()
}

fn draft(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Episode,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
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
    memory
        .for_pet(&owner())
        .create(
            operation("first"),
            draft("tea before breakfast", &source),
            100,
        )
        .unwrap();
    memory
        .for_pet(&owner())
        .create(operation("second"), draft("tea after lunch", &source), 100)
        .unwrap();
    memory
        .set_read_policy(&owner(), ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    (memory, source)
}

fn context(memory: &mut LocalMemory) -> RequestContext {
    let task = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    memory.assemble_context(&task, 100, 100).unwrap()
}

#[test]
fn local_projection_and_compaction_never_write_the_journal() {
    let scratch = Scratch::new();
    let (memory, source) = fixture(&scratch);
    drop(memory);
    let journal = fs::read(scratch.path()).unwrap();
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    memory
        .set_read_policy(&owner(), ReadPolicy::offline(1, vec![source]))
        .unwrap();
    let generation = memory.generation().unwrap();
    let mut context = context(&mut memory);
    let ticket = memory.begin_compaction(&context, 0..2, 100).unwrap();
    memory
        .apply_compaction(&mut context, &ticket, "tea routine", 110)
        .unwrap();
    let view = memory.context_view(&context, 110).unwrap();
    assert_eq!(view.blocks()[0].kind(), ContextBlockKind::UnverifiedSummary);
    assert_eq!(view.blocks()[0].evidence().len(), 2);
    assert_eq!(view.usage().used_chars, 11);
    assert_eq!(view.usage().revision, 2);
    assert_eq!(memory.generation().unwrap(), generation);
    let entries = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .retrieve(&RetrievalQuery::new("tea"), 110)
        .unwrap();
    assert_eq!(entries.items().len(), 2);
    assert!(
        !entries
            .items()
            .iter()
            .any(|item| item.text() == "tea routine")
    );
    drop(memory);
    assert_eq!(fs::read(scratch.path()).unwrap(), journal);
}

#[test]
fn restarting_and_reinstalling_policy_cannot_revive_old_projection() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(&scratch);
    let mut context = context(&mut memory);
    let ticket = memory.begin_compaction(&context, 0..2, 100).unwrap();
    drop(memory);
    let mut reopened = LocalMemory::open(scratch.path()).unwrap();
    reopened
        .set_read_policy(&owner(), ReadPolicy::offline(1, vec![source]))
        .unwrap();
    assert!(matches!(
        reopened.context_view(&context, 110),
        Err(PersistenceError::Memory(MemoryError::TaskCancelled))
    ));
    assert!(matches!(
        reopened.apply_compaction(&mut context, &ticket, "late", 110),
        Err(PersistenceError::Memory(MemoryError::TaskCancelled))
    ));
    let fresh = self::context(&mut reopened);
    assert_eq!(
        reopened.context_view(&fresh, 100).unwrap().blocks().len(),
        2
    );
}

#[test]
fn local_cancellation_and_revocation_reject_late_summary_without_writes() {
    for revoke in [false, true] {
        let scratch = Scratch::new();
        let (mut memory, source) = fixture(&scratch);
        let mut context = context(&mut memory);
        let ticket = memory.begin_compaction(&context, 0..2, 100).unwrap();
        if revoke {
            memory.revoke_source(operation("revoke"), &source).unwrap();
        } else {
            let generation = memory.generation().unwrap();
            memory
                .cancel_tasks(operation("cancel"), generation)
                .unwrap();
        }
        let journal_len = fs::metadata(scratch.path()).unwrap().len();
        let generation = memory.generation().unwrap();
        assert!(matches!(
            memory.context_view(&context, 110),
            Err(PersistenceError::Memory(MemoryError::TaskCancelled))
        ));
        assert!(matches!(
            memory.apply_compaction(&mut context, &ticket, "late", 110),
            Err(PersistenceError::Memory(MemoryError::TaskCancelled))
        ));
        assert_eq!(fs::metadata(scratch.path()).unwrap().len(), journal_len);
        assert_eq!(memory.generation().unwrap(), generation);
    }
}

#[test]
fn budget_failure_and_invalid_summary_leave_projection_and_journal_intact() {
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
    drop(memory);
    let journal = fs::read(scratch.path()).unwrap();
    let mut memory = LocalMemory::open(scratch.path()).unwrap();
    memory
        .set_identity_view(&owner(), 1, "我是小萌", vec![authority.clone()])
        .unwrap();
    memory
        .set_read_policy(&owner(), ReadPolicy::offline(2, vec![source, authority]))
        .unwrap();
    let task = memory
        .for_session(&owner(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("tea"), 100)
        .unwrap();
    assert!(matches!(
        memory.assemble_context(&task, 3, 100),
        Err(PersistenceError::Memory(
            MemoryError::ContextBudgetExceeded { .. }
        ))
    ));
    let mut context = memory.assemble_context(&task, 100, 100).unwrap();
    let before = memory.context_view(&context, 100).unwrap().clone();
    let ticket = memory.begin_compaction(&context, 0..1, 100).unwrap();
    assert!(matches!(
        memory.apply_compaction(&mut context, &ticket, "tea before breakfast", 100),
        Err(PersistenceError::Memory(MemoryError::InvalidInput(_)))
    ));
    assert_eq!(memory.context_view(&context, 100).unwrap(), &before);
    drop(memory);
    assert_eq!(fs::read(scratch.path()).unwrap(), journal);
}

#[test]
fn adapter_preserves_compaction_ticket_cas() {
    let scratch = Scratch::new();
    let (mut memory, _) = fixture(&scratch);
    let mut context = context(&mut memory);
    let first = memory.begin_compaction(&context, 0..2, 100).unwrap();
    let second = memory.begin_compaction(&context, 0..2, 100).unwrap();
    memory
        .apply_compaction(&mut context, &second, "tea", 100)
        .unwrap();
    assert!(matches!(
        memory.apply_compaction(&mut context, &first, "older", 100),
        Err(PersistenceError::Memory(
            MemoryError::ContextVersionConflict
        ))
    ));
    assert_eq!(
        memory.context_view(&context, 100).unwrap().blocks()[0].text(),
        "tea"
    );
}
