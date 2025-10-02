use crate::bmca::ForeignClock;
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{AnnounceMessage, EventMessage, GeneralMessage, SystemMessage};

pub trait Port {
    type Clock: SynchronizableClock;

    fn clock(&self) -> &LocalClock<Self::Clock>;
    fn consider_announce(&self, msg: AnnounceMessage);
    fn best_foreign_clock(&self) -> Option<ForeignClock>;
    fn send_event(&self, msg: EventMessage);
    fn send_general(&self, msg: GeneralMessage);
    fn schedule(&self, msg: SystemMessage, delay: std::time::Duration);
}

impl<P: Port> Port for Box<P> {
    type Clock = P::Clock;

    fn clock(&self) -> &LocalClock<Self::Clock> {
        self.as_ref().clock()
    }

    fn consider_announce(&self, msg: AnnounceMessage) {
        self.as_ref().consider_announce(msg)
    }

    fn best_foreign_clock(&self) -> Option<ForeignClock> {
        self.as_ref().best_foreign_clock()
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
