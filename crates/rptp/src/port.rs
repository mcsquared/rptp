use crate::bmca::Bmca;
use crate::clock::{LocalClock, SynchronizableClock};
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

pub struct DomainPort<'a, C: SynchronizableClock, B: Bmca, P: PhysicalPort, T: TimerHost> {
    local_clock: &'a LocalClock<C>,
    bmca: B,
    physical_port: P,
    timer_host: T,
    domain_number: u8,
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
    ) -> Self {
        Self {
            local_clock,
            bmca,
            physical_port,
            timer_host,
            domain_number,
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
        let mut buf = msg.to_wire();
        buf[4] = self.domain_number;
        self.physical_port.send_event(&buf);
    }

    fn send_general(&self, msg: GeneralMessage) {
        let mut buf = msg.to_wire();
        buf[4] = self.domain_number;
        self.physical_port.send_general(&buf);
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
    fn process_event_message(&mut self, msg: EventMessage, timestamp: TimeStamp);
    fn process_general_message(&mut self, msg: GeneralMessage);
    fn process_system_message(&mut self, msg: SystemMessage);
}

impl<P: Port> PortIngress for Option<PortState<P>> {
    fn process_event_message(&mut self, msg: EventMessage, timestamp: TimeStamp) {
        *self = self
            .take()
            .and_then(|state| Some(state.process_event_message(msg, timestamp)));
    }

    fn process_general_message(&mut self, msg: GeneralMessage) {
        *self = self
            .take()
            .and_then(|state| Some(state.process_general_message(msg)));
    }

    fn process_system_message(&mut self, msg: SystemMessage) {
        *self = self
            .take()
            .and_then(|state| Some(state.process_system_message(msg)));
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
