use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use moe_memory_search::{CohereReranker, OnlineEmbedding, OnlineOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[path = "../examples/support/quality.rs"]
mod quality;
#[path = "../examples/support/quality_runner.rs"]
mod quality_runner;

const DIMENSION: usize = 64;

struct Loopback {
    endpoint: String,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Loopback {
    async fn start(constant: bool, fail_rerank: bool) -> Self {
        let fixture = quality::load().unwrap();
        let record_ids: BTreeMap<_, _> = fixture
            .records
            .iter()
            .chain(&fixture.calibration_records)
            .enumerate()
            .map(|(index, record)| (record.id.clone(), index))
            .collect();
        let mut positions: BTreeMap<_, _> = fixture
            .records
            .iter()
            .chain(&fixture.calibration_records)
            .map(|record| (record.text.clone(), record_ids[&record.id]))
            .collect();
        for case in fixture.evaluation.iter().chain(&fixture.calibration) {
            positions.insert(
                case.text.clone(),
                case.relevant
                    .keys()
                    .next()
                    .map(|id| record_ids[id])
                    .unwrap_or(DIMENSION - 1),
            );
        }
        let approved: BTreeSet<_> = positions.keys().cloned().collect();
        let positions = Arc::new(positions);
        let approved = Arc::new(approved);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let served = calls.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (path, request) = read_request(&mut stream).await;
                served.fetch_add(1, Ordering::SeqCst);
                let body = if path == "/embeddings" {
                    assert_eq!(request["model"], "manual-quality-embedding");
                    let inputs = request["input"].as_array().unwrap();
                    assert!((1..=32).contains(&inputs.len()));
                    let data: Vec<_> = inputs
                        .iter()
                        .enumerate()
                        .map(|(index, input)| {
                            let text = input.as_str().unwrap();
                            assert!(approved.contains(text));
                            let position = if constant { 0 } else { positions[text] };
                            let mut vector = vec![0.0; DIMENSION];
                            vector[position] = 1.0;
                            json!({"index":index,"embedding":vector})
                        })
                        .collect();
                    json!({"model":"manual-quality-embedding","data":data})
                } else {
                    assert_eq!(path, "/v2/rerank");
                    let query = request["query"].as_str().unwrap();
                    assert!(approved.contains(query));
                    let documents = request["documents"].as_array().unwrap();
                    let results: Vec<_> = documents.iter().enumerate().map(|(index, document)| {
                        let text = document.as_str().unwrap();
                        assert!(approved.contains(text));
                        json!({"index": index,"relevance_score": if positions[text] == positions[query] { 1.0 } else { 0.0 }})
                    }).collect();
                    json!({"results":results})
                };
                let status = if fail_rerank && path == "/v2/rerank" {
                    "503 Service Unavailable"
                } else {
                    "200 OK"
                };
                let body = body.to_string();
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        Self {
            endpoint,
            calls,
            task,
        }
    }

    fn embedding(&self) -> OnlineEmbedding {
        OnlineEmbedding::new(
            &format!("{}/embeddings", self.endpoint),
            "synthetic-only",
            "manual-quality-embedding",
            DIMENSION,
            OnlineOptions::default(),
        )
        .unwrap()
    }

    fn reranker(&self) -> CohereReranker {
        CohereReranker::new(
            &format!("{}/v2/rerank", self.endpoint),
            "synthetic-only",
            "manual-quality-rerank",
            OnlineOptions::default(),
        )
        .unwrap()
    }
}

async fn read_request(stream: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let head_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        assert!(bytes.len() < 16 * 1024);
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    };
    let head = std::str::from_utf8(&bytes[..head_end]).unwrap();
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer synthetic-only")
    );
    let path = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .unwrap()
        .1
        .trim()
        .parse::<usize>()
        .unwrap();
    assert!(length <= 256 * 1024);
    while bytes.len() < head_end + length {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    (
        path,
        serde_json::from_slice(&bytes[head_end..head_end + length]).unwrap(),
    )
}

#[tokio::test]
async fn manual_loopback_vectors_exercise_frozen_cold_hot_and_rerank_engineering_only() {
    let fixture = quality::load().unwrap();
    let server = Loopback::start(false, false).await;
    let embedding = server.embedding();
    let reranker = server.reranker();
    let endpoint = format!("{}/embeddings", server.endpoint);
    let rerank_endpoint = format!("{}/v2/rerank", server.endpoint);
    let mut calibration =
        quality_runner::World::new(&fixture.calibration_records, "loopback-calibration").unwrap();
    calibration.authorize(&endpoint, None).unwrap();
    let observations = calibration
        .calibration_scores(&fixture.calibration, &embedding)
        .await
        .unwrap();
    let threshold = quality::calibrate(&fixture, &observations).unwrap();
    assert_eq!(threshold, 1.0);
    assert_eq!(server.calls.load(Ordering::SeqCst), 24);
    let mut world = quality_runner::World::new(&fixture.records, "loopback-evaluation").unwrap();
    world.authorize(&endpoint, Some(&rerank_endpoint)).unwrap();
    let keyword = world
        .evaluate(
            &fixture.evaluation,
            &embedding,
            None,
            threshold,
            ("manual-KeywordOnly", true),
        )
        .await
        .unwrap();
    assert_eq!(server.calls.load(Ordering::SeqCst), 24);
    let hybrid = world
        .evaluate(
            &fixture.evaluation,
            &embedding,
            None,
            threshold,
            ("manual-Hybrid", false),
        )
        .await
        .unwrap();
    let reranked = world
        .evaluate(
            &fixture.evaluation,
            &embedding,
            Some(&reranker),
            threshold,
            ("manual-Hybrid+Rerank", false),
        )
        .await
        .unwrap();
    assert!(hybrid.cold.passes(&keyword.cold));
    assert!(hybrid.hot.passes(&keyword.hot));
    assert!(reranked.cold.passes(&keyword.cold));
    assert!(reranked.hot.passes(&keyword.hot));
    keyword.cold.print("manual-KeywordOnly-engineering-only");
    hybrid.cold.print("manual-Hybrid-engineering-only");
    reranked.cold.print("manual-Hybrid+Rerank-engineering-only");
    assert!(reranked.cold.ndcg_sum >= hybrid.cold.ndcg_sum);
}

#[tokio::test]
async fn constant_fixture_vectors_do_not_pass_the_frozen_quality_gate() {
    let fixture = quality::load().unwrap();
    let server = Loopback::start(true, false).await;
    let embedding = server.embedding();
    let mut world = quality_runner::World::new(&fixture.records, "constant-vector-test").unwrap();
    world
        .authorize(&format!("{}/embeddings", server.endpoint), None)
        .unwrap();
    let keyword = world
        .evaluate(
            &fixture.evaluation,
            &embedding,
            None,
            1.0,
            ("constant-KeywordOnly", true),
        )
        .await
        .unwrap();
    let hybrid = world
        .evaluate(
            &fixture.evaluation,
            &embedding,
            None,
            1.0,
            ("constant-Hybrid", false),
        )
        .await
        .unwrap();
    assert!(!hybrid.cold.passes(&keyword.cold));
    assert!(!hybrid.hot.passes(&keyword.hot));
}

#[tokio::test]
async fn required_rerank_failure_never_returns_a_degraded_quality_summary() {
    let fixture = quality::load().unwrap();
    let server = Loopback::start(false, true).await;
    let embedding = server.embedding();
    let reranker = server.reranker();
    let mut world = quality_runner::World::new(&fixture.records, "rerank-failure-test").unwrap();
    world
        .authorize(
            &format!("{}/embeddings", server.endpoint),
            Some(&format!("{}/v2/rerank", server.endpoint)),
        )
        .unwrap();
    let result = world
        .evaluate(
            &fixture.evaluation[..1],
            &embedding,
            Some(&reranker),
            1.0,
            ("manual-rerank-required-failure", false),
        )
        .await;
    assert!(result.is_err());
    assert!(result.err().unwrap().to_string().contains("503"));
    assert_eq!(server.calls.load(Ordering::SeqCst), 3);
}
