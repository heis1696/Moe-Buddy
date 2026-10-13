use std::error::Error;

use moe_memory_search::{CohereReranker, OnlineEmbedding, OnlineOptions};

#[path = "support/quality.rs"]
mod quality;
#[path = "support/quality_runner.rs"]
mod quality_runner;

const EMBEDDING_PERMISSION: &str = "synthetic-only:query+entry:private+shared:all-fixture-sources";
const RERANK_PERMISSION: &str = "synthetic-only:query+shortlist:private+shared:all-fixture-sources";

struct Configuration {
    endpoint: String,
    key: String,
    model: String,
    revision: String,
    dimension: usize,
    rerank: Option<RerankConfiguration>,
}

struct RerankConfiguration {
    endpoint: String,
    key: String,
    model: String,
    revision: String,
}

fn required(read: &impl Fn(&str) -> Option<String>, name: &str) -> Result<String, &'static str> {
    read(name)
        .filter(|value| !value.trim().is_empty())
        .ok_or("missing explicit trusted-host configuration; see search README")
}

fn credential(read: &impl Fn(&str) -> Option<String>, name: &str) -> Result<String, &'static str> {
    let reference = required(read, name)?;
    if !reference.starts_with("MOE_R4_")
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err("credential reference must explicitly name a MOE_R4_ environment variable");
    }
    required(read, &reference)
}

fn configuration(
    args: &[String],
    read: &impl Fn(&str) -> Option<String>,
) -> Result<Configuration, &'static str> {
    if args.len() != 4
        || args[0] != "--backend"
        || args[2] != "--rerank"
        || !["off", "on"].contains(&args[3].as_str())
    {
        return Err(
            "usage: embedding_check --backend online|local --rerank off|on; no implicit backend or permission",
        );
    }
    if args[1] == "local" {
        return Err(
            "local model/tokenizer asset backend is not implemented; no fallback or download",
        );
    }
    if args[1] != "online" {
        return Err("unsupported backend");
    }
    if required(read, "MOE_R4_ALLOW_EMBEDDING")? != EMBEDDING_PERMISSION {
        return Err(
            "explicit embedding purposes, private/shared scopes and all synthetic sources required",
        );
    }
    let rerank = if args[3] == "on" {
        if required(read, "MOE_R4_ALLOW_RERANK")? != RERANK_PERMISSION {
            return Err("separate rerank purposes, scopes and all synthetic sources required");
        }
        Some(RerankConfiguration {
            endpoint: required(read, "MOE_R4_RERANK_ENDPOINT")?,
            key: credential(read, "MOE_R4_RERANK_KEY_REF")?,
            model: required(read, "MOE_R4_RERANK_MODEL")?,
            revision: required(read, "MOE_R4_RERANK_REVISION")?,
        })
    } else {
        None
    };
    let dimension = required(read, "MOE_R4_EMBEDDING_DIMENSION")?
        .parse::<usize>()
        .map_err(|_| "invalid explicit embedding dimension")?;
    if !(1..=8192).contains(&dimension) {
        return Err("invalid explicit embedding dimension");
    }
    Ok(Configuration {
        endpoint: required(read, "MOE_R4_EMBEDDING_ENDPOINT")?,
        key: credential(read, "MOE_R4_EMBEDDING_KEY_REF")?,
        model: required(read, "MOE_R4_EMBEDDING_MODEL")?,
        revision: required(read, "MOE_R4_EMBEDDING_REVISION")?,
        dimension,
        rerank,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let config = configuration(&args, &|name| std::env::var(name).ok())?;
    let fixture = quality::load()?;
    let embedding = OnlineEmbedding::new(
        &config.endpoint,
        &config.key,
        &config.model,
        config.dimension,
        OnlineOptions::default(),
    )?;
    let reranker = config
        .rerank
        .as_ref()
        .map(|rerank| {
            CohereReranker::new(
                &rerank.endpoint,
                &rerank.key,
                &rerank.model,
                OnlineOptions::default(),
            )
        })
        .transpose()?;
    let fingerprint = quality::digest(
        format!(
            "{}|{}|{}|{}|{}|chunk=128/16/1024|calibration-grid=0:0.02:1|{}",
            config.endpoint,
            config.model,
            config.revision,
            config.dimension,
            config
                .rerank
                .as_ref()
                .map(|rerank| format!("{}|{}|{}", rerank.endpoint, rerank.model, rerank.revision))
                .unwrap_or_else(|| "disabled".to_owned()),
            quality::FIXTURE_SHA256
        )
        .as_bytes(),
    );
    println!(
        "backend=online config_sha256={fingerprint} fixture_sha256={} dimension={} max_input_bytes=1024 max_attempts=1 calibration_queries=12 evaluation_queries=40 tokenizer_gate=unverified remote_revision=host_declared_not_attested",
        quality::FIXTURE_SHA256,
        config.dimension
    );
    let mut calibration = quality_runner::World::new(&fixture.calibration_records, "calibration")?;
    calibration.authorize(&config.endpoint, None)?;
    let observations = calibration
        .calibration_scores(&fixture.calibration, &embedding)
        .await?;
    let threshold = quality::calibrate(&fixture, &observations)?;
    println!(
        "threshold={threshold:.2} calibration_vector_calls=24 evaluation_labels_not_used_for_calibration=true"
    );
    let mut world = quality_runner::World::new(&fixture.records, "evaluation")?;
    world.authorize(
        &config.endpoint,
        config
            .rerank
            .as_ref()
            .map(|rerank| rerank.endpoint.as_str()),
    )?;
    let keyword = world
        .evaluate(
            &fixture.evaluation,
            &embedding,
            None,
            threshold,
            ("KeywordOnly", true),
        )
        .await?;
    let hybrid = world
        .evaluate(
            &fixture.evaluation,
            &embedding,
            None,
            threshold,
            ("Hybrid", false),
        )
        .await?;
    keyword.cold.print("KeywordOnly-cold");
    keyword.hot.print("KeywordOnly-hot");
    hybrid.cold.print("Hybrid-cold");
    hybrid.hot.print("Hybrid-hot");
    if let Some(reranker) = &reranker {
        let reranked = world
            .evaluate(
                &fixture.evaluation,
                &embedding,
                Some(reranker),
                threshold,
                ("Hybrid+Rerank", false),
            )
            .await?;
        reranked.cold.print("Hybrid+Rerank-cold");
        reranked.hot.print("Hybrid+Rerank-hot");
        println!(
            "rerank_hit_delta={} rerank_ndcg_delta={:.6} rerank_default=off_no_guaranteed_gain",
            reranked.cold.hits as isize - hybrid.cold.hits as isize,
            (reranked.cold.ndcg_sum - hybrid.cold.ndcg_sum) / 32.0
        );
        if !reranked.cold.passes(&keyword.cold) || !reranked.hot.passes(&keyword.hot) {
            return Err("enabled rerank quality or safety gate failed; disable explicitly rather than weakening labels".into());
        }
    } else {
        println!("real_rerank_effect=unverified_disabled");
    }
    if !hybrid.cold.passes(&keyword.cold) || !hybrid.hot.passes(&keyword.hot) {
        return Err(
            "real embedding quality gate failed; labels and acceptance thresholds remain frozen"
                .into(),
        );
    }
    println!(
        "real_embedding_quality_gate=passed full_R4_gate=incomplete_tokenizer_and_other_unverified_boundaries_remain"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn settings() -> BTreeMap<String, String> {
        [
            ("MOE_R4_ALLOW_EMBEDDING", EMBEDDING_PERMISSION),
            (
                "MOE_R4_EMBEDDING_ENDPOINT",
                "https://synthetic.invalid/v1/embeddings",
            ),
            ("MOE_R4_EMBEDDING_MODEL", "synthetic"),
            ("MOE_R4_EMBEDDING_REVISION", "unattested-test"),
            ("MOE_R4_EMBEDDING_DIMENSION", "2"),
            ("MOE_R4_EMBEDDING_KEY_REF", "MOE_R4_TEST_SECRET"),
            ("MOE_R4_TEST_SECRET", "synthetic-not-a-user-key"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
    }

    fn args(backend: &str, rerank: &str) -> Vec<String> {
        ["--backend", backend, "--rerank", rerank]
            .map(str::to_owned)
            .to_vec()
    }

    #[test]
    fn absent_configuration_and_local_backend_never_select_network_fallback() {
        assert!(configuration(&[], &|_| None).is_err());
        assert!(configuration(&args("online", "off"), &|_| None).is_err());
        assert!(
            configuration(&args("local", "off"), &|_| panic!(
                "local must not read online credentials"
            ))
            .is_err()
        );
    }

    #[test]
    fn every_explicit_embedding_setting_is_required_before_execution() {
        let settings = settings();
        assert!(configuration(&args("online", "off"), &|name| settings.get(name).cloned()).is_ok());
        for missing in settings.keys() {
            assert!(
                configuration(&args("online", "off"), &|name| if name == missing {
                    None
                } else {
                    settings.get(name).cloned()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn rerank_permission_and_credential_cannot_be_inherited_from_embedding() {
        let settings = settings();
        assert!(configuration(&args("online", "on"), &|name| settings.get(name).cloned()).is_err());
        let mut settings = settings;
        settings.insert(
            "MOE_R4_ALLOW_EMBEDDING".to_owned(),
            "shared-only".to_owned(),
        );
        assert!(
            configuration(&args("online", "off"), &|name| settings.get(name).cloned()).is_err()
        );
        settings.insert(
            "MOE_R4_ALLOW_EMBEDDING".to_owned(),
            EMBEDDING_PERMISSION.to_owned(),
        );
        settings.insert(
            "MOE_R4_EMBEDDING_KEY_REF".to_owned(),
            "UNRELATED_CREDENTIAL".to_owned(),
        );
        assert!(
            configuration(&args("online", "off"), &|name| settings.get(name).cloned()).is_err()
        );
    }
}
