use std::cell::Cell;

use crate::{
    bmca::ForeignClock,
    message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    },
    offsets::{MasterSlaveOffset, SlaveMasterOffset},
    time::TimeStamp,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockIdentity([u8; 8]);

impl ClockIdentity {
    pub fn new(id: [u8; 8]) -> Self {
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
    pub fn new(clock_class: u8, clock_accuracy: u8, offset_scaled_log_variance: u16) -> Self {
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
    master_slave_offset: MasterSlaveOffset,
    slave_master_offset: SlaveMasterOffset,
    self_bmca_view: ForeignClock,
}

impl<C: SynchronizableClock> LocalClock<C> {
    pub fn new(clock: C, self_bmca_view: ForeignClock) -> Self {
        Self {
            clock,
            master_slave_offset: MasterSlaveOffset::new(),
            slave_master_offset: SlaveMasterOffset::new(),
            self_bmca_view,
        }
    }

    pub fn announce(&self, sequence_id: u16) -> AnnounceMessage {
        AnnounceMessage::new(sequence_id, self.self_bmca_view)
    }

    pub fn outranks_foreign(&self, other: &ForeignClock) -> bool {
        self.self_bmca_view.outranks_other(other)
    }

    pub fn ingest_two_step_sync(&self, sync: TwoStepSyncMessage, timestamp: TimeStamp) {
        self.master_slave_offset
            .ingest_two_step_sync(sync, timestamp);
        self.discipline();
    }

    pub fn ingest_follow_up(&self, follow_up: FollowUpMessage) {
        self.master_slave_offset.ingest_follow_up(follow_up);
        self.discipline();
    }

    pub fn ingest_delay_request(&self, req: DelayRequestMessage, timestamp: TimeStamp) {
        self.slave_master_offset
            .ingest_delay_request(req, timestamp);
        self.discipline();
    }

    pub fn ingest_delay_response(&self, resp: DelayResponseMessage) {
        self.slave_master_offset.ingest_delay_response(resp);
        self.discipline();
    }

    fn discipline(&self) {
        if let Some(estimate) = self
            .master_slave_offset
            .master_estimate(&self.slave_master_offset)
        {
            self.clock.synchronize(estimate);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::rc::Rc;

    #[test]
    fn local_clock_adjusts_wrapped_clock() {
        let clock = Rc::new(FakeClock::default());
        let sync_clock = LocalClock::new(clock.clone(), ForeignClock::mid_grade_test_clock());

        sync_clock.ingest_two_step_sync(TwoStepSyncMessage::new(0), TimeStamp::new(1, 0));
        sync_clock.ingest_follow_up(FollowUpMessage::new(0, TimeStamp::new(1, 0)));
        sync_clock.ingest_delay_request(DelayRequestMessage::new(0), TimeStamp::new(0, 0));
        sync_clock.ingest_delay_response(DelayResponseMessage::new(0, TimeStamp::new(2, 0)));

        assert_eq!(clock.now(), TimeStamp::new(2, 0));
    }
}
