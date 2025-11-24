use core::fmt::{Display, Formatter};
use std::cell::Cell;
use std::ops::Range;

use crate::{
    bmca::{DefaultDS, ForeignClockDS},
    message::{AnnounceMessage, SequenceId},
    time::TimeStamp,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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

impl Display for ClockIdentity {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02x}{:02x}{:02x}.{:02x}{:02x}.{:02x}{:02x}{:02x}",
            self.id[0],
            self.id[1],
            self.id[2],
            self.id[3],
            self.id[4],
            self.id[5],
            self.id[6],
            self.id[7]
        )
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

    pub fn is_grandmaster_capable(&self) -> bool {
        self.clock_class >= 1 && self.clock_class <= 127
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

impl Ord for ClockQuality {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let a = (
            &self.clock_class,
            &self.clock_accuracy,
            &self.offset_scaled_log_variance,
        );
        let b = (
            &other.clock_class,
            &other.clock_accuracy,
            &other.offset_scaled_log_variance,
        );

        a.cmp(&b)
    }
}

impl PartialOrd for ClockQuality {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
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
    default_ds: DefaultDS,
    steps_removed: StepsRemoved,
}

impl<C: SynchronizableClock> LocalClock<C> {
    pub fn new(clock: C, default_ds: DefaultDS, steps_removed: StepsRemoved) -> Self {
        Self {
            clock,
            default_ds,
            steps_removed,
        }
    }

    pub fn identity(&self) -> &ClockIdentity {
        self.default_ds.identity()
    }

    pub fn now(&self) -> TimeStamp {
        self.clock.now()
    }
    pub fn steps_removed(&self) -> StepsRemoved {
        self.steps_removed
    }

    pub fn announce(&self, sequence_id: SequenceId) -> AnnounceMessage {
        self.default_ds.announce(sequence_id, self.steps_removed)
    }

    pub fn is_grandmaster_capable(&self) -> bool {
        self.default_ds.is_grandmaster_capable()
    }

    pub fn better_than(&self, other: &ForeignClockDS) -> bool {
        self.default_ds.better_than(other, &self.steps_removed)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StepsRemoved(u16);

impl StepsRemoved {
    pub fn new(steps_removed: u16) -> Self {
        Self(steps_removed)
    }

    pub fn increment(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub fn as_u16(&self) -> u16 {
        self.0
    }

    pub fn to_be_bytes(&self) -> [u8; 2] {
        self.0.to_be_bytes()
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
