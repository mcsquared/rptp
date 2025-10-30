use std::cell::Cell;

use crate::{
    bmca::{ForeignClockDS, LocalClockDS},
    message::{AnnounceMessage, SequenceId},
    time::TimeStamp,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockIdentity([u8; 8]);

impl ClockIdentity {
    pub const fn new(id: [u8; 8]) -> Self {
        Self(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockQuality {
    pub clock_class: u8,
    pub clock_accuracy: u8,
    pub offset_scaled_log_variance: u16,
}

impl ClockQuality {
    pub const fn new(clock_class: u8, clock_accuracy: u8, offset_scaled_log_variance: u16) -> Self {
        Self {
            clock_class,
            clock_accuracy,
            offset_scaled_log_variance,
        }
    }

    pub fn outranks_other(&self, other: &ClockQuality) -> bool {
        if self.clock_class != other.clock_class {
            return self.clock_class < other.clock_class;
        }
        if self.clock_accuracy != other.clock_accuracy {
            return self.clock_accuracy < other.clock_accuracy;
        }
        self.offset_scaled_log_variance < other.offset_scaled_log_variance
    }
}

pub trait Clock {
    fn now(&self) -> TimeStamp;
}

pub trait SynchronizableClock: Clock {
    fn synchronize(&self, to: TimeStamp);
}

pub struct LocalClock<C: SynchronizableClock> {
    clock: C,
    localds: LocalClockDS,
}

impl<C: SynchronizableClock> LocalClock<C> {
    pub fn new(clock: C, localds: LocalClockDS) -> Self {
        Self { clock, localds }
    }

    pub fn announce(&self, sequence_id: SequenceId) -> AnnounceMessage {
        self.localds.announce(sequence_id)
    }

    pub fn outranks_foreign(&self, other: &ForeignClockDS) -> bool {
        self.localds.outranks_foreign(other)
    }

    pub fn discipline(&self, estimate: TimeStamp) {
        // TODO: apply filtering, slew rate limiting, feed to servo, etc.
        self.clock.synchronize(estimate);
    }
}

impl<C: SynchronizableClock> Clock for LocalClock<C> {
    fn now(&self) -> TimeStamp {
        self.clock.now()
    }
}

pub struct FakeClock {
    now: Cell<TimeStamp>,
}

impl FakeClock {
    pub fn new(now: TimeStamp) -> Self {
        Self {
            now: Cell::new(now),
        }
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new(TimeStamp::new(0, 0))
    }
}

impl Clock for FakeClock {
    fn now(&self) -> TimeStamp {
        self.now.get()
    }
}

impl SynchronizableClock for FakeClock {
    fn synchronize(&self, to: TimeStamp) {
        self.now.set(to);
    }
}
