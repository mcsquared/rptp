use std::cell::Cell;
use std::ops::Range;

use crate::{
    bmca::{ForeignClockDS, LocalClockDS},
    message::{AnnounceMessage, SequenceId},
    time::TimeStamp,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockIdentity {
    id: [u8; 8],
}

impl ClockIdentity {
    pub const fn new(id: &[u8; 8]) -> Self {
        Self { id: *id }
    }

    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockQuality {
    clock_class: u8,
    clock_accuracy: u8,
    offset_scaled_log_variance: u16,
}

impl ClockQuality {
    const CLOCK_CLASS_OFFSET: usize = 0;
    const CLOCK_ACCURACY_OFFSET: usize = 1;
    const OFFSET_SCALED_LOG_VARIANCE_OFFSET: Range<usize> = 2..4;

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

    pub fn from_slice(buf: &[u8; 4]) -> Self {
        Self {
            clock_class: buf[Self::CLOCK_CLASS_OFFSET],
            clock_accuracy: buf[Self::CLOCK_ACCURACY_OFFSET],
            offset_scaled_log_variance: u16::from_be_bytes([
                buf[Self::OFFSET_SCALED_LOG_VARIANCE_OFFSET.start],
                buf[Self::OFFSET_SCALED_LOG_VARIANCE_OFFSET.end - 1],
            ]),
        }
    }

    pub fn to_bytes(&self) -> [u8; 4] {
        let mut bytes = [0u8; 4];
        bytes[Self::CLOCK_CLASS_OFFSET] = self.clock_class;
        bytes[Self::CLOCK_ACCURACY_OFFSET] = self.clock_accuracy;
        bytes[Self::OFFSET_SCALED_LOG_VARIANCE_OFFSET]
            .copy_from_slice(&self.offset_scaled_log_variance.to_be_bytes());
        bytes
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

    pub fn identity(&self) -> &ClockIdentity {
        self.localds.identity()
    }

    pub fn now(&self) -> TimeStamp {
        self.clock.now()
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
