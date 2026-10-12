use std::collections::VecDeque;

/// Number of recent RTT measurements kept for outlier detection (#27).
///
/// Every measurement enters the window, rejected ones included: after a
/// durable RTT change (switch to Wi-Fi, a busier route) the median follows
/// the new path within `RTT_WINDOW / 2 + 1` exchanges, and the gate accepts
/// again. A gate that only learned from accepted samples would lock itself
/// out forever (review of #30).
pub const RTT_WINDOW: usize = 16;

/// Outlier threshold, in scaled MADs above the window median (#27).
///
/// The MAD (median absolute deviation) is scaled by 1.4826 so that it
/// estimates a standard deviation for normally distributed noise: 5 scaled
/// MADs is about 5 sigma. The median and the MAD both ignore a minority of
/// spikes, unlike a mean or an EMA, which a single 500 ms exchange drags up.
pub const RTT_OUTLIER_MADS: f64 = 5.0;

/// Absolute floor of the outlier margin above the median, in nanoseconds.
///
/// On a quiet LAN the MAD can be a few microseconds, or zero (a zero
/// baseline would otherwise reject every positive RTT). Below this margin a
/// sample is always accepted: its worst-case offset error is RTT/2, i.e.
/// 0.5 ms, and the EMA divides that by 8 again.
pub const RTT_OUTLIER_FLOOR_NS: f64 = 1_000_000.0;

const MAD_TO_SIGMA: f64 = 1.4826;

/// EMA-filtered clock synchronization state.
///
/// Uses a PTP-inspired 4-timestamp exchange:
///   t1: endpoint sends request
///   t2: controller receives request
///   t3: controller sends response
///   t4: endpoint receives response
///
/// offset = ((t2 - t1) + (t3 - t4)) / 2
/// rtt = (t4 - t1) - (t3 - t2)
///
/// Once bootstrapped, an exchange whose RTT exceeds
/// `median + max(RTT_OUTLIER_MADS × 1.4826 × MAD, RTT_OUTLIER_FLOOR_NS)` of
/// the last `RTT_WINDOW` measurements is rejected: it updates neither the
/// offset, nor the RTT, nor the jitter estimate (#27). An asymmetric delay
/// spike is exactly what biases the offset by up to RTT/2.
pub struct ClockState {
    alpha: f64,
    offset_ns: f64,
    rtt_ns: f64,
    samples: u32,
    bootstrap_count: u32,
    jitter_mean: f64,
    jitter_var: f64,
    /// Recent RTTs (accepted and rejected), oldest first.
    rtt_window: VecDeque<f64>,
    rejected: u64,
}

impl ClockState {
    pub fn new() -> Self {
        Self {
            alpha: 0.125,
            offset_ns: 0.0,
            rtt_ns: 0.0,
            samples: 0,
            bootstrap_count: 10,
            jitter_mean: 0.0,
            jitter_var: 0.0,
            rtt_window: VecDeque::with_capacity(RTT_WINDOW),
            rejected: 0,
        }
    }

    pub fn update(&mut self, t1: u64, t2: u64, t3: u64, t4: u64) {
        let offset = ((t2 as i128 - t1 as i128) + (t3 as i128 - t4 as i128)) as f64 / 2.0;
        // i128 then clamp: with measurement noise on a near-zero RTT,
        // (t3-t2) can exceed (t4-t1); a plain u64 subtraction would panic in
        // debug and poison the EMA with a wrapped huge value in release.
        let rtt = ((t4 as i128 - t1 as i128) - (t3 as i128 - t2 as i128)).max(0) as f64;

        // Bootstrap is unaffected: there is no baseline to compare against.
        let outlier = self.is_bootstrapped() && self.is_rtt_outlier(rtt);
        if self.rtt_window.len() == RTT_WINDOW {
            self.rtt_window.pop_front();
        }
        self.rtt_window.push_back(rtt);
        if outlier {
            self.rejected += 1;
            return;
        }

        if self.samples == 0 {
            self.offset_ns = offset;
            self.rtt_ns = rtt;
            self.jitter_mean = offset;
        } else {
            let alpha = if self.samples < self.bootstrap_count {
                0.5
            } else {
                self.alpha
            };
            self.offset_ns = self.offset_ns * (1.0 - alpha) + offset * alpha;
            self.rtt_ns = self.rtt_ns * (1.0 - alpha) + rtt * alpha;
        }
        self.samples += 1;

        // Exponentially weighted variance for jitter tracking. Unlike a
        // cumulative (Welford) variance, this forgets old samples: a slow
        // clock drift shifts the mean instead of inflating the "jitter"
        // forever, so the adaptive sync interval recovers on long sessions.
        let diff = offset - self.jitter_mean;
        let incr = self.alpha * diff;
        self.jitter_mean += incr;
        self.jitter_var = (1.0 - self.alpha) * (self.jitter_var + diff * incr);
    }

    /// True when `rtt` lies above the outlier threshold of the current
    /// window (see the type docs).
    fn is_rtt_outlier(&self, rtt: f64) -> bool {
        if self.rtt_window.is_empty() {
            return false;
        }
        let mut sorted: Vec<f64> = self.rtt_window.iter().copied().collect();
        let median = median_of(&mut sorted);
        let mut deviations: Vec<f64> = sorted.iter().map(|x| (x - median).abs()).collect();
        let mad = median_of(&mut deviations);
        let margin = (RTT_OUTLIER_MADS * MAD_TO_SIGMA * mad).max(RTT_OUTLIER_FLOOR_NS);
        rtt > median + margin
    }

    /// Exchanges rejected as RTT outliers since creation.
    pub fn rejected_samples(&self) -> u64 {
        self.rejected
    }

    pub fn offset_ns(&self) -> i64 {
        self.offset_ns.round() as i64
    }

    pub fn rtt_ns(&self) -> u64 {
        self.rtt_ns.round() as u64
    }

    pub fn is_bootstrapped(&self) -> bool {
        self.samples >= self.bootstrap_count
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Standard deviation of recent clock offset measurements in
    /// nanoseconds (exponentially weighted).
    pub fn jitter_ns(&self) -> f64 {
        if self.samples < 2 {
            return 0.0;
        }
        self.jitter_var.max(0.0).sqrt()
    }

    /// Suggested sync interval in milliseconds based on measured jitter.
    /// High jitter (>1ms) → 500ms, low jitter (<100us) → 5s, else 2s.
    pub fn suggested_interval_ms(&self) -> u64 {
        if self.samples < self.bootstrap_count {
            return 100;
        }
        let jitter_us = self.jitter_ns() / 1000.0;
        if jitter_us > 1000.0 {
            500
        } else if jitter_us > 100.0 {
            2000
        } else {
            5000
        }
    }

    /// Convert a local timestamp to controller clock domain.
    pub fn local_to_controller(&self, local_ns: u64) -> u64 {
        (local_ns as i64).saturating_add(self.offset_ns()).max(0) as u64
    }

    /// Convert a controller timestamp to local clock domain.
    pub fn controller_to_local(&self, controller_ns: u64) -> u64 {
        (controller_ns as i64).saturating_sub(self.offset_ns()).max(0) as u64
    }
}

/// Median of a non-empty slice (sorts it in place).
fn median_of(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

impl Default for ClockState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_offset_symmetric() {
        let mut clock = ClockState::new();
        // Symmetric trip: 10us each way, no offset
        clock.update(1000, 1010, 1015, 1025);
        assert_eq!(clock.offset_ns(), 0);
        assert_eq!(clock.rtt_ns(), 20);
    }

    #[test]
    fn positive_offset() {
        let mut clock = ClockState::new();
        // Controller clock is 100ns ahead
        clock.update(1000, 1110, 1115, 1025);
        assert_eq!(clock.offset_ns(), 100);
    }

    #[test]
    fn ema_convergence() {
        let mut clock = ClockState::new();
        for _ in 0..100 {
            // Consistent 50ns offset, 20ns RTT
            clock.update(1000, 1060, 1065, 1025);
        }
        let offset = clock.offset_ns();
        assert!(
            (offset - 50).abs() < 2,
            "offset should converge to ~50, got {offset}"
        );
    }

    #[test]
    fn bootstrap_phase() {
        let clock = ClockState::new();
        assert!(!clock.is_bootstrapped());
    }

    #[test]
    fn jitter_low_stable() {
        let mut clock = ClockState::new();
        for i in 0..50u64 {
            // Consistent offset ~50ns, very low jitter
            clock.update(1000 + i, 1060 + i, 1065 + i, 1025 + i);
        }
        assert!(clock.jitter_ns() < 1000.0, "jitter should be very low for stable offsets");
        assert_eq!(clock.suggested_interval_ms(), 5000);
    }

    #[test]
    fn jitter_medium() {
        let mut clock = ClockState::new();
        for i in 0..50u64 {
            // Alternating offset swings of 500us → medium jitter
            let wobble = if i % 2 == 0 { 0 } else { 500_000 };
            clock.update(1000, 1060 + wobble, 1065 + wobble, 1025);
        }
        let jitter_us = clock.jitter_ns() / 1000.0;
        assert!(jitter_us > 100.0, "jitter should be medium: {jitter_us}us");
        assert_eq!(clock.suggested_interval_ms(), 2000);
    }

    #[test]
    fn jitter_very_high() {
        let mut clock = ClockState::new();
        for i in 0..50u64 {
            // Wild offset swings: 0 and 5ms
            let wobble = if i % 2 == 0 { 0 } else { 5_000_000 };
            clock.update(1000, 1060 + wobble, 1065 + wobble, 1025);
        }
        let jitter_us = clock.jitter_ns() / 1000.0;
        assert!(jitter_us > 1000.0, "jitter should be very high: {jitter_us}us");
        assert_eq!(clock.suggested_interval_ms(), 500);
    }

    #[test]
    fn suggested_interval_during_bootstrap() {
        let mut clock = ClockState::new();
        clock.update(1000, 1060, 1065, 1025);
        assert_eq!(clock.suggested_interval_ms(), 100);
    }

    /// One exchange with forward delay `fwd`, return delay `back` and a true
    /// controller offset `off` (all in ns): rtt = fwd + back, and the
    /// measured offset is `off + (fwd - back) / 2`.
    fn exchange(c: &mut ClockState, base: u64, fwd: i64, back: i64, off: i64) {
        let t1 = base as i64;
        let t2 = t1 + fwd + off;
        let t3 = t2 + 5_000;
        let t4 = t3 - off + back;
        c.update(t1 as u64, t2 as u64, t3 as u64, t4 as u64);
    }

    /// #27: after bootstrap, one 500 ms exchange with all the delay on the
    /// return path must not move the offset (it used to shift it by
    /// 250 ms × 0.125 ≈ 31 ms).
    #[test]
    fn rtt_outlier_after_bootstrap_does_not_move_offset() {
        let mut c = ClockState::new();
        let mut base = 1_000_000_000_000u64;
        for _ in 0..20 {
            exchange(&mut c, base, 1_000_000, 1_000_000, 0);
            base += 1_000_000_000;
        }
        assert!(c.is_bootstrapped());
        let before = c.offset_ns();
        let samples = c.samples();
        exchange(&mut c, base, 1_000_000, 499_000_000, 0);
        let shift = (c.offset_ns() - before).abs();
        assert!(shift < 100_000, "offset moved by {shift} ns on one outlier");
        assert_eq!(c.samples(), samples, "the outlier is not counted as a sample");
        assert_eq!(c.rejected_samples(), 1);
        assert!(c.rtt_ns() < 3_000_000, "rtt EMA poisoned: {}", c.rtt_ns());
    }

    /// During bootstrap there is no baseline: a high RTT is accepted.
    #[test]
    fn bootstrap_accepts_high_rtt() {
        let mut c = ClockState::new();
        exchange(&mut c, 1_000_000_000, 1_000_000, 1_000_000, 0);
        exchange(&mut c, 2_000_000_000, 250_000_000, 250_000_000, 0);
        assert_eq!(c.samples(), 2);
        assert_eq!(c.rejected_samples(), 0);
    }

    /// Review of #30: a durable RTT increase must be re-admitted, and the
    /// offset must follow a real change instead of freezing.
    #[test]
    fn durable_rtt_increase_is_readmitted() {
        let mut c = ClockState::new();
        let mut base = 1_000_000_000_000u64;
        for _ in 0..20 {
            exchange(&mut c, base, 1_000_000, 1_000_000, 0);
            base += 1_000_000_000;
        }
        for _ in 0..1000 {
            exchange(&mut c, base, 4_000_000, 4_000_000, 1_000_000);
            base += 1_000_000_000;
        }
        assert!(
            (c.offset_ns() - 1_000_000).abs() < 10_000,
            "offset must track the real +1 ms, got {}",
            c.offset_ns()
        );
        assert!((c.rtt_ns() as i64 - 8_000_000).abs() < 100_000);
        assert!(c.samples() > 1000, "samples stuck at {}", c.samples());
        assert!(
            c.rejected_samples() <= (RTT_WINDOW / 2 + 1) as u64,
            "re-admission took {} rejections",
            c.rejected_samples()
        );
    }

    /// Review of #30: a zero RTT baseline must not reject every positive RTT.
    #[test]
    fn zero_rtt_baseline_accepts_small_positive_rtt() {
        let mut c = ClockState::new();
        let mut base = 1_000_000_000u64;
        for _ in 0..20 {
            exchange(&mut c, base, 0, 0, 0);
            base += 1_000_000_000;
        }
        for _ in 0..50 {
            exchange(&mut c, base, 25_000, 25_000, 0);
            base += 1_000_000_000;
        }
        assert_eq!(c.rejected_samples(), 0);
        assert_eq!(c.samples(), 70);
    }

    #[test]
    fn timestamp_conversion_saturates_on_overflow() {
        let mut cs = ClockState::default();
        cs.update(0, 1_000_000, 1_000_000, 2_000_000);
        assert_eq!(cs.local_to_controller(0), 0);
        assert_eq!(cs.controller_to_local(0), 0);
        let big = u64::MAX / 2;
        let result = cs.local_to_controller(big);
        assert!(result > 0);
    }
}
