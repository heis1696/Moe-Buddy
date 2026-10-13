//! Synthetic-scale Hybrid benchmark over a real `SearchIndex`. A loopback-only
//! OpenAI-compatible endpoint (see `support/bench.rs`) returns deterministic
//! artificial vectors, and the index is filled strictly through the authorized
//! `take_batch` → `embed_index_batch` → `commit` path; no production raw-vector
//! entry exists or is used. Cold and hot Hybrid queries are then measured end
//! to end: ticket, query encoding, recall, core completion, and assembly.
//! Vectors are hash-clustered synthetic values, so results measure offline
//! throughput only, not semantic quality; no real model, dataset, outbound
//! traffic, or file cache is involved.

#[path = "support/bench.rs"]
mod bench;

use std::error::Error;
use std::mem::size_of;
use std::time::{Duration, Instant};

use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryScope, MemoryService, Namespace, PetIdentity,
    ReadPolicy, ReadPurpose, RetrievalLayer, RetrievalPlan, RetrievalQuery, SearchScope, SessionId,
    SourceRef, TransmissionPolicy, TransmissionUsage,
};
use moe_memory_search::{
    CallControl, Cancellation, ChunkConfig, HybridConfig, OnlineEmbedding, OnlineOptions,
    RerankMode, SearchHost, SearchIndex, SearchMode, StorageDomain, hybrid_rank,
};

use bench::{LoopbackEmbeddingServer, VECTOR_DIMENSION};

const NOW_MS: u64 = 1_000_000;
/// Transmission receipts the core authority allows per ticket; a fresh ticket
/// continues the batch fill once they are exhausted.
const MAX_SENDS_PER_TICKET: usize = 8;
const INDEX_QUEUE_CAPACITY: usize = 4_096;
const INDEX_BATCH_LIMIT: usize = 32;
const INDEX_VECTOR_CAPACITY: usize = 65_536;
const MINIMUM_COSINE: f64 = 0.8;
const HOT_QUERY_SAMPLES: usize = 7;
/// Explicit overall loopback budget, one fresh budget per phase.
const PHASE_BUDGET: Duration = Duration::from_secs(120);
const DEFAULT_SCALES: &[usize] = &[1_000, 10_000];

struct Host {
    memory: MemoryService,
}

impl SearchHost for Host {
    fn memory(&mut self) -> &mut MemoryService {
        &mut self.memory
    }

    fn now_ms(&mut self) -> u64 {
        NOW_MS
    }
}

struct QueryReport {
    total: Duration,
    ticket: Duration,
    encode: Duration,
    recall: Duration,
    completion: Duration,
    assembly: Duration,
    mode: SearchMode,
    vector_hits: usize,
    lexical_hits: usize,
    top5: Vec<String>,
}

struct QueryFixture<'a> {
    host: &'a mut Host,
    index: &'a mut SearchIndex,
    embedding: &'a OnlineEmbedding,
    control: &'a CallControl,
    owner: &'a PetIdentity,
    session: &'a SessionId,
    source: &'a SourceRef,
}

fn build_authority(
    count: usize,
    endpoint: &str,
) -> Result<(Host, PetIdentity, SessionId, SourceRef), Box<dyn Error>> {
    let mut memory = MemoryService::new();
    let owner = PetIdentity::new(
        Namespace::new("scan-bench", "synthetic-user")?,
        "synthetic-pet",
    )?;
    let scope = MemoryScope::Private(owner.clone());
    let source = memory.register_source(
        scope.clone(),
        "synthetic-benchmark-fixture",
        1,
        Epistemic::UserStatement,
    )?;

    for record_id in 0..count {
        let text =
            format!("synthetic benchmark record {record_id:05}: jasmine tea preference sample");
        memory.for_pet(&owner).create(
            MemoryDraft::new(
                text,
                Category::Episode,
                Epistemic::UserStatement,
                vec![source.clone()],
            ),
            NOW_MS,
        )?;
    }
    memory.set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))?;
    memory.set_transmission_policy(
        &owner,
        TransmissionPolicy::new(
            1,
            TransmissionUsage::QueryEmbedding,
            endpoint,
            vec![source.clone()],
        )?
        .with_scope(scope.clone())
        .transmit_query(),
    )?;
    memory.set_transmission_policy(
        &owner,
        TransmissionPolicy::new(
            1,
            TransmissionUsage::EntryEmbedding,
            endpoint,
            vec![source.clone()],
        )?
        .with_scope(scope),
    )?;

    Ok((
        Host { memory },
        owner,
        SessionId::new("scan-bench-session")?,
        source,
    ))
}

async fn run_query(
    fixture: QueryFixture<'_>,
    query: &RetrievalQuery,
    plan: &RetrievalPlan,
    config: &HybridConfig,
) -> Result<QueryReport, Box<dyn Error>> {
    let QueryFixture {
        host,
        index,
        embedding,
        control,
        owner,
        session,
        source,
    } = fixture;
    let started = Instant::now();
    let ticket_started = Instant::now();
    let mut ticket = host
        .memory
        .for_session(owner, session, ReadPurpose::OfflineReasoning)
        .begin_candidates(query, plan.clone(), NOW_MS)?;
    host.memory
        .bind_query_provenance(&mut ticket, vec![source.clone()], &[], NOW_MS)?;
    let ticket_elapsed = ticket_started.elapsed();

    let encode_started = Instant::now();
    let encoded = embedding.embed_query(host, &mut ticket, control).await?;
    let encode_elapsed = encode_started.elapsed();

    let recall_started = Instant::now();
    let ranking = hybrid_rank(index, host, &ticket, Some(&encoded), config)?;
    let ranked = ranking.ranking().to_vec();
    let mode = ranking.mode();
    let vector_hits = ranking.vector_hits();
    let lexical_hits = ranking.lexical_hits();
    let recall_elapsed = recall_started.elapsed();

    let completion_started = Instant::now();
    let task = host
        .memory
        .for_session(owner, session, ReadPurpose::OfflineReasoning)
        .complete_retrieval(&ticket, &ranked, NOW_MS)?;
    let completion_elapsed = completion_started.elapsed();
    let top5 = task
        .result()
        .items()
        .iter()
        .take(5)
        .map(|item| item.text().to_owned())
        .collect();

    let assembly_started = Instant::now();
    let context = host.memory.assemble_context(&task, 16_384, NOW_MS)?;
    host.memory.context_view(&context, NOW_MS)?;
    let assembly_elapsed = assembly_started.elapsed();

    Ok(QueryReport {
        total: started.elapsed(),
        ticket: ticket_elapsed,
        encode: encode_elapsed,
        recall: recall_elapsed,
        completion: completion_elapsed,
        assembly: assembly_elapsed,
        mode,
        vector_hits,
        lexical_hits,
        top5,
    })
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn percentile_ms(samples: &[Duration], fraction: f64) -> f64 {
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort();
    let rank = ((samples.len() as f64 * fraction).ceil() as usize).clamp(1, samples.len());
    millis(sorted[rank - 1])
}

async fn run_case(count: usize) -> Result<(), Box<dyn Error>> {
    let case_started = Instant::now();
    let authority_started = Instant::now();
    let server = LoopbackEmbeddingServer::bind("synthetic-bench-embedding").await?;
    let endpoint = server.endpoint();
    let embedding = OnlineEmbedding::new(
        &endpoint,
        "synthetic-bench-loopback",
        "synthetic-bench-embedding",
        VECTOR_DIMENSION,
        OnlineOptions::default(),
    )?;
    let profile = embedding.profile().clone();
    let (mut host, owner, session, source) = build_authority(count, &endpoint)?;
    let authority_elapsed = authority_started.elapsed();

    let chunk_config = ChunkConfig::new(1, 256, 16, 1_024)?;
    let config = HybridConfig::new(
        profile.clone(),
        1,
        MINIMUM_COSINE,
        true,
        RerankMode::Disabled,
        20,
    )?;
    let mut index = SearchIndex::new(
        StorageDomain::new("synthetic-scan-bench")?,
        profile.clone(),
        chunk_config,
        INDEX_QUEUE_CAPACITY,
        INDEX_BATCH_LIMIT,
        INDEX_VECTOR_CAPACITY,
    )?;
    let plan = RetrievalPlan::new(profile);
    let query = RetrievalQuery::new("jasmine tea")
        .with_scope(SearchScope::Private)
        .with_layer(RetrievalLayer::LongTerm)
        .with_limit(5);
    let fill_started = Instant::now();
    let mut fill_encoding = Duration::ZERO;
    let mut fill_tickets = 0usize;
    let mut fill_batches = 0usize;
    let final_status = loop {
        let mut ticket = host
            .memory
            .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
            .begin_candidates(&query, plan.clone(), NOW_MS)?;
        let status = index.reconcile(&mut host, &ticket)?;
        if status.missing_chunks == 0 {
            break status;
        }
        fill_tickets += 1;
        let fill_control = CallControl::new(Cancellation::default(), PHASE_BUDGET)?;
        let mut ticket_batches = 0usize;
        for _ in 0..MAX_SENDS_PER_TICKET {
            let Some(batch) = index.take_batch(&mut host, &ticket)? else {
                break;
            };
            let encode_started = Instant::now();
            let completed = embedding
                .embed_index_batch(&mut host, &mut ticket, batch, &fill_control)
                .await?;
            fill_encoding += encode_started.elapsed();
            index.commit(&mut host, &ticket, completed)?;
            ticket_batches += 1;
        }
        fill_batches += ticket_batches;
        if ticket_batches == 0 {
            return Err(std::io::Error::other("index queue stalled before completion").into());
        }
    };
    let fill_elapsed = fill_started.elapsed();
    if final_status.indexed_records != count || final_status.queued_records != 0 {
        return Err(std::io::Error::other(format!(
            "index fill incomplete: indexed={} missing={} queued={}",
            final_status.indexed_records, final_status.missing_chunks, final_status.queued_records
        ))
        .into());
    }

    let query_control = CallControl::new(Cancellation::default(), PHASE_BUDGET)?;
    let cold = run_query(
        QueryFixture {
            host: &mut host,
            index: &mut index,
            embedding: &embedding,
            control: &query_control,
            owner: &owner,
            session: &session,
            source: &source,
        },
        &query,
        &plan,
        &config,
    )
    .await?;
    let mut hot = Vec::with_capacity(HOT_QUERY_SAMPLES);
    for _ in 0..HOT_QUERY_SAMPLES {
        hot.push(
            run_query(
                QueryFixture {
                    host: &mut host,
                    index: &mut index,
                    embedding: &embedding,
                    control: &query_control,
                    owner: &owner,
                    session: &session,
                    source: &source,
                },
                &query,
                &plan,
                &config,
            )
            .await?,
        );
    }
    if cold.mode != SearchMode::Hybrid
        || hot
            .iter()
            .any(|report| report.mode != SearchMode::Hybrid || report.top5 != cold.top5)
    {
        return Err(std::io::Error::other("hybrid queries were not deterministic").into());
    }

    let hot_total: Vec<Duration> = hot.iter().map(|report| report.total).collect();
    let hot_ticket: Vec<Duration> = hot.iter().map(|report| report.ticket).collect();
    let hot_encode: Vec<Duration> = hot.iter().map(|report| report.encode).collect();
    let hot_recall: Vec<Duration> = hot.iter().map(|report| report.recall).collect();
    let hot_completion: Vec<Duration> = hot.iter().map(|report| report.completion).collect();
    let hot_assembly: Vec<Duration> = hot.iter().map(|report| report.assembly).collect();
    let vector_payload_bytes = count * VECTOR_DIMENSION * size_of::<f64>();

    println!("=== loopback hybrid benchmark: records={count} ===");
    println!(
        "authority=MemoryService(in-memory) authority_build_ms={:.3} authorized_candidates={count}",
        millis(authority_elapsed),
    );
    println!(
        "loopback_endpoint=127.0.0.1/embeddings dimension={VECTOR_DIMENSION} vectors=hash_clustered_artificial server_requests={} server_vectors={}",
        server.requests(),
        server.vectors(),
    );
    println!(
        "index_fill tickets={fill_tickets} batches={fill_batches} total_ms={:.3} loopback_encoding_ms={:.3} core_index_bookkeeping_ms={:.3} bookkeeping_note=\"includes per-batch reconcile over the full authorized candidate snapshot (O(n) per batch, n={count})\"",
        millis(fill_elapsed),
        millis(fill_encoding),
        millis(fill_elapsed - fill_encoding),
    );
    println!(
        "final_status indexed_records={} missing_chunks=0 queued_records=0 fill_note=\"queue capacity {INDEX_QUEUE_CAPACITY} drained across tickets; completeness asserted, no partial delivery\"",
        final_status.indexed_records,
    );
    println!(
        "index_logical_vector_bytes={vector_payload_bytes} formula=\"{count} records × 1 chunk × {VECTOR_DIMENSION} dims × {} B\" rss=not_measured file_cache=disabled(unmeasured)",
        size_of::<f64>(),
    );
    println!(
        "cold_query e2e_ms={:.3} ticket_ms={:.3} query_encode_ms={:.3} recall_ms={:.3} core_completion_ms={:.3} assembly_ms={:.3} mode={:?} vector_hits={} lexical_hits={} top5={:?}",
        millis(cold.total),
        millis(cold.ticket),
        millis(cold.encode),
        millis(cold.recall),
        millis(cold.completion),
        millis(cold.assembly),
        cold.mode,
        cold.vector_hits,
        cold.lexical_hits,
        cold.top5,
    );
    println!(
        "hot_query samples={HOT_QUERY_SAMPLES} e2e_p50_ms={:.3} e2e_p95_ms={:.3} ticket_p50_ms={:.3} query_encode_p50_ms={:.3} recall_p50_ms={:.3} recall_p95_ms={:.3} completion_p50_ms={:.3} assembly_p50_ms={:.3} vector_hits={} lexical_hits={}",
        percentile_ms(&hot_total, 0.50),
        percentile_ms(&hot_total, 0.95),
        percentile_ms(&hot_ticket, 0.50),
        percentile_ms(&hot_encode, 0.50),
        percentile_ms(&hot_recall, 0.50),
        percentile_ms(&hot_recall, 0.95),
        percentile_ms(&hot_completion, 0.50),
        percentile_ms(&hot_assembly, 0.50),
        hot[0].vector_hits,
        hot[0].lexical_hits,
    );
    println!(
        "semantic_quality=not_measured real_embedding=unverified loopback_vectors_only=true rerank=disabled case_total_ms={:.3}",
        millis(case_started.elapsed()),
    );
    println!();
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    println!(
        "No outbound network or model assets: the embedding endpoint is an in-process 127.0.0.1 loopback returning deterministic artificial vectors; results measure offline throughput only."
    );
    let scales: Vec<usize> = match std::env::args().skip(1).collect::<Vec<_>>() {
        args if args.is_empty() => DEFAULT_SCALES.to_vec(),
        args => args
            .iter()
            .map(|scale| scale.parse::<usize>().map_err(|_| "invalid scale argument"))
            .collect::<Result<Vec<_>, _>>()?,
    };
    for count in &scales {
        run_case(*count).await?;
    }
    Ok(())
}
