use std::cell::Cell;

use crate::clock::{Clock, ClockIdentity, SynchronizableClock};
use crate::port::{
    PhysicalPort, PortIdentity, PortNumber, SendError, SendResult, Timeout, TimerHost,
};
use crate::result::Result;
use crate::time::TimeStamp;
use crate::timestamping::TxTimestamping;
use crate::wire::{MessageHeader, UnvalidatedMessage};

use std::cell::RefCell;
use std::rc::Rc;

use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::time::Duration;

pub struct FakeClock {
    now: Cell<TimeStamp>,
    last_adjust: Cell<Option<f64>>,
}

impl FakeClock {
    pub fn new(now: TimeStamp) -> Self {
        Self {
            now: Cell::new(now),
            last_adjust: Cell::new(None),
        }
    }

    pub fn last_adjust(&self) -> Option<f64> {
        self.last_adjust.get()
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

impl Clock for &FakeClock {
    fn now(&self) -> TimeStamp {
        (*self).now()
    }
}

impl SynchronizableClock for FakeClock {
    fn step(&self, to: TimeStamp) {
        self.now.set(to);
    }

    fn adjust(&self, rate: f64) {
        self.last_adjust.set(Some(rate));
    }
}

impl SynchronizableClock for &FakeClock {
    fn step(&self, to: TimeStamp) {
        (*self).step(to);
    }

    fn adjust(&self, rate: f64) {
        (*self).adjust(rate);
    }
}

impl Clock for Rc<FakeClock> {
    fn now(&self) -> TimeStamp {
        self.as_ref().now()
    }
}

pub struct FakeTimestamping;

impl FakeTimestamping {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for FakeTimestamping {
    fn default() -> Self {
        Self::new()
    }
}

impl TxTimestamping for FakeTimestamping {
    fn stamp_egress(&self, _msg: EventMessage) {
        // no-op
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

impl Default for FakeTimerHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TimerHost for FakeTimerHost {
    type Timeout = FakeTimeout;

    fn timeout(&self, msg: SystemMessage) -> Self::Timeout {
        FakeTimeout::from_system_message(self.system_messages.clone(), msg)
    }
}

impl TimerHost for &FakeTimerHost {
    type Timeout = FakeTimeout;

    fn timeout(&self, msg: SystemMessage) -> Self::Timeout {
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

pub struct TestMessage<'a> {
    buf: &'a [u8],
}

impl<'a> TestMessage<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn event(&self) -> Result<EventMessage> {
        let length_checked = UnvalidatedMessage::new(self.buf).length_checked_v2()?;
        let header = MessageHeader::new(length_checked);
        EventMessage::new(
            header.message_type()?,
            header.sequence_id(),
            header.flags(),
            header.log_message_interval(),
            header.payload(),
        )
    }

    pub fn general(&self) -> Result<GeneralMessage> {
        let length_checked = UnvalidatedMessage::new(self.buf).length_checked_v2()?;
        let header = MessageHeader::new(length_checked);
        GeneralMessage::new(
            header.message_type()?,
            header.sequence_id(),
            header.flags(),
            header.log_message_interval(),
            header.payload(),
        )
    }
}

pub struct FakePort {
    event_messages: Rc<RefCell<Vec<Vec<u8>>>>,
    general_messages: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl FakePort {
    pub fn new() -> Self {
        Self {
            event_messages: Rc::new(RefCell::new(Vec::new())),
            general_messages: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn contains_event_message(&self, expected: &EventMessage) -> bool {
        let messages = self.event_messages.borrow().clone();
        messages.iter().enumerate().any(|(index, buf)| {
            let msg = TestMessage::new(buf.as_slice())
                .event()
                .unwrap_or_else(|err| {
                    panic!("FakePort stored undecodable event message at index {index}: {err:?}")
                });
            msg == *expected
        })
    }

    pub fn contains_general_message(&self, expected: &GeneralMessage) -> bool {
        let messages = self.general_messages.borrow().clone();
        messages.iter().enumerate().any(|(index, buf)| {
            let msg = TestMessage::new(buf.as_slice())
                .general()
                .unwrap_or_else(|err| {
                    panic!("FakePort stored undecodable general message at index {index}: {err:?}")
                });
            msg == *expected
        })
    }

    pub fn is_empty(&self) -> bool {
        self.event_messages.borrow().is_empty() && self.general_messages.borrow().is_empty()
    }
}

impl Default for FakePort {
    fn default() -> Self {
        Self::new()
    }
}

impl PhysicalPort for FakePort {
    fn send_event(&self, buf: &[u8]) -> SendResult {
        self.event_messages.borrow_mut().push(buf.to_vec());
        Ok(())
    }

    fn send_general(&self, buf: &[u8]) -> SendResult {
        self.general_messages.borrow_mut().push(buf.to_vec());
        Ok(())
    }
}

impl PhysicalPort for &FakePort {
    fn send_event(&self, buf: &[u8]) -> SendResult {
        (*self).send_event(buf)
    }

    fn send_general(&self, buf: &[u8]) -> SendResult {
        (*self).send_general(buf)
    }
}

pub struct FailingPort;

impl PhysicalPort for FailingPort {
    fn send_event(&self, _buf: &[u8]) -> SendResult {
        Err(SendError)
    }

    fn send_general(&self, _buf: &[u8]) -> SendResult {
        Err(SendError)
    }
}
