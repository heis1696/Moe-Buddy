use std::io;
use std::sync::{Arc, Mutex};

use super::*;
use moe_memory_core::{Category, Namespace};

#[derive(Clone, Copy)]
enum PublicationRoute {
    Shared,
    FormalCreate,
    FormalRevision,
}

fn publication_fixture() -> (LocalMemory, Arc<Mutex<State>>, PetIdentity, SourceRef) {
    let (mut memory, state, pet, _) = fixture();
    let source = memory
        .register_source(
            OperationId::new("shared-source").unwrap(),
            MemoryScope::Shared(pet.namespace().clone()),
            "reference",
            1,
            Epistemic::Reference,
        )
        .unwrap()
        .value()
        .clone();
    memory
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    memory
        .set_publication_policy(
            &pet,
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    memory
        .set_formal_publication_policy(
            pet.namespace(),
            PublicationPolicy::allow_sources(1, vec![source.clone()]),
        )
        .unwrap();
    (memory, state, pet, source)
}

fn shared_candidate(
    memory: &mut LocalMemory,
    pet: &PetIdentity,
    source: &SourceRef,
) -> SharedCandidate {
    memory
        .for_session(
            pet,
            &SessionId::new("current").unwrap(),
            ReadPurpose::OfflineReasoning,
        )
        .begin_task(&RetrievalQuery::new("no-match"), 100)
        .unwrap()
        .propose_shared(
            MemoryDraft::new(
                "shared candidate",
                Category::Knowledge,
                Epistemic::Reference,
                vec![source.clone()],
            ),
            100,
        )
        .unwrap()
}

fn formal_draft(source: &SourceRef, text: &str) -> MemoryDraft {
    MemoryDraft::new(
        text,
        Category::Protocol,
        Epistemic::Reference,
        vec![source.clone()],
    )
}

fn publication_command(
    memory: &mut LocalMemory,
    pet: &PetIdentity,
    source: &SourceRef,
    route: PublicationRoute,
) -> MemoryCommand {
    match route {
        PublicationRoute::Shared => {
            let candidate = shared_candidate(memory, pet, source);
            memory.prepare_shared_publication(&candidate, 100).unwrap()
        }
        PublicationRoute::FormalCreate => memory
            .prepare_formal_publication(pet.namespace(), formal_draft(source, "formal v1"), 100)
            .unwrap(),
        PublicationRoute::FormalRevision => {
            let first = memory
                .prepare_formal_publication(pet.namespace(), formal_draft(source, "formal v1"), 100)
                .unwrap();
            let id = memory
                .commit_publication(OperationId::new("install").unwrap(), first)
                .unwrap()
                .value()
                .id();
            memory
                .prepare_formal_revision(
                    pet.namespace(),
                    id,
                    1,
                    formal_draft(source, "formal v2"),
                    200,
                )
                .unwrap()
        }
    }
}

#[test]
fn publication_before_write_failure_never_publishes_and_still_requires_reopen() {
    for route in [
        PublicationRoute::Shared,
        PublicationRoute::FormalCreate,
        PublicationRoute::FormalRevision,
    ] {
        let (mut memory, state, pet, source) = publication_fixture();
        let command = publication_command(&mut memory, &pet, &source, route);
        let generation = memory.service.generation();
        let before = state.lock().unwrap().bytes.clone();
        state.lock().unwrap().fault = Fault::BeforeWrite;
        assert!(matches!(
            memory.commit_publication(OperationId::new("publish").unwrap(), command.clone()),
            Err(PersistenceError::Io(_))
        ));
        assert_eq!(memory.service.generation(), generation);
        assert_eq!(state.lock().unwrap().bytes, before);
        assert!(matches!(
            memory.generation(),
            Err(PersistenceError::RecoveryRequired)
        ));
        drop(memory);
        state.lock().unwrap().fault = Fault::None;
        let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state))).unwrap();
        assert_eq!(recovered.service.generation(), generation);
        assert!(matches!(
            recovered.commit_publication(OperationId::new("publish").unwrap(), command),
            Err(PersistenceError::Memory(MemoryError::PublicationDenied))
        ));
    }
}

#[test]
fn publication_partial_frame_is_preserved_and_refused_for_every_route() {
    for route in [
        PublicationRoute::Shared,
        PublicationRoute::FormalCreate,
        PublicationRoute::FormalRevision,
    ] {
        let (mut memory, state, pet, source) = publication_fixture();
        let command = publication_command(&mut memory, &pet, &source, route);
        let generation = memory.service.generation();
        state.lock().unwrap().fault = Fault::PartialWrite;
        assert!(matches!(
            memory.commit_publication(OperationId::new("publish").unwrap(), command),
            Err(PersistenceError::Io(_))
        ));
        assert_eq!(memory.service.generation(), generation);
        let damaged = state.lock().unwrap().bytes.clone();
        drop(memory);
        state.lock().unwrap().fault = Fault::None;
        assert!(matches!(
            LocalMemory::recover(Box::new(FaultJournal(state.clone()))),
            Err(PersistenceError::Corrupt { .. })
        ));
        assert_eq!(state.lock().unwrap().bytes, damaged);
    }
}

#[test]
fn publication_sync_uncertainty_blocks_all_m4_methods_then_recovers_receipt_without_grants() {
    for route in [
        PublicationRoute::Shared,
        PublicationRoute::FormalCreate,
        PublicationRoute::FormalRevision,
    ] {
        let (mut memory, state, pet, source) = publication_fixture();
        let existing = memory
            .for_pet(&pet)
            .create(
                OperationId::new("existing-private").unwrap(),
                MemoryDraft::new(
                    "private",
                    Category::Knowledge,
                    Epistemic::Reference,
                    vec![source.clone()],
                ),
                100,
            )
            .unwrap()
            .value()
            .id();
        let command = publication_command(&mut memory, &pet, &source, route);
        let proposal = shared_candidate(&mut memory, &pet, &source);
        let generation = memory.service.generation();
        state.lock().unwrap().fault = Fault::Sync;
        let operation = OperationId::new("publish").unwrap();
        assert!(matches!(
            memory.commit_publication(operation.clone(), command.clone()),
            Err(PersistenceError::Io(_))
        ));
        assert_eq!(memory.service.generation(), generation);
        macro_rules! blocked {
            ($expression:expr) => {
                assert!(matches!(
                    $expression,
                    Err(PersistenceError::RecoveryRequired)
                ));
            };
        }
        blocked!(memory.commit_publication(operation.clone(), command.clone()));
        blocked!(memory.prepare_shared_publication(&proposal, 100));
        blocked!(memory.prepare_automatic_publication(&proposal));
        blocked!(memory.prepare_formal_publication(
            pet.namespace(),
            formal_draft(&source, "new"),
            200
        ));
        blocked!(memory.prepare_formal_revision(
            pet.namespace(),
            existing,
            1,
            formal_draft(&source, "new"),
            200
        ));
        blocked!(memory.set_publication_policy(&pet, PublicationPolicy::deny_all(2)));
        blocked!(
            memory.set_formal_publication_policy(pet.namespace(), PublicationPolicy::deny_all(2))
        );
        blocked!(
            memory
                .for_session(
                    &pet,
                    &SessionId::new("current").unwrap(),
                    ReadPurpose::OfflineReasoning
                )
                .retrieve(
                    &RetrievalQuery::new("formal")
                        .with_layer(moe_memory_core::RetrievalLayer::FormalMaterial),
                    200
                )
        );
        drop(memory);
        state.lock().unwrap().fault = Fault::None;
        let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
        assert_eq!(recovered.generation().unwrap(), generation + 1);
        let appends = state.lock().unwrap().appends;
        let decoded = MemoryCommand::from_bytes(&command.to_bytes().unwrap()).unwrap();
        let receipt = recovered
            .commit_publication(operation.clone(), decoded.clone())
            .unwrap();
        assert_eq!(state.lock().unwrap().appends, appends);
        assert_eq!(
            receipt,
            recovered.commit_publication(operation, command).unwrap()
        );
        assert!(matches!(
            recovered.commit_publication(OperationId::new("new-operation").unwrap(), decoded),
            Err(PersistenceError::Memory(MemoryError::PublicationDenied))
        ));
        assert!(matches!(
            recovered
                .for_session(
                    &pet,
                    &SessionId::new("current").unwrap(),
                    ReadPurpose::OfflineReasoning
                )
                .retrieve(&RetrievalQuery::new(""), 200),
            Err(PersistenceError::Memory(MemoryError::AccessDenied))
        ));
        assert!(matches!(
            recovered.prepare_formal_publication(
                pet.namespace(),
                formal_draft(&source, "new"),
                200
            ),
            Err(PersistenceError::Memory(MemoryError::PublicationDenied))
        ));
        let record = recovered
            .for_pet(&pet)
            .get(receipt.value().id(), 200)
            .unwrap();
        assert_eq!(
            record.is_protected(),
            !matches!(route, PublicationRoute::Shared)
        );
        assert_eq!(
            record.revision(),
            if matches!(route, PublicationRoute::FormalRevision) {
                2
            } else {
                1
            }
        );
    }
}

#[test]
fn complete_uncertain_publication_requires_a_successful_recovery_sync() {
    let (mut memory, state, pet, source) = publication_fixture();
    let command = publication_command(&mut memory, &pet, &source, PublicationRoute::Shared);
    state.lock().unwrap().fault = Fault::Sync;
    assert!(matches!(
        memory.commit_publication(OperationId::new("publish").unwrap(), command),
        Err(PersistenceError::Io(_))
    ));
    drop(memory);
    state.lock().unwrap().fault = Fault::None;
    state.lock().unwrap().sync_fails = true;
    let bytes = state.lock().unwrap().bytes.clone();
    assert!(matches!(
        LocalMemory::recover(Box::new(FaultJournal(state.clone()))),
        Err(PersistenceError::Io(_))
    ));
    assert_eq!(state.lock().unwrap().bytes, bytes);
}

#[derive(Clone, Copy)]
enum Fault {
    None,
    BeforeWrite,
    PartialWrite,
    Sync,
}

#[derive(Clone, Copy)]
enum DialogueRoute {
    Private,
    Shared,
    Correction,
}

fn failed_dialogue(
    route: DialogueRoute,
    fault: Fault,
) -> (
    LocalMemory,
    Arc<Mutex<State>>,
    crate::DialogueError,
    PetIdentity,
) {
    use crate::{ContextPreference, CorrectionDecision, HostDecision, OfflineFixture};
    use moe_memory_core::{ReadPurpose, RecordVersion, RetrievalQuery, SessionId};
    let (mut memory, state, pet, shared) = publication_fixture();
    let private = memory
        .register_source(
            OperationId::new("original-source").unwrap(),
            MemoryScope::Private(pet.clone()),
            "original",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    memory
        .set_read_policy(
            &pet,
            ReadPolicy::offline(2, vec![private.clone(), shared.clone()]),
        )
        .unwrap();
    let original = || {
        MemoryDraft::new(
            "original",
            Category::UserProfile,
            Epistemic::UserStatement,
            vec![private.clone()],
        )
    };
    let target = memory
        .for_pet(&pet)
        .create(OperationId::new("original-record").unwrap(), original(), 90)
        .unwrap()
        .value()
        .id();
    let session = SessionId::new("current").unwrap();
    let mut dialogue = memory
        .offline_dialogue(&pet, &session, ReadPurpose::OfflineReasoning)
        .unwrap();
    let captured = dialogue
        .begin_turn(
            &RetrievalQuery::new("empty-public-task"),
            ContextPreference::Entries,
            100,
        )
        .unwrap();
    let candidate = match route {
        DialogueRoute::Shared => MemoryDraft::new(
            "shared sample",
            Category::Knowledge,
            Epistemic::Reference,
            vec![shared],
        ),
        _ => original(),
    };
    let proposed = dialogue
        .generate(captured, OfflineFixture::Candidate(candidate), 100)
        .unwrap();
    state.lock().unwrap().fault = fault;
    let operation = OperationId::new("dialogue-write").unwrap();
    let error = match route {
        DialogueRoute::Private => dialogue.review_and_commit(&proposed, operation, |_| {
            HostDecision::CreatePrivate { now_ms: 110 }
        }),
        DialogueRoute::Shared => dialogue.review_and_commit(&proposed, operation, |_| {
            HostDecision::PublishShared { now_ms: 110 }
        }),
        DialogueRoute::Correction => dialogue.correct_original(
            operation,
            RecordVersion::new(target, 1).unwrap(),
            original(),
            100,
            |_| CorrectionDecision::Commit { now_ms: 110 },
        ),
    }
    .unwrap_err();
    (memory, state, error, pet)
}

#[test]
fn dialogue_write_failures_block_binding_generation_review_correction_and_confirmation() {
    use crate::{
        ContextPreference, CorrectionDecision, DialogueFailure, HostDecision, OfflineFixture,
    };
    use moe_memory_core::{ReadPurpose, RecordVersion, RetrievalQuery, SessionId};
    for route in [
        DialogueRoute::Private,
        DialogueRoute::Shared,
        DialogueRoute::Correction,
    ] {
        let (mut memory, state, error, pet) = failed_dialogue(route, Fault::BeforeWrite);
        assert_eq!(error.failure(), &DialogueFailure::CommitIndeterminate);
        assert_eq!(
            memory
                .confirm_dialogue(error.request().unwrap())
                .unwrap_err()
                .failure(),
            &DialogueFailure::RecoveryRequired
        );
        let session = SessionId::new("current").unwrap();
        assert_eq!(
            memory
                .offline_dialogue(&pet, &session, ReadPurpose::OfflineReasoning)
                .err()
                .unwrap()
                .failure(),
            &DialogueFailure::RecoveryRequired
        );
        drop(memory);
        let mut memory = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
        memory
            .set_read_policy(&pet, ReadPolicy::offline(1, vec![]))
            .unwrap();
        let original_source = memory
            .service
            .for_pet(&pet)
            .search(&moe_memory_core::SearchQuery::new("original"), 120)
            .unwrap()[0]
            .sources()[0]
            .clone();
        memory
            .set_read_policy(&pet, ReadPolicy::offline(2, vec![original_source]))
            .unwrap();
        let mut bound = memory
            .offline_dialogue(&pet, &session, ReadPurpose::OfflineReasoning)
            .unwrap();
        let captured = bound
            .begin_turn(&RetrievalQuery::new(""), ContextPreference::Entries, 120)
            .unwrap();
        let draft = captured
            .context()
            .items()
            .iter()
            .find_map(|item| match item {
                moe_memory_core::RetrievalItem::LongTerm(record) => Some((
                    record.version_ref(),
                    MemoryDraft::new(
                        "original",
                        Category::UserProfile,
                        Epistemic::UserStatement,
                        record.sources().to_vec(),
                    ),
                )),
                _ => None,
            })
            .unwrap();
        let second = bound
            .begin_turn(&RetrievalQuery::new(""), ContextPreference::Entries, 120)
            .unwrap();
        let proposed = bound
            .generate(second, OfflineFixture::Candidate(draft.1.clone()), 120)
            .unwrap();
        assert_eq!(
            bound
                .review_and_commit(
                    &proposed,
                    OperationId::new("trigger-again").unwrap(),
                    |_| HostDecision::CreatePrivate { now_ms: 120 }
                )
                .unwrap_err()
                .failure(),
            &DialogueFailure::CommitIndeterminate
        );
        assert_eq!(
            bound
                .begin_turn(&RetrievalQuery::new(""), ContextPreference::Entries, 120)
                .unwrap_err()
                .failure(),
            &DialogueFailure::RecoveryRequired
        );
        assert_eq!(
            bound
                .generate(captured, OfflineFixture::Unavailable, 120)
                .unwrap_err()
                .failure(),
            &DialogueFailure::RecoveryRequired
        );
        assert_eq!(
            bound
                .review_and_commit(&proposed, OperationId::new("blocked").unwrap(), |_| {
                    HostDecision::CreatePrivate { now_ms: 120 }
                })
                .unwrap_err()
                .failure(),
            &DialogueFailure::RecoveryRequired
        );
        assert_eq!(
            bound
                .correct_original(
                    OperationId::new("blocked-correction").unwrap(),
                    RecordVersion::new(draft.0.id(), 1).unwrap(),
                    draft.1,
                    120,
                    |_| CorrectionDecision::Commit { now_ms: 120 }
                )
                .unwrap_err()
                .failure(),
            &DialogueFailure::RecoveryRequired
        );
        let bytes = state.lock().unwrap().bytes.clone();
        drop(memory);
        state.lock().unwrap().fault = Fault::None;
        let reopened = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
        assert!(matches!(
            reopened
                .confirm_dialogue(error.request().unwrap())
                .unwrap_err()
                .failure(),
            DialogueFailure::Memory(MemoryError::NotFound)
        ));
        assert_eq!(state.lock().unwrap().bytes, bytes);
    }
}

#[test]
fn dialogue_uncertain_complete_frames_confirm_only_after_recovery_sync_and_never_restore_grants() {
    use crate::{ContextPreference, DialogueCommit, DialogueFailure};
    use moe_memory_core::{ReadPurpose, RetrievalQuery, SessionId};
    for route in [
        DialogueRoute::Private,
        DialogueRoute::Shared,
        DialogueRoute::Correction,
    ] {
        let (memory, state, error, pet) = failed_dialogue(route, Fault::Sync);
        let request = error.request().unwrap();
        let request = DialogueCommit::from_bytes(
            request.operation_id().clone(),
            &request.to_bytes().unwrap(),
        )
        .unwrap();
        assert_eq!(error.failure(), &DialogueFailure::CommitIndeterminate);
        let bytes = state.lock().unwrap().bytes.clone();
        drop(memory);
        state.lock().unwrap().sync_fails = true;
        assert!(matches!(
            LocalMemory::recover(Box::new(FaultJournal(state.clone()))),
            Err(PersistenceError::Io(_))
        ));
        state.lock().unwrap().sync_fails = false;
        let mut memory = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
        let generation = memory.generation().unwrap();
        let receipt = memory.confirm_dialogue(&request).unwrap();
        assert_eq!(receipt.committed().durability(), Durability::FileSynced);
        assert_eq!(memory.generation().unwrap(), generation);
        assert_eq!(state.lock().unwrap().bytes, bytes);
        let mut bound = memory
            .offline_dialogue(
                &pet,
                &SessionId::new("current").unwrap(),
                ReadPurpose::OfflineReasoning,
            )
            .unwrap();
        assert_eq!(
            bound
                .begin_turn(&RetrievalQuery::new(""), ContextPreference::Entries, 120)
                .unwrap_err()
                .failure(),
            &DialogueFailure::Memory(MemoryError::AccessDenied)
        );
    }
}

#[test]
fn dialogue_torn_frames_are_preserved_and_cannot_be_confirmed_or_auto_repaired() {
    for route in [
        DialogueRoute::Private,
        DialogueRoute::Shared,
        DialogueRoute::Correction,
    ] {
        let (memory, state, _, _) = failed_dialogue(route, Fault::PartialWrite);
        let bytes = state.lock().unwrap().bytes.clone();
        drop(memory);
        assert!(matches!(
            LocalMemory::recover(Box::new(FaultJournal(state.clone()))),
            Err(PersistenceError::Corrupt { .. })
        ));
        assert_eq!(state.lock().unwrap().bytes, bytes);
    }
}

struct State {
    bytes: Vec<u8>,
    fault: Fault,
    sync_fails: bool,
    appends: usize,
}

struct FaultJournal(Arc<Mutex<State>>);

impl Journal for FaultJournal {
    fn read_all(&mut self) -> Result<Vec<u8>, PersistenceError> {
        Ok(self.0.lock().unwrap().bytes.clone())
    }

    fn append_synced(&mut self, frame: &[u8]) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.appends += 1;
        match state.fault {
            Fault::None => {
                state.bytes.extend_from_slice(frame);
                Ok(())
            }
            Fault::BeforeWrite => Err(io::Error::other("disk write failed")),
            Fault::PartialWrite => {
                state.bytes.extend_from_slice(&frame[..frame.len() / 2]);
                Err(io::Error::other("disk full after partial write"))
            }
            Fault::Sync => {
                state.bytes.extend_from_slice(frame);
                Err(io::Error::other("sync failed after write"))
            }
        }
    }

    fn sync(&mut self) -> io::Result<()> {
        if self.0.lock().unwrap().sync_fails {
            Err(io::Error::other("recovery sync failed"))
        } else {
            Ok(())
        }
    }
}

fn fixture() -> (LocalMemory, Arc<Mutex<State>>, PetIdentity, SourceRef) {
    let state = Arc::new(Mutex::new(State {
        bytes: HEADER.to_vec(),
        fault: Fault::None,
        sync_fails: false,
        appends: 0,
    }));
    let mut memory = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
    let pet = PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap();
    let source = memory
        .register_source(
            OperationId::new("source").unwrap(),
            MemoryScope::Private(pet.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    (memory, state, pet, source)
}

fn draft(source: &SourceRef) -> MemoryDraft {
    MemoryDraft::new(
        "candidate-secret",
        Category::UserProfile,
        Epistemic::UserStatement,
        vec![source.clone()],
    )
}

#[test]
fn uncertain_write_blocks_every_context_entry_point() {
    use moe_memory_core::{ReadPolicy, ReadPurpose, RetrievalQuery, SessionId};
    let (mut memory, state, pet, source) = fixture();
    let id = memory
        .for_pet(&pet)
        .create(OperationId::new("create").unwrap(), draft(&source), 100)
        .unwrap()
        .value()
        .id();
    memory
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    let task = memory
        .for_session(
            &pet,
            &SessionId::new("current").unwrap(),
            ReadPurpose::OfflineReasoning,
        )
        .begin_task(&RetrievalQuery::new("candidate"), 100)
        .unwrap();
    let mut context = memory.assemble_context(&task, 100, 100).unwrap();
    let ticket = memory.begin_compaction(&context, 0..1, 100).unwrap();
    state.lock().unwrap().fault = Fault::Sync;
    assert!(matches!(
        memory.for_pet(&pet).update(
            OperationId::new("uncertain").unwrap(),
            id,
            1,
            draft(&source),
            110
        ),
        Err(PersistenceError::Io(_))
    ));
    let appends = state.lock().unwrap().appends;
    assert!(matches!(
        memory.assemble_context(&task, 100, 110),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        memory.context_view(&context, 110),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        memory.begin_compaction(&context, 0..1, 110),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        memory.apply_compaction(&mut context, &ticket, "late", 110),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert_eq!(state.lock().unwrap().appends, appends);
}

#[test]
fn uncertain_archive_blocks_all_m3_entry_points_and_recovers_without_grants() {
    use moe_memory_core::{ReadPolicy, ReadPurpose, RecordState, RetrievalQuery, SessionId};
    let (mut memory, state, pet, source) = fixture();
    let session = SessionId::new("current").unwrap();
    let id = memory
        .for_pet(&pet)
        .create(OperationId::new("create").unwrap(), draft(&source), 100)
        .unwrap()
        .value()
        .id();
    memory
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    let task = memory
        .for_session(&pet, &session, ReadPurpose::OfflineReasoning)
        .begin_task(&RetrievalQuery::new("candidate"), 100)
        .unwrap();
    let operation = OperationId::new("archive").unwrap();
    state.lock().unwrap().fault = Fault::Sync;
    assert!(matches!(
        memory.for_pet(&pet).archive(operation.clone(), id, 1, 200),
        Err(PersistenceError::Io(_))
    ));
    assert_eq!(
        memory
            .service
            .for_pet(&pet)
            .status(id, 200)
            .unwrap()
            .state(),
        RecordState::Active
    );
    macro_rules! blocked {
        ($expression:expr) => {
            assert!(matches!(
                $expression,
                Err(PersistenceError::RecoveryRequired)
            ));
        };
    }
    blocked!(memory.generation());
    blocked!(memory.deletion_boundary());
    blocked!(memory.cancel_tasks(OperationId::new("cancel").unwrap(), task.generation()));
    blocked!(memory.revoke_source(OperationId::new("revoke").unwrap(), &source));
    blocked!(memory.restore_source(OperationId::new("restore-source").unwrap(), &source, 2));
    blocked!(memory.revoke_access(&pet, 2));
    blocked!(memory.remove_identity_view(&pet, 1));
    blocked!(memory.remove_working_context(&pet, &session, "context", 1, 200));
    blocked!(memory.validate_task(&task, 200));
    blocked!(memory.create_from_task(
        OperationId::new("task").unwrap(),
        &task,
        draft(&source),
        200
    ));
    blocked!(memory.for_pet(&pet).status(id, 200));
    blocked!(memory.for_pet(&pet).restore_archive(
        OperationId::new("restore").unwrap(),
        id,
        2,
        300
    ));
    blocked!(memory.for_pet(&pet).replace(
        OperationId::new("replace").unwrap(),
        id,
        1,
        draft(&source),
        200
    ));
    blocked!(
        memory
            .for_session(&pet, &session, ReadPurpose::OfflineReasoning)
            .begin_task(&RetrievalQuery::new("candidate"), 200)
    );
    drop(memory);
    state.lock().unwrap().fault = Fault::None;
    let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
    assert_eq!(
        recovered.for_pet(&pet).status(id, 200).unwrap().state(),
        RecordState::Archived
    );
    let appends = state.lock().unwrap().appends;
    recovered
        .for_pet(&pet)
        .archive(operation, id, 1, 200)
        .unwrap();
    assert_eq!(state.lock().unwrap().appends, appends);
    assert!(matches!(
        recovered
            .for_session(&pet, &session, ReadPurpose::OfflineReasoning)
            .retrieve(&RetrievalQuery::new("candidate"), 200),
        Err(PersistenceError::Memory(MemoryError::AccessDenied))
    ));
}

#[test]
fn uncertain_replacement_publishes_neither_half_until_recovery_sync() {
    use moe_memory_core::RecordState;
    let (mut memory, state, pet, source) = fixture();
    let id = memory
        .for_pet(&pet)
        .create(OperationId::new("create").unwrap(), draft(&source), 100)
        .unwrap()
        .value()
        .id();
    state.lock().unwrap().fault = Fault::Sync;
    let operation = OperationId::new("replace").unwrap();
    assert!(
        memory
            .for_pet(&pet)
            .replace(operation.clone(), id, 1, draft(&source), 200)
            .is_err()
    );
    assert_eq!(
        memory
            .service
            .for_pet(&pet)
            .status(id, 200)
            .unwrap()
            .state(),
        RecordState::Active
    );
    assert_eq!(
        memory
            .service
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 200)
            .unwrap()
            .len(),
        1
    );
    drop(memory);
    state.lock().unwrap().fault = Fault::None;
    let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
    let receipt = recovered
        .for_pet(&pet)
        .replace(operation, id, 1, draft(&source), 200)
        .unwrap();
    assert_eq!(
        recovered.for_pet(&pet).status(id, 200).unwrap().state(),
        RecordState::Superseded(receipt.value().replacement().id())
    );
    assert_eq!(
        recovered
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 200)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn uncertain_revocation_is_replayed_without_restoring_stale_source_access() {
    use moe_memory_core::ReadPolicy;
    let (mut memory, state, pet, source) = fixture();
    let id = memory
        .for_pet(&pet)
        .create(OperationId::new("create").unwrap(), draft(&source), 100)
        .unwrap()
        .value()
        .id();
    state.lock().unwrap().fault = Fault::Sync;
    let operation = OperationId::new("revoke").unwrap();
    assert!(memory.revoke_source(operation.clone(), &source).is_err());
    assert!(memory.service.for_pet(&pet).get(id, 200).is_ok());
    drop(memory);
    state.lock().unwrap().fault = Fault::None;
    let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
    recovered.revoke_source(operation, &source).unwrap();
    assert!(recovered.for_pet(&pet).get(id, 200).is_err());
    assert!(
        recovered
            .set_read_policy(&pet, ReadPolicy::offline(1, vec![source]))
            .is_err()
    );
}

#[test]
fn cancellation_faults_preserve_publication_order_and_epoch_fences() {
    use moe_memory_core::{ReadPolicy, ReadPurpose, RetrievalQuery, SessionId};
    for fault in [Fault::BeforeWrite, Fault::Sync] {
        let (mut memory, state, pet, source) = fixture();
        let session = SessionId::new("current").unwrap();
        memory
            .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
            .unwrap();
        let task = memory
            .for_session(&pet, &session, ReadPurpose::OfflineReasoning)
            .begin_task(&RetrievalQuery::new(""), 100)
            .unwrap();
        let generation = memory.generation().unwrap();
        state.lock().unwrap().fault = fault;
        let operation = OperationId::new("cancel").unwrap();
        assert!(memory.cancel_tasks(operation.clone(), generation).is_err());
        assert_eq!(memory.service.generation(), generation);
        drop(memory);
        state.lock().unwrap().fault = Fault::None;
        let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
        assert_eq!(
            recovered.generation().unwrap(),
            generation + u64::from(matches!(fault, Fault::Sync))
        );
        recovered
            .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
            .unwrap();
        assert!(matches!(
            recovered.create_from_task(
                OperationId::new("late").unwrap(),
                &task,
                draft(&source),
                200
            ),
            Err(PersistenceError::Memory(MemoryError::TaskCancelled))
        ));
        assert_eq!(
            *recovered
                .cancel_tasks(operation, generation)
                .unwrap()
                .value(),
            generation + 1
        );
    }
}

#[test]
fn torn_management_frames_are_preserved_and_never_auto_repaired() {
    let (mut memory, state, _, source) = fixture();
    state.lock().unwrap().fault = Fault::PartialWrite;
    assert!(
        memory
            .revoke_source(OperationId::new("revoke").unwrap(), &source)
            .is_err()
    );
    drop(memory);
    let bytes = state.lock().unwrap().bytes.clone();
    assert!(matches!(
        LocalMemory::recover(Box::new(FaultJournal(state.clone()))),
        Err(PersistenceError::Corrupt { .. })
    ));
    assert_eq!(state.lock().unwrap().bytes, bytes);
}

#[test]
fn previously_valid_v1_expiry_corrections_replay_without_granting_live_renewal() {
    let (mut memory, state, pet, source) = fixture();
    let id = memory
        .for_pet(&pet)
        .create(
            OperationId::new("create").unwrap(),
            draft(&source).with_expiration(200),
            100,
        )
        .unwrap()
        .value()
        .id();
    assert!(matches!(
        memory.for_pet(&pet).update(
            OperationId::new("live").unwrap(),
            id,
            1,
            draft(&source),
            300
        ),
        Err(PersistenceError::Memory(MemoryError::InvalidState))
    ));
    let payload = MemoryCommand::update(pet.clone(), id, 1, draft(&source), 300)
        .to_bytes()
        .unwrap();
    assert_eq!(payload[0], 1);
    let historical_frame = journal::frame(3, memory.last_checksum, "historical", &payload);
    drop(memory);
    state
        .lock()
        .unwrap()
        .bytes
        .extend_from_slice(&historical_frame);
    let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
    let receipt = recovered
        .for_pet(&pet)
        .update(
            OperationId::new("historical").unwrap(),
            id,
            1,
            draft(&source),
            300,
        )
        .unwrap();
    assert_eq!(receipt.value().revision(), 2);
    let session = SessionId::new("current").unwrap();
    assert!(matches!(
        recovered
            .for_session(&pet, &session, ReadPurpose::OfflineReasoning)
            .retrieve(&RetrievalQuery::new("candidate"), 300),
        Err(PersistenceError::Memory(MemoryError::AccessDenied))
    ));
}

#[test]
fn failed_journal_blocks_m2_views_and_volatile_configuration() {
    use moe_memory_core::{ReadPolicy, ReadPurpose, RetrievalQuery, SessionId};
    let (mut memory, state, pet, source) = fixture();
    let session = SessionId::new("current").unwrap();
    memory
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    let query = RetrievalQuery::new("candidate");
    memory
        .for_session(&pet, &session, ReadPurpose::OfflineReasoning)
        .entity(&query, 100)
        .unwrap();
    state.lock().unwrap().fault = Fault::Sync;
    assert!(
        memory
            .for_pet(&pet)
            .create(OperationId::new("fail").unwrap(), draft(&source), 100)
            .is_err()
    );
    let mut reader = memory.for_session(&pet, &session, ReadPurpose::OfflineReasoning);
    assert!(matches!(
        reader.retrieve(&query, 100),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        reader.entity(&query, 100),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        reader.identity(),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        memory.set_read_policy(&pet, ReadPolicy::deny_all(2)),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        memory.set_entity_cache_capacity(0),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        memory.set_identity_view(&pet, 1, "anchor", vec![source.clone()]),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        memory.put_working_context(&pet, &session, "context", 1, draft(&source), 100),
        Err(PersistenceError::RecoveryRequired)
    ));
}

#[test]
fn valid_v1_journal_with_more_than_ten_thousand_deletions_still_recovers() {
    fn append(
        bytes: &mut Vec<u8>,
        previous: &mut u64,
        sequence: u64,
        operation: &str,
        command: &MemoryCommand,
    ) {
        let payload = command.to_bytes().unwrap();
        assert_eq!(payload[0], 1);
        let frame = journal::frame(sequence, *previous, operation, &payload);
        *previous = journal::checksum(&frame[..frame.len() - 8]);
        bytes.extend_from_slice(&frame);
    }
    let pet = PetIdentity::new(Namespace::new("home", "user").unwrap(), "alice").unwrap();
    let mut service = MemoryService::new();
    let mut bytes = HEADER.to_vec();
    let mut previous = 0;
    let command = MemoryCommand::register_source(
        MemoryScope::Private(pet.clone()),
        "message",
        1,
        Epistemic::UserStatement,
    );
    let CommandResult::Source(source) = command.execute(&mut service).unwrap() else {
        panic!()
    };
    append(&mut bytes, &mut previous, 1, "source", &command);
    let mut last_id = None;
    for counter in 0..10_001 {
        let command = MemoryCommand::create(pet.clone(), draft(&source), 100);
        let CommandResult::Write(created) = command.execute(&mut service).unwrap() else {
            panic!()
        };
        append(
            &mut bytes,
            &mut previous,
            counter * 2 + 2,
            &format!("create-{counter}"),
            &command,
        );
        let command = MemoryCommand::delete(pet.clone(), created.id(), 1, 200);
        command.execute(&mut service).unwrap();
        append(
            &mut bytes,
            &mut previous,
            counter * 2 + 3,
            &format!("delete-{counter}"),
            &command,
        );
        last_id = Some(created.id());
    }
    let state = Arc::new(Mutex::new(State {
        bytes,
        fault: Fault::None,
        sync_fails: false,
        appends: 0,
    }));
    let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state))).unwrap();
    assert_eq!(recovered.operations.len(), 20_003);
    assert_eq!(recovered.generation().unwrap(), 20_003);
    assert_eq!(
        recovered
            .for_pet(&pet)
            .status(last_id.unwrap(), 200)
            .unwrap()
            .state(),
        moe_memory_core::RecordState::Deleted
    );
}

#[test]
fn indeterminate_frame_recovery_requires_fresh_m2_grants_before_confirmation_reads() {
    use moe_memory_core::{ReadPolicy, ReadPurpose, RetrievalQuery, SessionId};
    let (mut memory, state, pet, source) = fixture();
    let session = SessionId::new("current").unwrap();
    memory
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![source.clone()]))
        .unwrap();
    let operation = OperationId::new("uncertain").unwrap();
    state.lock().unwrap().fault = Fault::Sync;
    assert!(
        memory
            .for_pet(&pet)
            .create(operation.clone(), draft(&source), 100)
            .is_err()
    );
    drop(memory);
    state.lock().unwrap().fault = Fault::None;
    let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
    let query = RetrievalQuery::new("candidate");
    assert!(matches!(
        recovered
            .for_session(&pet, &session, ReadPurpose::OfflineReasoning)
            .retrieve(&query, 100),
        Err(PersistenceError::Memory(MemoryError::AccessDenied))
    ));
    let receipt = recovered
        .for_pet(&pet)
        .create(operation, draft(&source), 100)
        .unwrap();
    assert_eq!(receipt.sequence(), 2);
    recovered
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![source]))
        .unwrap();
    assert_eq!(
        recovered
            .for_session(&pet, &session, ReadPurpose::OfflineReasoning)
            .retrieve(&query, 100)
            .unwrap()
            .items()
            .len(),
        1
    );
}

fn check_failure(fault: Fault) {
    let (mut memory, state, pet, source) = fixture();
    state.lock().unwrap().fault = fault;
    assert!(matches!(
        memory
            .for_pet(&pet)
            .create(OperationId::new("create").unwrap(), draft(&source), 100),
        Err(PersistenceError::Io(_))
    ));
    assert!(
        memory
            .service
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 100)
            .unwrap()
            .is_empty()
    );
    assert_eq!(memory.operations.len(), 1);
    assert!(matches!(
        memory.for_pet(&pet).search(&SearchQuery::new(""), 100),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert!(matches!(
        memory
            .for_pet(&pet)
            .create(OperationId::new("create").unwrap(), draft(&source), 100),
        Err(PersistenceError::RecoveryRequired)
    ));
    assert_eq!(state.lock().unwrap().appends, 2);
}

#[test]
fn disk_write_failure_never_publishes_state_or_success() {
    check_failure(Fault::BeforeWrite);
}

#[test]
fn partial_disk_write_never_publishes_state_or_success() {
    check_failure(Fault::PartialWrite);
}

#[test]
fn sync_failure_never_publishes_state_or_success() {
    check_failure(Fault::Sync);
}

#[test]
fn complete_indeterminate_commit_is_resynced_and_idempotent_on_recovery() {
    let (mut memory, state, pet, source) = fixture();
    state.lock().unwrap().fault = Fault::Sync;
    let operation_id = OperationId::new("create").unwrap();
    assert!(
        memory
            .for_pet(&pet)
            .create(operation_id.clone(), draft(&source), 100)
            .is_err()
    );
    drop(memory);
    state.lock().unwrap().fault = Fault::None;
    let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state.clone()))).unwrap();
    let receipt = recovered
        .for_pet(&pet)
        .create(operation_id, draft(&source), 100)
        .unwrap();
    assert_eq!(receipt.sequence(), 2);
    assert_eq!(state.lock().unwrap().appends, 2);
    assert_eq!(
        recovered
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 100)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn torn_tail_is_rejected_not_truncated_or_replaced_by_empty() {
    let (mut memory, state, pet, source) = fixture();
    state.lock().unwrap().fault = Fault::PartialWrite;
    assert!(
        memory
            .for_pet(&pet)
            .create(OperationId::new("create").unwrap(), draft(&source), 100)
            .is_err()
    );
    drop(memory);
    let before = state.lock().unwrap().bytes.clone();
    assert!(matches!(
        LocalMemory::recover(Box::new(FaultJournal(state.clone()))),
        Err(PersistenceError::Corrupt { .. })
    ));
    assert_eq!(state.lock().unwrap().bytes, before);
}

#[test]
fn recovery_sync_failure_does_not_produce_a_usable_service() {
    let (memory, state, _, _) = fixture();
    drop(memory);
    state.lock().unwrap().sync_fails = true;
    assert!(matches!(
        LocalMemory::recover(Box::new(FaultJournal(state))),
        Err(PersistenceError::Io(_))
    ));
}

#[test]
fn semantic_corruption_is_rejected_even_with_a_correct_checksum() {
    let (memory, state, pet, source) = fixture();
    let previous = memory.last_checksum;
    drop(memory);
    let command = MemoryCommand::create(pet, draft(&source), 100)
        .to_bytes()
        .unwrap();
    for (sequence, previous, op, payload) in [
        (3, previous, "new", command.clone()),
        (2, 0, "new", command.clone()),
        (2, previous, "source", command.clone()),
        (2, previous, "new", vec![255]),
    ] {
        let mut bytes = state.lock().unwrap().bytes.clone();
        bytes.extend_from_slice(&journal::frame(sequence, previous, op, &payload));
        let bad = Arc::new(Mutex::new(State {
            bytes,
            fault: Fault::None,
            sync_fails: false,
            appends: 0,
        }));
        assert!(matches!(
            LocalMemory::recover(Box::new(FaultJournal(bad))),
            Err(PersistenceError::Corrupt { .. })
        ));
    }
}

#[test]
fn capacity_rejection_leaves_state_and_journal_unchanged() {
    let (mut memory, state, pet, source) = fixture();
    memory.length = MAX_JOURNAL_BYTES;
    assert!(matches!(
        memory
            .for_pet(&pet)
            .create(OperationId::new("create").unwrap(), draft(&source), 100),
        Err(PersistenceError::CapacityExceeded)
    ));
    assert!(!memory.failed);
    assert_eq!(state.lock().unwrap().appends, 1);
    assert!(
        memory
            .for_pet(&pet)
            .search(&SearchQuery::new(""), 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn checksummed_command_with_missing_evidence_is_still_corrupt() {
    let (memory, state, pet, source) = fixture();
    drop(memory);
    let payload = MemoryCommand::create(pet, draft(&source), 100)
        .to_bytes()
        .unwrap();
    state.lock().unwrap().bytes = HEADER.to_vec();
    state
        .lock()
        .unwrap()
        .bytes
        .extend_from_slice(&journal::frame(1, 0, "create", &payload));
    assert!(matches!(
        LocalMemory::recover(Box::new(FaultJournal(state))),
        Err(PersistenceError::Corrupt { .. })
    ));
}

#[test]
fn failure_before_writing_can_retry_after_reopen_without_consuming_an_id() {
    let (mut memory, state, pet, source) = fixture();
    state.lock().unwrap().fault = Fault::BeforeWrite;
    assert!(
        memory
            .for_pet(&pet)
            .create(OperationId::new("create").unwrap(), draft(&source), 100)
            .is_err()
    );
    drop(memory);
    state.lock().unwrap().fault = Fault::None;
    let mut recovered = LocalMemory::recover(Box::new(FaultJournal(state))).unwrap();
    let created = recovered
        .for_pet(&pet)
        .create(OperationId::new("create").unwrap(), draft(&source), 100)
        .unwrap();
    assert_eq!(created.sequence(), 2);
    assert_eq!(format!("{:?}", created.value().id()), "MemoryId(1)");
}

#[test]
fn failed_delete_does_not_publish_a_tombstone_or_a_success_receipt() {
    let (mut memory, state, pet, source) = fixture();
    let id = memory
        .for_pet(&pet)
        .create(OperationId::new("create").unwrap(), draft(&source), 100)
        .unwrap()
        .value()
        .id();
    state.lock().unwrap().fault = Fault::Sync;
    assert!(matches!(
        memory
            .for_pet(&pet)
            .delete(OperationId::new("delete").unwrap(), id, 1, 200),
        Err(PersistenceError::Io(_))
    ));
    assert_eq!(
        memory
            .service
            .for_pet(&pet)
            .get(id, 200)
            .unwrap()
            .revision(),
        1
    );
    assert_eq!(memory.operations.len(), 2);
    assert!(matches!(
        memory.for_pet(&pet).get(id, 200),
        Err(PersistenceError::RecoveryRequired)
    ));
}

#[test]
fn failed_source_change_does_not_publish_invalidation() {
    let (mut memory, state, pet, source) = fixture();
    let id = memory
        .for_pet(&pet)
        .create(OperationId::new("create").unwrap(), draft(&source), 100)
        .unwrap()
        .value()
        .id();
    state.lock().unwrap().fault = Fault::Sync;
    assert!(matches!(
        memory.register_source(
            OperationId::new("source-new").unwrap(),
            MemoryScope::Private(pet.clone()),
            "message",
            2,
            Epistemic::UserStatement
        ),
        Err(PersistenceError::Io(_))
    ));
    assert!(memory.service.for_pet(&pet).get(id, 100).is_ok());
    assert_eq!(memory.operations.len(), 2);
    assert!(matches!(
        memory.for_pet(&pet).get(id, 100),
        Err(PersistenceError::RecoveryRequired)
    ));
}
