use core::cell::Cell;

use crate::clock::SynchronizableClock;
use crate::log::ClockMetrics;
use crate::time::{TimeInterval, TimeStamp};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServoState {
    Unlocked,
    Calibrating,
    Locked,
}

pub enum Servo {
    Stepping(SteppingServo),
    PI(PiServo),
}

impl Servo {
    pub(crate) fn feed<C: SynchronizableClock>(
        &self,
        clock: &C,
        sample: ServoSample,
    ) -> ServoState {
        match self {
            Servo::Stepping(servo) => servo.feed(clock, sample),
            Servo::PI(servo) => servo.feed(clock, sample),
        }
    }
}

pub struct SteppingServo {
    metrics: &'static dyn ClockMetrics,
}

impl SteppingServo {
    pub fn new(metrics: &'static dyn ClockMetrics) -> Self {
        Self { metrics }
    }

    pub(crate) fn feed<C: SynchronizableClock>(
        &self,
        clock: &C,
        sample: ServoSample,
    ) -> ServoState {
        clock.step(sample.master_estimate());
        sample.log(self.metrics);
        ServoState::Locked
    }
}

pub struct PiServo {
    step_policy: StepPolicy,
    drift_estimate: ServoDriftEstimate,
    pi_loop: PiLoop,
    state: Cell<ServoState>,
    metrics: &'static dyn ClockMetrics,
}

impl PiServo {
    pub fn new(
        step_policy: StepPolicy,
        drift_estimate: ServoDriftEstimate,
        pi_loop: PiLoop,
        initial_state: ServoState,
        metrics: &'static dyn ClockMetrics,
    ) -> Self {
        Self {
            step_policy,
            drift_estimate,
            pi_loop,
            state: Cell::new(initial_state),
            metrics,
        }
    }

    fn feed<C: SynchronizableClock>(&self, clock: &C, sample: ServoSample) -> ServoState {
        sample.log(self.metrics);

        if let Some(state) = self.step(clock, sample) {
            self.state.set(state);
            return state;
        }

        let state = match self.state.get() {
            ServoState::Unlocked | ServoState::Calibrating => self.calibrate(clock, sample),
            ServoState::Locked => {
                self.pi_loop.feed(clock, sample.offset());
                ServoState::Locked
            }
        };

        self.state.set(state);
        state
    }

    fn step<C: SynchronizableClock>(&self, clock: &C, sample: ServoSample) -> Option<ServoState> {
        match self.step_policy.should_step(&sample) {
            ServoStepDecision::StepTo(ts) => {
                clock.step(ts);
                self.pi_loop.reset();
                self.drift_estimate.reset();
                Some(ServoState::Unlocked)
            }
            ServoStepDecision::NoStep => None,
        }
    }

    fn calibrate<C: SynchronizableClock>(&self, clock: &C, sample: ServoSample) -> ServoState {
        match self.drift_estimate.estimate(&sample) {
            None => ServoState::Calibrating,
            Some(d) => {
                self.drift_estimate.reset();
                clock.adjust(d.as_rate());
                ServoState::Locked
            }
        }
    }
}

pub struct PiLoop {
    kp: f64,
    ki: f64,
    integral: Cell<f64>,
}

impl PiLoop {
    pub fn new(kp: f64, ki: f64) -> Self {
        Self {
            kp,
            ki,
            integral: Cell::new(0.0),
        }
    }

    fn feed<C: SynchronizableClock>(&self, clock: &C, error: f64) {
        let integral = self.integral.get() + error;
        self.integral.set(integral);

        let rate = 1.0 - self.kp * error - self.ki * integral;
        clock.adjust(rate);
    }

    fn reset(&self) {
        self.integral.set(0.0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drift {
    drift: f64,
}

impl Drift {
    pub fn from_ppb(pbb: i32) -> Self {
        Self {
            drift: pbb as f64 * 1e-9,
        }
    }

    pub fn new(drift: f64) -> Self {
        Self { drift }
    }

    fn clamp(&self, min: &Drift, max: &Drift) -> Self {
        Self {
            drift: self.drift.clamp(min.drift, max.drift),
        }
    }

    fn as_rate(&self) -> f64 {
        1.0 + self.drift
    }
}

pub struct ServoDriftEstimate {
    sample: Cell<Option<ServoSample>>,
    min: Drift,
    max: Drift,
    min_delta: TimeInterval,
}

impl ServoDriftEstimate {
    pub fn new(min: Drift, max: Drift, min_delta: TimeInterval) -> Self {
        Self {
            sample: Cell::new(None),
            min,
            max,
            min_delta,
        }
    }

    fn estimate(&self, sample: &ServoSample) -> Option<Drift> {
        if let Some(prev) = self.sample.get() {
            match sample.drift(&prev, self.min_delta) {
                ServoSampleOrdering::InOrder(drift) => {
                    self.sample.set(Some(*sample));
                    Some(drift.clamp(&self.min, &self.max))
                }
                ServoSampleOrdering::InOrderButLowDelta => None,
                ServoSampleOrdering::OutOfOrder => {
                    // Non-increasing ingress timestamps invalidate the baseline.
                    self.sample.set(None);
                    None
                }
            }
        } else {
            self.sample.set(Some(*sample));
            None
        }
    }

    fn reset(&self) {
        self.sample.set(None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServoStepDecision {
    StepTo(TimeStamp),
    NoStep,
}

pub enum ServoThreshold {
    Disabled,
    Enabled(TimeInterval),
}

impl ServoThreshold {
    pub fn new(threshold: TimeInterval) -> Self {
        ServoThreshold::Enabled(threshold)
    }

    pub fn disabled() -> Self {
        ServoThreshold::Disabled
    }

    fn exceeded_by(&self, sample: &ServoSample) -> bool {
        match self {
            ServoThreshold::Disabled => false,
            ServoThreshold::Enabled(t) => t > &TimeInterval::ZERO && sample.beyond_threshold(*t),
        }
    }
}

pub struct StepPolicy {
    initial_threshold: Cell<Option<ServoThreshold>>,
    threshold: ServoThreshold,
}

impl StepPolicy {
    pub fn new(initial: ServoThreshold, threshold: ServoThreshold) -> Self {
        Self {
            initial_threshold: Cell::new(Some(initial)),
            threshold,
        }
    }

    fn should_step(&self, sample: &ServoSample) -> ServoStepDecision {
        if self
            .initial_threshold
            .take()
            .is_some_and(|t| t.exceeded_by(sample))
        {
            return ServoStepDecision::StepTo(sample.master_estimate());
        }

        if self.threshold.exceeded_by(sample) {
            ServoStepDecision::StepTo(sample.master_estimate())
        } else {
            ServoStepDecision::NoStep
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ServoSampleOrdering {
    InOrder(Drift),
    InOrderButLowDelta,
    OutOfOrder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ServoSample {
    ingress: TimeStamp,
    offset: TimeInterval,
}

impl ServoSample {
    pub(crate) fn new(ingress: TimeStamp, offset: TimeInterval) -> Self {
        Self { ingress, offset }
    }

    fn master_estimate(&self) -> TimeStamp {
        self.ingress - self.offset
    }

    fn log(&self, metrics: &dyn ClockMetrics) {
        metrics.record_offset_from_master(self.offset);
    }

    fn offset(&self) -> f64 {
        self.offset.as_f64_seconds()
    }

    fn beyond_threshold(&self, threshold: TimeInterval) -> bool {
        self.offset.abs() > threshold
    }

    fn drift(&self, other: &ServoSample, min_delta: TimeInterval) -> ServoSampleOrdering {
        let delta_ingress = self.ingress - other.ingress;
        let delta_offset = self.offset - other.offset;

        if delta_ingress <= TimeInterval::new(0, 0) {
            ServoSampleOrdering::OutOfOrder
        } else if delta_ingress.abs() < min_delta {
            ServoSampleOrdering::InOrderButLowDelta
        } else {
            let drift = delta_offset.as_f64_seconds() / delta_ingress.as_f64_seconds();
            ServoSampleOrdering::InOrder(Drift::new(drift))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::NOOP_CLOCK_METRICS;
    use crate::test_support::FakeClock;

    #[test]
    fn first_sample_below_both_thresholds_does_not_step() {
        let step_policy = StepPolicy::new(
            ServoThreshold::new(TimeInterval::new(2, 0)),
            ServoThreshold::new(TimeInterval::new(1, 0)),
        );
        let sample = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(0, 500_000_000));

        let decision = step_policy.should_step(&sample);

        assert_eq!(decision, ServoStepDecision::NoStep);
    }

    #[test]
    fn first_sample_exceeding_first_step_threshold_steps() {
        let step_policy = StepPolicy::new(
            ServoThreshold::new(TimeInterval::new(1, 0)),
            ServoThreshold::new(TimeInterval::new(2, 0)),
        );
        let sample = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(1, 500_000_000));

        let decision = step_policy.should_step(&sample);

        assert_eq!(
            decision,
            ServoStepDecision::StepTo(sample.master_estimate())
        );
    }

    #[test]
    fn first_sample_exceeding_step_threshold_steps_even_if_first_step_not_exceeded() {
        let step_policy = StepPolicy::new(
            ServoThreshold::new(TimeInterval::new(2, 0)),
            ServoThreshold::new(TimeInterval::new(1, 0)),
        );
        let sample = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(1, 500_000_000));

        let decision = step_policy.should_step(&sample);

        assert_eq!(
            decision,
            ServoStepDecision::StepTo(sample.master_estimate())
        );
    }

    #[test]
    fn subsequent_samples_use_only_step_threshold() {
        let step_policy = StepPolicy::new(
            ServoThreshold::new(TimeInterval::new(2, 0)),
            ServoThreshold::new(TimeInterval::new(1, 0)),
        );

        // First sample below both thresholds: no step
        let first = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(0, 500_000_000));
        let decision = step_policy.should_step(&first);
        assert_eq!(decision, ServoStepDecision::NoStep);

        // Second sample exceeding steady threshold should step, regardless of first step threshold.
        let second = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(1, 500_000_000));
        let decision = step_policy.should_step(&second);
        assert_eq!(
            decision,
            ServoStepDecision::StepTo(second.master_estimate())
        );
    }

    #[test]
    fn zero_steady_threshold_disables_subsequent_steps() {
        let step_policy = StepPolicy::new(
            ServoThreshold::new(TimeInterval::new(2, 0)),
            ServoThreshold::disabled(),
        );

        // First sample below first threshold: no step, first_update becomes false.
        let first = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(0, 500_000_000));
        assert_eq!(step_policy.should_step(&first), ServoStepDecision::NoStep);

        // Second sample would have exceeded steady threshold if it were enabled; zero disables it.
        let second = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(3, 0));
        assert_eq!(step_policy.should_step(&second), ServoStepDecision::NoStep);
    }

    #[test]
    fn calibration_locks_after_spaced_samples() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(10, 0)),
                ServoThreshold::new(TimeInterval::new(10, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-500_000_000),
                Drift::from_ppb(500_000_000),
                TimeInterval::new(4, 0),
            ),
            PiLoop::new(0.0, 0.0),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        let first = ServoSample::new(TimeStamp::new(0, 0), TimeInterval::new(0, 0));
        let second = ServoSample::new(TimeStamp::new(5, 0), TimeInterval::new(1, 0));

        let state_first = servo.feed(&clock, first);
        assert_eq!(state_first, ServoState::Calibrating);
        assert_eq!(clock.last_adjust(), None);

        let state_second = servo.feed(&clock, second);
        assert_eq!(state_second, ServoState::Locked);
        let rate = clock.last_adjust().unwrap();
        assert!((rate - 1.2).abs() < 1e-12);
    }

    #[test]
    fn step_resets_drift_estimate() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(1, 0)),
                ServoThreshold::new(TimeInterval::new(10, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-1_000_000),
                Drift::from_ppb(1_000_000),
                TimeInterval::new(1, 0),
            ),
            PiLoop::new(0.0, 0.0),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        let first = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(5, 0));
        let state_after_step = servo.feed(&clock, first);
        assert_eq!(state_after_step, ServoState::Unlocked);
        assert_eq!(clock.last_adjust(), None);

        let second = ServoSample::new(TimeStamp::new(20, 0), TimeInterval::new(6, 0));
        let state_after_second = servo.feed(&clock, second);
        assert_eq!(state_after_second, ServoState::Calibrating);
        assert_eq!(clock.last_adjust(), None);
    }

    #[test]
    fn pi_servo_reaches_locked_and_runs_pi_loop() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(10, 0)),
                ServoThreshold::new(TimeInterval::new(10, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-1_000_000_000),
                Drift::from_ppb(1_000_000_000),
                TimeInterval::new(2, 0),
            ),
            PiLoop::new(0.5, 0.5),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        let first = ServoSample::new(TimeStamp::new(0, 0), TimeInterval::new(0, 0));
        let second = ServoSample::new(TimeStamp::new(5, 0), TimeInterval::new(1, 0));
        let third = ServoSample::new(TimeStamp::new(6, 0), TimeInterval::new(0, 100_000_000));

        let state_first = servo.feed(&clock, first);
        assert_eq!(state_first, ServoState::Calibrating);
        assert_eq!(clock.last_adjust(), None);

        let state_second = servo.feed(&clock, second);
        assert_eq!(state_second, ServoState::Locked);
        assert!((clock.last_adjust().unwrap() - 1.2).abs() < 1e-12);

        let state_third = servo.feed(&clock, third);
        assert_eq!(state_third, ServoState::Locked);
        assert!((clock.last_adjust().unwrap() - 0.9).abs() < 1e-12);
    }

    #[test]
    fn calibration_ignores_samples_without_minimum_delta() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(10, 0)),
                ServoThreshold::new(TimeInterval::new(10, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-1_000_000),
                Drift::from_ppb(1_000_000),
                TimeInterval::new(4, 0),
            ),
            PiLoop::new(0.0, 0.0),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        let first = ServoSample::new(TimeStamp::new(0, 0), TimeInterval::new(0, 0));
        let too_close = ServoSample::new(TimeStamp::new(2, 0), TimeInterval::new(1, 0));

        let state_first = servo.feed(&clock, first);
        assert_eq!(state_first, ServoState::Calibrating);
        assert_eq!(clock.last_adjust(), None);

        let state_second = servo.feed(&clock, too_close);
        assert_eq!(state_second, ServoState::Calibrating);
        assert_eq!(clock.last_adjust(), None);
    }

    #[test]
    fn drift_estimate_is_clamped_to_bounds() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(100, 0)),
                ServoThreshold::new(TimeInterval::new(100, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-1_000),
                Drift::from_ppb(1_000),
                TimeInterval::new(1, 0),
            ),
            PiLoop::new(0.0, 0.0),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        let first = ServoSample::new(TimeStamp::new(0, 0), TimeInterval::new(0, 0));
        let huge_drift = ServoSample::new(TimeStamp::new(5, 0), TimeInterval::new(5, 0));

        servo.feed(&clock, first);
        let state = servo.feed(&clock, huge_drift);

        assert_eq!(state, ServoState::Locked);
        let rate = clock.last_adjust().unwrap();
        assert!((rate - (1.0 + 1e-6)).abs() < 1e-12);
    }

    #[test]
    fn pi_loop_integral_resets_after_step() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(1, 0)),
                ServoThreshold::new(TimeInterval::new(1, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-1_000_000_000),
                Drift::from_ppb(1_000_000_000),
                TimeInterval::new(2, 0),
            ),
            PiLoop::new(0.0, 1.0),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        // Calibrate and lock.
        servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(0, 0), TimeInterval::new(0, 0)),
        );
        let locked = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(3, 0), TimeInterval::new(1, 0)),
        );
        assert_eq!(locked, ServoState::Locked);

        // Build some integral in the PI loop while locked.
        let locked_again = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(4, 0), TimeInterval::new(0, 100_000_000)),
        );
        assert_eq!(locked_again, ServoState::Locked);
        assert!(clock.last_adjust().unwrap() < 1.0);

        // Force a step, which should reset integral and drift estimate.
        let unlocked = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(5, 0), TimeInterval::new(5, 0)),
        );
        assert_eq!(unlocked, ServoState::Unlocked);
        // No new adjustment on step; last value remains from the PI loop before the jump.
        assert!((clock.last_adjust().unwrap() - 0.9).abs() < 1e-12);

        // Re-calibrate after the step.
        let cal_state = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(0, 0)),
        );
        assert_eq!(cal_state, ServoState::Calibrating);
        let relock = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(13, 0), TimeInterval::new(0, 0)),
        );
        assert_eq!(relock, ServoState::Locked);

        // First locked update after reset should have zero integral influence.
        let post_reset = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(14, 0), TimeInterval::new(0, 0)),
        );
        assert_eq!(post_reset, ServoState::Locked);
        assert_eq!(clock.last_adjust().unwrap(), 1.0);
    }

    #[test]
    fn calibration_rejects_non_increasing_ingress() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(100, 0)),
                ServoThreshold::new(TimeInterval::new(100, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-1_000_000),
                Drift::from_ppb(1_000_000),
                TimeInterval::new(1, 0),
            ),
            PiLoop::new(0.0, 0.0),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        let first = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(0, 0));
        let stale = ServoSample::new(TimeStamp::new(9, 500_000_000), TimeInterval::new(1, 0));

        let state_first = servo.feed(&clock, first);
        assert_eq!(state_first, ServoState::Calibrating);
        let state_second = servo.feed(&clock, stale);
        assert_eq!(state_second, ServoState::Calibrating);
        assert_eq!(clock.last_adjust(), None);
    }

    #[test]
    fn pi_loop_accumulates_over_multiple_locked_samples() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(100, 0)),
                ServoThreshold::new(TimeInterval::new(100, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-1_000_000),
                Drift::from_ppb(1_000_000),
                TimeInterval::new(1, 0),
            ),
            PiLoop::new(0.2, 0.3),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        // Calibrate to lock with zero drift.
        servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(0, 0), TimeInterval::new(0, 0)),
        );
        let locked = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(2, 0), TimeInterval::new(0, 0)),
        );
        assert_eq!(locked, ServoState::Locked);

        // Two consecutive locked samples with the same error build integral.
        let first_locked = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(3, 0), TimeInterval::new(0, 100_000_000)),
        );
        assert_eq!(first_locked, ServoState::Locked);
        let rate1 = clock.last_adjust().unwrap();
        let second_locked = servo.feed(
            &clock,
            ServoSample::new(TimeStamp::new(4, 0), TimeInterval::new(0, 100_000_000)),
        );
        assert_eq!(second_locked, ServoState::Locked);
        let rate2 = clock.last_adjust().unwrap();

        // First adjustment: error=0.1 => rate = 1 - 0.2*0.1 - 0.3*0.1 = 0.95
        assert!((rate1 - 0.95).abs() < 1e-12);
        // Second adjustment: integral=0.2 => rate = 1 - 0.2*0.1 - 0.3*0.2 = 0.92
        assert!((rate2 - 0.92).abs() < 1e-12);
    }

    #[test]
    fn calibration_waits_until_min_interval_before_locking() {
        let clock = FakeClock::default();
        let servo = PiServo::new(
            StepPolicy::new(
                ServoThreshold::new(TimeInterval::new(100, 0)),
                ServoThreshold::new(TimeInterval::new(100, 0)),
            ),
            ServoDriftEstimate::new(
                Drift::from_ppb(-1_000_000_000),
                Drift::from_ppb(1_000_000_000),
                TimeInterval::new(2, 0),
            ),
            PiLoop::new(0.0, 0.0),
            ServoState::Unlocked,
            &NOOP_CLOCK_METRICS,
        );

        let first = ServoSample::new(TimeStamp::new(0, 0), TimeInterval::new(0, 0));
        let too_close = ServoSample::new(TimeStamp::new(1, 0), TimeInterval::new(1, 0));
        let third = ServoSample::new(TimeStamp::new(5, 0), TimeInterval::new(1, 0));

        assert_eq!(servo.feed(&clock, first), ServoState::Calibrating);
        assert_eq!(clock.last_adjust(), None);
        assert_eq!(servo.feed(&clock, too_close), ServoState::Calibrating);
        assert_eq!(clock.last_adjust(), None);

        // When a sufficiently spaced sample arrives, calibration should complete using
        // the original baseline.
        let locked = servo.feed(&clock, third);
        assert_eq!(locked, ServoState::Locked);
        // delta_offset = 1s over delta_ingress = 5s => drift = 0.2
        assert!((clock.last_adjust().unwrap() - 1.2).abs() < 1e-12);
    }
}
