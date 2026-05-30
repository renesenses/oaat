use crate::session::SessionState;

#[derive(Debug, thiserror::Error)]
pub enum OaatError {
    #[error("unknown audio format wire ID: 0x{0:02x}")]
    UnknownFormat(u8),

    #[error("invalid packet flags: 0x{0:02x}")]
    InvalidPacketFlags(u8),

    #[error("invalid clock sync type: 0x{0:02x}")]
    InvalidClockSyncType(u8),

    #[error("invalid state transition: {from} -> {to}")]
    InvalidStateTransition {
        from: SessionState,
        to: SessionState,
    },

    #[error("invalid capability string: {0:?}")]
    InvalidCapabilityString(String),

    #[error("protocol version mismatch: expected {expected}, got {got}")]
    VersionMismatch { expected: u32, got: u32 },

    #[error("message too large: {0} bytes")]
    MessageTooLarge(usize),

    #[error("incomplete frame: need {need} bytes, have {have}")]
    IncompleteFrame { need: usize, have: usize },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}
