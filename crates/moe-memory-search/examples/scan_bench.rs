use std::error::Error;
use std::mem::size_of;
use std::time::{Duration, Instant};

use moe_memory_core::{
    Category, EmbeddingProfile, Epistemic, MemoryDraft, MemoryScope, MemoryService, Namespace,
    PetIdentity, ReadPolicy, ReadPurpose, RetrievalLayer, RetrievalPlan, RetrievalQuery,
    SearchScope, SessionId,
};
use moe_memory_search::{
    ChunkConfig, HybridConfig, RerankMode, SearchHost, SearchIndex, StorageDomain, hybrid_rank,
};

const NOW_MS: u64 = 1_000_000;
const VECTOR_DIMENSION: usize = 32;

struct Host {
    memory: MemoryService,
    now_ms: u64,
}

impl SearchHost for Host {
    fn memory(&mut self) -> &mut MemoryService {
        &mut self.memory
    }

    fn now_ms(&mut self) -> u64 {
        self.now_ms
    }
}

struct QueryReport {
    total: Duration,
    ticket: Duration,
    retrieval: Duration,
    completion: Duration,
    assembly: Duration,
    mode: moe_memory_search::SearchMode,
    vector_hits: usize,
    top5: Vec<String>,
}

fn build_authority(count: usize) -> Result<(Host, PetIdentity, SessionId), Box<dyn Error>> {
    let mut memory = MemoryService::new();
    let owner = PetIdentity::new(
        Namespace::new("scan-bench", "synthetic-user")?,
        "synthetic-pet",
    )?;
    let source = memory.register_source(
        MemoryScope::Private(owner.clone()),
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
    memory.set_read_policy(&owner, ReadPolicy::offline(1, vec![source]))?;

    Ok((
        Host {
            memory,
            now_ms: NOW_MS,
        },
        owner,
        SessionId::new("scan-bench-session")?,
    ))
}

fn synthetic_unit_vector(state: &mut u64) -> Vec<f32> {
    let mut vector = Vec::with_capacity(VECTOR_DIMENSION);
    let mut norm_squared = 0.0_f64;
    for _ in 0..VECTOR_DIMENSION {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let component = (((*state >> 32) as u32 as f64 / u32::MAX as f64) * 2.0 - 1.0) as f32;
        norm_squared += f64::from(component) * f64::from(component);
        vector.push(component);
    }
    let norm = norm_squared.sqrt() as f32;
    for component in &mut vector {
        *component /= norm;
    }
    vector
}

fn encode_synthetic_vectors(count: usize) -> (Vec<Vec<f32>>, Vec<f32>, Duration) {
    let started = Instant::now();
    let mut state = 0x4d4f_452d_5343_414e_u64;
    let vectors: Vec<_> = (0..count)
        .map(|_| synthetic_unit_vector(&mut state))
        .collect();
    let query = vectors.last().expect("benchmark has records").clone();
    (vectors, query, started.elapsed())
}

fn scan_top5(query: &[f32], vectors: &[Vec<f32>]) -> Vec<(usize, f32)> {
    let mut scores: Vec<_> = vectors
        .iter()
        .enumerate()
        .map(|(row, vector)| {
            let score: f32 = query
                .iter()
                .zip(vector)
                .map(|(query_value, value)| query_value * value)
                .sum();
            (row, score)
        })
        .collect();
    scores.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    scores.truncate(5);
    scores
}

fn run_query(
    host: &mut Host,
    index: &mut SearchIndex,
    owner: &PetIdentity,
    session: &SessionId,
    query: &RetrievalQuery,
    plan: &RetrievalPlan,
    config: &HybridConfig,
) -> Result<QueryReport, Box<dyn Error>> {
    let started = Instant::now();
    let ticket_started = Instant::now();
    let ticket = host
        .memory
        .for_session(owner, session, ReadPurpose::OfflineReasoning)
        .begin_candidates(query, plan.clone(), NOW_MS)?;
    let ticket_elapsed = ticket_started.elapsed();

    let retrieval_started = Instant::now();
    let ranking = hybrid_rank(index, host, &ticket, None, config)?;
    let mode = ranking.mode();
    let vector_hits = ranking.vector_hits();
    let ranked = ranking.ranking().to_vec();
    let retrieval_elapsed = retrieval_started.elapsed();

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
        retrieval: retrieval_elapsed,
        completion: completion_elapsed,
        assembly: assembly_elapsed,
        mode,
        vector_hits,
        top5,
    })
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn report_query(label: &str, report: &QueryReport) {
    println!(
        "{label}: e2e_ms={:.3} ticket_ms={:.3} retrieval_ms={:.3} core_completion_ms={:.3} assembly_ms={:.3} mode={:?} vector_hits={} top5={:?}",
        millis(report.total),
        millis(report.ticket),
        millis(report.retrieval),
        millis(report.completion),
        millis(report.assembly),
        report.mode,
        report.vector_hits,
        report.top5,
    );
}

fn run_case(count: usize) -> Result<(), Box<dyn Error>> {
    let authority_started = Instant::now();
    let (mut host, owner, session) = build_authority(count)?;
    let authority_elapsed = authority_started.elapsed();

    let (vectors, vector_query, fixture_encoding_elapsed) = encode_synthetic_vectors(count);
    let vector_payload_bytes: usize = vectors
        .iter()
        .map(|vector| vector.len() * size_of::<f32>())
        .sum();
    let cold_scan_started = Instant::now();
    let cold_scan = scan_top5(&vector_query, &vectors);
    let cold_scan_elapsed = cold_scan_started.elapsed();
    let hot_scan_started = Instant::now();
    let hot_scan = scan_top5(&vector_query, &vectors);
    let hot_scan_elapsed = hot_scan_started.elapsed();

    let profile = EmbeddingProfile::new("synthetic", "scan-cost-only", VECTOR_DIMENSION)?;
    let plan = RetrievalPlan::new(profile.clone());
    let query = RetrievalQuery::new("jasmine tea")
        .with_scope(SearchScope::Private)
        .with_layer(RetrievalLayer::LongTerm)
        .with_limit(5);
    let config = HybridConfig::new(profile.clone(), 1, 0.8, true, RerankMode::Disabled, 20)?;
    let chunk_config = ChunkConfig::new(1, 256, 16, 1024)?;
    let mut index = SearchIndex::new(
        StorageDomain::new("synthetic-scan-bench")?,
        profile,
        chunk_config,
        4096,
        32,
        65_536,
    )?;

    let ticket_started = Instant::now();
    let ticket = host
        .memory
        .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
        .begin_candidates(&query, plan.clone(), NOW_MS)?;
    let candidate_elapsed = ticket_started.elapsed();
    let first_index_started = Instant::now();
    let first_batch = index.take_batch(&mut host, &ticket)?;
    let first_batch_slices = first_batch.as_ref().map_or(0, |batch| batch.len());
    drop(first_batch);
    let status = index.reconcile(&mut host, &ticket)?;
    let first_index_elapsed = first_index_started.elapsed();
    let authorized_candidates = ticket.candidates().len();
    drop(ticket);

    let cold_query = run_query(
        &mut host, &mut index, &owner, &session, &query, &plan, &config,
    )?;
    let hot_query = run_query(
        &mut host, &mut index, &owner, &session, &query, &plan, &config,
    )?;
    let retrieval_top5_stable = cold_query.top5 == hot_query.top5;
    let vector_top5_stable = cold_scan == hot_scan;
    if !retrieval_top5_stable || !vector_top5_stable {
        return Err(std::io::Error::other("top-5 results were not deterministic").into());
    }

    println!("=== synthetic offline scan benchmark: records={count} ===");
    println!(
        "authority=MemoryService(in-memory) authority_build_ms={:.3} authorized_candidates={authorized_candidates}",
        millis(authority_elapsed),
    );
    println!(
        "fixture_vector_encode_ms={:.3} dimension={VECTOR_DIMENSION} manual_vector_payload_bytes={vector_payload_bytes}",
        millis(fixture_encoding_elapsed),
    );
    println!(
        "candidate_ticket_ms={:.3} first_index_prepare_ms={:.3} first_index_state=queued_only first_batch_slices={first_batch_slices} queued_records={} indexed_records={} missing_chunks={}",
        millis(candidate_elapsed),
        millis(first_index_elapsed),
        status.queued_records,
        status.indexed_records,
        status.missing_chunks,
    );
    println!(
        "manual_vector_scan_cold_ms={:.3} manual_vector_scan_hot_ms={:.3} manual_top5_rows={:?}",
        millis(cold_scan_elapsed),
        millis(hot_scan_elapsed),
        cold_scan.iter().map(|(row, _)| row).collect::<Vec<_>>(),
    );
    println!(
        "search_index_vector_bytes=0 search_index_entries={} file_cache_bytes=unmeasured(file cache disabled)",
        status.indexed_records,
    );
    report_query("cold_query", &cold_query);
    report_query("hot_query", &hot_query);
    println!(
        "embedding=unmeasured tokenizer=unmeasured rerank=disabled semantic_quality=not_measured synthetic_vectors=scan_cost_only retrieval_top5_stable={retrieval_top5_stable} manual_scan_top5_stable={vector_top5_stable}",
    );
    println!();
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    println!(
        "No network, model assets, or file cache; Hybrid ranking uses its explicit keyword fallback."
    );
    for count in [1_000, 10_000] {
        run_case(count)?;
    }
    Ok(())
}
