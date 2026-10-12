use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryError {
    InvalidInput(&'static str),
    InvalidEvidence,
    NotFound,
    ProtectedContent,
    AccessDenied,
    TransmissionDenied,
    PublicationDenied,
    AutomaticPublicationDisabled,
    PublicationVersionConflict,
    PolicyVersionConflict,
    IdentityVersionConflict,
    ContextVersionConflict,
    ContextBudgetExceeded { required: usize, available: usize },
    InvalidState,
    TaskCancelled,
    RevisionConflict { expected: u64, actual: u64 },
    SourceVersionConflict,
    TimeWentBackwards,
    CapacityExceeded,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(formatter, "invalid input: {reason}"),
            Self::InvalidEvidence => formatter.write_str("evidence is unavailable or incompatible"),
            Self::NotFound => formatter.write_str("memory not found"),
            Self::ProtectedContent => {
                formatter.write_str("protected content requires an authority")
            }
            Self::AccessDenied => formatter.write_str("read purpose is not authorized"),
            Self::TransmissionDenied => {
                formatter.write_str("online transmission is not authorized")
            }
            Self::PublicationDenied => formatter.write_str("shared publication is not authorized"),
            Self::AutomaticPublicationDisabled => {
                formatter.write_str("automatic publication rules are undetermined")
            }
            Self::PublicationVersionConflict => {
                formatter.write_str("publication policy version must advance")
            }
            Self::PolicyVersionConflict => formatter.write_str("read policy version must advance"),
            Self::IdentityVersionConflict => formatter.write_str("identity version must advance"),
            Self::ContextVersionConflict => formatter.write_str("context version must advance"),
            Self::ContextBudgetExceeded {
                required,
                available,
            } => write!(
                formatter,
                "protected context requires {required} characters; budget is {available}"
            ),
            Self::InvalidState => formatter.write_str("operation is not allowed in this state"),
            Self::TaskCancelled => formatter.write_str("task fence is no longer current"),
            Self::RevisionConflict { expected, actual } => {
                write!(
                    formatter,
                    "revision conflict: expected {expected}, actual {actual}"
                )
            }
            Self::SourceVersionConflict => formatter.write_str("source version conflict"),
            Self::TimeWentBackwards => {
                formatter.write_str("mutation time precedes current revision")
            }
            Self::CapacityExceeded => formatter.write_str("memory capacity exceeded"),
        }
    }
}

impl std::error::Error for MemoryError {}
