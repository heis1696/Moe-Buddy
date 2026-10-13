use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use moe_memory_core::{
    CandidateTicket, Category, CommandResult, Epistemic, MemoryDraft, MemoryId, MemoryScope,
    MemoryService, Namespace, PetIdentity, PublicationPolicy, ReadPolicy, ReadPurpose,
    RetrievalLayer, RetrievalPlan, RetrievalQuery, SearchScope, SessionId, SourceRef,
    TransmissionPolicy, TransmissionUsage,
};
use moe_memory_search::{
    CallControl, Cancellation, ChunkConfig, CohereReranker, HybridConfig, OnlineEmbedding,
    RerankMode, SearchHost, SearchIndex, StorageDomain, exact_scan, hybrid_rank,
    rerank_or_fallback,
};

use super::quality::{CalibrationScores, Case, Record, Scores, Summary};

pub type ResultValue<Value> = Result<Value, Box<dyn Error>>;

pub struct Host(pub MemoryService);

impl SearchHost for Host {
    fn memory(&mut self) -> &mut MemoryService {
        &mut self.0
    }
    fn now_ms(&mut self) -> u64 {
        now_ms()
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("host clock before epoch")
        .as_millis()
        .try_into()
        .expect("host clock overflow")
}

pub struct Evaluation {
    pub cold: Summary,
    pub hot: Summary,
}

pub struct World {
    pub host: Host,
    owner: PetIdentity,
    session: SessionId,
    ids: BTreeMap<MemoryId, String>,
    records: Vec<Record>,
    private: SourceRef,
    shared: SourceRef,
}

impl World {
    pub fn new(records: &[Record], domain: &str) -> ResultValue<Self> {
        let mut host = Host(MemoryService::new());
        let namespace = Namespace::new("synthetic-r4", domain)?;
        let owner = PetIdentity::new(namespace.clone(), "pet-a")?;
        let private = host.0.register_source(
            MemoryScope::Private(owner.clone()),
            "synthetic-private",
            1,
            Epistemic::UserStatement,
        )?;
        let shared = host.0.register_source(
            MemoryScope::Shared(namespace.clone()),
            "synthetic-shared",
            1,
            Epistemic::Reference,
        )?;
        host.0.set_read_policy(
            &owner,
            ReadPolicy::offline(1, vec![private.clone(), shared.clone()]),
        )?;
        host.0.set_publication_policy(
            &owner,
            PublicationPolicy::allow_sources(1, vec![shared.clone()]),
        )?;
        host.0.set_formal_publication_policy(
            &namespace,
            PublicationPolicy::allow_sources(1, vec![shared.clone()]),
        )?;
        let session = SessionId::new("synthetic-r4-session")?;
        let mut ids = BTreeMap::new();
        for record in records {
            let receipt = match record.scope.as_str() {
                "private" => host.0.for_pet(&owner).create(
                    MemoryDraft::new(
                        &record.text,
                        Category::Episode,
                        Epistemic::UserStatement,
                        vec![private.clone()],
                    ),
                    now_ms(),
                )?,
                "shared" => {
                    let task = host
                        .0
                        .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
                        .begin_task(
                            &RetrievalQuery::new("").with_scope(SearchScope::Shared),
                            now_ms(),
                        )?;
                    let candidate = task.propose_shared(
                        MemoryDraft::new(
                            &record.text,
                            Category::Knowledge,
                            Epistemic::Reference,
                            vec![shared.clone()],
                        ),
                        now_ms(),
                    )?;
                    let command = host.0.prepare_shared_publication(&candidate, now_ms())?;
                    match command.execute(&mut host.0)? {
                        CommandResult::Write(receipt) => receipt,
                        _ => return Err("unexpected publication result".into()),
                    }
                }
                "formal" => {
                    let command = host.0.prepare_formal_publication(
                        &namespace,
                        MemoryDraft::new(
                            &record.text,
                            Category::Protocol,
                            Epistemic::Reference,
                            vec![shared.clone()],
                        ),
                        now_ms(),
                    )?;
                    match command.execute(&mut host.0)? {
                        CommandResult::Write(receipt) => receipt,
                        _ => return Err("unexpected formal publication result".into()),
                    }
                }
                _ => return Err("unsupported fixture scope".into()),
            };
            ids.insert(receipt.id(), record.id.clone());
        }
        let other = PetIdentity::new(namespace, "pet-b")?;
        let other_source = host.0.register_source(
            MemoryScope::Private(other.clone()),
            "synthetic-other-pet",
            1,
            Epistemic::UserStatement,
        )?;
        let denied_source = host.0.register_source(
            MemoryScope::Private(owner.clone()),
            "synthetic-denied-source",
            1,
            Epistemic::UserStatement,
        )?;
        for record in records {
            host.0.for_pet(&other).create(
                MemoryDraft::new(
                    &record.text,
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![other_source.clone()],
                ),
                now_ms(),
            )?;
            host.0.for_pet(&owner).create(
                MemoryDraft::new(
                    &record.text,
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![denied_source.clone()],
                ),
                now_ms(),
            )?;
            let issued = now_ms().saturating_sub(2);
            host.0.for_pet(&owner).create(
                MemoryDraft::new(
                    &record.text,
                    Category::Episode,
                    Epistemic::UserStatement,
                    vec![private.clone()],
                )
                .with_expiration(issued + 1),
                issued,
            )?;
        }
        Ok(Self {
            host,
            owner,
            session,
            ids,
            records: records.to_vec(),
            private,
            shared,
        })
    }

    pub fn authorize(
        &mut self,
        embedding_endpoint: &str,
        rerank_endpoint: Option<&str>,
    ) -> ResultValue<()> {
        let sources = vec![self.private.clone(), self.shared.clone()];
        let scopes = [
            MemoryScope::Private(self.owner.clone()),
            MemoryScope::Shared(self.owner.namespace().clone()),
        ];
        for (usage, endpoint) in [
            (TransmissionUsage::QueryEmbedding, Some(embedding_endpoint)),
            (TransmissionUsage::EntryEmbedding, Some(embedding_endpoint)),
            (TransmissionUsage::Rerank, rerank_endpoint),
        ] {
            if let Some(endpoint) = endpoint {
                let mut policy =
                    TransmissionPolicy::new(1, usage, endpoint, sources.clone())?.transmit_query();
                for scope in &scopes {
                    policy = policy.with_scope(scope.clone());
                }
                self.host.0.set_transmission_policy(&self.owner, policy)?;
            }
        }
        Ok(())
    }

    pub fn ticket(
        &mut self,
        case: &Case,
        embedding: &OnlineEmbedding,
        reranker: Option<&CohereReranker>,
    ) -> ResultValue<CandidateTicket> {
        let scope = if case.scope == "private" {
            SearchScope::Private
        } else {
            SearchScope::Shared
        };
        let layer = if case.scope == "formal" {
            RetrievalLayer::FormalMaterial
        } else {
            RetrievalLayer::LongTerm
        };
        let query = RetrievalQuery::new(&case.text)
            .with_scope(scope)
            .with_layer(layer)
            .with_limit(5)
            .with_char_budget(4096);
        let mut plan = RetrievalPlan::new(embedding.profile().clone());
        if let Some(reranker) = reranker {
            plan = plan.with_rerank(reranker.profile().clone());
        }
        let mut ticket = self
            .host
            .0
            .for_session(&self.owner, &self.session, ReadPurpose::OfflineReasoning)
            .begin_candidates(&query, plan, now_ms())?;
        let allowed: BTreeSet<_> = self
            .records
            .iter()
            .filter(|record| record.scope == case.scope)
            .map(|record| &record.id)
            .collect();
        let actual: BTreeSet<_> = ticket
            .candidates()
            .iter()
            .filter_map(|candidate| {
                candidate
                    .candidate_ref()
                    .record()
                    .and_then(|record| self.ids.get(&record.id()))
            })
            .collect();
        if actual != allowed || actual.len() != ticket.candidates().len() {
            return Err("unauthorized, stale, or missing candidate detected before scoring".into());
        }
        self.host.0.bind_query_provenance(
            &mut ticket,
            vec![self.private.clone()],
            &[],
            now_ms(),
        )?;
        Ok(ticket)
    }

    fn scores(&self, ranked: &[(moe_memory_core::CandidateRef, f64)]) -> ResultValue<Scores> {
        ranked
            .iter()
            .map(|(reference, score)| {
                let id = reference
                    .record()
                    .and_then(|record| self.ids.get(&record.id()))
                    .ok_or("unknown result identifier")?;
                Ok((id.clone(), *score))
            })
            .collect()
    }

    fn finish(
        &mut self,
        ticket: &CandidateTicket,
        ranked: &[(moe_memory_core::CandidateRef, f64)],
    ) -> ResultValue<(Vec<String>, u128)> {
        let start = Instant::now();
        let task = self
            .host
            .0
            .for_session(&self.owner, &self.session, ReadPurpose::OfflineReasoning)
            .complete_retrieval(ticket, ranked, now_ms())?;
        let context = self.host.0.assemble_context(&task, 8192, now_ms())?;
        self.host.0.context_view(&context, now_ms())?;
        let ids = task
            .result()
            .items()
            .iter()
            .map(|item| match item {
                moe_memory_core::RetrievalItem::LongTerm(record) => self
                    .ids
                    .get(&record.id())
                    .cloned()
                    .ok_or("unauthorized or stale delivered item"),
                moe_memory_core::RetrievalItem::WorkingContext(_) => {
                    Err("unexpected working context in long-term evaluation")
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((ids, start.elapsed().as_micros()))
    }

    pub async fn calibration_scores(
        &mut self,
        cases: &[Case],
        embedding: &OnlineEmbedding,
    ) -> ResultValue<CalibrationScores> {
        let mut observations = Vec::new();
        for case in cases {
            let mut ticket = self.ticket(case, embedding, None)?;
            let references: Vec<_> = ticket
                .candidates()
                .iter()
                .map(|candidate| candidate.candidate_ref())
                .collect();
            let control = CallControl::new(Cancellation::default(), Duration::from_secs(120))?;
            let query = embedding
                .embed_query(&mut self.host, &mut ticket, &control)
                .await?;
            let entries = embedding
                .embed_entries(&mut self.host, &mut ticket, &references, &control)
                .await?;
            let ranked = exact_scan(&ticket, &query, &entries, -1.0, 100)?;
            let scores = self.scores(&ranked)?;
            self.host.0.validate_candidate_ticket(&ticket, now_ms())?;
            observations.push((case.id.clone(), scores));
        }
        Ok(observations)
    }

    pub async fn evaluate(
        &mut self,
        cases: &[Case],
        embedding: &OnlineEmbedding,
        reranker: Option<&CohereReranker>,
        threshold: f64,
        mode: (&str, bool),
    ) -> ResultValue<Evaluation> {
        let (name, keyword) = mode;
        let mut summary = Summary::default();
        let mut hot_summary = Summary::default();
        for case in cases {
            let start = Instant::now();
            let mut ticket = self.ticket(case, embedding, reranker)?;
            let mut index = SearchIndex::new(
                StorageDomain::new(format!("r4-{name}"))?,
                embedding.profile().clone(),
                ChunkConfig::new(1, 128, 16, 1024)?,
                64,
                32,
                128,
            )?;
            let config = HybridConfig::new(
                embedding.profile().clone(),
                1,
                threshold,
                keyword,
                if reranker.is_some() {
                    RerankMode::Required
                } else {
                    RerankMode::Disabled
                },
                20,
            )?;
            let control = CallControl::new(Cancellation::default(), Duration::from_secs(120))?;
            let index_start = Instant::now();
            let mut entry_requests = 0;
            if !keyword {
                while let Some(batch) = index.take_batch(&mut self.host, &ticket)? {
                    let completed = embedding
                        .embed_index_batch(&mut self.host, &mut ticket, batch, &control)
                        .await?;
                    index.commit(&mut self.host, &ticket, completed)?;
                    entry_requests += 1;
                }
            }
            let index_micros = index_start.elapsed().as_micros();
            let query_start = Instant::now();
            let encoded = if keyword {
                None
            } else {
                Some(
                    embedding
                        .embed_query(&mut self.host, &mut ticket, &control)
                        .await?,
                )
            };
            let query_micros = query_start.elapsed().as_micros();
            let recall_start = Instant::now();
            let ranking = hybrid_rank(
                &mut index,
                &mut self.host,
                &ticket,
                encoded.as_ref(),
                &config,
            )?;
            let recall_micros = recall_start.elapsed().as_micros();
            let rerank_expected = reranker.is_some() && !ranking.ranking().is_empty();
            let rerank_start = Instant::now();
            let ranked = rerank_or_fallback(
                &index,
                &mut self.host,
                ticket,
                ranking,
                encoded.as_ref(),
                reranker,
                &control,
            )
            .await?;
            let rerank_micros = rerank_start.elapsed().as_micros();
            let (returned, assembly_micros) =
                self.finish(ranked.ticket(), ranked.result().ranking())?;
            let allowed = self
                .records
                .iter()
                .filter(|record| record.scope == case.scope)
                .map(|record| record.id.clone())
                .collect();
            summary.observe(case, &returned, &allowed, &BTreeSet::new())?;
            println!(
                "case={} mode={name} ids={} cold_e2e_us={} index_with_encoding_us={index_micros} query_encoding_us={query_micros} recall_us={recall_micros} rerank_us={rerank_micros} assembly_us={assembly_micros} entry_calls={entry_requests} query_calls={} rerank_calls={} retry_attempts=0 usage=unavailable cost=unavailable",
                case.id,
                returned.join(","),
                start.elapsed().as_micros(),
                usize::from(!keyword),
                usize::from(rerank_expected)
            );
            let hot_start = Instant::now();
            let mut hot_ticket = self.ticket(case, embedding, reranker)?;
            let hot_control = CallControl::new(Cancellation::default(), Duration::from_secs(120))?;
            let hot_query_start = Instant::now();
            let hot_encoded = if keyword {
                None
            } else {
                Some(
                    embedding
                        .embed_query(&mut self.host, &mut hot_ticket, &hot_control)
                        .await?,
                )
            };
            let hot_query_micros = hot_query_start.elapsed().as_micros();
            let hot_recall_start = Instant::now();
            let hot_ranking = hybrid_rank(
                &mut index,
                &mut self.host,
                &hot_ticket,
                hot_encoded.as_ref(),
                &config,
            )?;
            let hot_recall_micros = hot_recall_start.elapsed().as_micros();
            let hot_rerank_expected = reranker.is_some() && !hot_ranking.ranking().is_empty();
            let hot_rerank_start = Instant::now();
            let hot_ranked = rerank_or_fallback(
                &index,
                &mut self.host,
                hot_ticket,
                hot_ranking,
                hot_encoded.as_ref(),
                reranker,
                &hot_control,
            )
            .await?;
            let hot_rerank_micros = hot_rerank_start.elapsed().as_micros();
            let (hot_ids, hot_assembly_micros) =
                self.finish(hot_ranked.ticket(), hot_ranked.result().ranking())?;
            hot_summary.observe(case, &hot_ids, &allowed, &BTreeSet::new())?;
            println!(
                "case={} mode={name} hot_ids={} hot_e2e_us={} entry_calls=0 query_encoding_us={hot_query_micros} recall_us={hot_recall_micros} rerank_us={hot_rerank_micros} assembly_us={hot_assembly_micros} query_calls={} rerank_calls={} retry_attempts=0 usage=unavailable cost=unavailable",
                case.id,
                hot_ids.join(","),
                hot_start.elapsed().as_micros(),
                usize::from(!keyword),
                usize::from(hot_rerank_expected)
            );
        }
        Ok(Evaluation {
            cold: summary,
            hot: hot_summary,
        })
    }
}
