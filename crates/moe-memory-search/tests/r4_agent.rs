//! R4 offline Agent regressions over the persistent `SearchMemory` facade.
//!
//! Each scenario builds a real journal-backed `LocalMemory`, arms a sealed
//! ranking through the `LocalMemory` authority bridge, and injects the facade
//! into the existing offline Agent: a paraphrased question still recovers a
//! preference through manual fixture vectors served by a loopback endpoint, a
//! corrected record supersedes the old sealed ranking, a missing vector keeps
//! KeywordOnly delivery with a visible reason, and an unrelated question
//! delivers nothing while the protected persona stays assembled. Manual
//! vectors, manual embeddings, and synthetic replies are offline fixtures
//! only; they prove wiring, not real model quality, and no request leaves the
//! loopback endpoint.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;

use moe_agent_runtime::{
    AgentAccess, AgentBinding, AgentDelivery, AgentLimits, AgentRuntime, AgentServices,
    CallerSubject, ChannelId, ChannelScope, IngressToken, ModelOutcome, OfflineInput, OfflineModel,
    PresentationEvent, PresetConfig, PresetId, PresetPurpose, PresetRegistry, RawIngressCall,
    Registration, ReplyTarget, SessionAccess, SessionLimits, SessionRuntime, SessionTiming,
    TerminalState, TrustedIngress, TrustedRequest, UserEvent, UserEventKind,
};
use moe_memory_core::{
    Category, EmbeddingProfile, Epistemic, MemoryDraft, MemoryError, MemoryScope, Namespace,
    PetIdentity, ReadPolicy, ReadPurpose, RetrievalPlan, RetrievalQuery, SessionId, SourceRef,
    TransmissionPolicy, TransmissionUsage,
};
use moe_memory_local::{LocalMemory, OperationId, PersistenceError};
use moe_memory_search::{
    CallControl, Cancellation, ChunkConfig, HybridConfig, LocalSearchHost, OnlineEmbedding,
    OnlineOptions, RankedCandidates, RerankMode, SearchError, SearchIndex, SearchMemory,
    SearchMode, SearchReason, StorageDomain, hybrid_rank, rerank_or_fallback,
};
use serde_json::{Value, json};

const PERSONA: &str = "synthetic protected persona";
const REPLY: &str = "synthetic offline reply";
const INGRESS_TEXT: &str = "synthetic-private-user-text";
const EMBEDDING_MODEL: &str = "manual-fixture-model";
const PREFERENCE: &str = "我喜欢茉莉花茶";
const PREFERENCE_V1: &str = "我早餐喜欢喝咖啡";
const PREFERENCE_V2: &str = "我早餐改喝茉莉花茶了";
const KEYWORD_HIT: &str = "我早餐喜欢喝茉莉花茶";
const DISTRACTOR: &str = "我每周慢跑三次";

struct Directory(PathBuf);

impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "moe-r4-agent-{name}-{}-{}",
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
        Namespace::new("r4-agent-regression", "owner").unwrap(),
        "stable-pet",
    )
    .unwrap()
}

fn keyword_profile() -> EmbeddingProfile {
    EmbeddingProfile::new("r4-offline", "keyword-fixture-model", 2).unwrap()
}

struct Store {
    local: LocalMemory,
    owner: PetIdentity,
    source: SourceRef,
    authority: SourceRef,
}

fn persistent_memory(directory: &Directory, records: &[&str]) -> Store {
    let mut local = LocalMemory::create(directory.journal()).unwrap();
    let owner = identity();
    let source = local
        .register_source(
            OperationId::new("r4-register-source").unwrap(),
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
            OperationId::new("r4-register-authority").unwrap(),
            MemoryScope::Private(owner.clone()),
            "synthetic-persona",
            1,
            Epistemic::AuthoritativeState,
        )
        .unwrap()
        .value()
        .clone();
    for (position, text) in records.iter().enumerate() {
        local
            .for_pet(&owner)
            .create(
                OperationId::new(format!("r4-create-{position}")).unwrap(),
                MemoryDraft::new(
                    *text,
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![source.clone()],
                ),
                100,
            )
            .unwrap();
    }
    local
        .set_identity_view(&owner, 1, PERSONA, vec![authority.clone()])
        .unwrap();
    local
        .set_read_policy(
            &owner,
            ReadPolicy::offline(1, vec![source.clone(), authority.clone()]),
        )
        .unwrap();
    Store {
        local,
        owner,
        source,
        authority,
    }
}

/// Sealed keyword-only ranking (no vectors anywhere) through the same
/// LocalMemory authority bridge the Agent consumes afterwards.
async fn arm_keyword(
    local: &mut LocalMemory,
    owner: &PetIdentity,
    session: &SessionId,
    query: &RetrievalQuery,
    now_ms: u64,
) -> RankedCandidates {
    let mut index = SearchIndex::new(
        StorageDomain::new("r4-agent-keyword-root").unwrap(),
        keyword_profile(),
        ChunkConfig::new(1, 512, 16, 1536).unwrap(),
        32,
        16,
        1024,
    )
    .unwrap();
    let config =
        HybridConfig::new(keyword_profile(), 1, 0.8, true, RerankMode::Disabled, 20).unwrap();
    let ticket = local
        .for_session(owner, session, ReadPurpose::OfflineReasoning)
        .begin_candidates(query, RetrievalPlan::new(keyword_profile()), now_ms)
        .unwrap();
    let mut host = LocalSearchHost::new(local, now_ms);
    let ranking = hybrid_rank(&mut index, &mut host, &ticket, None, &config).unwrap();
    let control = CallControl::new(Cancellation::default(), Duration::from_secs(1)).unwrap();
    rerank_or_fallback(&index, &mut host, ticket, ranking, None, None, &control)
        .await
        .unwrap()
}

struct Harness {
    ingress: TrustedIngress,
    presets: PresetRegistry,
    request: TrustedRequest,
    sessions: SessionRuntime,
    registration: Registration,
    agent: AgentRuntime,
    binding: AgentBinding,
}

fn harness(owner: &PetIdentity, granted: Vec<SourceRef>) -> Harness {
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
                INGRESS_TEXT,
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
    Harness {
        ingress,
        presets,
        request,
        sessions,
        registration,
        agent,
        binding,
    }
}

/// Runs prepare → infer → finish against the sealed facade. The model probe
/// asserts the delivered persona and memory blocks inside `infer`.
fn converse(
    harness: &mut Harness,
    memory: &mut SearchMemory,
    query: &RetrievalQuery,
    now_ms: u64,
    model: &mut dyn OfflineModel,
) -> AgentDelivery {
    let access = AgentAccess::new(&harness.ingress, &harness.request, &harness.presets, now_ms);
    let mut task = harness
        .agent
        .prepare(
            &access,
            AgentServices::new(&mut harness.sessions, memory),
            &harness.binding,
            &harness.registration.interaction,
            query,
        )
        .unwrap();
    let result = harness
        .agent
        .infer(
            &access,
            AgentServices::new(&mut harness.sessions, memory),
            &mut task,
            model,
        )
        .unwrap();
    harness
        .agent
        .finish(
            &access,
            AgentServices::new(&mut harness.sessions, memory),
            &mut task,
            &result,
        )
        .unwrap()
}

/// Offline reply fixture: asserts the protected persona and the exact memory
/// blocks delivered by the sealed ranking, then answers with fixed text.
struct MemoryProbe {
    persona: &'static str,
    expected: &'static [&'static str],
}

impl OfflineModel for MemoryProbe {
    fn infer(&mut self, input: OfflineInput<'_>) -> ModelOutcome {
        assert_eq!(
            input.persona(),
            self.persona,
            "protected persona must stay assembled"
        );
        let delivered: Vec<_> = input.memory().iter().map(|block| block.text()).collect();
        assert_eq!(
            delivered.as_slice(),
            self.expected,
            "delivered memory blocks must match the sealed ranking exactly"
        );
        ModelOutcome::Text {
            text: REPLY.to_owned(),
            presentation: vec![PresentationEvent::Neutral],
        }
    }
}

/// Manual fixture vectors: only the drink-preference topic shares one
/// direction (the record and its paraphrased question have no lexical
/// overlap); every other text is orthogonal. This encodes wiring only, not a
/// real embedding model.
fn manual_embedding(text: &str) -> [f64; 2] {
    if text.contains("茉莉花茶") || text.contains("偏爱") || text.contains("饮品") {
        [3.0, 4.0]
    } else {
        [4.0, -3.0]
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, Value)> {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    let (header_end, length) = loop {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header = String::from_utf8(bytes[..position].to_vec()).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })?
                .parse::<usize>()
                .ok()?;
            break (position + 4, length);
        }
    };
    while bytes.len() < header_end + length {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let header = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let body = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
    Some((
        header
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned(),
        body,
    ))
}

/// Loopback-only endpoint that answers every embeddings request with the
/// manual fixture vectors. No reply queue: each response is computed from the
/// received input texts, so batching order cannot desynchronize the fixture.
struct Loopback {
    endpoint: String,
    count: Arc<AtomicUsize>,
    paths: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Loopback {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = stop.clone();
        let worker_count = count.clone();
        let worker_paths = paths.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Some((path, body)) = read_request(&mut stream) {
                            worker_paths.lock().unwrap().push(path);
                            worker_count.fetch_add(1, Ordering::SeqCst);
                            let inputs = body
                                .get("input")
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default();
                            let data: Vec<Value> = inputs
                                .iter()
                                .enumerate()
                                .map(|(index, text)| {
                                    json!({
                                        "index": index,
                                        "embedding":
                                            manual_embedding(text.as_str().unwrap_or(""))
                                                .to_vec(),
                                    })
                                })
                                .collect();
                            let response =
                                json!({"model": EMBEDDING_MODEL, "data": data}).to_string();
                            let _ = stream.write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    response.len(),
                                    response
                                )
                                .as_bytes(),
                            );
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("loopback accept failed: {error}"),
                }
            }
        });
        Self {
            endpoint,
            count,
            paths,
            stop,
            worker: Some(worker),
        }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[tokio::test]
async fn paraphrased_query_recovers_preference_through_agent() {
    let directory = Directory::new("paraphrase");
    let store = persistent_memory(
        &directory,
        &[PREFERENCE, DISTRACTOR, "车辆维修手册放在车库里"],
    );
    let mut local = store.local;
    let owner = store.owner;

    let loopback = Loopback::start();
    let embedding_endpoint = format!("{}/v1/embeddings", loopback.endpoint);
    let embedding = OnlineEmbedding::new(
        &embedding_endpoint,
        "synthetic-key",
        EMBEDDING_MODEL,
        2,
        OnlineOptions::default(),
    )
    .unwrap();
    for usage in [
        TransmissionUsage::QueryEmbedding,
        TransmissionUsage::EntryEmbedding,
    ] {
        let policy =
            TransmissionPolicy::new(1, usage, &embedding_endpoint, vec![store.source.clone()])
                .unwrap()
                .with_scope(MemoryScope::Private(owner.clone()))
                .transmit_query();
        local.set_transmission_policy(&owner, policy).unwrap();
    }

    let mut harness = harness(&owner, vec![store.source.clone(), store.authority.clone()]);
    let session = harness.binding.memory_session().clone();
    let query = RetrievalQuery::new("偏爱的饮品").with_limit(3);
    let profile = embedding.profile().clone();

    let mut ticket_diagnostic = local
        .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
        .begin_candidates(&query, RetrievalPlan::new(profile.clone()), 100)
        .unwrap();
    let mut ticket_agent = local
        .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
        .begin_candidates(&query, RetrievalPlan::new(profile.clone()), 100)
        .unwrap();
    local
        .bind_query_provenance(&mut ticket_diagnostic, vec![store.source.clone()], &[], 100)
        .unwrap();
    local
        .bind_query_provenance(&mut ticket_agent, vec![store.source.clone()], &[], 100)
        .unwrap();

    let mut index = SearchIndex::new(
        StorageDomain::new("r4-agent-vector-root").unwrap(),
        profile.clone(),
        ChunkConfig::new(1, 512, 16, 1536).unwrap(),
        32,
        16,
        1024,
    )
    .unwrap();
    let config = HybridConfig::new(profile, 1, 0.8, true, RerankMode::Disabled, 20).unwrap();

    let (sealed_diagnostic, sealed_agent) = {
        let mut host = LocalSearchHost::new(&mut local, 100);
        let control = CallControl::new(Cancellation::default(), Duration::from_secs(3)).unwrap();
        let encoded = embedding
            .embed_query(&mut host, &mut ticket_diagnostic, &control)
            .await
            .unwrap();
        while let Some(batch) = index.take_batch(&mut host, &ticket_diagnostic).unwrap() {
            let completed = embedding
                .embed_index_batch(&mut host, &mut ticket_diagnostic, batch, &control)
                .await
                .unwrap();
            index
                .commit(&mut host, &ticket_diagnostic, completed)
                .unwrap();
        }
        let encoded_agent = embedding
            .embed_query(&mut host, &mut ticket_agent, &control)
            .await
            .unwrap();
        let diagnostic_ranking = hybrid_rank(
            &mut index,
            &mut host,
            &ticket_diagnostic,
            Some(&encoded),
            &config,
        )
        .unwrap();
        let agent_ranking = hybrid_rank(
            &mut index,
            &mut host,
            &ticket_agent,
            Some(&encoded_agent),
            &config,
        )
        .unwrap();
        let sealed_diagnostic = rerank_or_fallback(
            &index,
            &mut host,
            ticket_diagnostic,
            diagnostic_ranking,
            None,
            None,
            &control,
        )
        .await
        .unwrap();
        let sealed_agent = rerank_or_fallback(
            &index,
            &mut host,
            ticket_agent,
            agent_ranking,
            None,
            None,
            &control,
        )
        .await
        .unwrap();
        (sealed_diagnostic, sealed_agent)
    };

    // Exactly one query encoding, one index batch, and one agent-request query
    // encoding were sent, all to the loopback embeddings path.
    assert_eq!(loopback.count(), 3);
    assert!(loopback.paths().iter().all(|path| path == "/v1/embeddings"));

    let mut facade = SearchMemory::with_ranked(local, sealed_diagnostic);
    let search_task = facade
        .begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100)
        .unwrap();
    let diagnostic = search_task.diagnostic().unwrap();
    assert_eq!(diagnostic.mode(), SearchMode::Hybrid);
    assert_eq!(
        diagnostic.lexical_hits(),
        0,
        "the paraphrase must not match lexically"
    );
    assert_eq!(
        diagnostic.vector_hits(),
        1,
        "manual fixture vectors must carry the paraphrase"
    );
    let task = search_task.into_task();
    assert_eq!(task.result().items()[0].text(), PREFERENCE);
    assert_eq!(task.identity_view().unwrap().text(), PERSONA);
    let local = facade.into_memory();

    let mut facade = SearchMemory::with_ranked(local, sealed_agent);
    let mut model = MemoryProbe {
        persona: PERSONA,
        expected: &[PREFERENCE],
    };
    let delivery = converse(&mut harness, &mut facade, &query, 100, &mut model);
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert_eq!(delivery.text(), Some(REPLY));
}

#[tokio::test]
async fn correction_supersedes_old_ranking_and_agent_uses_only_new_version() {
    let directory = Directory::new("correction");
    let store = persistent_memory(&directory, &[]);
    let mut local = store.local;
    let owner = store.owner;
    let created = local
        .for_pet(&owner)
        .create(
            OperationId::new("r4-create-preference").unwrap(),
            MemoryDraft::new(
                PREFERENCE_V1,
                Category::Episode,
                Epistemic::UserStatement,
                vec![store.source.clone()],
            ),
            100,
        )
        .unwrap();
    local
        .for_pet(&owner)
        .create(
            OperationId::new("r4-create-distractor").unwrap(),
            MemoryDraft::new(
                DISTRACTOR,
                Category::Episode,
                Epistemic::UserStatement,
                vec![store.source.clone()],
            ),
            100,
        )
        .unwrap();

    let mut harness = harness(&owner, vec![store.source.clone(), store.authority.clone()]);
    let session = harness.binding.memory_session().clone();
    let query = RetrievalQuery::new("早餐");

    let sealed_before_a = arm_keyword(&mut local, &owner, &session, &query, 100).await;
    let sealed_before_b = arm_keyword(&mut local, &owner, &session, &query, 100).await;

    // The pre-correction ranking is deliverable and carries the original text.
    let mut facade = SearchMemory::with_ranked(local, sealed_before_a);
    let search_task = facade
        .begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100)
        .unwrap();
    assert_eq!(search_task.task().result().items()[0].text(), PREFERENCE_V1);
    let mut local = facade.into_memory();

    // After the correction, the second pre-correction sealed ranking must be
    // undeliverable instead of resurfacing the stale version.
    local
        .for_pet(&owner)
        .update(
            OperationId::new("r4-correct-preference").unwrap(),
            created.value().id(),
            1,
            MemoryDraft::new(
                PREFERENCE_V2,
                Category::Episode,
                Epistemic::UserStatement,
                vec![store.source.clone()],
            ),
            200,
        )
        .unwrap();
    let mut stale_facade = SearchMemory::with_ranked(local, sealed_before_b);
    assert!(matches!(
        stale_facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 200),
        Err(SearchError::Persistence(PersistenceError::Memory(
            MemoryError::TaskCancelled
        )))
    ));
    assert!(matches!(
        stale_facade.begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 200),
        Err(SearchError::InvalidInput(
            "prepared hybrid retrieval was already consumed"
        ))
    ));
    let mut local = stale_facade.into_memory();

    let sealed_after = arm_keyword(&mut local, &owner, &session, &query, 200).await;
    let mut facade = SearchMemory::with_ranked(local, sealed_after);
    let mut model = MemoryProbe {
        persona: PERSONA,
        expected: &[PREFERENCE_V2],
    };
    let delivery = converse(&mut harness, &mut facade, &query, 200, &mut model);
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert_eq!(delivery.text(), Some(REPLY));
}

#[tokio::test]
async fn missing_vectors_keep_keyword_only_with_visible_reason() {
    let directory = Directory::new("keyword-fallback");
    let store = persistent_memory(&directory, &[KEYWORD_HIT, DISTRACTOR]);
    let mut local = store.local;
    let owner = store.owner;

    let mut harness = harness(&owner, vec![store.source.clone(), store.authority.clone()]);
    let session = harness.binding.memory_session().clone();
    let query = RetrievalQuery::new("早餐");

    let sealed_diagnostic = arm_keyword(&mut local, &owner, &session, &query, 100).await;
    let sealed_agent = arm_keyword(&mut local, &owner, &session, &query, 100).await;

    let mut facade = SearchMemory::with_ranked(local, sealed_diagnostic);
    let search_task = facade
        .begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100)
        .unwrap();
    let diagnostic = search_task.diagnostic().unwrap();
    assert_eq!(diagnostic.mode(), SearchMode::KeywordOnly);
    assert_eq!(diagnostic.vector_hits(), 0);
    assert_eq!(diagnostic.lexical_hits(), 1);
    assert!(
        diagnostic
            .status()
            .reasons
            .contains(&SearchReason::ModelUnavailable),
        "the missing-vector reason must be visible: {:?}",
        diagnostic.status().reasons
    );
    assert!(
        diagnostic
            .status()
            .reasons
            .contains(&SearchReason::PartialIndex),
        "the missing-index reason must be visible: {:?}",
        diagnostic.status().reasons
    );
    let task = search_task.into_task();
    assert_eq!(task.result().items()[0].text(), KEYWORD_HIT);
    assert_eq!(task.identity_view().unwrap().text(), PERSONA);
    let local = facade.into_memory();

    let mut facade = SearchMemory::with_ranked(local, sealed_agent);
    let mut model = MemoryProbe {
        persona: PERSONA,
        expected: &[KEYWORD_HIT],
    };
    let delivery = converse(&mut harness, &mut facade, &query, 100, &mut model);
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert_eq!(delivery.text(), Some(REPLY));
}

#[tokio::test]
async fn unrelated_question_delivers_empty_and_persona_stays_protected() {
    let directory = Directory::new("unrelated");
    let store = persistent_memory(&directory, &[PREFERENCE, DISTRACTOR]);
    let mut local = store.local;
    let owner = store.owner;

    let mut harness = harness(&owner, vec![store.source.clone(), store.authority.clone()]);
    let session = harness.binding.memory_session().clone();
    let query = RetrievalQuery::new("今天天气怎么样");

    let sealed_diagnostic = arm_keyword(&mut local, &owner, &session, &query, 100).await;
    let sealed_agent = arm_keyword(&mut local, &owner, &session, &query, 100).await;

    let mut facade = SearchMemory::with_ranked(local, sealed_diagnostic);
    let search_task = facade
        .begin_task(&owner, &session, ReadPurpose::OfflineReasoning, &query, 100)
        .unwrap();
    let diagnostic = search_task.diagnostic().unwrap();
    assert_eq!(diagnostic.mode(), SearchMode::KeywordOnly);
    assert_eq!(diagnostic.lexical_hits(), 0);
    assert_eq!(diagnostic.vector_hits(), 0);
    let task = search_task.into_task();
    assert!(
        task.result().items().is_empty(),
        "an unrelated question must deliver no memories"
    );
    assert_eq!(task.identity_view().unwrap().text(), PERSONA);
    let local = facade.into_memory();

    let mut facade = SearchMemory::with_ranked(local, sealed_agent);
    let mut model = MemoryProbe {
        persona: PERSONA,
        expected: &[],
    };
    let delivery = converse(&mut harness, &mut facade, &query, 100, &mut model);
    assert_eq!(delivery.state(), TerminalState::Completed);
    assert_eq!(delivery.text(), Some(REPLY));
}
