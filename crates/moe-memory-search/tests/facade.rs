//! Public-API sealed hybrid retrieval through the existing offline Agent.

use std::collections::BTreeMap;
use std::path::PathBuf;

use moe_agent_runtime::{
    AgentAccess, AgentError, AgentLimits, AgentResult, AgentRuntime, AgentServices, AgentTask,
    CallerSubject, ChannelId, ChannelScope, IngressToken, ModelOutcome, OfflineInput, OfflineModel,
    PresentationEvent, PresetConfig, PresetId, PresetPurpose, PresetRegistry, RawIngressCall,
    Registration, ReplyTarget, SessionAccess, SessionLimits, SessionRuntime, SessionTiming,
    TerminalState, TrustedIngress, TrustedRequest, UserEvent, UserEventKind,
};
use moe_memory_core::{
    Category, EmbeddingProfile, Epistemic, MemoryContextAccess, MemoryDraft, MemoryError,
    MemoryScope, Namespace, PetIdentity, ReadPolicy, ReadPurpose, RerankProfile, RetrievalPlan,
    RetrievalQuery, SessionId, SourceRef, TransmissionPolicy, TransmissionUsage,
};
use moe_memory_local::{LocalMemory, OperationId};
use moe_memory_search::{
    CallControl, Cancellation, ChunkConfig, HybridConfig, LocalSearchHost, RankedCandidates,
    RerankMode, SearchError, SearchIndex, SearchMemory, SearchMode, StorageDomain, hybrid_rank,
    rerank_or_fallback,
};

const PERSONA: &str = "synthetic protected persona";
const FACT: &str = "synthetic authorized fact";
const QUERY_TEXT: &str = "";
const HIDDEN: &str = "synthetic withheld private record";
const ENDPOINT: &str = "https://example.invalid/offline-receipt";

struct Directory(PathBuf);

impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "moe-facade-integration-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn journal(&self) -> PathBuf {
        self.0.join("memory.journal")
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn identity() -> PetIdentity {
    PetIdentity::new(
        Namespace::new("facade-integration", "owner").unwrap(),
        "stable-pet",
    )
    .unwrap()
}

fn profile() -> EmbeddingProfile {
    EmbeddingProfile::new("fixture", "integration-model", 2).unwrap()
}

fn persistent_memory(directory: &Directory, persona: Option<&str>) -> LocalMemory {
    let mut local = LocalMemory::create(directory.journal()).unwrap();
    let owner = identity();
    let source = local
        .register_source(
            OperationId::new("register-source").unwrap(),
            MemoryScope::Private(owner.clone()),
            "synthetic",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    let authority = local
        .register_source(
            OperationId::new("register-authority").unwrap(),
            MemoryScope::Private(owner.clone()),
            "synthetic-persona",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap()
        .value()
        .clone();
    local
        .for_pet(&owner)
        .create(
            OperationId::new("create-fact").unwrap(),
            MemoryDraft::new(
                FACT,
                Category::Episode,
                Epistemic::UserStatement,
                vec![source.clone()],
            ),
            0,
        )
        .unwrap();
    if let Some(persona) = persona {
        local
            .set_identity_view(&owner, 1, persona, vec![authority.clone()])
            .unwrap();
    }
    local
        .set_read_policy(
            &owner,
            ReadPolicy::offline(1, vec![source.clone(), authority]),
        )
        .unwrap();
    local
}

fn attach_send_fixture(
    local: &mut LocalMemory,
    owner: &PetIdentity,
    sources: &[SourceRef],
) -> (SourceRef, SourceRef) {
    let mut added = Vec::new();
    for label in ["hidden", "query"] {
        added.push(
            local
                .register_source(
                    OperationId::new(format!("register-{label}")).unwrap(),
                    MemoryScope::Private(owner.clone()),
                    label,
                    1,
                    Epistemic::UserStatement,
                )
                .unwrap()
                .value()
                .clone(),
        );
    }
    let hidden = added[0].clone();
    let query = added[1].clone();
    local
        .for_pet(owner)
        .create(
            OperationId::new("create-hidden").unwrap(),
            MemoryDraft::new(
                HIDDEN,
                Category::Episode,
                Epistemic::UserStatement,
                vec![hidden.clone()],
            ),
            0,
        )
        .unwrap();
    let mut allowed = sources.to_vec();
    allowed.extend(added.clone());
    local
        .set_read_policy(owner, ReadPolicy::offline(2, allowed))
        .unwrap();
    local
        .set_transmission_policy(
            owner,
            TransmissionPolicy::new(1, TransmissionUsage::Rerank, ENDPOINT, added)
                .unwrap()
                .transmit_query()
                .with_scope(MemoryScope::Private(owner.clone())),
        )
        .unwrap();
    (hidden, query)
}

/// Sealed keyword-only hybrid ranking through the LocalMemory authority bridge.
async fn arm(
    local: &mut LocalMemory,
    owner: &PetIdentity,
    session: &SessionId,
    query: &RetrievalQuery,
    sent_sources: Option<(&SourceRef, &SourceRef)>,
) -> RankedCandidates {
    let mut plan = RetrievalPlan::new(profile());
    if sent_sources.is_some() {
        plan = plan.with_rerank(RerankProfile::new("fixture", "offline-receipt").unwrap());
    }
    let mut ticket = local
        .for_session(owner, session, ReadPurpose::OfflineReasoning)
        .begin_candidates(query, plan, 0)
        .unwrap();
    if let Some((hidden, query_source)) = sent_sources {
        local
            .bind_query_provenance(&mut ticket, vec![query_source.clone()], &[], 0)
            .unwrap();
        let hidden_ref = ticket
            .candidates()
            .iter()
            .find(|candidate| candidate.item().sources().contains(hidden))
            .unwrap()
            .candidate_ref();
        let sent = local
            .prepare_transmission(
                &ticket,
                TransmissionUsage::Rerank,
                ENDPOINT,
                &[hidden_ref],
                0,
            )
            .unwrap();
        local.record_transmission(&mut ticket, &sent, 0).unwrap();
    }
    let mut index = SearchIndex::new(
        StorageDomain::new("facade-integration-root").unwrap(),
        profile(),
        ChunkConfig::new(1, 512, 16, 1536).unwrap(),
        32,
        16,
        1024,
    )
    .unwrap();
    let config = HybridConfig::new(profile(), 1, 0.8, true, RerankMode::Disabled, 20).unwrap();
    let mut host = LocalSearchHost::new(local, 0);
    let ranking = hybrid_rank(&mut index, &mut host, &ticket, None, &config).unwrap();
    let control =
        CallControl::new(Cancellation::default(), std::time::Duration::from_secs(1)).unwrap();
    rerank_or_fallback(&index, &mut host, ticket, ranking, None, None, &control)
        .await
        .unwrap()
}

#[tokio::test]
async fn sealed_hybrid_ranking_enters_protected_context_once_and_refuses_mismatch() {
    let directory = Directory::new("hybrid-context");
    let mut local = persistent_memory(&directory, Some(PERSONA));
    let owner = identity();
    let session = SessionId::new("integration-session").unwrap();
    let ranked = arm(
        &mut local,
        &owner,
        &session,
        &RetrievalQuery::new(QUERY_TEXT),
        None,
    )
    .await;
    let query = RetrievalQuery::new(QUERY_TEXT);
    let mut facade = SearchMemory::with_ranked(local, ranked);

    let other_session = SessionId::new("other-session").unwrap();
    {
        let mut reader = facade.for_session(&owner, &other_session, ReadPurpose::OfflineReasoning);
        assert!(matches!(
            reader.begin_task(&query, 0),
            Err(SearchError::InvalidInput(
                "request does not match the prepared hybrid retrieval"
            ))
        ));
        assert_eq!(
            reader.retrieve(&query, 0),
            Err(SearchError::InvalidInput(
                "hybrid mode delivers only the sealed ranking"
            ))
        );
    }
    let mut reader = facade.for_session(&owner, &session, ReadPurpose::OfflineReasoning);
    let search_task = reader.begin_task(&query, 0).unwrap();
    let diagnostic = search_task.diagnostic().unwrap();
    assert_eq!(diagnostic.mode(), SearchMode::KeywordOnly);
    assert_eq!(diagnostic.status().candidates, 1);
    let diagnostic_text = format!("{diagnostic:?}");
    assert!(!diagnostic_text.contains(FACT));
    assert!(!diagnostic_text.contains(PERSONA));
    let task = search_task.into_task();
    assert_eq!(task.result().items()[0].text(), FACT);
    assert_eq!(task.identity_view().unwrap().text(), PERSONA);
    assert!(matches!(
        reader.begin_task(&query, 0),
        Err(SearchError::InvalidInput(
            "prepared hybrid retrieval was already consumed"
        ))
    ));
    assert!(matches!(
        reader.retrieve(&query, 0),
        Err(SearchError::InvalidInput(
            "hybrid mode delivers only the sealed ranking"
        ))
    ));
    drop(reader);
    let context = facade.assemble_context(&task, 4096, 0).unwrap();
    let view = facade.context_view(&context, 0).unwrap();
    assert_eq!(view.identity().unwrap().text(), PERSONA);
    assert!(view.blocks().iter().any(|block| block.text() == FACT));
}

struct EvidenceProbe {
    persona: String,
    fact: &'static str,
}

impl OfflineModel for EvidenceProbe {
    fn infer(&mut self, input: OfflineInput<'_>) -> ModelOutcome {
        assert_eq!(input.persona(), self.persona);
        assert_eq!(input.memory().len(), 1);
        assert!(input.memory().iter().all(|block| block.text() != HIDDEN));
        assert!(input.memory().iter().any(|block| block.text() == self.fact));
        ModelOutcome::Text {
            text: "synthetic offline reply".to_owned(),
            presentation: vec![PresentationEvent::Neutral],
        }
    }
}

async fn run_agent_with_sent_evidence(exclude: Option<&str>) {
    let owner = identity();
    let mut ingress = TrustedIngress::new();
    ingress
        .register_route("buddy/synthetic", owner.clone())
        .unwrap();
    ingress
        .install_credential(
            IngressToken::new("desktop-token").unwrap(),
            CallerSubject::new("synthetic-caller").unwrap(),
            ["buddy/synthetic".to_owned()],
            ChannelId::new("desktop").unwrap(),
            ChannelScope::new("private-thread").unwrap(),
            ReplyTarget::new("desktop-target").unwrap(),
        )
        .unwrap();
    let mut presets = PresetRegistry::new(4).unwrap();
    let preset = PresetId::new("synthetic-chat").unwrap();
    presets
        .create(
            preset.clone(),
            PresetConfig::new(
                "synthetic",
                "offline-fixture",
                "https://example.invalid/v1",
                None,
                "synthetic-model",
                BTreeMap::new(),
            )
            .unwrap(),
        )
        .unwrap();
    presets.bind(PresetPurpose::Chat, preset).unwrap();
    let request: TrustedRequest = ingress
        .admit(
            RawIngressCall::new(
                IngressToken::new("desktop-token").unwrap(),
                "buddy/synthetic",
                "synthetic-private-user-text",
            )
            .with_session_hint("same-client-hint"),
        )
        .unwrap();
    let mut sessions = SessionRuntime::new(
        &ingress,
        SessionLimits::new(16, 128, 512, 8192, 65536, 4).unwrap(),
        SessionTiming::default(),
        0,
    );
    let registration: Registration = sessions
        .register_user(
            SessionAccess::new(&ingress, &request, 0),
            UserEvent::new(UserEventKind::New),
        )
        .unwrap();
    let directory = Directory::new("agent-hybrid-direct");
    let mut memory = persistent_memory(&directory, Some(PERSONA));
    let source = memory
        .register_source(
            OperationId::new("register-source").unwrap(),
            MemoryScope::Private(owner.clone()),
            "synthetic",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    let authority = memory
        .register_source(
            OperationId::new("register-authority").unwrap(),
            MemoryScope::Private(owner.clone()),
            "synthetic-persona",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap()
        .value()
        .clone();
    let (hidden_source, query_source) =
        attach_send_fixture(&mut memory, &owner, &[source.clone(), authority.clone()]);
    let mut granted = vec![source.clone(), authority.clone()];
    if exclude != Some("hidden") {
        granted.push(hidden_source.clone());
    }
    if exclude != Some("query") {
        granted.push(query_source.clone());
    }
    let mut agent =
        AgentRuntime::new(&ingress, AgentLimits::new(16, 64, 2048, 4096).unwrap()).unwrap();
    let binding = agent
        .bind_session(
            &AgentAccess::new(&ingress, &request, &presets, 0),
            &mut sessions,
            &registration.session,
            granted,
        )
        .unwrap();

    let query = RetrievalQuery::new(FACT).with_limit(1);
    let ranked = arm(
        &mut memory,
        &owner,
        binding.memory_session(),
        &query,
        Some((&hidden_source, &query_source)),
    )
    .await;
    assert_eq!(
        ranked
            .ticket()
            .candidate(&ranked.result().ranking()[0].0)
            .unwrap()
            .text(),
        FACT
    );
    assert_eq!(ranked.ticket().transmissions().len(), 1);
    let mut memory = SearchMemory::with_ranked(memory, ranked);
    let prepared = agent.prepare(
        &AgentAccess::new(&ingress, &request, &presets, 0),
        AgentServices::new(&mut sessions, &mut memory),
        &binding,
        &registration.interaction,
        &query,
    );
    if exclude.is_some() {
        assert!(matches!(prepared, Err(AgentError::MemoryScopeDenied)));
        return;
    }
    let mut task: AgentTask = prepared.unwrap();
    let mut model = EvidenceProbe {
        persona: PERSONA.to_owned(),
        fact: FACT,
    };
    let result: AgentResult = agent
        .infer(
            &AgentAccess::new(&ingress, &request, &presets, 0),
            AgentServices::new(&mut sessions, &mut memory),
            &mut task,
            &mut model,
        )
        .unwrap();
    let delivery = agent
        .finish(
            &AgentAccess::new(&ingress, &request, &presets, 0),
            AgentServices::new(&mut sessions, &mut memory),
            &mut task,
            &result,
        )
        .unwrap();
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert_eq!(delivery.text(), Some("synthetic offline reply"));

    assert!(matches!(
        memory.begin_task(
            &owner,
            binding.memory_session(),
            ReadPurpose::OfflineReasoning,
            &query,
            0
        ),
        Err(SearchError::InvalidInput(
            "prepared hybrid retrieval was already consumed"
        ))
    ));
}

#[tokio::test]
async fn offline_agent_consumes_persistent_hybrid_facade_directly() {
    run_agent_with_sent_evidence(None).await;
}

#[tokio::test]
async fn offline_agent_rejects_unselected_private_send_outside_grant() {
    run_agent_with_sent_evidence(Some("hidden")).await;
}

#[tokio::test]
async fn offline_agent_rejects_query_source_outside_grant() {
    run_agent_with_sent_evidence(Some("query")).await;
}

#[tokio::test]
async fn facade_writeback_keeps_query_and_unselected_send_evidence() {
    let directory = Directory::new("proposal-send-evidence");
    let mut local = persistent_memory(&directory, None);
    let owner = identity();
    let source = local
        .register_source(
            OperationId::new("register-source").unwrap(),
            MemoryScope::Private(owner.clone()),
            "synthetic",
            1,
            Epistemic::UserStatement,
        )
        .unwrap()
        .value()
        .clone();
    let (hidden, query_source) =
        attach_send_fixture(&mut local, &owner, std::slice::from_ref(&source));
    let session = SessionId::new("proposal-session").unwrap();
    let query = RetrievalQuery::new(FACT).with_limit(1);
    let ranked = arm(
        &mut local,
        &owner,
        &session,
        &query,
        Some((&hidden, &query_source)),
    )
    .await;
    let hidden_version = ranked
        .ticket()
        .candidates()
        .iter()
        .find(|candidate| candidate.item().sources().contains(&hidden))
        .unwrap()
        .candidate_ref()
        .record()
        .unwrap();
    let mut facade = SearchMemory::with_ranked(local, ranked);
    let task = facade
        .begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 0)
        .unwrap()
        .into_task();
    assert_eq!(task.result().items().len(), 1);
    assert_eq!(task.transmissions().len(), 1);
    for sources in [
        vec![source.clone(), hidden.clone()],
        vec![source.clone(), query_source.clone()],
    ] {
        let incomplete = MemoryDraft::new(
            "synthetic inference",
            Category::Episode,
            Epistemic::Inference,
            sources,
        );
        assert!(matches!(
            task.propose_create(incomplete, 0),
            Err(MemoryError::InvalidEvidence)
        ));
    }
    let complete = MemoryDraft::new(
        "synthetic inference",
        Category::Episode,
        Epistemic::Inference,
        vec![source, hidden, query_source],
    );
    assert!(task.propose_create(complete.clone(), 0).is_ok());
    let mut local = facade.into_memory();
    let committed = local
        .create_from_task(
            OperationId::new("proposal-writeback").unwrap(),
            &task,
            complete,
            0,
        )
        .unwrap();
    let records = local
        .for_pet(&owner)
        .get(committed.value().id(), 0)
        .unwrap();
    assert!(records.dependencies().contains(&hidden_version));
}
