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
    metrics: &'static dyn ClockMetrics,
    kp: f64,
    ki: f64,
    integral: Cell<f64>,
}

impl PiServo {
    pub fn new(metrics: &'static dyn ClockMetrics, kp: f64, ki: f64) -> Self {
        Self {
            metrics,
            kp,
            ki,
            integral: Cell::new(0.0),
        }
    }

    pub fn feed<C: SynchronizableClock>(&self, clock: &C, sample: ServoSample) {
        sample.log(self.metrics);

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
