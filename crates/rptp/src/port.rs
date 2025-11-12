use std::ops::Range;

use crate::bmca::Bmca;
use crate::buffer::MessageBuffer;
use crate::clock::{ClockIdentity, LocalClock, SynchronizableClock};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::portstate::PortState;
use crate::result::{ProtocolError, Result};
use crate::time::TimeStamp;

pub trait Timeout {
    fn restart(&self, timeout: std::time::Duration);
    fn restart_with(&self, msg: SystemMessage, timeout: std::time::Duration);
}

pub trait PhysicalPort {
    fn send_event(&self, buf: &[u8]);
    fn send_general(&self, buf: &[u8]);
}

impl<P: PhysicalPort> PhysicalPort for Box<P> {
    fn send_event(&self, buf: &[u8]) {
        self.as_ref().send_event(buf)
    }

    fn send_general(&self, buf: &[u8]) {
        self.as_ref().send_general(buf)
    }
}

pub trait TimerHost {
    type Timeout: Timeout + Drop;

    fn timeout(&self, msg: SystemMessage, delay: std::time::Duration) -> Self::Timeout;
}

pub trait Port {
    type Clock: SynchronizableClock;
    type PhysicalPort: PhysicalPort;
    type Bmca: Bmca;
    type Timeout: Timeout;

    fn local_clock(&self) -> &LocalClock<Self::Clock>;
    fn bmca(&self) -> &Self::Bmca;
    fn send_event(&self, msg: EventMessage);
    fn send_general(&self, msg: GeneralMessage);
    fn timeout(&self, msg: SystemMessage, delay: std::time::Duration) -> Self::Timeout;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortNumber(u16);

impl PortNumber {
    pub const fn new(n: u16) -> Self {
        Self(n)
    }

    pub fn to_be_bytes(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }

    pub fn from_be_bytes(bytes: [u8; 2]) -> Self {
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

    pub fn from_slice(buf: &[u8; 10]) -> Self {
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

    pub fn to_bytes(&self) -> [u8; 10] {
        let mut bytes = [0u8; 10];
        bytes[Self::CLOCK_IDENTITY_RANGE].copy_from_slice(self.clock_identity.as_bytes());
        bytes[Self::PORT_NUMBER_RANGE].copy_from_slice(&self.port_number.to_be_bytes());
        bytes
    }
}

pub struct DomainPort<'a, C: SynchronizableClock, B: Bmca, P: PhysicalPort, T: TimerHost> {
    local_clock: &'a LocalClock<C>,
    bmca: B,
    physical_port: P,
    timer_host: T,
    domain_number: u8,
    port_number: PortNumber,
}

impl<'a, C: SynchronizableClock, B: Bmca, P: PhysicalPort, T: TimerHost>
    DomainPort<'a, C, B, P, T>
{
    pub fn new(
        local_clock: &'a LocalClock<C>,
        bmca: B,
        physical_port: P,
        timer_host: T,
        domain_number: u8,
        port_number: PortNumber,
    ) -> Self {
        Self {
            local_clock,
            bmca,
            physical_port,
            timer_host,
            domain_number,
            port_number,
        }
    }
}

impl<'a, C: SynchronizableClock, B: Bmca, P: PhysicalPort, T: TimerHost> Port
    for DomainPort<'a, C, B, P, T>
{
    type Clock = C;
    type PhysicalPort = P;
    type Bmca = B;
    type Timeout = T::Timeout;

    fn local_clock(&self) -> &LocalClock<Self::Clock> {
        &self.local_clock
    }

    fn bmca(&self) -> &Self::Bmca {
        &self.bmca
    }

    fn send_event(&self, msg: EventMessage) {
        let mut buf = MessageBuffer::new(
            0,
            2,
            self.domain_number,
            PortIdentity::new(*self.local_clock.identity(), self.port_number),
            0x7F,
        );
        let finalized = msg.serialize(&mut buf);
        self.physical_port.send_event(finalized.as_ref());
    }

    fn send_general(&self, msg: GeneralMessage) {
        let mut buf = MessageBuffer::new(
            0,
            2,
            self.domain_number,
            PortIdentity::new(*self.local_clock.identity(), self.port_number),
            0x7F,
        );
        let finalized = msg.serialize(&mut buf);
        self.physical_port.send_general(finalized.as_ref());
    }

    fn timeout(&self, msg: SystemMessage, delay: std::time::Duration) -> Self::Timeout {
        self.timer_host.timeout(msg, delay)
    }
}

pub trait PortMap {
    fn port_by_domain(&mut self, domain_number: u8) -> Result<&mut dyn PortIngress>;
}

pub struct SingleDomainPortMap<P: Port> {
    domain_number: u8,
    port_state: Option<PortState<P>>,
}

impl<P: Port> SingleDomainPortMap<P> {
    pub fn new(domain_number: u8, port_state: PortState<P>) -> Self {
        Self {
            domain_number,
            port_state: Some(port_state),
        }
    }
}

impl<P: Port> PortMap for SingleDomainPortMap<P> {
    fn port_by_domain(&mut self, domain_number: u8) -> Result<&mut dyn PortIngress> {
        if self.domain_number == domain_number {
            Ok(&mut self.port_state)
        } else {
            Err(ProtocolError::DomainNotFound.into())
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
    fn process_general_message(&mut self, source_port_identity: PortIdentity, msg: GeneralMessage);
    fn process_system_message(&mut self, msg: SystemMessage);
}

impl<P: Port> PortIngress for Option<PortState<P>> {
    fn process_event_message(
        &mut self,
        source_port_identity: PortIdentity,
        msg: EventMessage,
        timestamp: TimeStamp,
    ) {
        *self = self.take().and_then(|state| {
            Some(state.process_event_message(source_port_identity, msg, timestamp))
        });
    }

    fn process_general_message(&mut self, source_port_identity: PortIdentity, msg: GeneralMessage) {
        *self = self
            .take()
            .and_then(|state| Some(state.process_general_message(source_port_identity, msg)));
    }

    fn process_system_message(&mut self, msg: SystemMessage) {
        *self = self
            .take()
            .and_then(|state| Some(state.process_system_message(msg)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use self::test_support::FakeTimerHost;

    use crate::bmca::{LocalClockDS, NoopBmca};
    use crate::clock::{ClockQuality, FakeClock};
    use crate::message::{DelayRequestMessage, FollowUpMessage};

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
        fn send_event(&self, buf: &[u8]) {
            self.sent.borrow_mut().push(buf.to_vec());
        }
        fn send_general(&self, buf: &[u8]) {
            self.sent.borrow_mut().push(buf.to_vec());
        }
    }

    #[test]
    fn port_sets_domain_and_identity_on_event() {
        let identity = ClockIdentity::new(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let local_clock = LocalClock::new(
            FakeClock::default(),
            LocalClockDS::new(identity, ClockQuality::new(248, 0xFE, 0xFFFF)),
        );
        let (cap_port, sent) = CapturePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_number = 3u8;
        let port_number = PortNumber::new(5);

        let port = DomainPort::new(
            &local_clock,
            NoopBmca,
            cap_port,
            &timer_host,
            domain_number,
            port_number,
        );

        port.send_event(EventMessage::DelayReq(DelayRequestMessage::new(42.into())));
        let bufs = sent.borrow();
        assert!(!bufs.is_empty());
        let bytes = &bufs[0];
        assert_eq!(bytes[4], domain_number);
        assert_eq!(
            &bytes[20..30],
            &crate::port::PortIdentity::new(identity, port_number).to_bytes()
        );
    }

    #[test]
    fn port_sets_domain_and_identity_on_general() {
        let identity = ClockIdentity::new(&[8, 7, 6, 5, 4, 3, 2, 1]);
        let local_clock = LocalClock::new(
            FakeClock::default(),
            LocalClockDS::new(identity, ClockQuality::new(248, 0xFE, 0xFFFF)),
        );
        let (cap_port, sent) = CapturePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_number = 9u8;
        let port_number = PortNumber::new(2);

        let port = DomainPort::new(
            &local_clock,
            NoopBmca,
            cap_port,
            &timer_host,
            domain_number,
            port_number,
        );

        let follow = FollowUpMessage::new(7.into(), TimeStamp::new(1, 2));
        port.send_general(GeneralMessage::FollowUp(follow));
        let bufs = sent.borrow();
        assert!(!bufs.is_empty());
        let bytes = &bufs[0];
        assert_eq!(bytes[4], domain_number);
        assert_eq!(
            &bytes[20..30],
            &crate::port::PortIdentity::new(identity, port_number).to_bytes()
        );
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use crate::message::{EventMessage, GeneralMessage, SystemMessage};

    use super::Timeout;

    pub struct FakeTimeout {
        msg: RefCell<SystemMessage>,
        system_messages: Rc<RefCell<Vec<SystemMessage>>>,
    }

    impl FakeTimeout {
        pub fn new(msg: SystemMessage, system_messages: Rc<RefCell<Vec<SystemMessage>>>) -> Self {
            Self {
                msg: RefCell::new(msg),
                system_messages,
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

        fn restart_with(&self, msg: SystemMessage, _timeout: Duration) {
            self.system_messages.borrow_mut().push(msg);
            self.msg.replace(msg);
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
}
