use crate::bmca::{ForeignClockRecord, SortedForeignClockRecords};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};

pub trait Timeout {
    fn restart(&self, timeout: std::time::Duration);
    fn restart_with(&self, msg: SystemMessage, timeout: std::time::Duration);
    fn cancel(&self);
}

pub struct DropTimeout<T: Timeout> {
    inner: T,
}

impl<T: Timeout> DropTimeout<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T: Timeout> Timeout for DropTimeout<T> {
    fn restart(&self, timeout: std::time::Duration) {
        self.inner.restart(timeout);
    }

    fn restart_with(&self, msg: SystemMessage, timeout: std::time::Duration) {
        self.inner.restart_with(msg, timeout);
    }

    fn cancel(&self) {
        self.inner.cancel();
    }
}

impl<T: Timeout> Drop for DropTimeout<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub trait Port {
    type Clock: SynchronizableClock;
    type ClockRecords: SortedForeignClockRecords;
    type Timeout: Timeout;

    fn clock(&self) -> &LocalClock<Self::Clock>;
    fn foreign_clock_records(&self, records: &[ForeignClockRecord]) -> Self::ClockRecords;
    fn send_event(&self, msg: EventMessage);
    fn send_general(&self, msg: GeneralMessage);
    fn schedule(&self, msg: SystemMessage, delay: std::time::Duration) -> Self::Timeout;
}

impl<P: Port> Port for Box<P> {
    type Clock = P::Clock;
    type ClockRecords = P::ClockRecords;
    type Timeout = P::Timeout;

    fn clock(&self) -> &LocalClock<Self::Clock> {
        self.as_ref().clock()
    }

    fn foreign_clock_records(&self, records: &[ForeignClockRecord]) -> Self::ClockRecords {
        self.as_ref().foreign_clock_records(records)
    }

    fn send_event(&self, msg: EventMessage) {
        self.as_ref().send_event(msg)
    }

    fn send_general(&self, msg: GeneralMessage) {
        self.as_ref().send_general(msg)
    }

    fn schedule(&self, msg: SystemMessage, delay: std::time::Duration) -> Self::Timeout {
        self.as_ref().schedule(msg, delay)
    }
}

#[cfg(test)]
pub mod test_support {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::Duration;

    use crate::bmca::{ForeignClockRecord, LocalClockDS};
    use crate::clock::{LocalClock, SynchronizableClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::message::{EventMessage, GeneralMessage, SystemMessage};

    use super::{Port, Timeout};

    pub struct FakeTimeout {
        msg: RefCell<SystemMessage>,
        system_messages: Rc<RefCell<Vec<SystemMessage>>>,
        is_active: Cell<bool>,
    }

    impl FakeTimeout {
        pub fn new(msg: SystemMessage, system_messages: Rc<RefCell<Vec<SystemMessage>>>) -> Self {
            Self {
                msg: RefCell::new(msg),
                system_messages,
                is_active: Cell::new(true),
            }
        }

        pub fn from_system_message(msg: SystemMessage) -> Self {
            Self {
                msg: RefCell::new(msg),
                system_messages: Rc::new(RefCell::new(Vec::new())),
                is_active: Cell::new(true),
            }
        }

        /// Return the currently scheduled system message so tests can simulate firing the timeout.
        pub fn fire(&self) -> SystemMessage {
            *self.msg.borrow()
        }

        pub fn is_active(&self) -> bool {
            self.is_active.get()
        }
    }

    impl Timeout for FakeTimeout {
        fn restart(&self, _timeout: Duration) {
            let msg = *self.msg.borrow();
            self.system_messages.borrow_mut().push(msg);
            self.is_active.set(true);
        }

        fn restart_with(&self, msg: SystemMessage, _timeout: Duration) {
            self.system_messages.borrow_mut().push(msg);
            self.msg.replace(msg);
            self.is_active.set(true);
        }

        fn cancel(&self) {
            self.is_active.set(false);
        }
    }

    impl Timeout for Rc<FakeTimeout> {
        fn restart(&self, timeout: Duration) {
            self.as_ref().restart(timeout);
        }

        fn restart_with(&self, msg: SystemMessage, _timeout: Duration) {
            self.as_ref().restart_with(msg, _timeout);
        }

        fn cancel(&self) {
            self.as_ref().cancel();
        }
    }

    pub struct FakePort<C: SynchronizableClock> {
        clock: LocalClock<C>,
        event_messages: Rc<RefCell<Vec<EventMessage>>>,
        general_messages: Rc<RefCell<Vec<GeneralMessage>>>,
        system_messages: Rc<RefCell<Vec<SystemMessage>>>,
    }

    impl<C: SynchronizableClock> FakePort<C> {
        pub fn new(clock: C, localds: LocalClockDS) -> Self {
            Self {
                clock: LocalClock::new(clock, localds),
                event_messages: Rc::new(RefCell::new(Vec::new())),
                general_messages: Rc::new(RefCell::new(Vec::new())),
                system_messages: Rc::new(RefCell::new(Vec::new())),
            }
        }

        pub fn take_event_messages(&self) -> Vec<EventMessage> {
            self.event_messages.borrow_mut().drain(..).collect()
        }

        pub fn take_general_messages(&self) -> Vec<GeneralMessage> {
            self.general_messages.borrow_mut().drain(..).collect()
        }

        pub fn take_system_messages(&self) -> Vec<SystemMessage> {
            self.system_messages.borrow_mut().drain(..).collect()
        }
    }

    impl<C: SynchronizableClock> Port for FakePort<C> {
        type Clock = C;
        type ClockRecords = SortedForeignClockRecordsVec;
        type Timeout = Rc<FakeTimeout>;

        fn clock(&self) -> &LocalClock<Self::Clock> {
            &self.clock
        }

        fn foreign_clock_records(&self, records: &[ForeignClockRecord]) -> Self::ClockRecords {
            SortedForeignClockRecordsVec::from_records(records)
        }

        fn send_event(&self, msg: EventMessage) {
            self.event_messages.borrow_mut().push(msg);
        }

        fn send_general(&self, msg: GeneralMessage) {
            self.general_messages.borrow_mut().push(msg);
        }

        fn schedule(&self, msg: SystemMessage, _delay: Duration) -> Self::Timeout {
            self.system_messages.borrow_mut().push(msg);
            Rc::new(FakeTimeout::new(msg, Rc::clone(&self.system_messages)))
        }
    }

    impl<C: SynchronizableClock> Port for &FakePort<C> {
        type Clock = C;
        type ClockRecords = SortedForeignClockRecordsVec;
        type Timeout = Rc<FakeTimeout>;

        fn clock(&self) -> &LocalClock<Self::Clock> {
            &self.clock
        }

        fn foreign_clock_records(&self, records: &[ForeignClockRecord]) -> Self::ClockRecords {
            SortedForeignClockRecordsVec::from_records(records)
        }

        fn send_event(&self, msg: EventMessage) {
            self.event_messages.borrow_mut().push(msg);
        }

        fn send_general(&self, msg: GeneralMessage) {
            self.general_messages.borrow_mut().push(msg);
        }

        fn schedule(&self, msg: SystemMessage, _delay: Duration) -> Self::Timeout {
            self.system_messages.borrow_mut().push(msg);
            Rc::new(FakeTimeout::new(msg, Rc::clone(&self.system_messages)))
        }
    }
}
