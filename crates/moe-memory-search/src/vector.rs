use std::collections::BTreeSet;
use std::fmt;

use moe_memory_core::{CandidateRef, CandidateTicket, EmbeddingProfile, RetrievalItem};

use crate::SearchError;

#[derive(Clone)]
pub(crate) struct UnitVector(pub(crate) Vec<f64>);

impl UnitVector {
    pub(crate) fn new(values: Vec<f64>, dimension: usize) -> Result<Self, SearchError> {
        if dimension == 0
            || values.len() != dimension
            || !values.iter().all(|value| value.is_finite())
        {
            return Err(SearchError::InvalidResponse(
                "invalid embedding dimension or value",
            ));
        }
        let scale = values
            .iter()
            .fold(0.0_f64, |largest, value| largest.max(value.abs()));
        if scale == 0.0 {
            return Err(SearchError::InvalidResponse("zero embedding norm"));
        }
        let scaled_norm = values
            .iter()
            .map(|value| (value / scale).powi(2))
            .sum::<f64>()
            .sqrt();
        let normalized = values
            .into_iter()
            .map(|value| (value / scale) / scaled_norm)
            .collect();
        Ok(Self(normalized))
    }

    pub(crate) fn cosine(&self, other: &Self) -> f64 {
        self.0
            .iter()
            .zip(&other.0)
            .map(|(left, right)| left * right)
            .sum::<f64>()
            .clamp(-1.0, 1.0)
    }
}

/// Encoded from the exact sealed query, never caller-supplied replacement text.
pub struct EncodedQuery {
    pub(crate) profile: EmbeddingProfile,
    pub(crate) text: String,
    pub(crate) vector: UnitVector,
}

/// The full input snapshot and ticket-bound reference accompany the vector.
pub struct EmbeddedCandidate {
    pub(crate) profile: EmbeddingProfile,
    pub(crate) reference: CandidateRef,
    pub(crate) input: RetrievalItem,
    pub(crate) vector: UnitVector,
}

impl EmbeddedCandidate {
    pub fn candidate_ref(&self) -> &CandidateRef {
        &self.reference
    }
}

impl fmt::Debug for EncodedQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EncodedQuery([redacted])")
    }
}

impl fmt::Debug for EmbeddedCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EmbeddedCandidate([redacted])")
    }
}

/// Scans only ticket-bound, snapshot-matching candidates in this vector space.
/// The host supplies a calibrated threshold; this function grants no authority.
/// Core completion and delivery must still revalidate current authorization.
pub fn exact_scan(
    ticket: &CandidateTicket,
    query: &EncodedQuery,
    candidates: &[EmbeddedCandidate],
    minimum_cosine: f64,
    limit: usize,
) -> Result<Vec<(CandidateRef, f64)>, SearchError> {
    if !minimum_cosine.is_finite()
        || !(-1.0..=1.0).contains(&minimum_cosine)
        || !(1..=100).contains(&limit)
    {
        return Err(SearchError::InvalidInput("invalid scan threshold or limit"));
    }
    if query.text.is_empty()
        || query.text != ticket.query().text()
        || &query.profile != ticket.plan().profile()
    {
        return Err(SearchError::ProfileMismatch);
    }
    if candidates.len() > ticket.candidates().len() {
        return Err(SearchError::InvalidInput("too many scan candidates"));
    }
    let mut seen = BTreeSet::new();
    let mut ranked = Vec::new();
    for candidate in candidates {
        if candidate.profile != query.profile {
            return Err(SearchError::ProfileMismatch);
        }
        if !seen.insert(&candidate.reference)
            || ticket.candidate(&candidate.reference) != Some(&candidate.input)
        {
            return Err(SearchError::InvalidInput(
                "foreign, duplicate or changed candidate",
            ));
        }
        let score = query.vector.cosine(&candidate.vector);
        if score >= minimum_cosine {
            ranked.push((candidate.reference.clone(), score));
        }
    }
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.truncate(limit);
    Ok(ranked)
}

#[cfg(test)]
mod tests {
    use super::UnitVector;

    #[test]
    fn normalization_rejects_wrong_dimensions_zero_and_nonfinite_values() {
        for values in [
            vec![],
            vec![1.0],
            vec![0.0, 0.0],
            vec![f64::NAN, 1.0],
            vec![f64::INFINITY, 1.0],
            vec![f64::NEG_INFINITY, 1.0],
        ] {
            assert!(UnitVector::new(values, 2).is_err());
        }
    }

    #[test]
    fn normalization_is_stable_at_extreme_finite_magnitudes() {
        for scale in [1e-300, 1.0, 1e300, f64::MAX / 4.0] {
            let vector = UnitVector::new(vec![scale, scale], 2).unwrap();
            assert!((vector.cosine(&vector) - 1.0).abs() < 1e-12);
            assert!(vector.0.iter().all(|value| value.is_finite()));
        }
    }
}
