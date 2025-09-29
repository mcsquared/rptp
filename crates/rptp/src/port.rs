use crate::message::{EventMessage, GeneralMessage, SystemMessage};

pub trait PortIo {
    fn send_event(&self, msg: EventMessage);
    fn send_general(&self, msg: GeneralMessage);
    fn schedule(&self, msg: SystemMessage, delay: std::time::Duration);
}
