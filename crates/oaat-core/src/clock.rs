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
pub struct ClockState {
    alpha: f64,
    offset_ns: f64,
    rtt_ns: f64,
    samples: u32,
    bootstrap_count: u32,
}

impl ClockState {
    pub fn new() -> Self {
        Self {
            alpha: 0.125,
            offset_ns: 0.0,
            rtt_ns: 0.0,
            samples: 0,
            bootstrap_count: 10,
        }
    }

    pub fn update(&mut self, t1: u64, t2: u64, t3: u64, t4: u64) {
        let offset = ((t2 as i128 - t1 as i128) + (t3 as i128 - t4 as i128)) as f64 / 2.0;
        let rtt = ((t4 - t1) - (t3 - t2)) as f64;

        if self.samples == 0 {
            self.offset_ns = offset;
            self.rtt_ns = rtt;
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

    /// Convert a local timestamp to controller clock domain.
    pub fn local_to_controller(&self, local_ns: u64) -> u64 {
        (local_ns as i64 + self.offset_ns()) as u64
    }

    /// Convert a controller timestamp to local clock domain.
    pub fn controller_to_local(&self, controller_ns: u64) -> u64 {
        (controller_ns as i64 - self.offset_ns()) as u64
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
        assert!((offset - 50).abs() < 2, "offset should converge to ~50, got {offset}");
    }

    #[test]
    fn bootstrap_phase() {
        let clock = ClockState::new();
        assert!(!clock.is_bootstrapped());
    }
}
