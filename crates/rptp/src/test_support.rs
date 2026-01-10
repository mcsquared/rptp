use std::cell::Cell;

use crate::bmca::{
    ClockDS, ForeignGrandMasterCandidates, GrandMasterTrackingBmca, ParentTrackingBmca, Priority1,
    Priority2,
};
use crate::clock::{
    Clock, ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, StepsRemoved,
    SynchronizableClock, TimeScale,
};
use crate::e2e::{DelayCycle, EndToEndDelayMechanism};
use crate::port::{
    PhysicalPort, PortIdentity, PortNumber, SendError, SendResult, Timeout, TimerHost,
};
use crate::result::Result;
use crate::time::{LogInterval, TimeStamp};
use crate::timestamping::TxTimestamping;
use crate::wire::{MessageHeader, UnvalidatedMessage};

use std::cell::RefCell;
use std::rc::Rc;

use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::time::Duration;

use crate::bmca::{
    BestForeignRecord, BestMasterClockAlgorithm, LocalGrandMasterCandidate,
    SortedForeignClockRecords,
};
use crate::port::{ParentPortIdentity, Port};
use crate::portstate::PortState;
use crate::profile::PortProfile;

pub struct TestClockCatalog {
    clock_identity: ClockIdentity,
    clock_class: ClockClass,
    clock_accuracy: ClockAccuracy,
    priority1: Priority1,
    priority2: Priority2,
}

impl TestClockCatalog {
    pub fn atomic_grandmaster() -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[0x00, 0x1A, 0xC2, 0xFF, 0xFE, 0x12, 0x34, 0x56]),
            clock_class: ClockClass::PrimaryReference,
            clock_accuracy: ClockAccuracy::Within25ns,
            priority1: Priority1::new(128),
            priority2: Priority2::new(128),
        }
    }

    pub fn gps_grandmaster() -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[0x00, 0x1B, 0xC3, 0xFF, 0xFE, 0x65, 0x43, 0x21]),
            clock_class: ClockClass::PrimaryReference,
            clock_accuracy: ClockAccuracy::Within100ns,
            priority1: Priority1::new(128),
            priority2: Priority2::new(128),
        }
    }

    pub fn ntp_grandmaster() -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[0x00, 0x1C, 0xC4, 0xFF, 0xFE, 0x78, 0x56, 0x34]),
            clock_class: ClockClass::PrimaryReference,
            clock_accuracy: ClockAccuracy::Within1us,
            priority1: Priority1::new(128),
            priority2: Priority2::new(128),
        }
    }

    pub fn application_specific_grandmaster() -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[0x00, 0x1D, 0xC5, 0xFF, 0xFE, 0x89, 0x67, 0x45]),
            clock_class: ClockClass::ApplicationSpecific,
            clock_accuracy: ClockAccuracy::Unknown,
            priority1: Priority1::new(128),
            priority2: Priority2::new(128),
        }
    }

    pub fn default_high_grade() -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[0x00, 0x1E, 0xC6, 0xFF, 0xFE, 0x90, 0x78, 0x56]),
            clock_class: ClockClass::Default,
            clock_accuracy: ClockAccuracy::Within100ns,
            priority1: Priority1::new(128),
            priority2: Priority2::new(128),
        }
    }

    pub fn default_mid_grade() -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[0x00, 0x1F, 0xC7, 0xFF, 0xFE, 0x91, 0x89, 0x67]),
            clock_class: ClockClass::Default,
            clock_accuracy: ClockAccuracy::Within10us,
            priority1: Priority1::new(128),
            priority2: Priority2::new(128),
        }
    }

    pub fn default_low_grade() -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[0x00, 0x20, 0xC8, 0xFF, 0xFE, 0x92, 0x90, 0x78]),
            clock_class: ClockClass::Default,
            clock_accuracy: ClockAccuracy::Within1ms,
            priority1: Priority1::new(128),
            priority2: Priority2::new(128),
        }
    }

    pub fn default_low_grade_slave_only() -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[0x00, 0x21, 0xC9, 0xFF, 0xFE, 0x93, 0x91, 0x89]),
            clock_class: ClockClass::SlaveOnly,
            clock_accuracy: ClockAccuracy::Within10ms,
            priority1: Priority1::new(128),
            priority2: Priority2::new(128),
        }
    }

    pub fn clock_identity(&self) -> ClockIdentity {
        self.clock_identity
    }

    pub fn clock_quality(&self) -> ClockQuality {
        ClockQuality::new(
            self.clock_class,
            self.clock_accuracy,
            0xFFFF, // offset_scaled_log_variance
        )
    }

    pub fn default_ds(&self) -> ClockDS {
        ClockDS::new(
            self.clock_identity,
            self.priority1,
            self.priority2,
            self.clock_quality(),
            StepsRemoved::new(0),
        )
    }

    pub fn foreign_ds(&self, steps_removed: StepsRemoved) -> ClockDS {
        ClockDS::new(
            self.clock_identity,
            self.priority1,
            self.priority2,
            self.clock_quality(),
            steps_removed,
        )
    }

    pub fn with_clock_identity(self, clock_identity: ClockIdentity) -> Self {
        Self {
            clock_identity,
            ..self
        }
    }

    pub fn with_accuracy(self, accuracy: ClockAccuracy) -> Self {
        Self {
            clock_accuracy: accuracy,
            ..self
        }
    }

    pub fn with_priority1(self, priority1: Priority1) -> Self {
        Self { priority1, ..self }
    }

    pub fn with_priority2(self, priority2: Priority2) -> Self {
        Self { priority2, ..self }
    }
}

pub struct FakeClock {
    now: Cell<TimeStamp>,
    time_scale: TimeScale,
    last_adjust: Cell<Option<f64>>,
}

impl FakeClock {
    pub fn new(now: TimeStamp, time_scale: TimeScale) -> Self {
        Self {
            now: Cell::new(now),
            time_scale,
            last_adjust: Cell::new(None),
        }
    }

    pub fn last_adjust(&self) -> Option<f64> {
        self.last_adjust.get()
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new(TimeStamp::new(0, 0), TimeScale::Ptp)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> TimeStamp {
        self.now.get()
    }

    fn time_scale(&self) -> TimeScale {
        self.time_scale
    }
}

impl Clock for &FakeClock {
    fn now(&self) -> TimeStamp {
        (*self).now()
    }

    fn time_scale(&self) -> TimeScale {
        (*self).time_scale()
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

pub fn master_test_port<'a, P: Port, S: SortedForeignClockRecords>(
    port: P,
    local_candidate: &'a dyn LocalGrandMasterCandidate,
    sorted_foreign_clock_records: S,
    foreign_candidates: &'a dyn ForeignGrandMasterCandidates,
    profile: PortProfile,
) -> PortState<'a, P, S> {
    let grandmaster_id = *port.local_clock().identity();
    let bmca = GrandMasterTrackingBmca::new(
        BestMasterClockAlgorithm::new(local_candidate, foreign_candidates, PortNumber::new(1)),
        BestForeignRecord::new(sorted_foreign_clock_records),
        grandmaster_id,
    );
    profile.master(port, bmca)
}

pub fn slave_test_port<'a, P: Port, S: SortedForeignClockRecords>(
    port: P,
    local_candidate: &'a dyn LocalGrandMasterCandidate,
    sorted_foreign_clock_records: S,
    foreign_candidates: &'a dyn ForeignGrandMasterCandidates,
    parent_port_identity: ParentPortIdentity,
    profile: PortProfile,
) -> PortState<'a, P, S> {
    let bmca = ParentTrackingBmca::new(
        BestMasterClockAlgorithm::new(local_candidate, foreign_candidates, PortNumber::new(1)),
        BestForeignRecord::new(sorted_foreign_clock_records),
        parent_port_identity,
    );
    let delay_timeout = port.timeout(SystemMessage::DelayRequestTimeout);
    delay_timeout.restart(Duration::from_secs(0));
    let delay_cycle = DelayCycle::new(0.into(), delay_timeout, LogInterval::new(0));
    let e2e = EndToEndDelayMechanism::new(delay_cycle);
    profile.slave(port, bmca, e2e)
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

    fn time_scale(&self) -> TimeScale {
        self.as_ref().time_scale()
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
