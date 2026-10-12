//! Deterministic lexical ranking over a core-authorized candidate ticket.
//!
//! Latin letters and ASCII digits form lowercase terms; punctuation and all
//! other characters separate terms. Contiguous Han characters form
//! overlapping bigrams, except a one-character Han run, which forms one
//! unigram. No Unicode normalization or external tokenizer is applied.
//!
//! BM25 document frequency and average length are computed only from every
//! candidate in the supplied ticket. Raw exact matches use the original,
//! nonempty query as a case-sensitive substring and remain separate from
//! normalized term matching.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use moe_memory_core::{CandidateRef, CandidateTicket, RecordVersion};

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// A lexical result tied to a reference from the input candidate ticket.
#[derive(Debug)]
pub struct LexicalHit {
    reference: CandidateRef,
    score: f64,
    exact_match: bool,
}

impl LexicalHit {
    /// Returns the ticket-bound reference for this result.
    pub fn reference(&self) -> &CandidateRef {
        &self.reference
    }

    /// Returns the BM25 score calculated from this ticket's candidates only.
    pub fn score(&self) -> f64 {
        self.score
    }

    /// Returns whether the original query is a case-sensitive substring.
    pub fn exact_match(&self) -> bool {
        self.exact_match
    }
}

struct DocumentTerms {
    reference: CandidateRef,
    frequencies: BTreeMap<String, usize>,
    length: usize,
    exact_match: bool,
}

/// Ranks matching candidates with deterministic BM25 and a separate exact
/// substring signal. Ties are resolved by exact-match status and stable
/// reference fields. An empty query and nonmatching candidates produce no hit.
pub fn lexical_rank(ticket: &CandidateTicket) -> Vec<LexicalHit> {
    let query = ticket.query().text();
    if query.is_empty() {
        return Vec::new();
    }

    let query_terms: BTreeSet<_> = tokenize(query).into_iter().collect();
    let documents: Vec<_> = ticket
        .candidates()
        .iter()
        .map(|candidate| {
            let text = candidate.item().text();
            let frequencies = term_frequencies(text);
            let length = frequencies.values().sum();
            DocumentTerms {
                reference: candidate.candidate_ref(),
                frequencies,
                length,
                exact_match: text.contains(query),
            }
        })
        .collect();

    if documents.is_empty() {
        return Vec::new();
    }

    let document_count = documents.len() as f64;
    let average_length = documents
        .iter()
        .map(|document| document.length as f64)
        .sum::<f64>()
        / document_count;

    let document_frequencies: BTreeMap<_, _> = query_terms
        .iter()
        .filter_map(|term| {
            let frequency = documents
                .iter()
                .filter(|document| document.frequencies.contains_key(term))
                .count();
            (frequency > 0).then_some((term.as_str(), frequency))
        })
        .collect();

    let mut hits = Vec::new();
    for document in documents {
        let mut score = 0.0;
        if average_length > 0.0 {
            for (term, document_frequency) in &document_frequencies {
                let Some(&term_frequency) = document.frequencies.get(*term) else {
                    continue;
                };
                let term_frequency = term_frequency as f64;
                let document_frequency = *document_frequency as f64;
                let inverse_document_frequency = (1.0
                    + (document_count - document_frequency + 0.5) / (document_frequency + 0.5))
                    .ln();
                let length_normalization =
                    BM25_K1 * (1.0 - BM25_B + BM25_B * document.length as f64 / average_length);
                score += inverse_document_frequency * term_frequency * (BM25_K1 + 1.0)
                    / (term_frequency + length_normalization);
            }
        }

        if score > 0.0 || document.exact_match {
            hits.push(LexicalHit {
                reference: document.reference,
                score,
                exact_match: document.exact_match,
            });
        }
    }

    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.exact_match.cmp(&left.exact_match))
            .then_with(|| stable_reference_cmp(&left.reference, &right.reference))
    });
    hits
}

fn term_frequencies(text: &str) -> BTreeMap<String, usize> {
    let mut frequencies = BTreeMap::new();
    for term in tokenize(text) {
        *frequencies.entry(term).or_insert(0) += 1;
    }
    frequencies
}

pub(crate) fn matching_terms(query: &str, text: &str) -> usize {
    let query: BTreeSet<_> = tokenize(query).into_iter().collect();
    let terms: BTreeSet<_> = tokenize(text).into_iter().collect();
    query.intersection(&terms).count()
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut characters = text.chars().peekable();

    while let Some(character) = characters.next() {
        if is_han(character) {
            let mut run = vec![character];
            while characters.peek().is_some_and(|next| is_han(*next)) {
                run.push(characters.next().expect("peeked Han character"));
            }
            if run.len() == 1 {
                tokens.push(run.into_iter().collect());
            } else {
                tokens.extend(run.windows(2).map(|pair| pair.iter().collect::<String>()));
            }
        } else if is_latin_letter_or_ascii_digit(character) {
            let mut token = String::new();
            token.extend(character.to_lowercase());
            while characters
                .peek()
                .is_some_and(|next| is_latin_letter_or_ascii_digit(*next))
            {
                token.extend(
                    characters
                        .next()
                        .expect("peeked Latin character")
                        .to_lowercase(),
                );
            }
            tokens.push(token);
        }
    }

    tokens
}

fn is_latin_letter_or_ascii_digit(character: char) -> bool {
    character.is_ascii_digit()
        || (character.is_alphabetic()
            && matches!(
                character as u32,
                0x0041..=0x024f
                    | 0x1e00..=0x1eff
                    | 0x2c60..=0x2c7f
                    | 0xa720..=0xa7ff
                    | 0xab30..=0xab6f
                    | 0x10780..=0x107bf
                    | 0x1df00..=0x1dfff
            ))
}

fn is_han(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xf900..=0xfaff
            | 0x20000..=0x2a6df
            | 0x2a700..=0x2b73f
            | 0x2b740..=0x2b81f
            | 0x2b820..=0x2ceaf
            | 0x2ceb0..=0x2ebef
            | 0x2ebf0..=0x2ee5f
            | 0x2f800..=0x2fa1f
            | 0x30000..=0x3134f
            | 0x31350..=0x323af
    )
}

fn stable_reference_cmp(left: &CandidateRef, right: &CandidateRef) -> Ordering {
    match (left.record(), right.record()) {
        (Some(left), Some(right)) => stable_record_cmp(left, right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.working_context().cmp(&right.working_context()),
    }
}

fn stable_record_cmp(left: RecordVersion, right: RecordVersion) -> Ordering {
    left.cmp(&right)
}

#[cfg(test)]
mod tests {
    use moe_memory_core::{
        Category, EmbeddingProfile, Epistemic, MemoryDraft, MemoryScope, MemoryService, Namespace,
        PetIdentity, ReadPolicy, ReadPurpose, RetrievalPlan, RetrievalQuery, SessionId,
    };

    use super::{lexical_rank, stable_reference_cmp, tokenize};

    fn ticket(query: &str, texts: &[&str]) -> moe_memory_core::CandidateTicket {
        let mut service = MemoryService::new();
        let owner =
            PetIdentity::new(Namespace::new("lexical-tests", "user").unwrap(), "alice").unwrap();
        let source = service
            .register_source(
                MemoryScope::Private(owner.clone()),
                "fixture",
                1,
                Epistemic::UserStatement,
            )
            .unwrap();

        for text in texts {
            service
                .for_pet(&owner)
                .create(
                    MemoryDraft::new(
                        *text,
                        Category::Episode,
                        Epistemic::UserStatement,
                        vec![source.clone()],
                    ),
                    100,
                )
                .unwrap();
        }
        service
            .set_read_policy(&owner, ReadPolicy::offline(1, vec![source]))
            .unwrap();

        let session = SessionId::new("lexical-test-session").unwrap();
        let plan =
            RetrievalPlan::new(EmbeddingProfile::new("fixture", "fixture-model", 2).unwrap());
        service
            .for_session(&owner, &session, ReadPurpose::OfflineReasoning)
            .begin_candidates(&RetrievalQuery::new(query), plan, 100)
            .unwrap()
    }

    #[test]
    fn tokenization_uses_latin_words_han_bigrams_and_punctuation_boundaries() {
        assert_eq!(
            tokenize("Rust语言，北京！AI-model 42"),
            ["rust", "语言", "北京", "ai", "model", "42"]
        );
        assert_eq!(tokenize("Café ÉCOLE"), ["café", "école"]);
    }

    #[test]
    fn a_single_han_character_is_one_unigram_and_long_runs_use_bigrams() {
        assert_eq!(tokenize("我"), ["我"]);
        assert_eq!(tokenize("南京市"), ["南京", "京市"]);
    }

    #[test]
    fn terms_ignore_latin_case_while_exact_substrings_preserve_it() {
        let ticket = ticket("Rust", &["RUST language", "Rustacean", "rust tool"]);
        let hits = lexical_rank(&ticket);

        assert_eq!(hits.len(), 3);
        assert!(!hits[0].exact_match());
        assert!(
            hits.iter()
                .any(|hit| hit.exact_match() && hit.score() == 0.0)
        );
        assert!(
            hits.iter()
                .any(|hit| !hit.exact_match() && hit.score() > 0.0)
        );
    }

    #[test]
    fn bm25_ranking_is_repeatable_and_ties_use_stable_ticket_references() {
        let ticket = ticket("tea", &["tea", "tea", "tea tea tea", "coffee"]);
        let first = lexical_rank(&ticket);
        let second = lexical_rank(&ticket);

        assert_eq!(first.len(), 3);
        assert!(first[0].score() >= first[1].score());
        assert!(first[1].score() >= first[2].score());
        assert_eq!(
            first
                .iter()
                .map(|hit| (
                    hit.reference().clone(),
                    hit.score().to_bits(),
                    hit.exact_match()
                ))
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|hit| (
                    hit.reference().clone(),
                    hit.score().to_bits(),
                    hit.exact_match()
                ))
                .collect::<Vec<_>>()
        );
        let tied_hits = first
            .windows(2)
            .find(|pair| pair[0].score().to_bits() == pair[1].score().to_bits())
            .unwrap();
        assert_eq!(
            stable_reference_cmp(tied_hits[0].reference(), tied_hits[1].reference()),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn nonmatching_and_empty_queries_do_not_fill_results() {
        assert!(lexical_rank(&ticket("Mars", &["Earth", "Venus"])).is_empty());
        assert!(lexical_rank(&ticket("", &["Earth", "Venus"])).is_empty());
    }
}
