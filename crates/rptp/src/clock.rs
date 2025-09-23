use std::cell::Cell;
use std::rc::Rc;

use crate::{
    message::{DelayRequestMessage, DelayResponseMessage, FollowUpMessage, TwoStepSyncMessage},
    offsets::{MasterSlaveOffset, SlaveMasterOffset},
    time::TimeStamp,
};

pub trait Clock {
    fn now(&self) -> TimeStamp;
}

pub trait SynchronizableClock: Clock {
    fn synchronize(&self, to: TimeStamp);
}

pub struct SynchronizedClock<C: SynchronizableClock> {
    clock: C,
    master_slave_offset: MasterSlaveOffset,
    slave_master_offset: SlaveMasterOffset,
}

impl<C: SynchronizableClock> SynchronizedClock<C> {
    pub fn new(clock: C) -> Self {
        Self {
            clock,
            master_slave_offset: MasterSlaveOffset::new(),
            slave_master_offset: SlaveMasterOffset::new(),
        }
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

impl<C: SynchronizableClock> Clock for SynchronizedClock<C> {
    fn now(&self) -> TimeStamp {
        self.clock.now()
    }
}

impl Clock for Rc<dyn SynchronizableClock> {
    fn now(&self) -> TimeStamp {
        self.as_ref().now()
    }
}

impl SynchronizableClock for Rc<dyn SynchronizableClock> {
    fn synchronize(&self, to: TimeStamp) {
        self.as_ref().synchronize(to);
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

impl Clock for Rc<FakeClock> {
    fn now(&self) -> TimeStamp {
        self.as_ref().now()
    }
}

impl SynchronizableClock for Rc<FakeClock> {
    fn synchronize(&self, to: TimeStamp) {
        self.as_ref().synchronize(to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronized_clock_adjusts_wrapped_clock() {
        let clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));
        let sync_clock = SynchronizedClock::new(clock.clone());

        sync_clock.ingest_two_step_sync(TwoStepSyncMessage::new(0), TimeStamp::new(1, 0));
        sync_clock.ingest_follow_up(FollowUpMessage::new(0, TimeStamp::new(1, 0)));
        sync_clock.ingest_delay_request(DelayRequestMessage::new(0), TimeStamp::new(0, 0));
        sync_clock.ingest_delay_response(DelayResponseMessage::new(0, TimeStamp::new(2, 0)));

        assert_eq!(clock.now(), TimeStamp::new(2, 0));
    }
}
