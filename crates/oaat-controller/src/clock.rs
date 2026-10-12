use std::sync::Arc;

/// Clock domain used by controller-side OAAT timestamps.
///
/// Production callers use [`MonotonicTimeSource`] (the process-wide
/// `oaat_core::time::now_ns`). Tests may inject a scripted
/// source through `ConnectedEndpoint::connect_with_time_source` or
/// `Zone::new_with_time_source` without changing the public configuration
/// struct used by existing applications.
pub trait TimeSource: Send + Sync {
    fn now_ns(&self) -> u64;
}

pub type SharedTimeSource = Arc<dyn TimeSource>;

/// The process-wide monotonic OAAT clock (`oaat_core::time::now_ns`):
/// epoch-shaped nanoseconds, never stepped by NTP (#34). Shared with the
/// clock responder and the CLI, so one process has one clock domain.
#[derive(Debug, Default)]
pub struct MonotonicTimeSource;

impl TimeSource for MonotonicTimeSource {
    fn now_ns(&self) -> u64 {
        oaat_core::time::now_ns()
    }
}

/// Former name of [`MonotonicTimeSource`], kept for existing callers. It no
/// longer reads `SystemTime` on every call (#34).
pub type SystemTimeSource = MonotonicTimeSource;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// #34 / review of #39: the controller's default time source and the
    /// rest of the process (clock responder, CLI) share one clock domain.
    #[test]
    fn default_time_source_is_the_process_clock() {
        let a = oaat_core::time::now_ns();
        let b = MonotonicTimeSource.now_ns();
        let c = oaat_core::time::now_ns();
        assert!(a <= b && b <= c, "{a} {b} {c}");
        assert!(c - a < 50_000_000, "same domain, a few µs apart: {}", c - a);
    }
}
