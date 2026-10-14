//! Internal index cancellation never collides with legitimate business IDs.
//!
//! Regression for the P2 where the persistent authority bridge constructed
//! `moe-memory-search-cancel-{generation}` itself: a business request that had
//! legitimately used that exact operation ID made every subsequent
//! `SearchIndex::rebuild` fail with `Persistence(OperationConflict)`. The
//! bridge now delegates ID selection to the `LocalMemory` facade, which picks
//! an unoccupied ID from its own idempotency ledger.

use std::path::PathBuf;

use moe_memory_core::{
    EmbeddingProfile, Epistemic, MemoryError, MemoryScope, Namespace, PetIdentity, ReadPolicy,
    ReadPurpose, RetrievalQuery, SessionId,
};
use moe_memory_local::{LocalMemory, OperationId, PersistenceError};
use moe_memory_search::{ChunkConfig, LocalSearchHost, SearchIndex, StorageDomain};

struct Directory(PathBuf);

impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "moe-cancel-ids-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn journal(&self) -> PathBuf {
        self.0.join("memory.journal")
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn identity() -> PetIdentity {
    PetIdentity::new(Namespace::new("cancel-ids", "owner").unwrap(), "pet").unwrap()
}

fn profile() -> EmbeddingProfile {
    EmbeddingProfile::new("fixture", "cancel-model", 2).unwrap()
}

fn config() -> ChunkConfig {
    ChunkConfig::new(1, 512, 16, 1536).unwrap()
}

fn fresh_index() -> SearchIndex {
    SearchIndex::new(
        StorageDomain::new("cancel-ids-root").unwrap(),
        profile(),
        config(),
        32,
        16,
        1024,
    )
    .unwrap()
}

fn rebuild(index: &mut SearchIndex, local: &mut LocalMemory) {
    let mut host = LocalSearchHost::new(local, 0);
    index.rebuild(&mut host, profile(), config()).unwrap();
}

#[test]
fn rebuild_with_legacy_business_operation_id_no_longer_conflicts() {
    let directory = Directory::new("legacy");
    let mut local = LocalMemory::create(directory.journal()).unwrap();
    let pet = identity();
    let business = local
        .register_source(
            OperationId::new("moe-memory-search-cancel-1").unwrap(),
            MemoryScope::Private(pet.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    assert_eq!(local.generation().unwrap(), 1);
    local
        .set_read_policy(&pet, ReadPolicy::offline(1, vec![business.value().clone()]))
        .unwrap();
    let task = local
        .for_session(
            &pet,
            &SessionId::new("session").unwrap(),
            ReadPurpose::OfflineReasoning,
        )
        .begin_task(&RetrievalQuery::new("tea"), 0)
        .unwrap();

    let mut index = fresh_index();
    rebuild(&mut index, &mut local);
    rebuild(&mut index, &mut local);

    assert!(matches!(
        local.validate_task(&task, 0),
        Err(PersistenceError::Memory(MemoryError::TaskCancelled))
    ));
    let confirmed = local
        .register_source(
            OperationId::new("moe-memory-search-cancel-1").unwrap(),
            MemoryScope::Private(pet.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    assert_eq!(confirmed.sequence(), business.sequence());
    assert_eq!(confirmed.value(), business.value());
    assert_eq!(local.generation().unwrap(), 3);
}

#[test]
fn internal_cancel_ids_survive_reopen_and_stay_available_to_business() {
    let directory = Directory::new("reopen");
    let mut local = LocalMemory::create(directory.journal()).unwrap();
    let pet = identity();
    local
        .register_source(
            OperationId::new("source").unwrap(),
            MemoryScope::Private(pet.clone()),
            "message",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    let mut index = fresh_index();
    rebuild(&mut index, &mut local);
    drop(local);

    let mut local = LocalMemory::open(directory.journal()).unwrap();
    rebuild(&mut index, &mut local);
    let generation = local.generation().unwrap();
    let claimed = local
        .register_source(
            OperationId::new(format!("moe-memory-local-cancel-{}-0", generation + 1)).unwrap(),
            MemoryScope::Private(pet),
            "business",
            1,
            Epistemic::UserStatement,
        )
        .unwrap();
    assert_eq!(claimed.sequence(), 4);
    rebuild(&mut index, &mut local);
    assert_eq!(local.generation().unwrap(), generation + 2);
}
