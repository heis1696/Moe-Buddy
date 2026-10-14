use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use moe_memory_core::{CandidateTicket, MemoryScope};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    IndexStatus, RetrievalHost, SearchError, SearchIndex, SearchReason,
    index::{IndexKey, StoredVector},
    vector::UnitVector,
};

const MAGIC: &[u8; 8] = b"MOEIDX\0\x01";

pub(crate) struct FileCache {
    directory: PathBuf,
    maximum: usize,
}

impl FileCache {
    pub(crate) fn new(directory: &Path, maximum: usize) -> Result<Self, SearchError> {
        if !(1024..=64 * 1024 * 1024).contains(&maximum) {
            return Err(SearchError::InvalidConfig("invalid cache size limit"));
        }
        let directory = directory
            .canonicalize()
            .map_err(|_| SearchError::CacheUnavailable)?;
        if !directory.is_dir() {
            return Err(SearchError::CacheUnavailable);
        }
        Ok(Self { directory, maximum })
    }

    fn path(&self) -> PathBuf {
        self.directory.join("moe-memory-vectors.cache")
    }

    fn read(&self) -> Result<Value, SearchError> {
        let path = self.path();
        let metadata = fs::symlink_metadata(&path).map_err(|_| SearchError::CacheUnavailable)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > self.maximum as u64
        {
            return Err(SearchError::CacheUnavailable);
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|_| SearchError::CacheUnavailable)?
            .take(self.maximum as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| SearchError::CacheUnavailable)?;
        if bytes.len() > self.maximum || bytes.len() < 48 || &bytes[..8] != MAGIC {
            return Err(SearchError::CacheUnavailable);
        }
        let length = u64::from_le_bytes(
            bytes[8..16]
                .try_into()
                .map_err(|_| SearchError::CacheUnavailable)?,
        );
        if length != (bytes.len() - 48) as u64 {
            return Err(SearchError::CacheUnavailable);
        }
        let body = &bytes[16..bytes.len() - 32];
        if Sha256::digest(body).as_slice() != &bytes[bytes.len() - 32..] {
            return Err(SearchError::CacheUnavailable);
        }
        serde_json::from_slice(body).map_err(|_| SearchError::CacheUnavailable)
    }

    fn write(&self, value: &Value) -> Result<(), SearchError> {
        let body = serde_json::to_vec(value).map_err(|_| SearchError::CacheUnavailable)?;
        if body.len() > self.maximum.saturating_sub(48) {
            return Err(SearchError::CacheUnavailable);
        }
        let temporary = self.directory.join("moe-memory-vectors.cache.pending");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| SearchError::CacheUnavailable)?;
        let result = (|| {
            file.write_all(MAGIC)?;
            file.write_all(&(body.len() as u64).to_le_bytes())?;
            file.write_all(&body)?;
            file.write_all(&Sha256::digest(&body))?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, self.path())?;
            #[cfg(unix)]
            File::open(&self.directory)?.sync_all()?;
            Ok::<(), std::io::Error>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result.map_err(|_| SearchError::CacheUnavailable)
    }
}

fn profile(index: &SearchIndex) -> Value {
    json!([
        index.profile.backend(),
        index.profile.model(),
        index.profile.dimension()
    ])
}

fn config(index: &SearchIndex) -> Value {
    json!([
        index.config.version(),
        index.config.max_chars(),
        index.config.overlap_chars(),
        index.config.max_utf8_bytes()
    ])
}

fn key_value(key: &IndexKey) -> Value {
    let partition = match &key.scope {
        MemoryScope::Private(owner) => json!([
            "private",
            owner.namespace().workspace_id(),
            owner.namespace().user_id(),
            owner.instance_id()
        ]),
        MemoryScope::Shared(namespace) => {
            json!(["shared", namespace.workspace_id(), namespace.user_id()])
        }
    };
    json!([
        key.domain.0,
        partition,
        key.parent.id().as_u64(),
        key.parent.revision(),
        key.ordinal,
        key.start,
        key.end,
        key.fingerprint.as_slice(),
        [
            key.profile.backend(),
            key.profile.model(),
            key.profile.dimension()
        ],
        [
            key.config.version(),
            key.config.max_chars(),
            key.config.overlap_chars(),
            key.config.max_utf8_bytes()
        ]
    ])
}

impl SearchIndex {
    /// Cache failure is a visible diagnostic, never an empty authoritative store.
    /// Loading intersects with newly validated inputs and cannot restore permissions.
    pub fn load_file_cache<Host: RetrievalHost>(
        &mut self,
        host: &mut Host,
        ticket: &CandidateTicket,
    ) -> Result<IndexStatus, SearchError> {
        self.authorize(host, ticket)?;
        let result = self.load_current_cache(ticket);
        match result {
            Ok(entries) => {
                self.entries.extend(entries);
            }
            Err(SearchError::ProfileMismatch) => self.notices.push(SearchReason::ProfileMismatch),
            Err(_) => self.notices.push(SearchReason::CacheUnavailable),
        }
        self.reconcile(host, ticket)
    }

    fn load_current_cache(
        &self,
        ticket: &CandidateTicket,
    ) -> Result<BTreeMap<IndexKey, StoredVector>, SearchError> {
        let value = self
            .cache
            .as_ref()
            .ok_or(SearchError::CacheUnavailable)?
            .read()?;
        if value["format"] != 1 || value["domain"] != self.domain.0 {
            return Err(SearchError::CacheUnavailable);
        }
        if value["profile"] != profile(self) || value["chunks"] != config(self) {
            return Err(SearchError::ProfileMismatch);
        }
        let entries = value["entries"]
            .as_array()
            .ok_or(SearchError::CacheUnavailable)?;
        if entries.len() > 65536 {
            return Err(SearchError::CacheUnavailable);
        }
        let mut cached = BTreeMap::new();
        for entry in entries {
            let key = entry["key"]
                .as_array()
                .filter(|key| key.len() == 10)
                .ok_or(SearchError::CacheUnavailable)?;
            let encoded_key = Value::Array(key.clone()).to_string();
            if cached.contains_key(&encoded_key) {
                return Err(SearchError::CacheUnavailable);
            }
            let values = entry["vector"]
                .as_array()
                .filter(|values| values.len() == self.profile.dimension())
                .ok_or(SearchError::CacheUnavailable)?;
            let mut norm = 0.0;
            for value in values {
                let value = value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or(SearchError::CacheUnavailable)?;
                norm += value * value;
            }
            if !norm.is_finite() || (norm - 1.0).abs() > 1e-8 {
                return Err(SearchError::CacheUnavailable);
            }
            cached.insert(encoded_key, &entry["vector"]);
        }
        let mut restored = BTreeMap::new();
        let mut parent_inputs = BTreeMap::new();
        self.visit_expected(ticket, |key, _, input, _| {
            if let Some(values) = cached
                .remove(&key_value(&key).to_string())
                .and_then(Value::as_array)
            {
                let values = values
                    .iter()
                    .map(|value| {
                        value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or(SearchError::CacheUnavailable)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let vector = UnitVector::new(values, self.profile.dimension())
                    .map_err(|_| SearchError::CacheUnavailable)?;
                let record = key.record_key();
                let parent = parent_inputs
                    .entry(record)
                    .or_insert_with(|| Arc::new(input.clone()));
                restored.insert(
                    key,
                    StoredVector {
                        input: Arc::clone(parent),
                        vector,
                    },
                );
            }
            Ok(true)
        })?;
        let added = restored
            .keys()
            .filter(|key| !self.entries.contains_key(*key))
            .count();
        if added > self.vector_capacity.saturating_sub(self.entries.len()) {
            return Err(SearchError::CacheUnavailable);
        }
        Ok(restored)
    }

    /// Writes only this current authorized set: no original text, query or session.
    /// Windows rename may refuse an existing destination; the old snapshot survives.
    pub fn save_file_cache<Host: RetrievalHost>(
        &self,
        host: &mut Host,
        ticket: &CandidateTicket,
    ) -> Result<(), SearchError> {
        self.authorize(host, ticket)?;
        let mut entries = Vec::new();
        self.visit_expected(ticket, |key, _, input, _| {
            if let Some(stored) = self
                .entries
                .get(&key)
                .filter(|stored| stored.input.as_ref() == input)
            {
                entries.push(json!({"key": key_value(&key), "vector": stored.vector.0}));
            }
            Ok(true)
        })?;
        let value = json!({"format": 1, "domain": self.domain.0, "profile": profile(self), "chunks": config(self), "entries": entries});
        self.cache
            .as_ref()
            .ok_or(SearchError::CacheUnavailable)?
            .write(&value)
    }

    /// Removes one fixed cache file, never a directory, journal, backup or memory root.
    pub fn clear_file_cache(&self) -> Result<(), SearchError> {
        let path = self
            .cache
            .as_ref()
            .ok_or(SearchError::CacheUnavailable)?
            .path();
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(SearchError::CacheUnavailable),
        }
    }
}
