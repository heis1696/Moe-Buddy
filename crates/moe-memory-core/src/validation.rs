use crate::MemoryError;

pub(crate) const MAX_TEXT_CHARS: usize = 4096;
pub(crate) const MAX_SOURCES: usize = 32;
pub(crate) const MAX_RECORDS: usize = 10_000;
pub(crate) const MAX_TOMBSTONES: usize = 100_000;
pub(crate) const MAX_QUERY_CHARS: usize = 256;
pub(crate) const MAX_RESULTS: usize = 100;

pub(crate) fn identifier(value: &str) -> Result<(), MemoryError> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > 128
        || value.chars().any(char::is_control)
    {
        return Err(MemoryError::InvalidInput("invalid identifier"));
    }
    Ok(())
}

pub(crate) fn text(value: &str) -> Result<(), MemoryError> {
    if value.trim().is_empty() || value.chars().count() > MAX_TEXT_CHARS || value.contains('\0') {
        return Err(MemoryError::InvalidInput("invalid memory text"));
    }
    Ok(())
}
