use core::fmt::{Display, Formatter};
use core::ops::Range;

use crate::bmca::SortedForeignClockRecords;
use crate::clock::{ClockIdentity, LocalClock, SynchronizableClock};
use crate::log::{PortEvent, PortLog};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::portstate::PortState;
use crate::result::{ProtocolError, Result};
use crate::time::{Duration, Instant, TimeStamp};
use crate::timestamping::TxTimestamping;
use crate::wire::{MessageBuffer, PtpVersion, TransportSpecific};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendError;

pub type SendResult = core::result::Result<(), SendError>;

pub trait Timeout {
    fn restart(&self, timeout: Duration);
}

pub trait PhysicalPort {
    fn send_event(&self, buf: &[u8]) -> SendResult;
    fn send_general(&self, buf: &[u8]) -> SendResult;
}

pub trait TimerHost {
    type Timeout: Timeout + Drop;

    /// Create a timeout handle for a message.
    ///
    /// Creating a timeout must not schedule it; scheduling happens when the handle is restarted.
    fn timeout(&self, msg: SystemMessage) -> Self::Timeout;
}

pub trait Port {
    type Clock: SynchronizableClock;
    type Timeout: Timeout;

    fn local_clock(&self) -> &LocalClock<Self::Clock>;
    fn send_event(&self, msg: EventMessage) -> SendResult;
    fn send_general(&self, msg: GeneralMessage) -> SendResult;
    /// Create a timeout handle for a message.
    ///
    /// Creating a timeout must not schedule it; scheduling happens when the handle is restarted.
    fn timeout(&self, msg: SystemMessage) -> Self::Timeout;
    fn log(&self, event: PortEvent);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortNumber(u16);

impl PortNumber {
    pub const fn new(n: u16) -> Self {
        Self(n)
    }

    pub(crate) fn to_be_bytes(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }

    pub(crate) fn from_be_bytes(bytes: [u8; 2]) -> Self {
        Self(u16::from_be_bytes(bytes))
    }
}

impl From<u16> for PortNumber {
    fn from(value: u16) -> Self {
        PortNumber::new(value)
    }
}

impl From<PortNumber> for u16 {
    fn from(value: PortNumber) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DomainNumber(u8);

impl DomainNumber {
    pub const fn new(n: u8) -> Self {
        Self(n)
    }

    pub(crate) fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for DomainNumber {
    fn from(value: u8) -> Self {
        DomainNumber::new(value)
    }
}

impl From<DomainNumber> for u8 {
    fn from(value: DomainNumber) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortIdentity {
    clock_identity: ClockIdentity,
    port_number: PortNumber,
}

impl PortIdentity {
    const CLOCK_IDENTITY_RANGE: Range<usize> = 0..8;
    const PORT_NUMBER_RANGE: Range<usize> = 8..10;

    pub fn new(clock_identity: ClockIdentity, port_number: PortNumber) -> Self {
        Self {
            clock_identity,
            port_number,
        }
    }

    pub(crate) fn from_wire(buf: &[u8; 10]) -> Self {
        Self {
            clock_identity: ClockIdentity::new(&[
                buf[Self::CLOCK_IDENTITY_RANGE.start],
                buf[Self::CLOCK_IDENTITY_RANGE.start + 1],
                buf[Self::CLOCK_IDENTITY_RANGE.start + 2],
                buf[Self::CLOCK_IDENTITY_RANGE.start + 3],
                buf[Self::CLOCK_IDENTITY_RANGE.start + 4],
                buf[Self::CLOCK_IDENTITY_RANGE.start + 5],
                buf[Self::CLOCK_IDENTITY_RANGE.start + 6],
                buf[Self::CLOCK_IDENTITY_RANGE.start + 7],
            ]),
            port_number: PortNumber::from_be_bytes([
                buf[Self::PORT_NUMBER_RANGE.start],
                buf[Self::PORT_NUMBER_RANGE.start + 1],
            ]),
        }
    }

    pub(crate) fn to_wire(self) -> [u8; 10] {
        let mut bytes = [0u8; 10];
        bytes[Self::CLOCK_IDENTITY_RANGE].copy_from_slice(self.clock_identity.as_bytes());
        bytes[Self::PORT_NUMBER_RANGE].copy_from_slice(&self.port_number.to_be_bytes());
        bytes
    }
}

impl Display for PortIdentity {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}-{}", self.clock_identity, self.port_number.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParentPortIdentity {
    parent_port_identity: PortIdentity,
}

impl ParentPortIdentity {
    pub fn new(parent_port_identity: PortIdentity) -> Self {
        Self {
            parent_port_identity,
        }
    }

    pub(crate) fn matches(&self, source_port_identity: &PortIdentity) -> bool {
        self.parent_port_identity == *source_port_identity
    }
}

impl Display for ParentPortIdentity {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.parent_port_identity)
    }
}

pub struct DomainPort<'a, C: SynchronizableClock, T: TimerHost, TS: TxTimestamping, L: PortLog> {
    local_clock: &'a LocalClock<C>,
    physical_port: &'a dyn PhysicalPort,
    timer_host: T,
    timestamping: TS,
    log: L,
    domain_number: DomainNumber,
    port_number: PortNumber,
}

impl<'a, C: SynchronizableClock, T: TimerHost, TS: TxTimestamping, L: PortLog>
    DomainPort<'a, C, T, TS, L>
{
    pub fn new(
        local_clock: &'a LocalClock<C>,
        physical_port: &'a dyn PhysicalPort,
        timer_host: T,
        timestamping: TS,
        log: L,
        domain_number: DomainNumber,
        port_number: PortNumber,
    ) -> Self {
        Self {
            local_clock,
            physical_port,
            timer_host,
            timestamping,
            log,
            domain_number,
            port_number,
        }
    }
}

impl<'a, C: SynchronizableClock, T: TimerHost, TS: TxTimestamping, L: PortLog> Port
    for DomainPort<'a, C, T, TS, L>
{
    type Clock = C;
    type Timeout = T::Timeout;

    fn local_clock(&self) -> &LocalClock<Self::Clock> {
        self.local_clock
    }

    fn send_event(&self, msg: EventMessage) -> SendResult {
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            self.domain_number,
            PortIdentity::new(*self.local_clock.identity(), self.port_number),
        );
        let finalized = msg.to_wire(&mut buf);
        let res = self.physical_port.send_event(finalized.as_ref());
        if res.is_ok() {
            self.timestamping.stamp_egress(msg);
        }
        res
    }

    fn send_general(&self, msg: GeneralMessage) -> SendResult {
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            self.domain_number,
            PortIdentity::new(*self.local_clock.identity(), self.port_number),
        );
        let finalized = msg.to_wire(&mut buf);
        self.physical_port.send_general(finalized.as_ref())
    }

    fn timeout(&self, msg: SystemMessage) -> Self::Timeout {
        self.timer_host.timeout(msg)
    }

    fn log(&self, event: PortEvent) {
        self.log.port_event(event);
    }
}

pub trait PortMap {
    fn port_by_domain(&mut self, domain_number: DomainNumber) -> Result<&mut dyn PortIngress>;
}

pub struct SingleDomainPortMap<P: Port, S: SortedForeignClockRecords> {
    domain_number: DomainNumber,
    port_state: Option<PortState<P, S>>,
}

impl<P: Port, S: SortedForeignClockRecords> SingleDomainPortMap<P, S> {
    pub fn new(domain_number: DomainNumber, port_state: PortState<P, S>) -> Self {
        Self {
            domain_number,
            port_state: Some(port_state),
        }
    }
}

impl<P: Port, S: SortedForeignClockRecords> PortMap for SingleDomainPortMap<P, S> {
    fn port_by_domain(&mut self, domain_number: DomainNumber) -> Result<&mut dyn PortIngress> {
        if self.domain_number == domain_number {
            Ok(&mut self.port_state)
        } else {
            Err(ProtocolError::DomainNotFound(domain_number.as_u8()).into())
        }
    }
}

pub trait PortIngress {
    fn process_event_message(
        &mut self,
        source_port_identity: PortIdentity,
        msg: EventMessage,
        timestamp: TimeStamp,
    );
    fn process_general_message(
        &mut self,
        source_port_identity: PortIdentity,
        msg: GeneralMessage,
        now: Instant,
    );
    fn process_system_message(&mut self, msg: SystemMessage);
}

impl<P: Port, S: SortedForeignClockRecords> PortIngress for Option<PortState<P, S>> {
    fn process_event_message(
        &mut self,
        source_port_identity: PortIdentity,
        msg: EventMessage,
        timestamp: TimeStamp,
    ) {
        if let Some(decision) = self
            .as_mut()
            .and_then(|state| state.dispatch_event(msg, source_port_identity, timestamp))
        {
            *self = self.take().map(|state| state.apply(decision));
        }
    }

    fn process_general_message(
        &mut self,
        source_port_identity: PortIdentity,
        msg: GeneralMessage,
        now: Instant,
    ) {
        if let Some(decision) = self
            .as_mut()
            .and_then(|state| state.dispatch_general(msg, source_port_identity, now))
        {
            *self = self.take().map(|state| state.apply(decision));
        }
    }

    fn process_system_message(&mut self, msg: SystemMessage) {
        if let Some(decision) = self.as_mut().and_then(|state| state.dispatch_system(msg)) {
            *self = self.take().map(|state| state.apply(decision));
        }
    }
}

pub(crate) struct AnnounceReceiptTimeout<T: Timeout> {
    timeout: T,
    interval: Duration,
}

impl<T: Timeout> AnnounceReceiptTimeout<T> {
    pub fn new(timeout: T, interval: Duration) -> Self {
        Self { timeout, interval }
    }

    pub fn restart(&self) {
        self.timeout.restart(self.interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::cell::RefCell;
    use std::rc::Rc;

    use crate::bmca::{ClockDS, Priority1, Priority2};
    use crate::clock::{ClockAccuracy, ClockClass, ClockQuality, StepsRemoved};
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{DelayRequestMessage, FollowUpMessage};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, FakeTimerHost, FakeTimestamping};
    use crate::time::LogMessageInterval;

    struct CapturePort {
        sent: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl CapturePort {
        fn new() -> (Self, Rc<RefCell<Vec<Vec<u8>>>>) {
            let rc = Rc::new(RefCell::new(Vec::new()));
            (Self { sent: rc.clone() }, rc)
        }
    }

    impl PhysicalPort for CapturePort {
        fn send_event(&self, buf: &[u8]) -> SendResult {
            self.sent.borrow_mut().push(buf.to_vec());
            Ok(())
        }
        fn send_general(&self, buf: &[u8]) -> SendResult {
            self.sent.borrow_mut().push(buf.to_vec());
            Ok(())
        }
    }

    #[test]
    fn port_sets_domain_and_identity_on_event() {
        let identity = ClockIdentity::new(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let local_clock = LocalClock::new(
            FakeClock::default(),
            ClockDS::new(
                identity,
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(ClockClass::Default, ClockAccuracy::Within100us, 0xFFFF),
                StepsRemoved::new(0),
            ),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let (cap_port, sent) = CapturePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_number = DomainNumber::new(3);
        let port_number = PortNumber::new(5);

        let port = DomainPort::new(
            &local_clock,
            &cap_port,
            &timer_host,
            FakeTimestamping::new(),
            NoopPortLog,
            domain_number,
            port_number,
        );

        assert!(
            port.send_event(EventMessage::DelayReq(DelayRequestMessage::new(42.into())))
                .is_ok()
        );
        let bufs = sent.borrow();
        assert!(!bufs.is_empty());
        let bytes = &bufs[0];
        assert_eq!(bytes[4], domain_number.as_u8());
        assert_eq!(
            &bytes[20..30],
            &crate::port::PortIdentity::new(identity, port_number).to_wire()
        );
    }

    #[test]
    fn port_sets_domain_and_identity_on_general() {
        let identity = ClockIdentity::new(&[8, 7, 6, 5, 4, 3, 2, 1]);
        let local_clock = LocalClock::new(
            FakeClock::default(),
            ClockDS::new(
                identity,
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(ClockClass::Default, ClockAccuracy::Within100us, 0xFFFF),
                StepsRemoved::new(0),
            ),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let (cap_port, sent) = CapturePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_number = DomainNumber::new(9);
        let port_number = PortNumber::new(2);

        let port = DomainPort::new(
            &local_clock,
            &cap_port,
            &timer_host,
            FakeTimestamping::new(),
            NoopPortLog,
            domain_number,
            port_number,
        );

        let follow =
            FollowUpMessage::new(7.into(), LogMessageInterval::new(3), TimeStamp::new(1, 2));
        let _ = port.send_general(GeneralMessage::FollowUp(follow));
        let bufs = sent.borrow();
        assert!(!bufs.is_empty());
        let bytes = &bufs[0];
        assert_eq!(bytes[4], domain_number.as_u8());
        assert_eq!(
            &bytes[20..30],
            &crate::port::PortIdentity::new(identity, port_number).to_wire()
        );
    }
}
