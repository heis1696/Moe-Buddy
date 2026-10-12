use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;

use moe_memory_core::{
    CandidateRef, CandidateTicket, Category, Epistemic, MemoryDraft, MemoryError, MemoryScope,
    MemoryService, Namespace, PetIdentity, ReadPolicy, ReadPurpose, RetrievalPlan, RetrievalQuery,
    SessionId, TransmissionPolicy, TransmissionUsage,
};
use moe_memory_search::{
    CallControl, Cancellation, CohereReranker, OnlineEmbedding, OnlineOptions, SearchError,
    SearchHost, exact_scan,
};
use serde_json::{Value, json};

#[path = "../examples/support/preference.rs"]
mod probe_preference;

struct Reply {
    status: u16,
    body: String,
    headers: String,
    delay: Duration,
    length: Option<usize>,
}

impl Reply {
    fn json(body: Value) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
            headers: String::new(),
            delay: Duration::ZERO,
            length: None,
        }
    }

    fn status(status: u16) -> Self {
        Self {
            status,
            ..Self::json(json!({}))
        }
    }
}

#[derive(Debug)]
struct Request {
    path: String,
    body: Value,
    authenticated: bool,
}

struct Server {
    endpoint: String,
    count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = stop.clone();
        let worker_count = count.clone();
        let worker_requests = requests.clone();
        let worker = thread::spawn(move || {
            let mut replies: VecDeque<_> = replies.into();
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Some(request) = read_request(&mut stream) {
                            worker_requests.lock().unwrap().push(request);
                            worker_count.fetch_add(1, Ordering::SeqCst);
                            let reply = replies.pop_front().unwrap_or_else(|| Reply::status(500));
                            thread::sleep(reply.delay);
                            let length = reply.length.unwrap_or(reply.body.len());
                            let response = format!(
                                "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                                reply.status, length, reply.headers, reply.body
                            );
                            let _ = stream.write_all(response.as_bytes());
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
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
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
    Some(Request {
        path: header
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned(),
        authenticated: header
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-key\r\n"),
        body: serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
    })
}

struct Host {
    service: MemoryService,
    owner: PetIdentity,
    checkpoints: usize,
    revoke_at: Option<usize>,
    now: u64,
}

impl SearchHost for Host {
    fn memory(&mut self) -> &mut MemoryService {
        &mut self.service
    }
    fn now_ms(&mut self) -> u64 {
        self.checkpoints += 1;
        if self.revoke_at == Some(self.checkpoints) {
            self.service
                .set_transmission_policy(
                    &self.owner,
                    TransmissionPolicy::deny_all(2, TransmissionUsage::QueryEmbedding),
                )
                .unwrap();
        }
        self.now
    }
}

struct Fixture {
    host: Host,
    ticket: CandidateTicket,
    embedding: OnlineEmbedding,
    reranker: CohereReranker,
    entries: Vec<CandidateRef>,
}

impl Fixture {
    fn new(server: &Server, options: OnlineOptions, grant: bool, rerank: bool) -> Self {
        Self::with_rerank_options(server, options.clone(), options, grant, rerank)
    }

    fn with_rerank_options(
        server: &Server,
        options: OnlineOptions,
        rerank_options: OnlineOptions,
        grant: bool,
        rerank: bool,
    ) -> Self {
        let embedding_endpoint = format!("{}/v1/embeddings", server.endpoint);
        let rerank_endpoint = format!("{}/v2/rerank", server.endpoint);
        let embedding = OnlineEmbedding::new(
            &embedding_endpoint,
            "synthetic-key",
            "fixture-model",
            2,
            options.clone(),
        )
        .unwrap();
        let reranker = CohereReranker::new(
            &rerank_endpoint,
            "synthetic-key",
            "fixture-rerank",
            rerank_options,
        )
        .unwrap();
        let owner =
            PetIdentity::new(Namespace::new("synthetic", "user").unwrap(), "alice").unwrap();
        let mut service = MemoryService::new();
        let source = service
            .register_source(
                MemoryScope::Private(owner.clone()),
                "synthetic-source",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();
        for text in ["我喜欢茉莉花茶", "车辆维修手册"] {
            service
                .for_pet(&owner)
                .create(
                    MemoryDraft::new(
                        text,
                        Category::Episode,
                        Epistemic::UserStatement,
                        vec![source.clone()],
                    ),
                    100,
                )
                .unwrap();
        }
        service
            .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))
            .unwrap();
        if grant {
            for (usage, endpoint) in [
                (TransmissionUsage::QueryEmbedding, &embedding_endpoint),
                (TransmissionUsage::EntryEmbedding, &embedding_endpoint),
                (TransmissionUsage::Rerank, &rerank_endpoint),
            ] {
                let policy = TransmissionPolicy::new(1, usage, endpoint, vec![source.clone()])
                    .unwrap()
                    .with_scope(MemoryScope::Private(owner.clone()))
                    .transmit_query();
                service.set_transmission_policy(&owner, policy).unwrap();
            }
        }
        let mut plan = RetrievalPlan::new(embedding.profile().clone());
        if rerank {
            plan = plan.with_rerank(reranker.profile().clone())
        }
        let session = SessionId::new("session").unwrap();
        let mut ticket = service
            .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
            .begin_candidates(&RetrievalQuery::new("偏爱的饮品"), plan, 100)
            .unwrap();
        service
            .bind_query_provenance(&mut ticket, vec![source], &[], 100)
            .unwrap();
        let entries = ticket
            .candidates()
            .iter()
            .map(|item| item.candidate_ref())
            .collect();
        Self {
            host: Host {
                service,
                owner,
                checkpoints: 0,
                revoke_at: None,
                now: 100,
            },
            ticket,
            embedding,
            reranker,
            entries,
        }
    }

    fn assert_closed(&mut self) {
        let identity = self.ticket.identity().clone();
        let session = self.ticket.session().clone();
        assert!(
            self.host
                .service
                .for_session(&identity, &session, ReadPurpose::OfflineReasoning)
                .complete_retrieval(&self.ticket, &[], 100)
                .is_err()
        );
    }

    fn renew_ticket(&mut self, bind_query: bool) {
        let sources = self.ticket.query_proof().sources().to_vec();
        let session = self.ticket.session().clone();
        let plan = self.ticket.plan().clone();
        self.ticket = self
            .host
            .service
            .for_session(&self.host.owner, &session, ReadPurpose::OfflineReasoning)
            .begin_candidates(&RetrievalQuery::new("偏爱的饮品"), plan, 100)
            .unwrap();
        if bind_query {
            self.host
                .service
                .bind_query_provenance(&mut self.ticket, sources, &[], 100)
                .unwrap()
        }
        self.entries = self
            .ticket
            .candidates()
            .iter()
            .map(|candidate| candidate.candidate_ref())
            .collect();
    }
}

fn control() -> CallControl {
    CallControl::new(Cancellation::default(), Duration::from_secs(3)).unwrap()
}

fn embeddings(data: Value) -> Reply {
    Reply::json(json!({"model":"fixture-model", "data":data}))
}

fn query_reply() -> Reply {
    embeddings(json!([{"index":0,"embedding":[3.0,4.0]}]))
}

#[tokio::test]
async fn float_protocol_reorders_indices_and_scans_non_substring_fixture() {
    let server = Server::new(vec![
        query_reply(),
        embeddings(json!([
            {"index":1,"embedding":[-3.0,-4.0]}, {"index":0,"embedding":[6.0,8.0]}
        ])),
    ]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    let query = fixture
        .embedding
        .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
        .await
        .unwrap();
    let vectors = fixture
        .embedding
        .embed_entries(
            &mut fixture.host,
            &mut fixture.ticket,
            &fixture.entries,
            &control(),
        )
        .await
        .unwrap();
    let ranked = exact_scan(&fixture.ticket, &query, &vectors, 0.8, 5).unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].0, fixture.entries[0]);
    assert!((ranked[0].1 - 1.0).abs() < 1e-12);
    assert!(
        !fixture
            .ticket
            .candidate(&ranked[0].0)
            .unwrap()
            .text()
            .contains(fixture.ticket.query().text())
    );
    let owner = fixture.ticket.identity().clone();
    let session = fixture.ticket.session().clone();
    let task = fixture
        .host
        .service
        .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
        .complete_retrieval(&fixture.ticket, &ranked, 100)
        .unwrap();
    fixture
        .host
        .service
        .assemble_context(&task, 4096, 100)
        .unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[0].path, "/v1/embeddings");
    assert!(requests.iter().all(|request| request.authenticated));
    assert_eq!(requests[0].body["input"], json!(["偏爱的饮品"]));
    assert_eq!(
        requests[1].body["input"],
        json!(["我喜欢茉莉花茶", "车辆维修手册"])
    );
    assert_eq!(requests[0].body["encoding_format"], "float");
    assert_eq!(fixture.ticket.transmissions().len(), 2);
}

#[tokio::test]
async fn key_and_read_permission_do_not_authorize_transmission() {
    let server = Server::new(vec![]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), false, true);
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::Core(MemoryError::TransmissionDenied)
    );
    assert!(
        fixture
            .embedding
            .embed_entries(
                &mut fixture.host,
                &mut fixture.ticket,
                &fixture.entries,
                &control()
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .reranker
            .rerank(
                &mut fixture.host,
                &mut fixture.ticket,
                &fixture.entries,
                &control()
            )
            .await
            .is_err()
    );
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn cohere_v2_maps_all_scores_to_original_references() {
    let server = Server::new(vec![Reply::json(
        json!({"results":[{"index":1,"relevance_score":0.95},{"index":0,"relevance_score":0.1}]}),
    )]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let ranked = fixture
        .reranker
        .rerank(
            &mut fixture.host,
            &mut fixture.ticket,
            &fixture.entries,
            &control(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ranked[0], (fixture.entries[1].clone(), 0.95));
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[0].path, "/v2/rerank");
    assert_eq!(requests[0].body["top_n"], 2);
    assert_eq!(requests[0].body["max_tokens_per_doc"], 4096);
    assert_eq!(requests[0].body["query"], "偏爱的饮品");
    assert_eq!(
        requests[0].body["documents"],
        json!(["我喜欢茉莉花茶", "车辆维修手册"])
    );
}

#[tokio::test]
async fn disabled_rerank_has_no_network_or_receipt() {
    let server = Server::new(vec![]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), false, false);
    assert!(
        fixture
            .reranker
            .rerank(&mut fixture.host, &mut fixture.ticket, &[], &control())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(server.count(), 0);
    assert!(fixture.ticket.transmissions().is_empty());
}

#[tokio::test]
async fn malformed_embeddings_permanently_close_ticket() {
    for data in [
        json!([]),
        json!([{"index":1,"embedding":[1,0]}]),
        json!([{"index":0,"embedding":[1]}]),
        json!([{"index":0,"embedding":[0,0]}]),
        json!([{"index":0,"embedding":[null,0]}]),
        json!([{"index":-1,"embedding":[1,0]}]),
        json!([{"index":0.5,"embedding":[1,0]}]),
    ] {
        let server = Server::new(vec![embeddings(data)]);
        let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
        assert!(matches!(
            fixture
                .embedding
                .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
                .await,
            Err(SearchError::InvalidResponse(_))
        ));
        fixture.assert_closed();
        assert_eq!(fixture.ticket.transmissions().len(), 1);
    }
}

#[tokio::test]
async fn embedding_duplicate_and_missing_indices_are_rejected() {
    let server = Server::new(vec![embeddings(
        json!([{"index":0,"embedding":[1,0]},{"index":0,"embedding":[0,1]}]),
    )]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    assert!(matches!(
        fixture
            .embedding
            .embed_entries(
                &mut fixture.host,
                &mut fixture.ticket,
                &fixture.entries,
                &control()
            )
            .await,
        Err(SearchError::InvalidResponse(_))
    ));
    fixture.assert_closed();
}

#[tokio::test]
async fn bad_rerank_indices_and_scores_are_rejected() {
    for results in [
        json!([]),
        json!([{"index":0,"relevance_score":0.5},{"index":0,"relevance_score":0.1}]),
        json!([{"index":2,"relevance_score":0.5},{"index":0,"relevance_score":0.1}]),
        json!([{"index":0,"relevance_score":1.1},{"index":1,"relevance_score":0.1}]),
        json!([{"index":0,"relevance_score":null},{"index":1,"relevance_score":0.1}]),
    ] {
        let server = Server::new(vec![Reply::json(json!({"results":results}))]);
        let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
        assert!(matches!(
            fixture
                .reranker
                .rerank(
                    &mut fixture.host,
                    &mut fixture.ticket,
                    &fixture.entries,
                    &control()
                )
                .await,
            Err(SearchError::InvalidResponse(_))
        ));
        fixture.assert_closed();
    }
}

#[tokio::test]
async fn transient_status_retries_are_finite_and_audited() {
    for status in [429, 502, 503, 504] {
        let server = Server::new(vec![Reply::status(status), query_reply()]);
        let options = OnlineOptions {
            max_attempts: 2,
            retry_delay: Duration::ZERO,
            ..OnlineOptions::default()
        };
        let mut fixture = Fixture::new(&server, options, true, false);
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap();
        assert_eq!(server.count(), 2);
        assert_eq!(fixture.ticket.transmissions().len(), 2);
    }
    let server = Server::new(vec![Reply::status(429), Reply::status(429), query_reply()]);
    let mut fixture = Fixture::new(
        &server,
        OnlineOptions {
            max_attempts: 2,
            retry_delay: Duration::ZERO,
            ..OnlineOptions::default()
        },
        true,
        false,
    );
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::HttpStatus(429)
    );
    assert_eq!(server.count(), 2);
    fixture.assert_closed();
}

#[tokio::test]
async fn invalid_credentials_input_and_unknown_statuses_never_retry() {
    for status in [400, 401, 403, 404, 408, 500] {
        let server = Server::new(vec![Reply::status(status), query_reply()]);
        let mut fixture = Fixture::new(
            &server,
            OnlineOptions {
                max_attempts: 3,
                ..OnlineOptions::default()
            },
            true,
            false,
        );
        assert_eq!(
            fixture
                .embedding
                .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
                .await
                .unwrap_err(),
            SearchError::HttpStatus(status)
        );
        assert_eq!(server.count(), 1);
        fixture.assert_closed();
    }
}

#[tokio::test]
async fn retry_after_is_not_bypassed_by_shorter_backoff() {
    let reply = Reply {
        headers: "Retry-After: 30\r\n".to_owned(),
        ..Reply::status(429)
    };
    let server = Server::new(vec![reply, query_reply()]);
    let mut fixture = Fixture::new(
        &server,
        OnlineOptions {
            max_attempts: 2,
            ..OnlineOptions::default()
        },
        true,
        false,
    );
    let budget = CallControl::new(Cancellation::default(), Duration::from_millis(80)).unwrap();
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &budget)
            .await
            .unwrap_err(),
        SearchError::DeadlineExceeded
    );
    assert_eq!(server.count(), 1);
    fixture.assert_closed();
}

#[tokio::test]
async fn unsupported_retry_after_fails_closed_instead_of_retrying_early() {
    for value in ["Wed, 21 Oct 2030 07:28:00 GMT", "99999999", "invalid"] {
        let reply = Reply {
            headers: format!("Retry-After: {value}\r\n"),
            ..Reply::status(429)
        };
        let server = Server::new(vec![reply, query_reply()]);
        let mut fixture = Fixture::new(
            &server,
            OnlineOptions {
                max_attempts: 2,
                ..OnlineOptions::default()
            },
            true,
            false,
        );
        assert_eq!(
            fixture
                .embedding
                .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
                .await
                .unwrap_err(),
            SearchError::HttpStatus(429)
        );
        assert_eq!(server.count(), 1);
        fixture.assert_closed();
    }
}

#[tokio::test]
async fn return_and_retry_check_current_permissions_before_delivery_or_resend() {
    for replies in [vec![query_reply()], vec![Reply::status(429), query_reply()]] {
        let server = Server::new(replies);
        let mut fixture = Fixture::new(
            &server,
            OnlineOptions {
                max_attempts: 2,
                retry_delay: Duration::ZERO,
                ..OnlineOptions::default()
            },
            true,
            false,
        );
        fixture.host.revoke_at = Some(2);
        assert!(matches!(
            fixture
                .embedding
                .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
                .await,
            Err(SearchError::Core(_))
        ));
        assert_eq!(server.count(), 1);
        fixture.assert_closed();
    }
}

#[tokio::test]
async fn redirected_requests_never_reach_unapproved_endpoint() {
    let destination = Server::new(vec![query_reply()]);
    let reply = Reply {
        headers: format!("Location: {}/v1/embeddings\r\n", destination.endpoint),
        ..Reply::status(307)
    };
    let server = Server::new(vec![reply]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::HttpStatus(307)
    );
    assert_eq!(destination.count(), 0);
    fixture.assert_closed();
}

#[tokio::test]
async fn timeout_does_not_retry_an_unknown_transmission_result() {
    let server = Server::new(vec![
        Reply {
            delay: Duration::from_millis(250),
            ..query_reply()
        },
        query_reply(),
    ]);
    let mut fixture = Fixture::new(
        &server,
        OnlineOptions {
            request_timeout: Duration::from_millis(80),
            max_attempts: 3,
            ..OnlineOptions::default()
        },
        true,
        false,
    );
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::RequestTimedOut
    );
    assert_eq!(server.count(), 1);
    fixture.assert_closed();
}

async fn cancel_after_request(count: Arc<AtomicUsize>, cancellation: Cancellation) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while count.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await
        }
    })
    .await
    .unwrap();
    cancellation.cancel();
}

#[tokio::test]
async fn cancellation_interrupts_inflight_request_and_retry_delay() {
    for reply in [
        Reply {
            delay: Duration::from_millis(250),
            ..query_reply()
        },
        Reply {
            headers: "Retry-After: 30\r\n".to_owned(),
            ..Reply::status(429)
        },
    ] {
        let server = Server::new(vec![reply, query_reply()]);
        let mut fixture = Fixture::new(
            &server,
            OnlineOptions {
                max_attempts: 2,
                ..OnlineOptions::default()
            },
            true,
            false,
        );
        let cancellation = Cancellation::default();
        let budget = CallControl::new(cancellation.clone(), Duration::from_secs(3)).unwrap();
        let cancel_task = tokio::spawn(cancel_after_request(server.count.clone(), cancellation));
        assert_eq!(
            fixture
                .embedding
                .embed_query(&mut fixture.host, &mut fixture.ticket, &budget)
                .await
                .unwrap_err(),
            SearchError::Cancelled
        );
        cancel_task.await.unwrap();
        assert_eq!(server.count(), 1);
        fixture.assert_closed();
    }
}

#[tokio::test]
async fn cancelled_or_expired_budget_sends_nothing() {
    let server = Server::new(vec![]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    let cancellation = Cancellation::default();
    cancellation.cancel();
    let budget = CallControl::new(cancellation, Duration::from_secs(1)).unwrap();
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &budget)
            .await
            .unwrap_err(),
        SearchError::Cancelled
    );
    let expired = CallControl::new(Cancellation::default(), Duration::from_millis(1)).unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &expired)
            .await
            .unwrap_err(),
        SearchError::DeadlineExceeded
    );
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn oversized_response_and_nonfinite_or_malformed_json_fail_closed() {
    for reply in [
        Reply {
            length: Some(9 * 1024 * 1024),
            ..query_reply()
        },
        Reply {
            body: "{\"secret\":\"synthetic-key\"".to_owned(),
            ..query_reply()
        },
        Reply {
            body: "{\"model\":\"fixture-model\",\"data\":[{\"index\":0,\"embedding\":[1e999,0]}]}"
                .to_owned(),
            ..query_reply()
        },
        Reply::json(json!({"model":"another-model","data":[{"index":0,"embedding":[1,0]}]})),
    ] {
        let server = Server::new(vec![reply]);
        let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
        let error = fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("synthetic-key"));
        fixture.assert_closed();
    }
}

#[tokio::test]
async fn input_and_batch_limits_reject_before_network_without_truncation() {
    let server = Server::new(vec![]);
    let mut fixture = Fixture::new(
        &server,
        OnlineOptions {
            max_input_bytes: 10,
            ..OnlineOptions::default()
        },
        true,
        true,
    );
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::InputTooLong
    );
    assert_eq!(
        fixture
            .embedding
            .embed_entries(
                &mut fixture.host,
                &mut fixture.ticket,
                &fixture.entries,
                &control()
            )
            .await
            .unwrap_err(),
        SearchError::InputTooLong
    );
    assert_eq!(
        fixture
            .reranker
            .rerank(
                &mut fixture.host,
                &mut fixture.ticket,
                &fixture.entries,
                &control()
            )
            .await
            .unwrap_err(),
        SearchError::InputTooLong
    );
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let duplicates = vec![fixture.entries[0].clone(); 2];
    assert!(
        fixture
            .embedding
            .embed_entries(
                &mut fixture.host,
                &mut fixture.ticket,
                &duplicates,
                &control()
            )
            .await
            .is_err()
    );
    let oversized = vec![fixture.entries[0].clone(); 33];
    assert!(
        fixture
            .embedding
            .embed_entries(
                &mut fixture.host,
                &mut fixture.ticket,
                &oversized,
                &control()
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .reranker
            .rerank(
                &mut fixture.host,
                &mut fixture.ticket,
                &oversized,
                &control()
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .embedding
            .embed_entries(&mut fixture.host, &mut fixture.ticket, &[], &control())
            .await
            .is_err()
    );
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn foreign_ticket_references_and_same_dimension_profiles_cannot_mix() {
    let server = Server::new(vec![]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let other = Fixture::new(&server, OnlineOptions::default(), true, true);
    assert!(
        fixture
            .embedding
            .embed_entries(
                &mut fixture.host,
                &mut fixture.ticket,
                &other.entries,
                &control()
            )
            .await
            .is_err()
    );
    let wrong = OnlineEmbedding::new(
        &format!("{}/other/embeddings", server.endpoint),
        "synthetic-key",
        "fixture-model",
        2,
        OnlineOptions::default(),
    )
    .unwrap();
    assert_eq!(
        wrong
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::ProfileMismatch
    );
    let wrong = OnlineEmbedding::new(
        &format!("{}/v1/embeddings", server.endpoint),
        "synthetic-key",
        "fixture-model",
        2,
        OnlineOptions {
            max_input_bytes: 512,
            ..OnlineOptions::default()
        },
    )
    .unwrap();
    assert_ne!(wrong.profile(), fixture.embedding.profile());
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn forked_same_version_body_is_rejected_before_any_send() {
    let server = Server::new(vec![]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    let mut original = fixture.host.service.clone();
    let mut fork = fixture.host.service.clone();
    let reference = fixture.entries[0].record().unwrap();
    let input = fixture.ticket.candidates()[0].item();
    let sources = input.sources().to_vec();
    original
        .for_pet(&fixture.host.owner)
        .update(
            reference.id(),
            reference.revision(),
            MemoryDraft::new(
                "原始分支正文",
                Category::Episode,
                Epistemic::UserStatement,
                sources.clone(),
            ),
            110,
        )
        .unwrap();
    fork.for_pet(&fixture.host.owner)
        .update(
            reference.id(),
            reference.revision(),
            MemoryDraft::new(
                "伪装分支正文",
                Category::Episode,
                Epistemic::UserStatement,
                sources.clone(),
            ),
            110,
        )
        .unwrap();
    let session = fixture.ticket.session().clone();
    fixture.ticket = original
        .for_session(&fixture.host.owner, &session, ReadPurpose::OfflineReasoning)
        .begin_candidates(
            &RetrievalQuery::new("偏爱的饮品"),
            RetrievalPlan::new(fixture.embedding.profile().clone()),
            110,
        )
        .unwrap();
    let query_input = fixture
        .ticket
        .candidates()
        .iter()
        .find(|candidate| candidate.candidate_ref().record().unwrap().id() == reference.id())
        .unwrap()
        .candidate_ref();
    original
        .bind_query_provenance(&mut fixture.ticket, sources, &[query_input], 110)
        .unwrap();
    fixture.host.service = fork;
    fixture.host.now = 110;
    assert!(matches!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await,
        Err(SearchError::Core(_))
    ));
    assert_eq!(server.count(), 0);
}

#[test]
fn constructors_are_offline_and_debug_is_redacted() {
    let server = Server::new(vec![]);
    let fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    assert!(!format!("{:?} {:?}", fixture.embedding, fixture.reranker).contains("synthetic-key"));
    assert_eq!(server.count(), 0);
    for endpoint in [
        "http://example.com/embeddings",
        "http://localhost/embeddings",
        "https://user:key@example.com/embeddings",
        "https://example.com/embeddings?key=x",
        "https://example.com/embeddings#frag",
        "https://example.com/chat",
        "http://127.1/embeddings",
        "https://EXAMPLE.com/embeddings",
    ] {
        assert!(
            OnlineEmbedding::new(
                endpoint,
                "synthetic-key",
                "model",
                2,
                OnlineOptions::default()
            )
            .is_err()
        );
    }
    assert!(
        OnlineEmbedding::new(
            "https://example.com/embeddings",
            "key\r\nheader:x",
            "model",
            2,
            OnlineOptions::default()
        )
        .is_err()
    );
    for options in [
        OnlineOptions {
            max_attempts: 0,
            ..OnlineOptions::default()
        },
        OnlineOptions {
            max_attempts: 4,
            ..OnlineOptions::default()
        },
        OnlineOptions {
            max_input_bytes: 4097,
            ..OnlineOptions::default()
        },
        OnlineOptions {
            request_timeout: Duration::ZERO,
            ..OnlineOptions::default()
        },
    ] {
        assert!(
            OnlineEmbedding::new(
                "https://example.com/embeddings",
                "synthetic-key",
                "model",
                2,
                options
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn loopback_fixture_accepts_real_http_client() {
    let server = Server::new(vec![query_reply()]);
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/v1/embeddings", server.endpoint))
        .body("{}")
        .send()
        .await;
    assert!(
        response.is_ok(),
        "synthetic loopback HTTP failure: {response:?}"
    );
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn exact_scan_threshold_empty_input_ties_and_ticket_binding_are_deterministic() {
    let server = Server::new(vec![
        query_reply(),
        embeddings(json!([
            {"index":0,"embedding":[3,4]},{"index":1,"embedding":[3,4]}
        ])),
    ]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    let query = fixture
        .embedding
        .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
        .await
        .unwrap();
    let vectors = fixture
        .embedding
        .embed_entries(
            &mut fixture.host,
            &mut fixture.ticket,
            &fixture.entries,
            &control(),
        )
        .await
        .unwrap();
    let ranked = exact_scan(&fixture.ticket, &query, &vectors, 0.0, 2).unwrap();
    assert_eq!(
        ranked.iter().map(|item| item.0.clone()).collect::<Vec<_>>(),
        fixture.entries
    );
    assert_eq!(
        exact_scan(&fixture.ticket, &query, &vectors, 0.0, 1)
            .unwrap()
            .len(),
        1
    );
    assert!(
        exact_scan(&fixture.ticket, &query, &[], 0.0, 2)
            .unwrap()
            .is_empty()
    );
    for threshold in [f64::NAN, f64::INFINITY, -1.1, 1.1] {
        assert!(exact_scan(&fixture.ticket, &query, &vectors, threshold, 2).is_err());
    }
    for limit in [0, 101] {
        assert!(exact_scan(&fixture.ticket, &query, &vectors, 0.0, limit).is_err())
    }
    let other = Fixture::new(&server, OnlineOptions::default(), true, false);
    assert!(exact_scan(&other.ticket, &query, &vectors, 0.0, 2).is_err());
    let mismatch = Fixture::new(
        &server,
        OnlineOptions {
            max_input_bytes: 512,
            ..OnlineOptions::default()
        },
        true,
        false,
    );
    assert_eq!(
        exact_scan(&mismatch.ticket, &query, &vectors, 0.0, 2).unwrap_err(),
        SearchError::ProfileMismatch
    );
}

#[tokio::test]
async fn exact_scan_does_not_fill_below_threshold_with_unrelated_candidates() {
    let server = Server::new(vec![
        query_reply(),
        embeddings(json!([
            {"index":0,"embedding":[-3,-4]},{"index":1,"embedding":[-4,3]}
        ])),
    ]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    let query = fixture
        .embedding
        .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
        .await
        .unwrap();
    let vectors = fixture
        .embedding
        .embed_entries(
            &mut fixture.host,
            &mut fixture.ticket,
            &fixture.entries,
            &control(),
        )
        .await
        .unwrap();
    assert!(
        exact_scan(&fixture.ticket, &query, &vectors, 0.8, 5)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn endpoint_sources_scope_query_flag_and_query_proof_are_separate_gates() {
    for case in 0..5 {
        let server = Server::new(vec![]);
        let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
        let endpoint = if case == 0 {
            format!("{}/other/embeddings", server.endpoint)
        } else {
            format!("{}/v1/embeddings", server.endpoint)
        };
        let sources = if case == 1 {
            vec![]
        } else {
            fixture.ticket.query_proof().sources().to_vec()
        };
        let scope = if case == 2 {
            MemoryScope::Shared(fixture.host.owner.namespace().clone())
        } else {
            MemoryScope::Private(fixture.host.owner.clone())
        };
        let mut policy =
            TransmissionPolicy::new(2, TransmissionUsage::QueryEmbedding, endpoint, sources)
                .unwrap()
                .with_scope(scope);
        if case != 3 {
            policy = policy.transmit_query()
        }
        fixture
            .host
            .service
            .set_transmission_policy(&fixture.host.owner, policy)
            .unwrap();
        fixture.renew_ticket(case != 4);
        assert_eq!(
            fixture
                .embedding
                .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
                .await
                .unwrap_err(),
            SearchError::Core(MemoryError::TransmissionDenied)
        );
        assert_eq!(server.count(), 0);
    }
}

#[tokio::test]
async fn core_transmission_quota_bounds_combined_online_calls() {
    let server = Server::new((0..9).map(|_| query_reply()).collect());
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    for _attempt in 0..8 {
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap();
    }
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::Core(MemoryError::CapacityExceeded)
    );
    assert_eq!(server.count(), 8);
}

#[tokio::test]
async fn quota_denied_during_retry_audits_failure_and_blocks_delivery() {
    let mut replies: Vec<_> = (0..7).map(|_| query_reply()).collect();
    replies.push(Reply::status(429));
    replies.push(query_reply());
    let server = Server::new(replies);
    let options = OnlineOptions {
        max_attempts: 2,
        retry_delay: Duration::ZERO,
        ..OnlineOptions::default()
    };
    let mut fixture = Fixture::new(&server, options, true, false);
    for _attempt in 0..7 {
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap();
    }
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::Core(MemoryError::CapacityExceeded)
    );
    assert_eq!(server.count(), 8);
    assert_eq!(fixture.ticket.transmissions().len(), 8);
    fixture.assert_closed();
    assert_eq!(
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap_err(),
        SearchError::Core(MemoryError::TaskCancelled)
    );
    assert_eq!(server.count(), 8);
}

#[tokio::test]
async fn constant_embedding_probe_evidence_is_rejected() {
    let server = Server::new(vec![
        embeddings(json!([{"index":0,"embedding":[1,0]}])),
        embeddings(json!([{"index":0,"embedding":[1,0]},{"index":1,"embedding":[1,0]}])),
    ]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    let query = fixture
        .embedding
        .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
        .await
        .unwrap();
    let vectors = fixture
        .embedding
        .embed_entries(
            &mut fixture.host,
            &mut fixture.ticket,
            &fixture.entries,
            &control(),
        )
        .await
        .unwrap();
    let ranked = exact_scan(&fixture.ticket, &query, &vectors, -1.0, 2).unwrap();
    assert_eq!(ranked[0].0, fixture.entries[0]);
    let evidence: Vec<_> = ranked
        .iter()
        .map(|(reference, score)| (reference == &fixture.entries[0], *score))
        .collect();
    assert!(!probe_preference::strictly_prefers_expected(&evidence));
}

#[tokio::test]
async fn equal_rerank_probe_scores_are_rejected() {
    let server = Server::new(vec![Reply::json(
        json!({"results":[{"index":0,"relevance_score":0.5},{"index":1,"relevance_score":0.5}]}),
    )]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let ranked = fixture
        .reranker
        .rerank(
            &mut fixture.host,
            &mut fixture.ticket,
            &fixture.entries,
            &control(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ranked[0].0, fixture.entries[0]);
    let evidence: Vec<_> = ranked
        .iter()
        .map(|(reference, score)| (reference == &fixture.entries[0], *score))
        .collect();
    assert!(!probe_preference::strictly_prefers_expected(&evidence));
}

#[test]
fn probe_requires_a_unique_expected_winner_over_a_real_distractor() {
    assert!(probe_preference::strictly_prefers_expected(&[
        (true, 0.9),
        (false, 0.1)
    ]));
    for ranked in [
        vec![],
        vec![(true, 0.9)],
        vec![(true, 0.5), (false, 0.5)],
        vec![(false, 0.9), (true, 0.1)],
        vec![(true, f64::NAN), (false, 0.1)],
        vec![(true, 0.9), (false, f64::INFINITY)],
        vec![(true, 0.9), (true, 0.1)],
    ] {
        assert!(!probe_preference::strictly_prefers_expected(&ranked));
    }
}

fn r2_index(fixture: &Fixture) -> moe_memory_search::SearchIndex {
    moe_memory_search::SearchIndex::new(
        moe_memory_search::StorageDomain::new("synthetic-root").unwrap(),
        fixture.embedding.profile().clone(),
        moe_memory_search::ChunkConfig::new(1, 512, 16, 1024).unwrap(),
        16,
        32,
        256,
    )
    .unwrap()
}

fn r2_config(
    fixture: &Fixture,
    mode: moe_memory_search::RerankMode,
) -> moe_memory_search::HybridConfig {
    moe_memory_search::HybridConfig::new(
        fixture.embedding.profile().clone(),
        1,
        0.7,
        true,
        mode,
        20,
    )
    .unwrap()
}

fn r2_replies(final_reply: Option<Reply>) -> Vec<Reply> {
    let mut replies = vec![
        embeddings(json!([{"index":0,"embedding":[1.0,0.0]}])),
        embeddings(json!([{"index":0,"embedding":[1.0,0.0]},{"index":1,"embedding":[0.8,0.6]}])),
    ];
    if let Some(reply) = final_reply {
        replies.push(reply);
    }
    replies
}

async fn r2_prepare(
    fixture: &mut Fixture,
    index: &mut moe_memory_search::SearchIndex,
    config: &moe_memory_search::HybridConfig,
) -> (
    moe_memory_search::EncodedQuery,
    moe_memory_search::HybridRanking,
) {
    let query = fixture
        .embedding
        .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
        .await
        .unwrap();
    let batch = index
        .take_batch(&mut fixture.host, &fixture.ticket)
        .unwrap()
        .unwrap();
    let completed = fixture
        .embedding
        .embed_index_batch(&mut fixture.host, &mut fixture.ticket, batch, &control())
        .await
        .unwrap();
    index
        .commit(&mut fixture.host, &fixture.ticket, completed)
        .unwrap();
    let ranking = moe_memory_search::hybrid_rank(
        index,
        &mut fixture.host,
        &fixture.ticket,
        Some(&query),
        config,
    )
    .unwrap();
    (query, ranking)
}

#[tokio::test]
async fn r2_disabled_rerank_delivers_rrf_without_extra_request() {
    let server = Server::new(r2_replies(None));
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::Disabled);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    assert_eq!(ranking.lexical_hits(), 0);
    assert_eq!(ranking.vector_hits(), 2);
    let ranked = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        None,
        &control(),
    )
    .await
    .unwrap();
    let task = fixture
        .host
        .service
        .for_session(
            &fixture.host.owner,
            ranked.ticket().session(),
            ReadPurpose::OfflineReasoning,
        )
        .complete_retrieval(ranked.ticket(), ranked.result().ranking(), 100)
        .unwrap();
    assert_eq!(task.result().items().len(), 2);
    assert_eq!(server.count(), 2);
    assert_eq!(ranked.ticket().transmissions().len(), 2);
}

#[tokio::test]
async fn r2_rerank_failure_creates_delivery_only_ticket_and_preserves_all_sends() {
    for reply in [
        Reply::status(503),
        Reply::json(
            json!({"results":[{"index":0,"relevance_score":0.5},{"index":0,"relevance_score":0.2}]}),
        ),
    ] {
        let server = Server::new(r2_replies(Some(reply)));
        let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
        let mut index = r2_index(&fixture);
        let config = r2_config(&fixture, moe_memory_search::RerankMode::AllowFallback);
        let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
        let old_runtime = fixture.ticket.runtime();
        let old_refs = fixture.entries.clone();
        let ranked = moe_memory_search::rerank_or_fallback(
            &index,
            &mut fixture.host,
            fixture.ticket,
            ranking,
            Some(&query),
            Some(&fixture.reranker),
            &control(),
        )
        .await
        .unwrap();
        assert_ne!(ranked.ticket().runtime(), old_runtime);
        assert!(ranked.ticket().candidate(&old_refs[0]).is_none());
        assert!(
            ranked
                .result()
                .status()
                .reasons
                .contains(&moe_memory_search::SearchReason::RerankUnavailable)
        );
        let evidence = ranked.ticket().transmissions();
        assert_eq!(evidence.len(), 3);
        assert_eq!(evidence[0].usage(), TransmissionUsage::QueryEmbedding);
        assert_eq!(evidence[1].usage(), TransmissionUsage::EntryEmbedding);
        assert_eq!(evidence[2].usage(), TransmissionUsage::Rerank);
        assert_eq!(evidence[2].sent_entries().len(), 2);
        assert!(
            fixture
                .host
                .service
                .prepare_transmission(
                    ranked.ticket(),
                    TransmissionUsage::Rerank,
                    &format!("{}/v2/rerank", server.endpoint),
                    &[ranked.result().ranking()[0].0.clone()],
                    100
                )
                .is_err()
        );
        let task = fixture
            .host
            .service
            .for_session(
                &fixture.host.owner,
                ranked.ticket().session(),
                ReadPurpose::OfflineReasoning,
            )
            .complete_retrieval(ranked.ticket(), &ranked.result().ranking()[..1], 100)
            .unwrap();
        fixture.host.service.validate_task(&task, 100).unwrap();
        assert_eq!(task.transmissions().len(), 3);
        assert_eq!(server.count(), 3);
    }
}

#[tokio::test]
async fn r2_required_rerank_never_silently_falls_back() {
    let server = Server::new(r2_replies(Some(Reply::status(503))));
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::Required);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    let result = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        Some(&fixture.reranker),
        &control(),
    )
    .await;
    assert!(matches!(
        result.unwrap_err().error(),
        SearchError::HttpStatus(503)
    ));
    assert_eq!(server.count(), 3);
}

#[tokio::test]
async fn r2_preflight_transmission_denial_can_keep_rrf_without_sending() {
    let server = Server::new(r2_replies(None));
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    fixture
        .host
        .service
        .set_transmission_policy(
            &fixture.host.owner,
            TransmissionPolicy::deny_all(2, TransmissionUsage::Rerank),
        )
        .unwrap();
    fixture.renew_ticket(true);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::AllowFallback);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    let ranked = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        Some(&fixture.reranker),
        &control(),
    )
    .await
    .unwrap();
    assert!(
        ranked
            .result()
            .status()
            .reasons
            .contains(&moe_memory_search::SearchReason::TransmissionDenied)
    );
    assert_eq!(ranked.ticket().transmissions().len(), 2);
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn r2_retry_quota_failure_retains_eighth_receipt_and_never_recovers() {
    let mut replies = r2_replies(None);
    for _ in 0..5 {
        replies.push(query_reply());
    }
    let mut limited = Reply::status(429);
    limited.headers = "Retry-After: 0\r\n".into();
    replies.push(limited);
    let server = Server::new(replies);
    let mut fixture = Fixture::new(
        &server,
        OnlineOptions {
            max_attempts: 2,
            ..OnlineOptions::default()
        },
        true,
        true,
    );
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::AllowFallback);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    for _ in 0..5 {
        fixture
            .embedding
            .embed_query(&mut fixture.host, &mut fixture.ticket, &control())
            .await
            .unwrap();
    }
    let failure = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        Some(&fixture.reranker),
        &control(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        failure.error(),
        SearchError::Core(MemoryError::CapacityExceeded)
    ));
    assert_eq!(failure.ticket().transmissions().len(), 8);
    assert_eq!(
        failure.ticket().transmissions().last().unwrap().usage(),
        TransmissionUsage::Rerank
    );
    assert!(
        fixture
            .host
            .service
            .for_session(
                &fixture.host.owner,
                failure.ticket().session(),
                ReadPurpose::OfflineReasoning
            )
            .complete_retrieval(failure.ticket(), &[], 100)
            .is_err()
    );
    assert_eq!(server.count(), 8);
}

#[tokio::test]
async fn r2_inflight_permission_revocation_keeps_failure_evidence_and_blocks_fallback() {
    let server = Server::new(r2_replies(Some(Reply::status(503))));
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::AllowFallback);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    fixture.host.revoke_at = Some(fixture.host.checkpoints + 3);
    let failure = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        Some(&fixture.reranker),
        &control(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        failure.error(),
        SearchError::Core(MemoryError::TaskCancelled)
    ));
    assert_eq!(failure.ticket().transmissions().len(), 3);
    assert_eq!(server.count(), 3);
}

#[tokio::test]
async fn r2_successful_rerank_reorders_only_qualified_record_shortlist() {
    let server = Server::new(r2_replies(Some(Reply::json(
        json!({"results":[{"index":1,"relevance_score":0.9},{"index":0,"relevance_score":0.1}]}),
    ))));
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::Required);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    let original_second = ranking.ranking()[1].0.clone();
    let ranked = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        Some(&fixture.reranker),
        &control(),
    )
    .await
    .unwrap();
    assert_eq!(ranked.result().ranking()[0].0, original_second);
    assert_eq!(ranked.result().ranking().len(), 2);
    assert_eq!(ranked.ticket().transmissions().len(), 3);
}

#[tokio::test]
async fn r2_long_record_sends_bounded_slices_but_delivers_complete_parent_body() {
    let long = format!("{}tea tail", "padding ".repeat(180));
    let config = moe_memory_search::ChunkConfig::new(1, 512, 16, 1024).unwrap();
    let count = moe_memory_search::split_chunks(&long, &config)
        .unwrap()
        .len()
        + 1;
    let data: Vec<_> = (0..count)
        .map(|position| json!({"index":position,"embedding":[1.0,0.0]}))
        .collect();
    let server = Server::new(vec![
        embeddings(json!([{"index":0,"embedding":[1.0,0.0]}])),
        embeddings(json!(data)),
        Reply::json(
            json!({"results":[{"index":0,"relevance_score":0.9},{"index":1,"relevance_score":0.1}]}),
        ),
    ]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let record = fixture.entries[0].record().unwrap();
    let source = fixture.ticket.candidates()[0].sources()[0].clone();
    fixture
        .host
        .service
        .for_pet(&fixture.host.owner)
        .update(
            record.id(),
            record.revision(),
            MemoryDraft::new(
                &long,
                Category::Episode,
                Epistemic::UserStatement,
                vec![source],
            ),
            100,
        )
        .unwrap();
    fixture.renew_ticket(true);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::Required);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    let ranked = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        Some(&fixture.reranker),
        &control(),
    )
    .await
    .unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[1].body["input"].as_array().unwrap().len(), count);
    for input in requests[1].body["input"]
        .as_array()
        .unwrap()
        .iter()
        .chain(requests[2].body["documents"].as_array().unwrap())
    {
        assert!(input.as_str().unwrap().len() <= 1024);
        assert_ne!(input.as_str().unwrap(), long);
    }
    let task = fixture
        .host
        .service
        .for_session(
            &fixture.host.owner,
            ranked.ticket().session(),
            ReadPurpose::OfflineReasoning,
        )
        .complete_retrieval(ranked.ticket(), ranked.result().ranking(), 100)
        .unwrap();
    assert!(task.result().items().iter().any(|item| item.text() == long));
    assert_eq!(task.transmissions()[1].sent_entries().len(), 2);
}

#[tokio::test]
async fn r2_cancellation_and_overall_deadline_do_not_recover_rerank_failure() {
    for cancelled in [true, false] {
        let mut slow = Reply::status(503);
        slow.delay = Duration::from_millis(100);
        let server = Server::new(r2_replies(Some(slow)));
        let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
        let mut index = r2_index(&fixture);
        let config = r2_config(&fixture, moe_memory_search::RerankMode::AllowFallback);
        let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
        let cancellation = Cancellation::default();
        let call = CallControl::new(
            cancellation.clone(),
            if cancelled {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(20)
            },
        )
        .unwrap();
        let count = server.count.clone();
        let trigger = async {
            if cancelled {
                while count.load(Ordering::SeqCst) < 3 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                cancellation.cancel();
            }
        };
        let (result, ()) = tokio::join!(
            moe_memory_search::rerank_or_fallback(
                &index,
                &mut fixture.host,
                fixture.ticket,
                ranking,
                Some(&query),
                Some(&fixture.reranker),
                &call
            ),
            trigger
        );
        assert!(matches!(
            result.unwrap_err().error(),
            SearchError::Cancelled | SearchError::DeadlineExceeded
        ));
    }
}

#[tokio::test]
async fn r2_short_request_timeout_can_fallback_but_keeps_unknown_send_evidence() {
    let mut slow = Reply::status(503);
    slow.delay = Duration::from_millis(250);
    let server = Server::new(r2_replies(Some(slow)));
    let options = OnlineOptions {
        request_timeout: Duration::from_millis(80),
        ..OnlineOptions::default()
    };
    let mut fixture =
        Fixture::with_rerank_options(&server, OnlineOptions::default(), options, true, true);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::AllowFallback);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    let ranked = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        Some(&fixture.reranker),
        &control(),
    )
    .await
    .unwrap();
    assert_eq!(ranked.ticket().transmissions().len(), 3);
    assert!(
        ranked
            .result()
            .status()
            .reasons
            .contains(&moe_memory_search::SearchReason::RerankUnavailable)
    );
    assert_eq!(server.count(), 3);
}

#[tokio::test]
async fn r2_unavailable_reranker_falls_back_only_before_sending_and_with_valid_authority() {
    let server = Server::new(r2_replies(None));
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, true);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::AllowFallback);
    let (query, ranking) = r2_prepare(&mut fixture, &mut index, &config).await;
    let runtime = fixture.ticket.runtime();
    let ranked = moe_memory_search::rerank_or_fallback(
        &index,
        &mut fixture.host,
        fixture.ticket,
        ranking,
        Some(&query),
        None,
        &control(),
    )
    .await
    .unwrap();
    assert_eq!(ranked.ticket().runtime(), runtime);
    assert_eq!(ranked.ticket().transmissions().len(), 2);
    assert!(
        ranked
            .result()
            .status()
            .reasons
            .contains(&moe_memory_search::SearchReason::RerankUnavailable)
    );
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn r2_foreign_ranking_cannot_be_rebound_even_when_results_are_empty() {
    let server = Server::new(vec![]);
    let mut fixture = Fixture::new(&server, OnlineOptions::default(), true, false);
    let mut index = r2_index(&fixture);
    let config = r2_config(&fixture, moe_memory_search::RerankMode::Disabled);
    let ranking = moe_memory_search::hybrid_rank(
        &mut index,
        &mut fixture.host,
        &fixture.ticket,
        None,
        &config,
    )
    .unwrap();
    assert!(ranking.ranking().is_empty());
    fixture.renew_ticket(true);
    assert!(
        moe_memory_search::rerank_or_fallback(
            &index,
            &mut fixture.host,
            fixture.ticket,
            ranking,
            None,
            None,
            &control()
        )
        .await
        .is_err()
    );
    assert_eq!(server.count(), 0);
}
