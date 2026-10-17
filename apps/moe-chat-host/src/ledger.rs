//! Bounded request-id ledger used to refuse replays inside one host process.
//!
//! The ledger never evicts an id it has accepted: dropping history would let a repeated
//! request through. Once the capacity is reached the ledger refuses new work, and the
//! caller answers with an explicit full-ledger error instead of silently forgetting ids.

use std::collections::HashSet;

/// Largest number of accepted request ids remembered per process.
pub(crate) const CAPACITY: usize = 4_096;

/// Why a request id was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The id was already accepted by this process.
    Duplicate,
    /// The bounded ledger is full and keeps every id it already accepted.
    Full,
}

pub(crate) struct Ledger {
    accepted: HashSet<String>,
    capacity: usize,
}

impl Ledger {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            accepted: HashSet::with_capacity(capacity),
            capacity,
        }
    }

    /// Records `id` as accepted, or refuses without changing the ledger.
    pub(crate) fn accept(&mut self, id: &str) -> Result<(), Refusal> {
        if self.accepted.contains(id) {
            return Err(Refusal::Duplicate);
        }
        if self.accepted.len() >= self.capacity {
            return Err(Refusal::Full);
        }
        self.accepted.insert(id.to_owned());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.accepted.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_ids_are_refused_without_growing_the_ledger() {
        let mut ledger = Ledger::new(4);
        assert_eq!(ledger.accept("first"), Ok(()));
        assert_eq!(ledger.accept("first"), Err(Refusal::Duplicate));
        assert_eq!(ledger.accept("first"), Err(Refusal::Duplicate));
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn a_full_ledger_refuses_new_ids_and_never_evicts_accepted_ones() {
        let mut ledger = Ledger::new(2);
        assert_eq!(ledger.accept("a"), Ok(()));
        assert_eq!(ledger.accept("b"), Ok(()));
        assert_eq!(ledger.accept("c"), Err(Refusal::Full));
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger.accept("a"), Err(Refusal::Duplicate));
        assert_eq!(ledger.accept("c"), Err(Refusal::Full));
        assert_eq!(ledger.len(), 2);
    }

    #[test]
    fn the_production_capacity_is_finite_and_keeps_every_accepted_id() {
        let mut ledger = Ledger::new(CAPACITY);
        for index in 0..CAPACITY {
            assert_eq!(ledger.accept(&format!("id-{index}")), Ok(()));
        }
        assert_eq!(ledger.len(), CAPACITY);
        assert_eq!(ledger.accept("id-0"), Err(Refusal::Duplicate));
        assert_eq!(ledger.accept("overflow"), Err(Refusal::Full));
        assert_eq!(ledger.len(), CAPACITY);
    }
}
