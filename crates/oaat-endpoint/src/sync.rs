//! Endpoint-side clock synchronization state and PTS-based playback tracking.
//!
//! The endpoint runs its own PTP-inspired exchange against the controller
//! (RFC §6.2: the endpoint sends SYNC_REQUEST, the controller responds).
//! `SharedClock` holds the EMA-filtered offset and publishes it through
//! atomics so the audio path can read it without taking a lock.
//!
//! `PtsTracker` converts "where should playback be according to the clock"
//! into a frame-domain error that drives skip/duplicate drift correction.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use oaat_core::clock::ClockState;

/// Thread-safe clock state shared between the transport (writer) and the
/// application/audio path (readers). Readers never block: the current offset
/// is published to atomics on every update.
pub struct SharedClock {
    state: Mutex<ClockState>,
    offset_ns: AtomicI64,
    rtt_ns: AtomicU64,
    bootstrapped: AtomicBool,
    /// Link quality counters, updated by the transport's audio receive loop
    /// (FEC receiver) and read by the application for stream_stats reports.
    packets_lost: AtomicU64,
    packets_recovered: AtomicU64,
}

impl SharedClock {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ClockState::new()),
            offset_ns: AtomicI64::new(0),
            rtt_ns: AtomicU64::new(0),
            bootstrapped: AtomicBool::new(false),
            packets_lost: AtomicU64::new(0),
            packets_recovered: AtomicU64::new(0),
        }
    }

    /// Publish link quality counters (transport side).
    pub fn set_link_stats(&self, lost: u64, recovered: u64) {
        self.packets_lost.store(lost, Ordering::Release);
        self.packets_recovered.store(recovered, Ordering::Release);
    }

    /// (packets lost beyond recovery, packets recovered from FEC parity).
    pub fn link_stats(&self) -> (u64, u64) {
        (
            self.packets_lost.load(Ordering::Acquire),
            self.packets_recovered.load(Ordering::Acquire),
        )
    }

    /// Feed a completed 4-timestamp exchange (t1/t4 local, t2/t3 controller).
    pub fn update(&self, t1: u64, t2: u64, t3: u64, t4: u64) {
        let mut state = self.state.lock().expect("clock state lock poisoned");
        state.update(t1, t2, t3, t4);
        self.offset_ns.store(state.offset_ns(), Ordering::Release);
        self.rtt_ns.store(state.rtt_ns(), Ordering::Release);
        self.bootstrapped
            .store(state.is_bootstrapped(), Ordering::Release);
    }

    /// Current EMA-filtered offset (controller clock − local clock), lock-free.
    pub fn offset_ns(&self) -> i64 {
        self.offset_ns.load(Ordering::Acquire)
    }

    pub fn rtt_ns(&self) -> u64 {
        self.rtt_ns.load(Ordering::Acquire)
    }

    /// True once the bootstrap exchanges have completed and the offset is usable.
    pub fn is_bootstrapped(&self) -> bool {
        self.bootstrapped.load(Ordering::Acquire)
    }

    /// Convert a controller-domain timestamp to the local clock domain.
    pub fn controller_to_local(&self, controller_ns: u64) -> u64 {
        (controller_ns as i64).saturating_sub(self.offset_ns()).max(0) as u64
    }

    /// Convert a local timestamp to the controller clock domain.
    pub fn local_to_controller(&self, local_ns: u64) -> u64 {
        (local_ns as i64).saturating_add(self.offset_ns()).max(0) as u64
    }

    /// Next sync interval suggested by the measured jitter (see ClockState).
    pub fn suggested_interval_ms(&self) -> u64 {
        self.state
            .lock()
            .expect("clock state lock poisoned")
            .suggested_interval_ms()
    }
}

impl Default for SharedClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Tracks where playback should be, in frames, relative to a start instant.
///
/// The start instant is the timestamp at which frame 0 is (or was) expected to
/// hit the DAC. Depending on availability of clock sync, the caller picks the
/// clock domain: controller domain (multi-room, absolute PTS) or local domain
/// (fallback when the PTS is relative or the clock is not bootstrapped).
/// `now_ns` passed to [`PtsTracker::drift_frames`] must be in the same domain.
#[derive(Debug, Clone, Copy)]
pub struct PtsTracker {
    start_ns: u64,
    sample_rate: u32,
}

impl PtsTracker {
    pub fn new(start_ns: u64, sample_rate: u32) -> Self {
        Self {
            start_ns,
            sample_rate,
        }
    }

    pub fn start_ns(&self) -> u64 {
        self.start_ns
    }

    /// Playback error in frames at `now_ns`.
    ///
    /// Positive: playback is behind schedule (frames must be skipped to catch
    /// up). Negative: playback is ahead (frames must be duplicated to slow
    /// down). Zero before the start instant.
    pub fn drift_frames(&self, now_ns: u64, frames_played: u64) -> i64 {
        let elapsed_ns = now_ns.saturating_sub(self.start_ns) as u128;
        let expected = (elapsed_ns * self.sample_rate as u128) / 1_000_000_000;
        expected as i64 - frames_played as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_clock_publishes_offset() {
        let clock = SharedClock::new();
        assert!(!clock.is_bootstrapped());
        assert_eq!(clock.offset_ns(), 0);

        // Controller clock 100ns ahead, symmetric 10ns each way.
        for _ in 0..15 {
            clock.update(1000, 1110, 1115, 1025);
        }
        assert!(clock.is_bootstrapped());
        let offset = clock.offset_ns();
        assert!((offset - 100).abs() < 2, "offset should be ~100, got {offset}");
    }

    #[test]
    fn shared_clock_domain_conversion() {
        let clock = SharedClock::new();
        for _ in 0..15 {
            clock.update(1000, 1110, 1115, 1025); // offset ~ +100
        }
        let local = 5_000_000u64;
        let controller = clock.local_to_controller(local);
        assert_eq!(controller, local + clock.offset_ns() as u64);
        assert_eq!(clock.controller_to_local(controller), local);
    }

    #[test]
    fn shared_clock_negative_offset_conversion() {
        let clock = SharedClock::new();
        for _ in 0..15 {
            // Controller clock 100ns behind: t2/t3 lower than the symmetric case.
            clock.update(1000, 910, 915, 1025);
        }
        assert!(clock.offset_ns() < 0);
        let local = 5_000_000u64;
        let roundtrip = clock.local_to_controller(clock.controller_to_local(local));
        // controller_to_local adds |offset|, local_to_controller removes it.
        assert_eq!(clock.controller_to_local(local) as i64, local as i64 - clock.offset_ns());
        assert_eq!(roundtrip, local);
    }

    #[test]
    fn pts_tracker_zero_before_start() {
        let tracker = PtsTracker::new(1_000_000_000, 48_000);
        assert_eq!(tracker.drift_frames(500_000_000, 0), 0);
    }

    #[test]
    fn pts_tracker_on_schedule() {
        let tracker = PtsTracker::new(0, 48_000);
        // After exactly 1 second, 48000 frames should have played.
        assert_eq!(tracker.drift_frames(1_000_000_000, 48_000), 0);
    }

    #[test]
    fn pts_tracker_behind_schedule() {
        let tracker = PtsTracker::new(0, 48_000);
        // 1 second elapsed but only 47900 frames played → 100 frames behind.
        assert_eq!(tracker.drift_frames(1_000_000_000, 47_900), 100);
    }

    #[test]
    fn pts_tracker_ahead_of_schedule() {
        let tracker = PtsTracker::new(0, 48_000);
        assert_eq!(tracker.drift_frames(1_000_000_000, 48_100), -100);
    }

    #[test]
    fn pts_tracker_100ppm_drift_after_one_minute() {
        // A DAC crystal 100 ppm slow plays 47995.2 frames/s instead of 48000.
        let tracker = PtsTracker::new(0, 48_000);
        let played_in_60s = (48_000.0f64 * 60.0 * (1.0 - 100e-6)) as u64;
        let drift = tracker.drift_frames(60_000_000_000, played_in_60s);
        // 100 ppm over 60 s = 288 frames = 6 ms. The servo must see this.
        assert!((drift - 288).abs() <= 1, "expected ~288 frames, got {drift}");
    }
}
