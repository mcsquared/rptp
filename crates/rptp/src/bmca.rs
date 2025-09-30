use std::cell::RefCell;

use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::AnnounceMessage;

pub struct ForeignClock {}

impl ForeignClock {
    pub fn new() -> Self {
        Self {}
    }

    pub fn outranks_local<C: SynchronizableClock>(&self, _local: &LocalClock<C>) -> bool {
        false
    }

    pub fn outranks_other(&self, _other: &ForeignClock) -> bool {
        false
    }
}

pub struct BestForeignClock {
    announce_count: RefCell<u8>,
}

impl BestForeignClock {
    pub fn new() -> Self {
        Self {
            announce_count: RefCell::new(0),
        }
    }

    pub fn consider(&self, _announce: AnnounceMessage) {
        *self.announce_count.borrow_mut() += 1;
    }

    pub fn best(&self) -> Option<ForeignClock> {
        if self.announce_count.borrow().clone() >= 2 {
            Some(ForeignClock::new())
        } else {
            None
        }
    }
}
