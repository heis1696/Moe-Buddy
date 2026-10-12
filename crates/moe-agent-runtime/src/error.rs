use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatewayError {
    InvalidInput(&'static str),
    Unauthorized,
    RouteDenied,
    UnknownRoute,
    MissingBinding,
    PresetNotFound,
    UnsupportedCapability,
    UnsupportedParameter,
    VersionConflict,
    CapacityExceeded,
    ReplayProtectionExhausted,
    IdentifierExhausted,
    Revoked,
    ForeignRequest,
    ClockWentBackwards,
    SessionExpired,
    StaleHandle,
    EventConflict,
    InteractionClosed,
    DeadlineExceeded,
    NotReady,
    BatchInFlight,
    RetentionNotConfigured,
    RawExpired,
    Recycled,
    LifecycleVersionConflict,
    RawSnapshotInvalid(&'static str),
}

impl fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(formatter, "invalid input: {reason}"),
            Self::Unauthorized => formatter.write_str("caller authentication failed"),
            Self::RouteDenied => formatter.write_str("route not authorized"),
            Self::UnknownRoute => formatter.write_str("route unavailable"),
            Self::MissingBinding => formatter.write_str("purpose has no preset binding"),
            Self::PresetNotFound => formatter.write_str("bound preset unavailable"),
            Self::UnsupportedCapability => formatter.write_str("capability not supported"),
            Self::UnsupportedParameter => formatter.write_str("parameter not supported"),
            Self::VersionConflict => formatter.write_str("configuration version conflict"),
            Self::CapacityExceeded => formatter.write_str("configured capacity exceeded"),
            Self::ReplayProtectionExhausted => {
                formatter.write_str("replay protection budget exhausted; new ingestion denied")
            }
            Self::IdentifierExhausted => formatter.write_str("identifier space exhausted"),
            Self::Revoked => formatter.write_str("request authorization is no longer current"),
            Self::ForeignRequest => formatter.write_str("request belongs to another runtime"),
            Self::ClockWentBackwards => formatter.write_str("monotonic time moved backwards"),
            Self::SessionExpired => formatter.write_str("session fragment is not active"),
            Self::StaleHandle => formatter.write_str("handle is no longer current"),
            Self::EventConflict => formatter.write_str("event or acknowledgement conflicts"),
            Self::InteractionClosed => formatter.write_str("interaction is already terminal"),
            Self::DeadlineExceeded => formatter.write_str("operation deadline reached"),
            Self::NotReady => formatter.write_str("idle batch prerequisites not satisfied"),
            Self::BatchInFlight => formatter.write_str("session already has an idle batch"),
            Self::RetentionNotConfigured => {
                formatter.write_str("raw retention is not configured on this runtime")
            }
            Self::RawExpired => formatter.write_str("raw retention deadline has passed"),
            Self::Recycled => formatter.write_str("fragment raw text is in the recycle zone"),
            Self::LifecycleVersionConflict => formatter.write_str("lifecycle version conflict"),
            Self::RawSnapshotInvalid(reason) => {
                write!(formatter, "raw snapshot is invalid: {reason}")
            }
        }
    }
}

impl std::error::Error for GatewayError {}
