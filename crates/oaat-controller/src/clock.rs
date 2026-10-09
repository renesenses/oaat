use std::sync::Arc;

/// Monotonic-enough clock domain used by controller-side OAAT timestamps.
///
/// Production callers use [`SystemTimeSource`]. Tests may inject a scripted
/// source through `ConnectedEndpoint::connect_with_time_source` or
/// `Zone::new_with_time_source` without changing the public configuration
/// struct used by existing applications.
pub trait TimeSource: Send + Sync {
    fn now_ns(&self) -> u64;
}

pub type SharedTimeSource = Arc<dyn TimeSource>;

#[derive(Debug, Default)]
pub struct SystemTimeSource;

impl TimeSource for SystemTimeSource {
    fn now_ns(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }
}

/// Immutable view of one endpoint's latest controller-side clock estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockMeasurement {
    pub samples: u32,
    pub bootstrapped: bool,
    pub offset_ns: i64,
    pub rtt_ns: u64,
    pub jitter_ns: u64,
}

/// Clock evidence associated with one endpoint.
///
/// `measurement == None` is intentional: connection may continue when the
/// endpoint cannot answer clock sync, but consumers must not turn that into a
/// precision claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointClockSnapshot {
    pub endpoint_id: String,
    pub measurement: Option<ClockMeasurement>,
}
