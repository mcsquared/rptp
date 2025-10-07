use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::AnnounceMessage;

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
}

impl<S: SortedForeignClocks> BestForeignClock<S> {
    pub fn new(sorted_clocks: S) -> Self {
        Self { sorted_clocks }
    }

    pub fn consider(&self, _announce: AnnounceMessage) {
        // In a real implementation, you would create a ForeignClock from the announce
        // and insert it into the sorted_clocks.
        let clock = ForeignClock::new();
        self.sorted_clocks.insert(clock);
    }

    pub fn clock(&self) -> Option<ForeignClock> {
        self.sorted_clocks.first()
    }
}
