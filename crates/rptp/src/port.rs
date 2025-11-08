use crate::bmca::Bmca;
use crate::clock::SynchronizableClock;
use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::portstate::PortState;
use crate::result::{ProtocolError, Result};
use crate::time::TimeStamp;

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

pub trait PhysicalPort {
    type Timeout: Timeout;

    fn send_event(&self, msg: EventMessage);
    fn send_general(&self, msg: GeneralMessage);
    fn timeout(&self, msg: SystemMessage, delay: std::time::Duration) -> Self::Timeout;
}

impl<P: PhysicalPort> PhysicalPort for Box<P> {
    type Timeout = P::Timeout;

    fn send_event(&self, msg: EventMessage) {
        self.as_ref().send_event(msg)
    }

    fn send_general(&self, msg: GeneralMessage) {
        self.as_ref().send_general(msg)
    }

    fn timeout(&self, msg: SystemMessage, delay: std::time::Duration) -> Self::Timeout {
        self.as_ref().timeout(msg, delay)
    }
}

pub trait PortMap {
    fn port_by_domain(&mut self, domain_number: u8) -> Result<&mut dyn Port>;
}

pub struct DomainZeroOnlyPortMap<'a, C: SynchronizableClock, P: PhysicalPort, B: Bmca> {
    pub port_state: Option<PortState<'a, C, P, B>>,
}

impl<'a, C: SynchronizableClock, P: PhysicalPort, B: Bmca> DomainZeroOnlyPortMap<'a, C, P, B> {
    pub fn new(port_state: PortState<'a, C, P, B>) -> Self {
        Self {
            port_state: Some(port_state),
        }
    }
}

impl<'a, C: SynchronizableClock, P: PhysicalPort, B: Bmca> PortMap
    for DomainZeroOnlyPortMap<'a, C, P, B>
{
    fn port_by_domain(&mut self, domain_number: u8) -> Result<&mut dyn Port> {
        if domain_number == 0 {
            Ok(&mut self.port_state)
        } else {
            Err(ProtocolError::DomainNotFound.into())
        }
    }
}

pub trait Port {
    fn process_event_message(&mut self, msg: EventMessage, timestamp: TimeStamp);
    fn process_general_message(&mut self, msg: GeneralMessage);
    fn process_system_message(&mut self, msg: SystemMessage);
}

impl<'a, C: SynchronizableClock, P: PhysicalPort, B: Bmca> Port for Option<PortState<'a, C, P, B>> {
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

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::Duration;

    use crate::message::{EventMessage, GeneralMessage, SystemMessage};

    use super::Timeout;

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

    pub struct FakePort {
        event_messages: Rc<RefCell<Vec<EventMessage>>>,
        general_messages: Rc<RefCell<Vec<GeneralMessage>>>,
        system_messages: Rc<RefCell<Vec<SystemMessage>>>,
    }

    impl FakePort {
        pub fn new() -> Self {
            Self {
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

    impl PhysicalPort for FakePort {
        type Timeout = Rc<FakeTimeout>;

        fn send_event(&self, msg: EventMessage) {
            self.event_messages.borrow_mut().push(msg);
        }

        fn send_general(&self, msg: GeneralMessage) {
            self.general_messages.borrow_mut().push(msg);
        }

        fn timeout(&self, msg: SystemMessage, _delay: Duration) -> Self::Timeout {
            self.system_messages.borrow_mut().push(msg);
            Rc::new(FakeTimeout::new(msg, Rc::clone(&self.system_messages)))
        }
    }

    impl PhysicalPort for &FakePort {
        type Timeout = Rc<FakeTimeout>;

        fn send_event(&self, msg: EventMessage) {
            self.event_messages.borrow_mut().push(msg);
        }

        fn send_general(&self, msg: GeneralMessage) {
            self.general_messages.borrow_mut().push(msg);
        }

        fn timeout(&self, msg: SystemMessage, _delay: Duration) -> Self::Timeout {
            self.system_messages.borrow_mut().push(msg);
            Rc::new(FakeTimeout::new(msg, Rc::clone(&self.system_messages)))
        }
    }
}
