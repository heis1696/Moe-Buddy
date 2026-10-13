//! Shared support for the offline `scan_bench` example: an in-process
//! loopback-only OpenAI-compatible `/embeddings` endpoint that returns
//! deterministic artificial vectors. No real model or dataset is involved;
//! nothing leaves 127.0.0.1 and the vectors measure encoding/scan throughput,
//! never semantic quality.

use std::io;
use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Artificial vector dimension shared by the benchmark profile and server.
pub const VECTOR_DIMENSION: usize = 32;
/// Deterministic cluster count in the synthetic vector space.
const CLUSTERS: u64 = 16;
/// Per-component noise around the cluster base vector.
const NOISE_AMPLITUDE: f64 = 0.05;
/// Component quantization step keeps JSON payloads compact.
const QUANTUM: f64 = 10_000.0;
const MAX_HEAD_BYTES: usize = 16 * 1024;

struct ServerState {
    model: String,
    requests: AtomicUsize,
    vectors: AtomicUsize,
}

/// Loopback HTTP server speaking just enough of the OpenAI-compatible
/// `/embeddings` protocol for the online adapter. It binds an ephemeral
/// 127.0.0.1 port and serves one request per connection (`connection: close`).
pub struct LoopbackEmbeddingServer {
    local_addr: SocketAddr,
    state: Arc<ServerState>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl Drop for LoopbackEmbeddingServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl LoopbackEmbeddingServer {
    /// Binds a loopback port and starts serving. Construction sends nothing.
    pub async fn bind(model: impl Into<String>) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let local_addr = listener.local_addr()?;
        let state = Arc::new(ServerState {
            model: model.into(),
            requests: AtomicUsize::new(0),
            vectors: AtomicUsize::new(0),
        });
        let accept_state = Arc::clone(&state);
        let accept_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = Arc::clone(&accept_state);
                tokio::spawn(handle_connection(stream, state));
            }
        });
        Ok(Self {
            local_addr,
            state,
            accept_task,
        })
    }

    /// Complete normalized endpoint URL, as the online adapter requires.
    pub fn endpoint(&self) -> String {
        format!("http://{}/embeddings", self.local_addr)
    }

    /// Total loopback `/embeddings` requests served.
    pub fn requests(&self) -> usize {
        self.state.requests.load(Ordering::Relaxed)
    }

    /// Total artificial vectors returned across all requests.
    pub fn vectors(&self) -> usize {
        self.state.vectors.load(Ordering::Relaxed)
    }
}

async fn handle_connection(mut stream: TcpStream, state: Arc<ServerState>) {
    let Ok(request) = read_request(&mut stream).await else {
        return;
    };
    state.requests.fetch_add(1, Ordering::Relaxed);
    let (status, body) = match embed_response(&state, &request) {
        Ok(body) => ("200 OK", body),
        Err(()) => (
            "400 Bad Request",
            json!({"error": {"message": "invalid embedding request"}}).to_string(),
        ),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body,
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> Result<Vec<u8>, io::Error> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized head"));
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated head",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&buffer[..head_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 head"))?;
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing content-length"))?;
    if content_length > 256 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized body"));
    }
    while buffer.len() < head_end + content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated body",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    buffer.truncate(head_end + content_length);
    buffer.drain(..head_end);
    Ok(buffer)
}

fn embed_response(state: &ServerState, body: &[u8]) -> Result<String, ()> {
    let request: Value = serde_json::from_slice(body).map_err(|_| ())?;
    let inputs = request["input"].as_array().ok_or(())?;
    if inputs.is_empty() || inputs.len() > 32 {
        return Err(());
    }
    let mut data = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        let text = input.as_str().ok_or(())?;
        data.push(json!({ "index": index, "embedding": synthetic_vector(text) }));
    }
    state.vectors.fetch_add(data.len(), Ordering::Relaxed);
    Ok(json!({ "model": state.model, "data": data }).to_string())
}

/// Deterministic artificial embedding: a hash-selected cluster base plus small
/// hash-driven noise. The same text always yields the same vector; no semantic
/// model, download, or randomness is involved.
pub fn synthetic_vector(text: &str) -> Vec<f64> {
    let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
    let cluster = u64::from_le_bytes(digest[0..8].try_into().expect("fixed-size slice")) % CLUSTERS;
    let mut base_seed = cluster.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1);
    let mut noise_seed = u64::from_le_bytes(digest[8..16].try_into().expect("fixed-size slice"));
    let mut vector = Vec::with_capacity(VECTOR_DIMENSION);
    for _ in 0..VECTOR_DIMENSION {
        base_seed = base_seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        noise_seed = noise_seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let base = ((base_seed >> 32) as f64 / u32::MAX as f64) * 2.0 - 1.0;
        let noise = (((noise_seed >> 32) as f64 / u32::MAX as f64) * 2.0 - 1.0) * NOISE_AMPLITUDE;
        vector.push(((base + noise) * QUANTUM).round() / QUANTUM);
    }
    vector
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
