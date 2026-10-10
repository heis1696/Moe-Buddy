use std::collections::BTreeSet;

use crate::store::Store;
use crate::validation::{MAX_QUERY_CHARS, MAX_RESULTS, MAX_SOURCES};
use crate::{
    Category, Epistemic, MemoryDraft, MemoryError, MemoryRecord, MemoryScope, PetIdentity,
    SearchQuery, SearchScope, Sensitivity, validation,
};

pub(crate) fn scope_readable(scope: &MemoryScope, identity: &PetIdentity) -> bool {
    match scope {
        MemoryScope::Private(owner) => owner == identity,
        MemoryScope::Shared(namespace) => namespace == identity.namespace(),
    }
}

pub(crate) fn sources_valid(draft: &MemoryDraft, scope: &MemoryScope, store: &Store) -> bool {
    if draft.sources.is_empty() || draft.sources.len() > MAX_SOURCES {
        return false;
    }
    let mut unique_sources = BTreeSet::new();
    draft.sources.iter().all(|source| {
        let accessible = match scope {
            MemoryScope::Private(identity) => scope_readable(&source.key.scope, identity),
            MemoryScope::Shared(namespace) => {
                source.key.scope == MemoryScope::Shared(namespace.clone())
            }
        };
        if !accessible || !unique_sources.insert(&source.key) {
            return false;
        }
        store.sources.get(&source.key).is_some_and(|current| {
            !current.revoked
                && current.revision == source.revision
                && (draft.epistemic == current.epistemic
                    || (draft.epistemic == Epistemic::Inference
                        && current.epistemic != Epistemic::Fiction))
        })
    })
}

pub(crate) fn validate_draft(
    draft: &MemoryDraft,
    identity: &PetIdentity,
    store: &Store,
    now_ms: u64,
) -> Result<(), MemoryError> {
    validation::text(&draft.text)?;
    if matches!(
        draft.category,
        Category::Identity | Category::ToolDefinition | Category::Protocol
    ) || draft.epistemic == Epistemic::AuthoritativeState
    {
        return Err(MemoryError::ProtectedContent);
    }
    if draft
        .expires_at_ms
        .is_some_and(|expiration| expiration <= now_ms)
    {
        return Err(MemoryError::InvalidInput(
            "expiration must be in the future",
        ));
    }
    if !sources_valid(draft, &MemoryScope::Private(identity.clone()), store) {
        return Err(MemoryError::InvalidEvidence);
    }
    if !dependencies_valid(
        draft,
        &MemoryScope::Private(identity.clone()),
        store,
        now_ms,
    ) {
        return Err(MemoryError::InvalidEvidence);
    }
    Ok(())
}

pub(crate) fn readable(
    record: &MemoryRecord,
    identity: &PetIdentity,
    store: &Store,
    now_ms: u64,
) -> bool {
    record.state == crate::RecordState::Active
        && scope_readable(&record.scope, identity)
        && record.draft.sensitivity != Sensitivity::Restricted
        && (!matches!(record.scope, MemoryScope::Shared(_))
            || record.draft.sensitivity == Sensitivity::Normal)
        && record
            .draft
            .expires_at_ms
            .is_none_or(|expiration| now_ms < expiration)
        && sources_valid(&record.draft, &record.scope, store)
        && dependencies_valid(&record.draft, &record.scope, store, now_ms)
}

pub(crate) fn dependencies_valid(
    draft: &MemoryDraft,
    scope: &MemoryScope,
    store: &Store,
    now_ms: u64,
) -> bool {
    let mut pending = vec![(draft, scope)];
    let mut visited = BTreeSet::new();
    while let Some((draft, scope)) = pending.pop() {
        if draft.dependencies.len() > MAX_SOURCES {
            return false;
        }
        let mut unique = BTreeSet::new();
        for dependency in &draft.dependencies {
            if !unique.insert(dependency.id()) {
                return false;
            }
            let Some(record) = store.records.get(&dependency.id()) else {
                return false;
            };
            let accessible = match scope {
                MemoryScope::Private(identity) => scope_readable(&record.scope, identity),
                MemoryScope::Shared(namespace) => {
                    record.scope == MemoryScope::Shared(namespace.clone())
                }
            };
            if !accessible
                || record.revision != dependency.revision()
                || record.state != crate::RecordState::Active
                || record.protected
                || record.sensitivity() == Sensitivity::Restricted
                || (matches!(record.scope, MemoryScope::Shared(_))
                    && record.sensitivity() != Sensitivity::Normal)
                || record.lifecycle() != crate::Lifecycle::LongTerm
                || matches!(
                    record.category(),
                    Category::WorkingContext
                        | Category::Identity
                        | Category::ToolDefinition
                        | Category::Protocol
                )
                || validate_sensitivity_change(record.sensitivity(), draft.sensitivity).is_err()
                || record
                    .expires_at_ms()
                    .is_some_and(|expiration| now_ms >= expiration)
                || !record
                    .sources()
                    .iter()
                    .all(|source| draft.sources.contains(source))
                || !sources_valid(&record.draft, &record.scope, store)
            {
                return false;
            }
            if visited.insert(record.id) {
                pending.push((&record.draft, &record.scope));
            }
        }
    }
    true
}

pub(crate) fn validate_dependency_target(
    draft: &MemoryDraft,
    target: crate::MemoryId,
    store: &Store,
) -> Result<(), MemoryError> {
    let mut pending = draft.dependencies.clone();
    let mut visited = BTreeSet::new();
    while let Some(dependency) = pending.pop() {
        if dependency.id() == target {
            return Err(MemoryError::InvalidEvidence);
        }
        if visited.insert(dependency.id())
            && let Some(record) = store.records.get(&dependency.id())
        {
            pending.extend_from_slice(record.dependencies());
        }
    }
    Ok(())
}

pub(crate) fn writable(record: &MemoryRecord, identity: &PetIdentity) -> Result<(), MemoryError> {
    if record.scope != MemoryScope::Private(identity.clone()) {
        return Err(MemoryError::NotFound);
    }
    if record.protected || record.draft.sensitivity == Sensitivity::Restricted {
        return Err(MemoryError::ProtectedContent);
    }
    Ok(())
}

pub(crate) fn validate_query(query: &SearchQuery) -> Result<(), MemoryError> {
    if query.limit == 0
        || query.limit > MAX_RESULTS
        || query.text.chars().count() > MAX_QUERY_CHARS
        || query.text.contains('\0')
    {
        return Err(MemoryError::InvalidInput("invalid search query"));
    }
    Ok(())
}

pub(crate) fn validate_sensitivity_change(
    current: Sensitivity,
    next: Sensitivity,
) -> Result<(), MemoryError> {
    if matches!(
        (current, next),
        (Sensitivity::Private, Sensitivity::Normal)
            | (
                Sensitivity::Restricted,
                Sensitivity::Normal | Sensitivity::Private
            )
    ) {
        return Err(MemoryError::ProtectedContent);
    }
    Ok(())
}

pub(crate) fn matches_scope(record: &MemoryRecord, query: &SearchQuery) -> bool {
    match query.scope {
        SearchScope::Private => matches!(record.scope, MemoryScope::Private(_)),
        SearchScope::Shared => matches!(record.scope, MemoryScope::Shared(_)),
        SearchScope::All => true,
    }
}
