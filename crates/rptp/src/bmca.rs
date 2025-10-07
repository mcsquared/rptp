use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::AnnounceMessage;
use std::cell::Cell;

pub trait SortedForeignClocks {
    fn insert(&self, clock: ForeignClock);
    fn first(&self) -> Option<ForeignClock>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

pub struct BestForeignClock<S: SortedForeignClocks> {
    sorted_clocks: S,
    last_announce: Cell<Option<AnnounceMessage>>,
}

impl<S: SortedForeignClocks> BestForeignClock<S> {
    pub fn new(sorted_clocks: S) -> Self {
        Self {
            sorted_clocks,
            last_announce: Cell::new(None),
        }
    }

    pub fn consider(&self, announce: AnnounceMessage) {
        if let Some(previous) = self.last_announce.replace(Some(announce)) {
            if let Some(clock) = announce.follows(previous) {
                self.sorted_clocks.insert(clock);
            }
        }
    }

    pub fn clock(&self) -> Option<ForeignClock> {
        self.sorted_clocks.first()
    }
}
