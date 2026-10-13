use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const FIXTURE: &str = include_str!("../../tests/fixtures/r4-chinese-v1.json");
pub const FIXTURE_SHA256: &str = "2d50243e19510c41514c1299e682ebbd8c490c7cefed18f06423b5c3f32dc6a8";

#[derive(Clone, Debug)]
pub struct Record {
    pub id: String,
    pub scope: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Case {
    pub id: String,
    pub scope: String,
    pub text: String,
    pub paraphrase: bool,
    pub relevant: BTreeMap<String, u8>,
}

pub struct Fixture {
    pub records: Vec<Record>,
    pub evaluation: Vec<Case>,
    pub calibration_records: Vec<Record>,
    pub calibration: Vec<Case>,
}

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn string(value: &Value, name: &str) -> Result<String, &'static str> {
    value[name]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .ok_or("invalid fixture string")
}

fn records(value: &Value) -> Result<Vec<Record>, &'static str> {
    value
        .as_array()
        .ok_or("invalid fixture records")?
        .iter()
        .map(|entry| {
            Ok(Record {
                id: string(entry, "id")?,
                scope: string(entry, "scope")?,
                text: string(entry, "text")?,
            })
        })
        .collect()
}

fn cases(value: &Value) -> Result<Vec<Case>, &'static str> {
    value
        .as_array()
        .ok_or("invalid fixture queries")?
        .iter()
        .map(|entry| {
            let relevant = entry["relevant"]
                .as_object()
                .ok_or("invalid relevance labels")?
                .iter()
                .map(|(id, gain)| {
                    let gain = gain
                        .as_u64()
                        .filter(|gain| (1..=3).contains(gain))
                        .ok_or("invalid relevance grade")?;
                    Ok((id.clone(), gain as u8))
                })
                .collect::<Result<_, &'static str>>()?;
            Ok(Case {
                id: string(entry, "id")?,
                scope: string(entry, "scope")?,
                text: string(entry, "text")?,
                paraphrase: entry["paraphrase"]
                    .as_bool()
                    .ok_or("invalid paraphrase label")?,
                relevant,
            })
        })
        .collect()
}

pub fn load() -> Result<Fixture, &'static str> {
    if digest(FIXTURE.as_bytes()) != FIXTURE_SHA256 {
        return Err("frozen fixture fingerprint changed");
    }
    let value: Value = serde_json::from_str(FIXTURE).map_err(|_| "invalid fixture JSON")?;
    let fixture = Fixture {
        records: records(&value["records"])?,
        evaluation: cases(&value["evaluation"])?,
        calibration_records: records(&value["calibration_records"])?,
        calibration: cases(&value["calibration"])?,
    };
    if value["version"] != "r4-chinese-v1"
        || fixture.evaluation.len() != 40
        || fixture.calibration.len() != 12
    {
        return Err("invalid frozen fixture version or counts");
    }
    for (records, cases) in [
        (&fixture.records, &fixture.evaluation),
        (&fixture.calibration_records, &fixture.calibration),
    ] {
        let ids: BTreeSet<_> = records.iter().map(|record| &record.id).collect();
        let queries: BTreeSet<_> = cases.iter().map(|case| &case.id).collect();
        if ids.len() != records.len() || queries.len() != cases.len() {
            return Err("duplicate fixture identifier");
        }
        for case in cases {
            if !["private", "shared", "formal"].contains(&case.scope.as_str())
                || case.relevant.iter().any(|(id, _)| {
                    !records.iter().any(|record| {
                        &record.id == id
                            && record.scope == case.scope
                            && (!case.paraphrase || !record.text.contains(&case.text))
                    })
                })
            {
                return Err("invalid scope, label or paraphrase claim");
            }
        }
    }
    let eval_text: BTreeSet<_> = fixture
        .evaluation
        .iter()
        .map(|case| &case.text)
        .chain(fixture.records.iter().map(|record| &record.text))
        .collect();
    if fixture
        .calibration
        .iter()
        .map(|case| &case.text)
        .chain(
            fixture
                .calibration_records
                .iter()
                .map(|record| &record.text),
        )
        .any(|text| eval_text.contains(text))
    {
        return Err("calibration overlaps evaluation");
    }
    Ok(fixture)
}

#[derive(Default, Debug)]
pub struct Summary {
    pub cases: usize,
    pub answers: usize,
    pub paraphrases: usize,
    pub unrelated: usize,
    pub hits: usize,
    pub paraphrase_hits: usize,
    pub unrelated_empty: usize,
    pub answer_empty: usize,
    pub noise: usize,
    pub unauthorized: usize,
    pub stale: usize,
    pub ndcg_sum: f64,
}

impl Summary {
    pub fn observe(
        &mut self,
        case: &Case,
        returned: &[String],
        allowed: &BTreeSet<String>,
        stale: &BTreeSet<String>,
    ) -> Result<(), &'static str> {
        if returned.len() > 5 || returned.iter().collect::<BTreeSet<_>>().len() != returned.len() {
            return Err("invalid top-five results");
        }
        self.cases += 1;
        self.unauthorized += returned.iter().filter(|id| !allowed.contains(*id)).count();
        self.stale += returned.iter().filter(|id| stale.contains(*id)).count();
        self.noise += returned
            .iter()
            .filter(|id| !case.relevant.contains_key(*id))
            .count();
        if case.relevant.is_empty() {
            self.unrelated += 1;
            self.unrelated_empty += usize::from(returned.is_empty());
        } else {
            self.answers += 1;
            let hit = returned.iter().any(|id| case.relevant.contains_key(id));
            self.hits += usize::from(hit);
            self.answer_empty += usize::from(returned.is_empty());
            if case.paraphrase {
                self.paraphrases += 1;
                self.paraphrase_hits += usize::from(hit);
            }
            let dcg = |gains: &[u8]| -> f64 {
                gains
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(position, gain)| {
                        (2.0_f64.powi(i32::from(*gain)) - 1.0) / ((position + 2) as f64).log2()
                    })
                    .sum()
            };
            let actual: Vec<_> = returned
                .iter()
                .map(|id| case.relevant.get(id).copied().unwrap_or(0))
                .collect();
            let mut ideal: Vec<_> = case.relevant.values().copied().collect();
            ideal.sort_by(|left, right| right.cmp(left));
            self.ndcg_sum += dcg(&actual) / dcg(&ideal);
        }
        Ok(())
    }

    pub fn passes(&self, keyword: &Self) -> bool {
        self.cases == 40
            && self.answers == 32
            && self.paraphrases == 16
            && self.unrelated == 8
            && self.hits >= 28
            && self.paraphrase_hits >= 12
            && self.unrelated_empty >= 7
            && self.unauthorized == 0
            && self.stale == 0
            && keyword.cases == 40
            && keyword.unauthorized == 0
            && keyword.stale == 0
            && self.paraphrase_hits > keyword.paraphrase_hits
    }

    pub fn print(&self, name: &str) {
        println!(
            "mode={name} hit_at_5={}/{} paraphrase={}/{} unrelated_empty={}/{} answer_empty={} noise_items={} unauthorized={} stale={} ndcg_at_5_answered={:.6}",
            self.hits,
            self.answers,
            self.paraphrase_hits,
            self.paraphrases,
            self.unrelated_empty,
            self.unrelated,
            self.answer_empty,
            self.noise,
            self.unauthorized,
            self.stale,
            self.ndcg_sum / self.answers.max(1) as f64
        );
    }
}

pub type Scores = Vec<(String, f64)>;
pub type CalibrationScores = Vec<(String, Scores)>;

pub fn calibrate(fixture: &Fixture, observations: &CalibrationScores) -> Result<f64, &'static str> {
    if observations.len() != fixture.calibration.len()
        || observations
            .iter()
            .map(|(id, _)| id)
            .collect::<BTreeSet<_>>()
            .len()
            != observations.len()
    {
        return Err("incomplete or duplicate calibration observations");
    }
    let mut labeled = Vec::new();
    for (id, scores) in observations {
        let case = fixture
            .calibration
            .iter()
            .find(|case| &case.id == id)
            .ok_or("evaluation data cannot calibrate threshold")?;
        let allowed: BTreeSet<_> = fixture
            .calibration_records
            .iter()
            .filter(|record| record.scope == case.scope)
            .map(|record| &record.id)
            .collect();
        if scores.len() != allowed.len()
            || scores.iter().any(|(id, score)| {
                !allowed.contains(id) || !score.is_finite() || !(-1.0..=1.0).contains(score)
            })
            || scores
                .iter()
                .map(|(id, _)| id)
                .collect::<BTreeSet<_>>()
                .len()
                != scores.len()
        {
            return Err("invalid calibration vector scores");
        }
        let mut sorted = scores.clone();
        sorted.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        labeled.push((case, sorted));
    }
    let mut best = (0, 0.0);
    for step in 0..=50 {
        let threshold = f64::from(step) / 50.0;
        let correct = labeled
            .iter()
            .filter(|(case, scores)| {
                let selected: Vec<_> = scores
                    .iter()
                    .filter(|(_, score)| *score >= threshold)
                    .take(5)
                    .collect();
                if case.relevant.is_empty() {
                    selected.is_empty()
                } else {
                    selected
                        .iter()
                        .any(|(id, _)| case.relevant.contains_key(id))
                }
            })
            .count();
        if correct >= best.0 {
            best = (correct, threshold);
        }
    }
    if best.0 == 0 {
        return Err("calibration contains no usable evidence");
    }
    Ok(best.1)
}
