use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryScope, Namespace, PetIdentity, PublicationPolicy,
    ReadPolicy, ReadPurpose, RecordVersion, RetrievalQuery, SearchScope, SessionId,
};
use moe_memory_local::{
    ContextPreference, CorrectionDecision, HostDecision, LocalMemory, OfflineFixture, OperationId,
};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("host clock")
        .as_millis()
        .try_into()
        .expect("host timestamp")
}

fn operation(name: &str) -> OperationId {
    OperationId::new(name).expect("static fixture ID")
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("provide a new journal path; existing files are never overwritten")?;
    let mut memory = LocalMemory::create(path)?;
    let namespace = Namespace::new("synthetic-workspace", "synthetic-user")?;
    let identity = PetIdentity::new(namespace.clone(), "synthetic-pet")?;
    let session = SessionId::new("synthetic-session")?;
    let private = memory
        .register_source(
            operation("private-source"),
            MemoryScope::Private(identity.clone()),
            "synthetic-message",
            1,
            Epistemic::UserStatement,
        )?
        .value()
        .clone();
    let public = memory
        .register_source(
            operation("public-source"),
            MemoryScope::Shared(namespace),
            "synthetic-reference",
            1,
            Epistemic::Reference,
        )?
        .value()
        .clone();
    memory.set_read_policy(
        &identity,
        ReadPolicy::offline(1, vec![private.clone(), public.clone()]),
    )?;
    memory.set_publication_policy(
        &identity,
        PublicationPolicy::allow_sources(1, vec![public.clone()]),
    )?;
    memory.set_entity_cache_capacity(0)?;
    let original = |text| {
        MemoryDraft::new(
            text,
            Category::UserProfile,
            Epistemic::UserStatement,
            vec![private.clone()],
        )
    };

    let mut dialogue =
        memory.offline_dialogue(&identity, &session, ReadPurpose::OfflineReasoning)?;
    let captured = dialogue.begin_turn(
        &RetrievalQuery::new("empty-fixture"),
        ContextPreference::PreferEntity,
        now_ms(),
    )?;
    println!(
        "offline fixture only; context report: {:?}",
        captured.report()
    );
    let proposed = dialogue.generate(
        captured,
        OfflineFixture::Candidate(original("synthetic preference: tea")),
        now_ms(),
    )?;
    let receipt = dialogue.review_and_commit(&proposed, operation("private-write"), |review| {
        if review.candidate().text() == "synthetic preference: tea"
            && review.context().items().is_empty()
        {
            HostDecision::CreatePrivate { now_ms: now_ms() }
        } else {
            HostDecision::Reject
        }
    })?;
    println!(
        "private fixture committed: sequence {}, {:?}",
        receipt.committed().sequence(),
        receipt.committed().durability()
    );
    let target = RecordVersion::new(receipt.committed().value().id(), 1)?;
    let correction = dialogue.correct_original(
        operation("original-correction"),
        target,
        original("synthetic preference: coffee"),
        now_ms(),
        |review| {
            if review.current().text() == "synthetic preference: tea"
                && review.correction().text() == "synthetic preference: coffee"
            {
                CorrectionDecision::Commit { now_ms: now_ms() }
            } else {
                CorrectionDecision::Reject
            }
        },
    )?;
    println!(
        "original-evidence correction committed: revision {}",
        correction.committed().value().revision()
    );
    let captured = dialogue.begin_turn(
        &RetrievalQuery::new("preference"),
        ContextPreference::PreferEntity,
        now_ms(),
    )?;
    println!("corrected context report: {:?}", captured.report());
    let unavailable = dialogue
        .generate(captured, OfflineFixture::Unavailable, now_ms())
        .unwrap_err();
    println!("explainable degradation: {unavailable}");

    let public_session = SessionId::new("independent-public-fixture")?;
    let mut dialogue =
        memory.offline_dialogue(&identity, &public_session, ReadPurpose::OfflineReasoning)?;
    let public_query = RetrievalQuery::new("public-fixture").with_scope(SearchScope::Shared);
    let captured = dialogue.begin_turn(&public_query, ContextPreference::Entries, now_ms())?;
    let draft = MemoryDraft::new(
        "synthetic public fact",
        Category::Knowledge,
        Epistemic::Reference,
        vec![public],
    );
    let proposed = dialogue.generate(captured, OfflineFixture::Candidate(draft), now_ms())?;
    let receipt = dialogue.review_and_commit(&proposed, operation("public-write"), |review| {
        if review.candidate().text() == "synthetic public fact"
            && review.context().items().is_empty()
            && review.identity_view().is_none()
        {
            HostDecision::PublishShared { now_ms: now_ms() }
        } else {
            HostDecision::Reject
        }
    })?;
    println!(
        "shared fixture committed: sequence {}, {:?}; no model transport or tool execution",
        receipt.committed().sequence(),
        receipt.committed().durability()
    );
    Ok(())
}
