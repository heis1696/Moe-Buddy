use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use moe_memory_core::{
    Category, Epistemic, MemoryCommand, MemoryDraft, MemoryError, MemoryScope, Namespace,
    PetIdentity, PublicationPolicy, ReadPolicy, ReadPurpose, RecordVersion, RetrievalLayer,
    RetrievalQuery, SessionId, SharedCandidate, SourceRef,
};
use moe_memory_local::{Durability, LocalMemory, OperationId, PersistenceError};

macro_rules! assert_err {
    ($actual:expr, Err($expected:pat) $(,)?) => {
        let actual = $actual;
        assert!(
            matches!(actual, Err($expected)),
            "unexpected result: {actual:?}"
        );
    };
}

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let directory =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/publication-tests");
        fs::create_dir_all(&directory).unwrap();
        Self(directory.join(format!(
            "publication-{}-{}.log",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn namespace() -> Namespace {
    Namespace::new("home", "user").unwrap()
}
fn pet() -> PetIdentity {
    PetIdentity::new(namespace(), "alice").unwrap()
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
        Category::Knowledge,
        Epistemic::Reference,
        vec![source.clone()],
    )
}
fn formal(text: &str, source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Protocol,
        Epistemic::Reference,
        vec![source.clone()],
    )
}
fn fixture(path: &Path) -> (LocalMemory, SourceRef) {
    let mut memory = LocalMemory::create(path).unwrap();
    let source = memory
        .register_source(
            operation("source"),
            MemoryScope::Shared(namespace()),
            "reference",
            1,
            Epistemic::Reference,
        )
        .unwrap()
        .value()
        .clone();
    memory
        .set_publication_policy(
            &pet(),
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    memory
        .set_formal_publication_policy(
            &namespace(),
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    memory
        .set_read_policy(&pet(), ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    (memory, source)
}
fn candidate(
    memory: &mut LocalMemory,
    source: &SourceRef,
    query: &str,
    text: &str,
) -> SharedCandidate {
    memory
        .for_session(&pet(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new(query), 100)
        .unwrap()
        .propose_shared(draft(text, source), 100)
        .unwrap()
}
fn denied(memory: &mut LocalMemory) {
    for purpose in [
        ReadPurpose::Unknown,
        ReadPurpose::OfflineReasoning,
        ReadPurpose::UserInspection,
        ReadPurpose::ModelTransmission,
    ] {
        assert!(matches!(
            memory
                .for_session(&pet(), &session(), purpose)
                .retrieve(&RetrievalQuery::new(""), 200),
            Err(PersistenceError::Memory(MemoryError::AccessDenied))
        ));
    }
}

#[test]
fn shared_publication_is_synced_locked_and_recovered_without_read_or_publish_grants() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(scratch.path());
    let candidate = candidate(&mut memory, &source, "", "reviewed experience");
    let command = memory.prepare_shared_publication(&candidate, 100).unwrap();
    let receipt = memory
        .commit_publication(operation("publish"), command.clone())
        .unwrap();
    assert_eq!(receipt.durability(), Durability::FileSynced);
    assert!(matches!(
        LocalMemory::open(scratch.path()),
        Err(PersistenceError::Locked)
    ));
    drop(memory);
    let mut recovered = LocalMemory::open(scratch.path()).unwrap();
    denied(&mut recovered);
    assert_eq!(
        recovered
            .for_pet(&pet())
            .get(receipt.value().id(), 200)
            .unwrap()
            .text(),
        "reviewed experience"
    );
    let generation = recovered.generation().unwrap();
    let bytes = fs::metadata(scratch.path()).unwrap().len();
    let decoded = MemoryCommand::from_bytes(&command.to_bytes().unwrap()).unwrap();
    assert_eq!(
        recovered
            .commit_publication(operation("publish"), decoded.clone())
            .unwrap(),
        receipt
    );
    assert_eq!(recovered.generation().unwrap(), generation);
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), bytes);
    assert_err!(
        recovered.commit_publication(operation("new-publish"), decoded),
        Err(PersistenceError::Memory(MemoryError::PublicationDenied))
    );
    assert_err!(
        recovered.prepare_shared_publication(&candidate, 100),
        Err(PersistenceError::Memory(MemoryError::TaskCancelled))
    );
    assert_err!(
        recovered.prepare_formal_publication(&namespace(), formal("new", &source), 200),
        Err(PersistenceError::Memory(MemoryError::PublicationDenied))
    );
}

#[test]
fn formal_create_and_cas_revision_survive_reopen_and_require_separate_read_layer() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(scratch.path());
    let install = memory
        .prepare_formal_publication(&namespace(), formal("protocol v1", &source), 100)
        .unwrap();
    let first = memory
        .commit_publication(operation("install"), install.clone())
        .unwrap();
    let revision = memory
        .prepare_formal_revision(
            &namespace(),
            first.value().id(),
            1,
            formal("protocol v2", &source),
            200,
        )
        .unwrap();
    let second = memory
        .commit_publication(operation("revise"), revision.clone())
        .unwrap();
    assert_eq!(second.value().revision(), 2);
    assert!(
        memory
            .for_session(&pet(), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&RetrievalQuery::new("protocol"), 200)
            .unwrap()
            .items()
            .is_empty()
    );
    let formal_query = RetrievalQuery::new("protocol").with_layer(RetrievalLayer::FormalMaterial);
    assert_eq!(
        memory
            .for_session(&pet(), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&formal_query, 200)
            .unwrap()
            .items()
            .len(),
        1
    );
    drop(memory);
    let mut recovered = LocalMemory::open(scratch.path()).unwrap();
    denied(&mut recovered);
    assert_eq!(
        recovered
            .commit_publication(operation("install"), install)
            .unwrap(),
        first
    );
    assert_eq!(
        recovered
            .commit_publication(operation("revise"), revision)
            .unwrap(),
        second
    );
    let record = recovered
        .for_pet(&pet())
        .get(first.value().id(), 200)
        .unwrap();
    assert_eq!(record.text(), "protocol v2");
    assert!(record.is_protected());
    assert_eq!(record.epistemic(), Epistemic::Reference);
    recovered
        .set_read_policy(&pet(), ReadPolicy::offline(1, vec![source]))
        .unwrap();
    assert_eq!(
        recovered
            .for_session(&pet(), &session(), ReadPurpose::OfflineReasoning)
            .retrieve(&formal_query, 200)
            .unwrap()
            .items()
            .len(),
        1
    );
}

#[test]
fn operation_id_covers_original_candidate_dependencies_and_task_query() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(scratch.path());
    let root = candidate(&mut memory, &source, "", "root");
    let command = memory.prepare_shared_publication(&root, 100).unwrap();
    let id = memory
        .commit_publication(operation("root"), command)
        .unwrap()
        .value()
        .id();
    let snapshot = memory
        .for_session(&pet(), &session(), ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("root"), 100)
        .unwrap();
    let omitted = snapshot
        .propose_shared(draft("child", &source), 100)
        .unwrap();
    let explicit = snapshot
        .propose_shared(
            draft("child", &source).with_dependencies(vec![RecordVersion::new(id, 1).unwrap()]),
            100,
        )
        .unwrap();
    assert_eq!(omitted.dependencies(), explicit.dependencies());
    let first = memory.prepare_shared_publication(&omitted, 100).unwrap();
    let second = memory.prepare_shared_publication(&explicit, 100).unwrap();
    let other_query = candidate(&mut memory, &source, "roo", "child");
    let third = memory
        .prepare_shared_publication(&other_query, 100)
        .unwrap();
    let receipt = memory
        .commit_publication(operation("child"), first.clone())
        .unwrap();
    let bytes = fs::metadata(scratch.path()).unwrap().len();
    assert_err!(
        memory.commit_publication(operation("child"), second),
        Err(PersistenceError::OperationConflict)
    );
    assert_err!(
        memory.commit_publication(operation("child"), third),
        Err(PersistenceError::OperationConflict)
    );
    assert_eq!(
        memory
            .commit_publication(operation("child"), first)
            .unwrap(),
        receipt
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), bytes);
}

#[test]
fn denied_auto_and_formal_policy_changes_do_not_append_or_install_implicit_permissions() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(scratch.path());
    let candidate = candidate(&mut memory, &source, "", "candidate");
    let bytes = fs::metadata(scratch.path()).unwrap().len();
    assert_err!(
        memory.prepare_automatic_publication(&candidate),
        Err(PersistenceError::Memory(
            MemoryError::AutomaticPublicationDisabled
        ))
    );
    memory
        .set_formal_publication_policy(&namespace(), PublicationPolicy::deny_all(2))
        .unwrap();
    assert_err!(
        memory.prepare_formal_publication(&namespace(), formal("denied", &source), 100),
        Err(PersistenceError::Memory(MemoryError::PublicationDenied))
    );
    assert_err!(
        memory.prepare_shared_publication(&candidate, 100),
        Err(PersistenceError::Memory(MemoryError::TaskCancelled))
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), bytes);
    drop(memory);
    let recovered = LocalMemory::open(scratch.path()).unwrap();
    assert_err!(
        recovered.prepare_formal_publication(&namespace(), formal("still denied", &source), 100),
        Err(PersistenceError::Memory(MemoryError::PublicationDenied))
    );
}

#[test]
fn exact_historical_publication_retry_after_revoke_confirms_receipt_not_content_or_authority() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(scratch.path());
    let candidate = candidate(&mut memory, &source, "", "root");
    let command = memory.prepare_shared_publication(&candidate, 100).unwrap();
    let receipt = memory
        .commit_publication(operation("publish"), command.clone())
        .unwrap();
    memory.revoke_source(operation("revoke"), &source).unwrap();
    let generation = memory.generation().unwrap();
    assert_eq!(
        memory
            .commit_publication(operation("publish"), command.clone())
            .unwrap(),
        receipt
    );
    assert_eq!(memory.generation().unwrap(), generation);
    assert_err!(
        memory.for_pet(&pet()).get(receipt.value().id(), 200),
        Err(PersistenceError::Memory(MemoryError::NotFound))
    );
    drop(memory);
    let mut recovered = LocalMemory::open(scratch.path()).unwrap();
    assert_eq!(
        recovered
            .commit_publication(operation("publish"), command)
            .unwrap(),
        receipt
    );
    assert_err!(
        recovered.for_pet(&pet()).get(receipt.value().id(), 200),
        Err(PersistenceError::Memory(MemoryError::NotFound))
    );
}

#[test]
fn restored_sources_do_not_revive_shared_records_or_old_publication_whitelists() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(scratch.path());
    let proposal = candidate(&mut memory, &source, "", "old");
    let command = memory.prepare_shared_publication(&proposal, 100).unwrap();
    let old = memory
        .commit_publication(operation("old"), command)
        .unwrap()
        .value()
        .id();
    memory.revoke_source(operation("revoke"), &source).unwrap();
    let restored = memory
        .restore_source(operation("restore"), &source, 2)
        .unwrap()
        .value()
        .clone();
    let proposal = candidate(&mut memory, &restored, "", "new");
    assert_err!(
        memory.prepare_shared_publication(&proposal, 100),
        Err(PersistenceError::Memory(MemoryError::PublicationDenied))
    );
    memory
        .set_publication_policy(
            &pet(),
            PublicationPolicy::allow_sources(2, vec![restored.clone()]),
        )
        .unwrap();
    let proposal = candidate(&mut memory, &restored, "", "new");
    let command = memory.prepare_shared_publication(&proposal, 100).unwrap();
    let new = memory
        .commit_publication(operation("new"), command)
        .unwrap()
        .value()
        .id();
    drop(memory);
    let mut recovered = LocalMemory::open(scratch.path()).unwrap();
    assert_err!(
        recovered.for_pet(&pet()).get(old, 200),
        Err(PersistenceError::Memory(MemoryError::NotFound))
    );
    assert!(recovered.for_pet(&pet()).get(new, 200).is_ok());
    denied(&mut recovered);
}

#[test]
fn non_publication_commands_cannot_use_the_new_commit_entry() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(scratch.path());
    let bytes = fs::metadata(scratch.path()).unwrap().len();
    let command = MemoryCommand::create(pet(), draft("private", &source), 100);
    assert_err!(
        memory.commit_publication(operation("wrong-route"), command),
        Err(PersistenceError::Memory(MemoryError::PublicationDenied))
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), bytes);
}

#[test]
fn stale_formal_cas_and_unapproved_decoded_requests_never_append() {
    let scratch = Scratch::new();
    let (mut memory, source) = fixture(scratch.path());
    let command = memory
        .prepare_formal_publication(&namespace(), formal("v1", &source), 100)
        .unwrap();
    let decoded = MemoryCommand::from_bytes(&command.to_bytes().unwrap()).unwrap();
    let bytes = fs::metadata(scratch.path()).unwrap().len();
    assert_err!(
        memory.commit_publication(operation("decoded-fresh"), decoded),
        Err(PersistenceError::Memory(MemoryError::PublicationDenied))
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), bytes);
    let id = memory
        .commit_publication(operation("v1"), command)
        .unwrap()
        .value()
        .id();
    let first = memory
        .prepare_formal_revision(&namespace(), id, 1, formal("v2", &source), 200)
        .unwrap();
    let second = memory
        .prepare_formal_revision(&namespace(), id, 1, formal("different", &source), 200)
        .unwrap();
    memory.commit_publication(operation("v2"), first).unwrap();
    let bytes = fs::metadata(scratch.path()).unwrap().len();
    assert_err!(
        memory.commit_publication(operation("stale"), second),
        Err(PersistenceError::Memory(MemoryError::TaskCancelled))
    );
    assert_err!(
        memory.prepare_formal_revision(&namespace(), id, 1, formal("stale", &source), 200),
        Err(PersistenceError::Memory(MemoryError::RevisionConflict {
            expected: 1,
            actual: 2
        }))
    );
    assert_eq!(fs::metadata(scratch.path()).unwrap().len(), bytes);
}
