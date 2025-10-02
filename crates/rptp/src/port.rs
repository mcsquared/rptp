use crate::bmca::{BestForeignClock, ForeignClockStore};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};

pub trait Infrastructure {
    type ForeignClockStore: ForeignClockStore;

    fn best_foreign_clock(&self) -> BestForeignClock<Self::ForeignClockStore>;
}

pub trait Port {
    type Clock: SynchronizableClock;
    type Infrastructure: Infrastructure;

    fn clock(&self) -> &LocalClock<Self::Clock>;
    fn infrastructure(&self) -> &Self::Infrastructure;
    fn send_event(&self, msg: EventMessage);
    fn send_general(&self, msg: GeneralMessage);
    fn schedule(&self, msg: SystemMessage, delay: std::time::Duration);
}

impl<P: Port> Port for Box<P> {
    type Clock = P::Clock;
    type Infrastructure = P::Infrastructure;

    fn clock(&self) -> &LocalClock<Self::Clock> {
        self.as_ref().clock()
    }

    fn infrastructure(&self) -> &Self::Infrastructure {
        self.as_ref().infrastructure()
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
