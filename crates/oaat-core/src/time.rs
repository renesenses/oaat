//! The OAAT clock: one monotonic, epoch-shaped clock per process (#34).
//!
//! Every PTP-style timestamp (t1..t4), every PTS and every scheduling or
//! drift computation must come from [`now_ns`]. It reads the monotonic clock
//! (`std::time::Instant`), which NTP or an administrator cannot step: a
//! wall-clock correction during a four-timestamp exchange would otherwise
//! inject a spurious offset/RTT sample, and a step between two PTS
//! computations would shift playback scheduling.
//!
//! # Where the wall clock is still used, and why
//!
//! The wall clock (`SystemTime`) is read **once per process**, the first
//! time [`now_ns`] is called, to anchor the monotonic clock on the UNIX
//! epoch. From then on only monotonic elapsed time advances the value. The
//! anchor exists for the protocol, not for the arithmetic:
//!
//! - Timestamps on the wire stay nanoseconds since the UNIX epoch, the shape
//!   peers that still stamp with the wall clock send and expect. Two peers
//!   of different versions keep a small offset (their wall clocks agreed at
//!   startup) instead of one as large as their uptimes.
//! - The endpoint's plausibility checks on PTS (a relative PTS from a legacy
//!   controller lands "hours in the past") rely on that epoch scale.
//!
//! Independent peers still need offset estimation (`ClockState`): each
//! process has its own anchor, and the anchors differ by however far apart
//! the wall clocks were at startup, plus the drift of the two oscillators.
//! Both are what the clock sync measures anyway.
//!
//! One process, one anchor: the anchor lives in a single `static` of this
//! crate, so the controller library, its clock responder, the endpoint
//! transport and the CLI all share one clock domain. (Per-module epochs made
//! the offset jump by their difference, see the review of #39.)
//!
//! Caveat: on Linux `Instant` is `CLOCK_MONOTONIC`, which NTP may slew (a
//! rate change of a few hundred ppm at most, absorbed by the drift servo)
//! but never steps, and which does not advance while the machine is
//! suspended. After a resume, the offset re-converges like after any other
//! clock change.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// A monotonic clock anchored once on a wall-clock reading.
///
/// `now_ns() = anchor_wall_ns + (mono_ns() − anchor_mono_ns)`. The wall
/// source is an `FnOnce`: it is read at construction and never again, so a
/// later wall-clock step cannot reach the returned values.
pub struct AnchoredClock {
    anchor_wall_ns: u64,
    anchor_mono_ns: u64,
    mono_ns: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl AnchoredClock {
    /// Build a clock from explicit sources (tests inject scripted ones).
    /// `mono_ns` must be non-decreasing.
    pub fn with_sources(
        wall_ns: impl FnOnce() -> u64,
        mono_ns: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        let anchor_mono_ns = mono_ns();
        let anchor_wall_ns = wall_ns();
        Self {
            anchor_wall_ns,
            anchor_mono_ns,
            mono_ns: Box::new(mono_ns),
        }
    }

    /// The production clock: `Instant` anchored on `SystemTime` now.
    pub fn system() -> Self {
        let origin = Instant::now();
        Self::with_sources(wall_clock_ns, move || origin.elapsed().as_nanos() as u64)
    }

    /// Nanoseconds since the UNIX epoch, advanced by monotonic time only.
    pub fn now_ns(&self) -> u64 {
        let elapsed = (self.mono_ns)().saturating_sub(self.anchor_mono_ns);
        self.anchor_wall_ns.saturating_add(elapsed)
    }
}

/// Wall-clock nanoseconds since the UNIX epoch. Only for the anchor.
fn wall_clock_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// The process-wide OAAT clock, in nanoseconds since the UNIX epoch (see the
/// module docs). Monotonic: never goes backwards, whatever the wall clock
/// does after the first call.
pub fn now_ns() -> u64 {
    static CLOCK: OnceLock<AnchoredClock> = OnceLock::new();
    CLOCK.get_or_init(AnchoredClock::system).now_ns()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ClockState;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

    /// One peer: its own wall clock (true time + a step the test controls)
    /// and the shared monotonic "true time" of the simulation.
    struct Peer {
        wall_step: Arc<AtomicI64>,
        clock: AnchoredClock,
    }

    fn peer(truth: &Arc<AtomicU64>, wall_offset_ns: i64) -> Peer {
        let wall_step = Arc::new(AtomicI64::new(0));
        let (t, s) = (truth.clone(), wall_step.clone());
        // Fn, not FnOnce: the clock is free to read it as often as it
        // likes, and must not.
        let wall = move || {
            (t.load(Ordering::SeqCst) as i64 + wall_offset_ns + s.load(Ordering::SeqCst)) as u64
        };
        let t = truth.clone();
        let mono = move || t.load(Ordering::SeqCst);
        Peer {
            wall_step,
            clock: AnchoredClock::with_sources(wall, mono),
        }
    }

    /// #34: the wall clock of either peer is stepped backwards and forwards,
    /// including between timestamps of a single exchange. The clock offset
    /// and RTT measured by `ClockState` must not move, before or after
    /// bootstrap.
    #[test]
    fn wall_clock_steps_do_not_perturb_clock_sync() {
        let truth = Arc::new(AtomicU64::new(1_000_000_000));
        // The controller's wall clock is 3 ms ahead of the endpoint's.
        let ctrl = peer(&truth, 1_700_000_000_000_000_000 + 3_000_000);
        let ep = peer(&truth, 1_700_000_000_000_000_000);
        let advance = |ns: u64| truth.fetch_add(ns, Ordering::SeqCst);

        let mut state = ClockState::new();
        let mut offsets = Vec::new();
        let mut rtts = Vec::new();
        for i in 0..40 {
            let t1 = ep.clock.now_ns();
            advance(1_000_000); // 1 ms one way
            match i {
                // NTP steps the endpoint back 10 s mid-exchange (bootstrap).
                5 => ep.wall_step.store(-10_000_000_000, Ordering::SeqCst),
                // ... then the controller forward 7 s (steady state).
                20 => ctrl.wall_step.store(7_000_000_000, Ordering::SeqCst),
                _ => {}
            }
            let t2 = ctrl.clock.now_ns();
            advance(10_000);
            let t3 = ctrl.clock.now_ns();
            if i == 30 {
                // And the endpoint forward again, between t3 and t4.
                ep.wall_step.store(2_000_000_000, Ordering::SeqCst);
            }
            advance(1_000_000);
            let t4 = ep.clock.now_ns();
            state.update(t1, t2, t3, t4);
            offsets.push(state.offset_ns());
            rtts.push(state.rtt_ns());
            advance(1_000_000_000); // next exchange in 1 s
        }
        assert!(
            offsets.iter().all(|&o| o == 3_000_000),
            "offset perturbed by a wall-clock step: {offsets:?}"
        );
        assert!(
            rtts.iter().all(|&r| r == 2_000_000),
            "rtt perturbed by a wall-clock step: {rtts:?}"
        );
    }

    /// A PTS scheduled before a backward wall-clock step keeps the same lead
    /// time: scheduling sees no discontinuity.
    #[test]
    fn wall_clock_step_does_not_shift_scheduling() {
        let truth = Arc::new(AtomicU64::new(0));
        let p = peer(&truth, 1_700_000_000_000_000_000);
        let pts = p.clock.now_ns() + 500_000_000;
        truth.fetch_add(100_000_000, Ordering::SeqCst);
        p.wall_step.store(-3_600_000_000_000, Ordering::SeqCst);
        let lead = pts.saturating_sub(p.clock.now_ns());
        assert_eq!(lead, 400_000_000);
    }

    #[test]
    fn values_are_epoch_shaped_and_monotonic() {
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
        // After 2020-01-01: the anchor is a UNIX-epoch wall reading.
        assert!(a > 1_577_836_800_000_000_000);
    }
}
