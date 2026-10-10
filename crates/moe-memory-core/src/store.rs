use std::collections::BTreeMap;

use crate::model::SourceKey;
use crate::{Epistemic, MemoryError, MemoryId, MemoryRecord};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceVersion {
    pub(crate) revision: u64,
    pub(crate) epistemic: Epistemic,
    pub(crate) revoked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_exhaustion_does_not_wrap_or_mutate_the_counter() {
        let mut store = Store {
            next_id: u64::MAX,
            ..Store::default()
        };
        assert_eq!(store.allocate_id(), Err(MemoryError::CapacityExceeded));
        assert_eq!(store.next_id, u64::MAX);
    }
}

#[derive(Clone, Default)]
pub(crate) struct Store {
    pub(crate) records: BTreeMap<MemoryId, MemoryRecord>,
    pub(crate) sources: BTreeMap<SourceKey, SourceVersion>,
    pub(crate) tombstones: BTreeMap<MemoryId, (crate::PetIdentity, crate::MemoryStatus)>,
    pub(crate) generation: u64,
    next_id: u64,
}

impl Store {
    pub(crate) fn next_generation(&self) -> Result<u64, MemoryError> {
        self.generation
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)
    }

    pub(crate) fn allocate_id(&mut self) -> Result<MemoryId, MemoryError> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(MemoryError::CapacityExceeded)?;
        Ok(MemoryId(self.next_id))
    }
}
