use crate::bmca::ForeignClockStore;
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};

pub trait Port {
    type Clock: SynchronizableClock;
    type ClockStore: ForeignClockStore;

    fn clock(&self) -> &LocalClock<Self::Clock>;
    fn foreign_clock_store(&self) -> Self::ClockStore;
    fn send_event(&self, msg: EventMessage);
    fn send_general(&self, msg: GeneralMessage);
    fn schedule(&self, msg: SystemMessage, delay: std::time::Duration);
}

impl<P: Port> Port for Box<P> {
    type Clock = P::Clock;
    type ClockStore = P::ClockStore;

    fn clock(&self) -> &LocalClock<Self::Clock> {
        self.as_ref().clock()
    }

    fn foreign_clock_store(&self) -> Self::ClockStore {
        self.as_ref().foreign_clock_store()
    }

    fn send_event(&self, msg: EventMessage) {
        self.as_ref().send_event(msg)
    }

    fn send_general(&self, msg: GeneralMessage) {
        self.as_ref().send_general(msg)
    }

    fn schedule(&self, msg: SystemMessage, delay: std::time::Duration) {
        self.as_ref().schedule(msg, delay)
    }
}
