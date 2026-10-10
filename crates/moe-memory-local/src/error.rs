use std::{fmt, io};

use moe_memory_core::MemoryError;

#[derive(Debug)]
pub enum PersistenceError {
    Io(io::Error),
    Locked,
    Corrupt { offset: u64, reason: &'static str },
    UnsupportedVersion(u64),
    OperationConflict,
    RecoveryRequired,
    CapacityExceeded,
    Memory(MemoryError),
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(
                formatter,
                "journal I/O failed; commit may be indeterminate: {error}"
            ),
            Self::Locked => formatter.write_str("journal already has a writer"),
            Self::Corrupt { offset, reason } => {
                write!(formatter, "invalid journal at byte {offset}: {reason}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported journal version {version}")
            }
            Self::OperationConflict => {
                formatter.write_str("operation ID is already bound to another request")
            }
            Self::RecoveryRequired => {
                formatter.write_str("journal failed; close and reopen before any further access")
            }
            Self::CapacityExceeded => formatter.write_str("journal capacity exceeded"),
            Self::Memory(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PersistenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Memory(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for PersistenceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<MemoryError> for PersistenceError {
    fn from(error: MemoryError) -> Self {
        Self::Memory(error)
    }
}
