use core::cell::Cell;

use crate::clock::SynchronizableClock;
use crate::log::ClockMetrics;
use crate::time::TimeStamp;

pub enum Servo {
    Stepping(SteppingServo),
    PI(PiServo),
}

impl Servo {
    pub fn feed<C: SynchronizableClock>(&self, clock: &C, estimate: TimeStamp) {
        match self {
            Servo::Stepping(servo) => servo.feed(clock, estimate),
            Servo::PI(servo) => servo.feed(clock, estimate),
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

    pub fn feed<C: SynchronizableClock>(&self, clock: &C, estimate: TimeStamp) {
        clock.step(estimate);
        self.metrics
            .record_offset_from_master(clock.now() - estimate);
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

    pub fn feed<C: SynchronizableClock>(&self, clock: &C, estimate: TimeStamp) {
        let offset = clock.now() - estimate;
        self.metrics.record_offset_from_master(offset);

        let error = offset.as_f64_seconds();
        let integral = self.integral.get() + error;
        self.integral.set(integral);

        let rate = 1.0 - self.kp * error - self.ki * integral;
        clock.adjust(rate);
    }
}
