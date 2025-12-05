use crate::clock::SynchronizableClock;
use crate::log::ClockMetrics;
use crate::time::TimeStamp;

pub(crate) struct SteppingServo {
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
