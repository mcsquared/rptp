use core::cell::Cell;

use crate::clock::SynchronizableClock;
use crate::log::ClockMetrics;
use crate::time::{TimeInterval, TimeStamp};

pub enum ServoState {
    Calibrating,
    Locked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServoSample {
    ingress: TimeStamp,
    offset: TimeInterval,
}

impl ServoSample {
    pub fn new(ingress: TimeStamp, offset: TimeInterval) -> Self {
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
}

pub enum Servo {
    Stepping(SteppingServo),
    PI(PiServo),
}

impl Servo {
    pub fn feed<C: SynchronizableClock>(&self, clock: &C, sample: ServoSample) {
        match self {
            Servo::Stepping(servo) => servo.feed(clock, sample),
            Servo::PI(servo) => servo.feed(clock, sample),
        }
    }

    pub fn state(&self) -> ServoState {
        match self {
            Servo::Stepping(servo) => servo.state(),
            Servo::PI(servo) => servo.state(),
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

    pub fn feed<C: SynchronizableClock>(&self, clock: &C, sample: ServoSample) {
        clock.step(sample.master_estimate());
        sample.log(self.metrics);
    }

    pub fn state(&self) -> ServoState {
        ServoState::Locked
    }
}

pub struct PiServo {
    step: ServoStep,
    kp: f64,
    ki: f64,
    integral: Cell<f64>,
    metrics: &'static dyn ClockMetrics,
}

impl PiServo {
    pub fn new(step: ServoStep, kp: f64, ki: f64, metrics: &'static dyn ClockMetrics) -> Self {
        Self {
            step,
            kp,
            ki,
            integral: Cell::new(0.0),
            metrics,
        }
    }

    pub fn feed<C: SynchronizableClock>(&self, clock: &C, sample: ServoSample) {
        sample.log(self.metrics);

        match self.step.decide(&sample) {
            ServoStepDecision::StepTo(ts) => {
                clock.step(ts);
                self.integral.set(0.0);
                return;
            }
            ServoStepDecision::NoStep => {}
        }

        let error = sample.offset();
        let integral = self.integral.get() + error;
        self.integral.set(integral);

        let rate = 1.0 - self.kp * error - self.ki * integral;
        clock.adjust(rate);
    }

    pub fn state(&self) -> ServoState {
        ServoState::Locked
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServoStepDecision {
    StepTo(TimeStamp),
    NoStep,
}

pub struct ServoStep {
    first_step_threshold: TimeInterval,
    step_threshold: TimeInterval,
    first_update: Cell<bool>,
}

impl ServoStep {
    pub fn new(first_step_threshold: TimeInterval, step_threshold: TimeInterval) -> Self {
        debug_assert!(first_step_threshold >= TimeInterval::new(0, 0));
        debug_assert!(step_threshold >= TimeInterval::new(0, 0));

        Self {
            first_step_threshold,
            step_threshold,
            first_update: Cell::new(true),
        }
    }

    fn decide(&self, sample: &ServoSample) -> ServoStepDecision {
        let exceeds_first = sample.beyond_threshold(self.first_step_threshold);
        let exceeds_step = sample.beyond_threshold(self.step_threshold);

        let decision = if (self.first_update.get() && exceeds_first) || exceeds_step {
            ServoStepDecision::StepTo(sample.master_estimate())
        } else {
            ServoStepDecision::NoStep
        };

        self.first_update.set(false);
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_below_both_thresholds_does_not_step() {
        let step = ServoStep::new(TimeInterval::new(2, 0), TimeInterval::new(1, 0));
        let sample = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(0, 500_000_000));

        let decision = step.decide(&sample);

        assert_eq!(decision, ServoStepDecision::NoStep);
    }

    #[test]
    fn first_sample_exceeding_first_step_threshold_steps() {
        let step = ServoStep::new(TimeInterval::new(1, 0), TimeInterval::new(2, 0));
        let sample = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(1, 500_000_000));

        let decision = step.decide(&sample);

        assert_eq!(
            decision,
            ServoStepDecision::StepTo(sample.master_estimate())
        );
    }

    #[test]
    fn first_sample_exceeding_step_threshold_steps_even_if_first_step_not_exceeded() {
        let step = ServoStep::new(TimeInterval::new(2, 0), TimeInterval::new(1, 0));
        let sample = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(1, 500_000_000));

        let decision = step.decide(&sample);

        assert_eq!(
            decision,
            ServoStepDecision::StepTo(sample.master_estimate())
        );
    }

    #[test]
    fn subsequent_samples_use_only_step_threshold() {
        let step = ServoStep::new(TimeInterval::new(2, 0), TimeInterval::new(1, 0));

        // First sample below both thresholds: no step, first_update becomes false.
        let first = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(0, 500_000_000));
        let decision = step.decide(&first);
        assert_eq!(decision, ServoStepDecision::NoStep);

        // Second sample exceeding step threshold should step, regardless of first_step_threshold.
        let second = ServoSample::new(TimeStamp::new(10, 0), TimeInterval::new(1, 500_000_000));
        let decision = step.decide(&second);
        assert_eq!(
            decision,
            ServoStepDecision::StepTo(second.master_estimate())
        );
    }
}
