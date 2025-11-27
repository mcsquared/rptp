use std::cell::Cell;

use crate::clock::{Clock, ClockIdentity, SynchronizableClock};
use crate::port::{PhysicalPort, PortIdentity, PortNumber, Timeout, TimerHost};
use crate::time::TimeStamp;

use std::cell::RefCell;
use std::rc::Rc;

use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::time::Duration;

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
    fn step(&self, to: TimeStamp) {
        self.now.set(to);
    }

    fn adjust(&self, _rate: f64) {
        // no-op
    }
}

impl Clock for Rc<FakeClock> {
    fn now(&self) -> TimeStamp {
        self.as_ref().now()
    }
}

#[derive(Debug)]
pub struct FakeTimeout {
    msg: RefCell<SystemMessage>,
    system_messages: Rc<RefCell<Vec<SystemMessage>>>,
}

impl FakeTimeout {
    pub fn new(msg: SystemMessage) -> Self {
        Self {
            msg: RefCell::new(msg),
            system_messages: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn from_system_message(
        system_messages: Rc<RefCell<Vec<SystemMessage>>>,
        msg: SystemMessage,
    ) -> Self {
        system_messages.borrow_mut().push(msg);
        Self {
            msg: RefCell::new(msg),
            system_messages,
        }
    }

    /// Return the currently scheduled system message so tests can simulate firing the timeout.
    pub fn fire(&self) -> SystemMessage {
        *self.msg.borrow()
    }
}

impl Timeout for FakeTimeout {
    fn restart(&self, _timeout: Duration) {
        let msg = *self.msg.borrow();
        self.system_messages.borrow_mut().push(msg);
    }
}

impl PartialEq for FakeTimeout {
    fn eq(&self, other: &Self) -> bool {
        *self.msg.borrow() == *other.msg.borrow()
    }
}

pub struct FakeTimerHost {
    system_messages: Rc<RefCell<Vec<SystemMessage>>>,
}

impl FakeTimerHost {
    pub fn new() -> Self {
        Self {
            system_messages: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn take_system_messages(&self) -> Vec<SystemMessage> {
        self.system_messages.borrow_mut().drain(..).collect()
    }
}

impl TimerHost for FakeTimerHost {
    type Timeout = FakeTimeout;

    fn timeout(&self, msg: SystemMessage, _delay: Duration) -> Self::Timeout {
        FakeTimeout::from_system_message(self.system_messages.clone(), msg)
    }
}

impl TimerHost for &FakeTimerHost {
    type Timeout = FakeTimeout;

    fn timeout(&self, msg: SystemMessage, _delay: Duration) -> Self::Timeout {
        FakeTimeout::from_system_message(self.system_messages.clone(), msg)
    }
}

impl Drop for FakeTimeout {
    fn drop(&mut self) {
        // When the timeout is dropped, we consider it cancelled, so do nothing.
    }
}

impl PortIdentity {
    pub fn fake() -> Self {
        PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            PortNumber::new(1),
        )
    }
}

pub struct FakePort {
    event_messages: Rc<RefCell<Vec<EventMessage>>>,
    general_messages: Rc<RefCell<Vec<GeneralMessage>>>,
}

impl FakePort {
    pub fn new() -> Self {
        Self {
            event_messages: Rc::new(RefCell::new(Vec::new())),
            general_messages: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn take_event_messages(&self) -> Vec<EventMessage> {
        self.event_messages.borrow_mut().drain(..).collect()
    }

    pub fn take_general_messages(&self) -> Vec<GeneralMessage> {
        self.general_messages.borrow_mut().drain(..).collect()
    }
}

impl PhysicalPort for FakePort {
    fn send_event(&self, buf: &[u8]) {
        self.event_messages
            .borrow_mut()
            .push(EventMessage::try_from(buf).unwrap());
    }

    fn send_general(&self, buf: &[u8]) {
        self.general_messages
            .borrow_mut()
            .push(GeneralMessage::try_from(buf).unwrap());
    }
}

impl PhysicalPort for &FakePort {
    fn send_event(&self, buf: &[u8]) {
        self.event_messages
            .borrow_mut()
            .push(EventMessage::try_from(buf).unwrap());
    }

    fn send_general(&self, buf: &[u8]) {
        self.general_messages
            .borrow_mut()
            .push(GeneralMessage::try_from(buf).unwrap());
    }
}
