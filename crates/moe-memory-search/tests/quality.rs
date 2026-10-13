use std::collections::BTreeSet;

use moe_memory_search::{OnlineEmbedding, OnlineOptions};

#[path = "../examples/support/quality.rs"]
mod quality;
#[path = "../examples/support/quality_runner.rs"]
mod quality_runner;

fn allowed(fixture: &quality::Fixture, case: &quality::Case) -> BTreeSet<String> {
    fixture
        .records
        .iter()
        .filter(|record| record.scope == case.scope)
        .map(|record| record.id.clone())
        .collect()
}

#[test]
fn frozen_labels_have_exact_counts_and_independent_calibration() {
    let fixture = quality::load().unwrap();
    assert_eq!(fixture.records.len(), 32);
    assert_eq!(
        fixture
            .evaluation
            .iter()
            .filter(|case| !case.relevant.is_empty())
            .count(),
        32
    );
    assert_eq!(
        fixture
            .evaluation
            .iter()
            .filter(|case| case.scope == "private" && !case.relevant.is_empty())
            .count(),
        16
    );
    assert_eq!(
        fixture
            .evaluation
            .iter()
            .filter(|case| case.scope != "private" && !case.relevant.is_empty())
            .count(),
        16
    );
    assert_eq!(
        fixture
            .evaluation
            .iter()
            .filter(|case| case.paraphrase)
            .count(),
        16
    );
    assert_eq!(
        fixture
            .evaluation
            .iter()
            .filter(|case| case.relevant.is_empty())
            .count(),
        8
    );
    assert_eq!(
        fixture
            .evaluation
            .iter()
            .filter(|case| case.scope == "formal" && !case.relevant.is_empty())
            .count(),
        4
    );
    let ids: BTreeSet<_> = fixture
        .evaluation
        .iter()
        .map(|case| &case.id)
        .chain(fixture.records.iter().map(|record| &record.id))
        .collect();
    assert!(
        fixture
            .calibration
            .iter()
            .map(|case| &case.id)
            .chain(fixture.calibration_records.iter().map(|record| &record.id))
            .all(|id| !ids.contains(id))
    );
}

#[test]
fn metrics_count_rank_discount_noise_empty_and_safety_without_hiding_hits() {
    let fixture = quality::load().unwrap();
    let case = &fixture.evaluation[0];
    let mut summary = quality::Summary::default();
    let returned = vec!["forbidden".to_owned(), "p01".to_owned()];
    summary
        .observe(
            case,
            &returned,
            &allowed(&fixture, case),
            &BTreeSet::from(["p01".to_owned()]),
        )
        .unwrap();
    assert_eq!(
        (
            summary.hits,
            summary.noise,
            summary.unauthorized,
            summary.stale
        ),
        (1, 1, 1, 1)
    );
    assert!((summary.ndcg_sum - 1.0 / 3.0_f64.log2()).abs() < 1e-12);
    assert!(
        summary
            .observe(
                case,
                &["p01".to_owned(), "p01".to_owned()],
                &allowed(&fixture, case),
                &BTreeSet::new()
            )
            .is_err()
    );
    assert!(
        summary
            .observe(
                case,
                &vec!["x".to_owned(); 6],
                &allowed(&fixture, case),
                &BTreeSet::new()
            )
            .is_err()
    );
    summary.print("metric-unit-test-not-model-quality");
}

#[test]
fn quality_gate_requires_all_frozen_denominators_and_strict_paraphrase_gain() {
    let fixture = quality::load().unwrap();
    let mut perfect = quality::Summary::default();
    let mut baseline = quality::Summary::default();
    for case in &fixture.evaluation {
        let expected: Vec<_> = case.relevant.keys().cloned().collect();
        perfect
            .observe(case, &expected, &allowed(&fixture, case), &BTreeSet::new())
            .unwrap();
        baseline
            .observe(case, &[], &allowed(&fixture, case), &BTreeSet::new())
            .unwrap();
    }
    assert!(perfect.passes(&baseline));
    assert!(!perfect.passes(&perfect));
    perfect.hits = 27;
    assert!(!perfect.passes(&baseline));
    perfect.hits = 32;
    perfect.paraphrase_hits = 11;
    assert!(!perfect.passes(&baseline));
    perfect.paraphrase_hits = 16;
    perfect.unrelated_empty = 6;
    assert!(!perfect.passes(&baseline));
    perfect.unrelated_empty = 8;
    perfect.unauthorized = 1;
    assert!(!perfect.passes(&baseline));
    perfect.unauthorized = 0;
    perfect.stale = 1;
    assert!(!perfect.passes(&baseline));
}

fn calibration_scores(fixture: &quality::Fixture) -> quality::CalibrationScores {
    fixture
        .calibration
        .iter()
        .map(|case| {
            (
                case.id.clone(),
                fixture
                    .calibration_records
                    .iter()
                    .filter(|record| record.scope == case.scope)
                    .map(|record| {
                        (
                            record.id.clone(),
                            if case.relevant.contains_key(&record.id) {
                                0.92
                            } else {
                                0.2
                            },
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn calibration_uses_only_independent_queries_and_rejects_malformed_scores() {
    let fixture = quality::load().unwrap();
    let observations = calibration_scores(&fixture);
    let threshold = quality::calibrate(&fixture, &observations).unwrap();
    assert_eq!(threshold, 0.92);
    let mut invalid = observations.clone();
    invalid[0].0 = fixture.evaluation[0].id.clone();
    assert!(quality::calibrate(&fixture, &invalid).is_err());
    invalid = observations.clone();
    invalid[0].1[0].1 = f64::NAN;
    assert!(quality::calibrate(&fixture, &invalid).is_err());
    invalid = observations.clone();
    invalid[0].1[0].0 = "foreign".to_owned();
    assert!(quality::calibrate(&fixture, &invalid).is_err());
    assert!(quality::calibrate(&fixture, &observations[..11].to_vec()).is_err());
}

#[tokio::test]
async fn offline_keyword_baseline_uses_identical_corpus_scopes_and_budget_without_network() {
    let fixture = quality::load().unwrap();
    let embedding = OnlineEmbedding::new(
        "https://offline.invalid/v1/embeddings",
        "synthetic-unused",
        "not-a-model-backend",
        2,
        OnlineOptions::default(),
    )
    .unwrap();
    let mut world = quality_runner::World::new(&fixture.records, "offline-keyword-test").unwrap();
    let evaluation = world
        .evaluate(
            &fixture.evaluation,
            &embedding,
            None,
            0.92,
            ("offline-KeywordOnly", true),
        )
        .await
        .unwrap();
    let summary = &evaluation.cold;
    assert_eq!(
        (
            summary.cases,
            summary.answers,
            summary.paraphrases,
            summary.unrelated
        ),
        (40, 32, 16, 8)
    );
    assert_eq!((summary.unauthorized, summary.stale), (0, 0));
    assert_eq!(
        (
            summary.hits,
            summary.paraphrase_hits,
            summary.unrelated_empty
        ),
        (17, 1, 7)
    );
    assert_eq!(
        (
            evaluation.hot.hits,
            evaluation.hot.paraphrase_hits,
            evaluation.hot.unrelated_empty
        ),
        (17, 1, 7)
    );
    summary.print("offline-KeywordOnly-not-real-quality");
    let mut calibration =
        quality_runner::World::new(&fixture.calibration_records, "calibration-unused-network")
            .unwrap();
    calibration
        .authorize("https://offline.invalid/v1/embeddings", None)
        .unwrap();
    assert!(
        calibration
            .calibration_scores(&[], &embedding)
            .await
            .unwrap()
            .is_empty()
    );
}
