use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moe_memory_core::{
    Category, Epistemic, MemoryDraft, MemoryScope, MemoryService, Namespace, PetIdentity,
    ReadPolicy, ReadPurpose, RetrievalPlan, RetrievalQuery, SessionId, TransmissionPolicy,
    TransmissionUsage,
};
use moe_memory_search::{
    CallControl, Cancellation, CohereReranker, OnlineEmbedding, OnlineOptions, SearchHost,
    exact_scan,
};

#[path = "support/preference.rs"]
mod preference;

struct ProbeHost(MemoryService);

impl SearchHost for ProbeHost {
    fn memory(&mut self) -> &mut MemoryService {
        &mut self.0
    }
    fn now_ms(&mut self) -> u64 {
        now_ms()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("host clock before epoch")
        .as_millis()
        .try_into()
        .expect("host clock overflow")
}

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("required trusted-host setting missing: {name}").into())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if required("MOE_SEARCH_ALLOW_SYNTHETIC_EMBEDDING")? != "query-and-entry-private-source" {
        return Err(
            "explicit synthetic query/entry private-source transmission permission required".into(),
        );
    }
    let endpoint = required("MOE_SEARCH_EMBEDDING_ENDPOINT")?;
    let key = required("MOE_SEARCH_EMBEDDING_KEY")?;
    let model = required("MOE_SEARCH_EMBEDDING_MODEL")?;
    let dimension = required("MOE_SEARCH_EMBEDDING_DIMENSION")?
        .parse::<usize>()
        .map_err(|_| "invalid embedding dimension")?;
    let embedding =
        OnlineEmbedding::new(&endpoint, &key, &model, dimension, OnlineOptions::default())?;
    let reranker = if env::var("MOE_SEARCH_ENABLE_RERANK").as_deref() == Ok("yes") {
        if required("MOE_SEARCH_ALLOW_SYNTHETIC_RERANK")? != "query-and-shortlist-private-source" {
            return Err("separate synthetic rerank transmission permission required".into());
        }
        Some(CohereReranker::new(
            &required("MOE_SEARCH_RERANK_ENDPOINT")?,
            &required("MOE_SEARCH_RERANK_KEY")?,
            &required("MOE_SEARCH_RERANK_MODEL")?,
            OnlineOptions::default(),
        )?)
    } else {
        None
    };
    let mut host = ProbeHost(MemoryService::new());
    let owner = PetIdentity::new(
        Namespace::new("synthetic-probe", "synthetic-user")?,
        "synthetic-pet",
    )?;
    let scope = MemoryScope::Private(owner.clone());
    let source = host.0.register_source(
        scope.clone(),
        "synthetic-only-probe",
        1,
        Epistemic::UserStatement,
    )?;
    let mut expected = None;
    for text in [
        "每天傍晚我都偏爱泡一壶茉莉花茶",
        "操作系统升级和磁盘分区指南",
    ] {
        let record = host.0.for_pet(&owner).create(
            MemoryDraft::new(
                text,
                Category::Episode,
                Epistemic::UserStatement,
                vec![source.clone()],
            ),
            now_ms(),
        )?;
        if expected.is_none() {
            expected = Some(record.id())
        }
    }
    host.0
        .set_read_policy(&owner, ReadPolicy::offline(1, vec![source.clone()]))?;
    for usage in [
        TransmissionUsage::QueryEmbedding,
        TransmissionUsage::EntryEmbedding,
    ] {
        host.0.set_transmission_policy(
            &owner,
            TransmissionPolicy::new(1, usage, &endpoint, vec![source.clone()])?
                .with_scope(scope.clone())
                .transmit_query(),
        )?;
    }
    let mut plan = RetrievalPlan::new(embedding.profile().clone());
    if let Some(reranker) = &reranker {
        let rerank_endpoint = required("MOE_SEARCH_RERANK_ENDPOINT")?;
        host.0.set_transmission_policy(
            &owner,
            TransmissionPolicy::new(
                1,
                TransmissionUsage::Rerank,
                rerank_endpoint,
                vec![source.clone()],
            )?
            .with_scope(scope)
            .transmit_query(),
        )?;
        plan = plan.with_rerank(reranker.profile().clone());
    }
    let session = SessionId::new("explicit-probe")?;
    let mut ticket = host
        .0
        .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
        .begin_candidates(&RetrievalQuery::new("我通常会选什么饮品"), plan, now_ms())?;
    host.0
        .bind_query_provenance(&mut ticket, vec![source], &[], now_ms())?;
    let references: Vec<_> = ticket
        .candidates()
        .iter()
        .map(|candidate| candidate.candidate_ref())
        .collect();
    let control = CallControl::new(Cancellation::default(), Duration::from_secs(60))?;
    let query = embedding
        .embed_query(&mut host, &mut ticket, &control)
        .await?;
    let vectors = embedding
        .embed_entries(&mut host, &mut ticket, &references, &control)
        .await?;
    let mut ranked = exact_scan(&ticket, &query, &vectors, -1.0, 2)?;
    let evidence: Vec<_> = ranked
        .iter()
        .map(|(reference, score)| {
            (
                reference.record().map(|record| record.id()) == expected,
                *score,
            )
        })
        .collect();
    if !preference::strictly_prefers_expected(&evidence) {
        return Err(
            "real embedding non-substring smoke failed; this is not the R4 quality evaluation"
                .into(),
        );
    }
    if let Some(reranker) = reranker {
        let shortlist: Vec<_> = ranked.iter().map(|item| item.0.clone()).collect();
        ranked = reranker
            .rerank(&mut host, &mut ticket, &shortlist, &control)
            .await?
            .ok_or("rerank unexpectedly disabled")?;
        let evidence: Vec<_> = ranked
            .iter()
            .map(|(reference, score)| {
                (
                    reference.record().map(|record| record.id()) == expected,
                    *score,
                )
            })
            .collect();
        if !preference::strictly_prefers_expected(&evidence) {
            return Err("real rerank smoke failed".into());
        }
    }
    let task = host
        .0
        .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
        .complete_retrieval(&ticket, &ranked, now_ms())?;
    host.0.validate_task(&task, now_ms())?;
    println!(
        "embedding_non_substring_smoke=passed; rerank={}; full_R4_quality_gate=not_run",
        if env::var("MOE_SEARCH_ENABLE_RERANK").as_deref() == Ok("yes") {
            "passed"
        } else {
            "disabled-unverified"
        }
    );
    Ok(())
}
