use crate::bmca::ForeignClockStore;
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};

pub trait Port {
    type Clock: SynchronizableClock;
    type ForeignClockStore: ForeignClockStore;

    fn clock(&self) -> &LocalClock<Self::Clock>;
    fn send_event(&self, msg: EventMessage);
    fn send_general(&self, msg: GeneralMessage);
    fn schedule(&self, msg: SystemMessage, delay: std::time::Duration);
}

impl<P: Port> Port for Box<P> {
    type Clock = P::Clock;
    type ForeignClockStore = P::ForeignClockStore;

    fn clock(&self) -> &LocalClock<Self::Clock> {
        self.as_ref().clock()
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
