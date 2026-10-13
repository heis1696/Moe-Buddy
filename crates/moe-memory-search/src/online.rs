use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use moe_memory_core::{
    CandidateRef, CandidateTicket, EmbeddingProfile, RerankProfile, TransmissionTicket,
    TransmissionUsage,
};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, RETRY_AFTER},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{EmbeddedCandidate, EncodedQuery, RetrievalHost, SearchError, vector::UnitVector};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_BATCH: usize = 32;
const MAX_RERANK: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnlineOptions {
    pub max_input_bytes: usize,
    pub request_timeout: Duration,
    pub max_attempts: u8,
    pub retry_delay: Duration,
}

impl Default for OnlineOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024,
            request_timeout: Duration::from_secs(15),
            max_attempts: 1,
            retry_delay: Duration::from_millis(250),
        }
    }
}

impl OnlineOptions {
    fn validate(&self) -> Result<(), SearchError> {
        if !(1..=4096).contains(&self.max_input_bytes)
            || self.request_timeout.is_zero()
            || self.request_timeout > Duration::from_secs(60)
            || !(1..=3).contains(&self.max_attempts)
            || self.retry_delay > Duration::from_secs(30)
        {
            return Err(SearchError::InvalidConfig("invalid online limits"));
        }
        Ok(())
    }

    fn text(&self, text: &str) -> Result<(), SearchError> {
        if text.trim().is_empty() {
            return Err(SearchError::InvalidInput("empty model input"));
        }
        if text.len() > self.max_input_bytes {
            return Err(SearchError::InputTooLong);
        }
        Ok(())
    }
}

/// Transport cancellation only; host task cancellation/revocation remains core authority.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

pub struct CallControl {
    cancellation: Cancellation,
    deadline: Instant,
}

impl CallControl {
    /// One explicit overall budget may be shared by encoding and reranking calls.
    pub fn new(cancellation: Cancellation, budget: Duration) -> Result<Self, SearchError> {
        if budget.is_zero() || budget > Duration::from_secs(120) {
            return Err(SearchError::InvalidConfig("invalid call budget"));
        }
        Ok(Self {
            cancellation,
            deadline: Instant::now() + budget,
        })
    }

    pub(crate) fn check(&self) -> Result<(), SearchError> {
        if self.cancellation.is_cancelled() {
            Err(SearchError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(SearchError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }
}

async fn guarded<ResultValue>(
    control: &CallControl,
    future: impl Future<Output = Result<ResultValue, SearchError>>,
) -> Result<ResultValue, SearchError> {
    control.check()?;
    tokio::select! {
        biased;
        () = control.cancellation.cancelled() => Err(SearchError::Cancelled),
        () = tokio::time::sleep_until(tokio::time::Instant::from_std(control.deadline)) => Err(SearchError::DeadlineExceeded),
        result = future => result,
    }
}

struct Transport {
    client: Client,
    endpoint: String,
    authorization: HeaderValue,
    options: OnlineOptions,
}

struct ModelRequest<'request> {
    usage: TransmissionUsage,
    entries: &'request [CandidateRef],
    payload: Value,
}

impl Transport {
    fn new(
        endpoint: &str,
        api_key: &str,
        options: OnlineOptions,
        suffix: &str,
    ) -> Result<Self, SearchError> {
        options.validate()?;
        let parsed =
            Url::parse(endpoint).map_err(|_| SearchError::InvalidConfig("invalid endpoint"))?;
        let loopback = parsed
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "[::1]");
        if endpoint.len() > 512
            || parsed.as_str() != endpoint
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.path().ends_with(suffix)
            || !(parsed.scheme() == "https" || parsed.scheme() == "http" && loopback)
            || api_key.is_empty()
            || api_key.len() > 4096
            || !api_key.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(SearchError::InvalidConfig("unsafe endpoint or credential"));
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| SearchError::InvalidConfig("invalid credential"))?;
        authorization.set_sensitive(true);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .referer(false)
            .connect_timeout(options.request_timeout)
            .timeout(options.request_timeout)
            .build()
            .map_err(|_| SearchError::TransportUnavailable)?;
        Ok(Self {
            client,
            endpoint: endpoint.to_owned(),
            authorization,
            options,
        })
    }

    fn configuration_id(&self, protocol: &str, model: &str, dimension: usize) -> String {
        let input = json!([
            protocol,
            self.endpoint,
            model,
            dimension,
            self.options.max_input_bytes,
            "raw-utf8-byte-bound-v1",
            "scaled-f64-unit-v1"
        ]);
        let digest = Sha256::digest(input.to_string().as_bytes());
        let encoded: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("{protocol}-{encoded}")
    }

    async fn fetch(
        &self,
        payload: Vec<u8>,
    ) -> Result<(u16, Option<Duration>, Vec<u8>), SearchError> {
        let mut response = self
            .client
            .post(&self.endpoint)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status().as_u16();
        let delay = match response.headers().get(RETRY_AFTER) {
            None => Some(self.options.retry_delay),
            Some(value) => value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|seconds| *seconds <= 30)
                .map(Duration::from_secs),
        };
        if !(200..300).contains(&status) {
            return Ok((status, delay, Vec::new()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(SearchError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                return Err(SearchError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, delay, body))
    }

    async fn execute<Host: RetrievalHost, Output>(
        &self,
        host: &mut Host,
        ticket: &mut CandidateTicket,
        request: ModelRequest<'_>,
        control: &CallControl,
        decode: impl Fn(&[u8]) -> Result<Output, SearchError>,
    ) -> Result<Output, SearchError> {
        control.check()?;
        let payload = serde_json::to_vec(&request.payload)
            .map_err(|_| SearchError::InvalidInput("invalid request"))?;
        if payload.len() > MAX_REQUEST_BYTES {
            return Err(SearchError::InputTooLong);
        }
        let now_ms = host.now_ms();
        let mut sent = host.authority().prepare_transmission(
            ticket,
            request.usage,
            &self.endpoint,
            request.entries,
            now_ms,
        )?;
        for attempt in 1..=self.options.max_attempts {
            let result = guarded(control, self.fetch(payload.clone())).await;
            let (status, delay, body) = match result {
                Ok(response) => response,
                Err(error) => return Err(fail(host, ticket, &sent, error)),
            };
            if (200..300).contains(&status) {
                let output = match decode(&body).and_then(|output| control.check().map(|()| output))
                {
                    Ok(output) => output,
                    Err(error) => return Err(fail(host, ticket, &sent, error)),
                };
                let now_ms = host.now_ms();
                if let Err(error) = control.check() {
                    return Err(fail(host, ticket, &sent, error));
                }
                host.authority()
                    .record_transmission(ticket, &sent, now_ms)?;
                return Ok(output);
            }
            if attempt < self.options.max_attempts
                && matches!(status, 429 | 502 | 503 | 504)
                && let Some(delay) = delay
            {
                if let Err(error) = guarded(control, async {
                    tokio::time::sleep(delay).await;
                    Ok(())
                })
                .await
                {
                    return Err(fail(host, ticket, &sent, error));
                }
                let now_ms = host.now_ms();
                if let Err(error) = control.check() {
                    return Err(fail(host, ticket, &sent, error));
                }
                let next = match host.authority().prepare_transmission(
                    ticket,
                    request.usage,
                    &self.endpoint,
                    request.entries,
                    now_ms,
                ) {
                    Ok(next) => next,
                    Err(error) => return Err(fail(host, ticket, &sent, error)),
                };
                host.authority()
                    .record_transmission(ticket, &sent, now_ms)?;
                sent = next;
                continue;
            }
            return Err(fail(host, ticket, &sent, SearchError::HttpStatus(status)));
        }
        Err(SearchError::InvalidConfig("empty retry budget"))
    }
}

fn transport_error(error: reqwest::Error) -> SearchError {
    if error.is_timeout() {
        SearchError::RequestTimedOut
    } else {
        SearchError::TransportUnavailable
    }
}

fn fail<Host: RetrievalHost>(
    host: &mut Host,
    ticket: &mut CandidateTicket,
    sent: &TransmissionTicket,
    error: SearchError,
) -> SearchError {
    let now_ms = host.now_ms();
    match host.authority().fail_transmission(ticket, sent, now_ms) {
        Ok(()) => error,
        Err(failure) => failure,
    }
}

fn document_inputs(
    ticket: &CandidateTicket,
    entries: &[CandidateRef],
    options: &OnlineOptions,
    maximum: usize,
) -> Result<Vec<String>, SearchError> {
    if entries.is_empty() || entries.len() > maximum {
        return Err(SearchError::InvalidInput("invalid model batch size"));
    }
    let mut seen = BTreeSet::new();
    entries
        .iter()
        .map(|reference| {
            if !seen.insert(reference) {
                return Err(SearchError::InvalidInput("duplicate model candidate"));
            }
            let item = ticket
                .candidate(reference)
                .ok_or(SearchError::InvalidInput("foreign model candidate"))?;
            options.text(item.text())?;
            Ok(item.text().to_owned())
        })
        .collect()
}

fn response_json(body: &[u8]) -> Result<Value, SearchError> {
    serde_json::from_slice(body).map_err(|_| SearchError::InvalidResponse("invalid JSON"))
}

fn indexed_results<'response>(
    response: &'response Value,
    name: &str,
    count: usize,
) -> Result<Vec<&'response Value>, SearchError> {
    let data = response[name]
        .as_array()
        .ok_or(SearchError::InvalidResponse("missing result array"))?;
    if data.len() != count {
        return Err(SearchError::InvalidResponse("wrong result count"));
    }
    let mut ordered = vec![None; count];
    for item in data {
        let index = item["index"]
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
            .filter(|index| *index < count)
            .ok_or(SearchError::InvalidResponse("invalid result index"))?;
        if ordered[index].replace(item).is_some() {
            return Err(SearchError::InvalidResponse("duplicate result index"));
        }
    }
    ordered
        .into_iter()
        .map(|item| item.ok_or(SearchError::InvalidResponse("missing result index")))
        .collect()
}

/// OpenAI-compatible float embeddings. Endpoint is the complete /embeddings URL.
pub struct OnlineEmbedding {
    transport: Transport,
    profile: EmbeddingProfile,
}

impl fmt::Debug for OnlineEmbedding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OnlineEmbedding([redacted])")
    }
}

impl OnlineEmbedding {
    pub fn new(
        endpoint: &str,
        api_key: &str,
        model: &str,
        dimension: usize,
        options: OnlineOptions,
    ) -> Result<Self, SearchError> {
        let transport = Transport::new(endpoint, api_key, options, "/embeddings")?;
        let profile = EmbeddingProfile::new(
            transport.configuration_id("openai-embedding-v1", model, dimension),
            model,
            dimension,
        )?;
        Ok(Self { transport, profile })
    }

    pub fn profile(&self) -> &EmbeddingProfile {
        &self.profile
    }

    pub async fn embed_query<Host: RetrievalHost>(
        &self,
        host: &mut Host,
        ticket: &mut CandidateTicket,
        control: &CallControl,
    ) -> Result<EncodedQuery, SearchError> {
        self.check_profile(ticket)?;
        let text = ticket.query().text().to_owned();
        self.transport.options.text(&text)?;
        let payload =
            json!({"model": self.profile.model(), "input": [text], "encoding_format": "float"});
        let mut vectors = self
            .transport
            .execute(
                host,
                ticket,
                ModelRequest {
                    usage: TransmissionUsage::QueryEmbedding,
                    entries: &[],
                    payload,
                },
                control,
                |body| self.decode(body, 1),
            )
            .await?;
        Ok(EncodedQuery {
            profile: self.profile.clone(),
            text,
            vector: vectors.remove(0),
        })
    }

    pub async fn embed_entries<Host: RetrievalHost>(
        &self,
        host: &mut Host,
        ticket: &mut CandidateTicket,
        entries: &[CandidateRef],
        control: &CallControl,
    ) -> Result<Vec<EmbeddedCandidate>, SearchError> {
        self.check_profile(ticket)?;
        let texts = document_inputs(ticket, entries, &self.transport.options, MAX_BATCH)?;
        let inputs: Vec<_> = entries
            .iter()
            .map(|reference| {
                ticket
                    .candidate(reference)
                    .expect("validated reference")
                    .clone()
            })
            .collect();
        let payload =
            json!({"model": self.profile.model(), "input": texts, "encoding_format": "float"});
        let vectors = self
            .transport
            .execute(
                host,
                ticket,
                ModelRequest {
                    usage: TransmissionUsage::EntryEmbedding,
                    entries,
                    payload,
                },
                control,
                |body| self.decode(body, entries.len()),
            )
            .await?;
        Ok(entries
            .iter()
            .zip(inputs)
            .zip(vectors)
            .map(|((reference, input), vector)| EmbeddedCandidate {
                profile: self.profile.clone(),
                reference: reference.clone(),
                input,
                vector,
            })
            .collect())
    }

    fn check_profile(&self, ticket: &CandidateTicket) -> Result<(), SearchError> {
        if ticket.plan().profile() == &self.profile {
            Ok(())
        } else {
            Err(SearchError::ProfileMismatch)
        }
    }

    /// Encodes only the exact authorized slices sealed by `SearchIndex`.
    /// The receipt conservatively retains every complete parent input/source.
    pub async fn embed_index_batch<Host: RetrievalHost>(
        &self,
        host: &mut Host,
        ticket: &mut CandidateTicket,
        batch: crate::IndexBatch,
        control: &CallControl,
    ) -> Result<crate::CompletedIndexBatch, SearchError> {
        self.check_profile(ticket)?;
        batch.validate(ticket)?;
        let mut entries = BTreeSet::new();
        let mut texts = Vec::new();
        for slice in &batch.slices {
            self.transport.options.text(slice.chunk.text())?;
            entries.insert(slice.reference.clone());
            texts.push(slice.chunk.text());
        }
        let entries: Vec<_> = entries.into_iter().collect();
        let payload =
            json!({"model": self.profile.model(), "input": texts, "encoding_format": "float"});
        let vectors = self
            .transport
            .execute(
                host,
                ticket,
                ModelRequest {
                    usage: TransmissionUsage::EntryEmbedding,
                    entries: &entries,
                    payload,
                },
                control,
                |body| self.decode(body, batch.len()),
            )
            .await?;
        Ok(crate::CompletedIndexBatch { batch, vectors })
    }

    fn decode(&self, body: &[u8], count: usize) -> Result<Vec<UnitVector>, SearchError> {
        let response = response_json(body)?;
        if response["model"].as_str() != Some(self.profile.model()) {
            return Err(SearchError::InvalidResponse("wrong response model"));
        }
        indexed_results(&response, "data", count)?
            .into_iter()
            .map(|item| {
                let values = item["embedding"]
                    .as_array()
                    .ok_or(SearchError::InvalidResponse("missing embedding"))?;
                let values = values
                    .iter()
                    .map(|value| {
                        value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or(SearchError::InvalidResponse("non-finite embedding"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                UnitVector::new(values, self.profile.dimension())
            })
            .collect()
    }
}

/// Explicit Cohere v2 rerank protocol, independent of embedding and Chat.
pub struct CohereReranker {
    transport: Transport,
    profile: RerankProfile,
}

impl fmt::Debug for CohereReranker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CohereReranker([redacted])")
    }
}

impl CohereReranker {
    pub fn new(
        endpoint: &str,
        api_key: &str,
        model: &str,
        options: OnlineOptions,
    ) -> Result<Self, SearchError> {
        let transport = Transport::new(endpoint, api_key, options, "/v2/rerank")?;
        let profile = RerankProfile::new(
            transport.configuration_id("cohere-rerank-v1", model, 0),
            model,
        )?;
        Ok(Self { transport, profile })
    }

    pub fn profile(&self) -> &RerankProfile {
        &self.profile
    }

    pub(crate) async fn rerank_slices<Host: RetrievalHost>(
        &self,
        host: &mut Host,
        ticket: &mut CandidateTicket,
        entries: &[CandidateRef],
        texts: &[String],
        control: &CallControl,
    ) -> Result<Vec<(CandidateRef, f64)>, SearchError> {
        if ticket.plan().rerank() != Some(&self.profile) {
            return Err(SearchError::ProfileMismatch);
        }
        if entries.is_empty() || entries.len() > MAX_RERANK || entries.len() != texts.len() {
            return Err(SearchError::InvalidInput("invalid rerank shortlist"));
        }
        let mut seen = BTreeSet::new();
        let query = ticket.query().text();
        self.transport.options.text(query)?;
        for (reference, text) in entries.iter().zip(texts) {
            let item = ticket
                .candidate(reference)
                .ok_or(SearchError::InvalidInput("foreign rerank reference"))?;
            if !seen.insert(reference) || !item.text().contains(text.as_str()) {
                return Err(SearchError::InvalidInput("invalid rerank slice"));
            }
            self.transport.options.text(text)?;
            if query.len() + text.len() > 3072 {
                return Err(SearchError::InputTooLong);
            }
        }
        let payload = json!({"model": self.profile.model(), "query": query, "documents": texts, "top_n": entries.len(), "max_tokens_per_doc": 4096});
        let scores = self
            .transport
            .execute(
                host,
                ticket,
                ModelRequest {
                    usage: TransmissionUsage::Rerank,
                    entries,
                    payload,
                },
                control,
                |body| {
                    let response = response_json(body)?;
                    indexed_results(&response, "results", entries.len())?
                        .into_iter()
                        .map(|item| {
                            item["relevance_score"]
                                .as_f64()
                                .filter(|score| score.is_finite() && (0.0..=1.0).contains(score))
                                .ok_or(SearchError::InvalidResponse("invalid rerank score"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                },
            )
            .await?;
        let mut ranked: Vec<_> = entries.iter().cloned().zip(scores).collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(ranked)
    }

    /// None means the core plan disabled reranking; no network or receipt is created.
    /// This adapter never silently falls back after a failed/unknown transmission.
    pub async fn rerank<Host: RetrievalHost>(
        &self,
        host: &mut Host,
        ticket: &mut CandidateTicket,
        entries: &[CandidateRef],
        control: &CallControl,
    ) -> Result<Option<Vec<(CandidateRef, f64)>>, SearchError> {
        let Some(profile) = ticket.plan().rerank() else {
            return Ok(None);
        };
        if profile != &self.profile {
            return Err(SearchError::ProfileMismatch);
        }
        let texts = document_inputs(ticket, entries, &self.transport.options, MAX_RERANK)?;
        let query = ticket.query().text();
        self.transport.options.text(query)?;
        if texts.iter().any(|text| query.len() + text.len() > 3072) {
            return Err(SearchError::InputTooLong);
        }
        let payload = json!({"model": self.profile.model(), "query": query, "documents": texts,
            "top_n": entries.len(), "max_tokens_per_doc": 4096});
        let scores = self
            .transport
            .execute(
                host,
                ticket,
                ModelRequest {
                    usage: TransmissionUsage::Rerank,
                    entries,
                    payload,
                },
                control,
                |body| {
                    let response = response_json(body)?;
                    indexed_results(&response, "results", entries.len())?
                        .into_iter()
                        .map(|item| {
                            item["relevance_score"]
                                .as_f64()
                                .filter(|score| score.is_finite() && (0.0..=1.0).contains(score))
                                .ok_or(SearchError::InvalidResponse("invalid rerank score"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                },
            )
            .await?;
        let mut ranked: Vec<_> = entries.iter().cloned().zip(scores).collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(Some(ranked))
    }
}
