use crate::node::{EventInterface, GeneralInterface, SystemInterface};

pub trait PortIo {
    type Event: EventInterface;
    type General: GeneralInterface;
    type System: SystemInterface;

    fn event(&self) -> &Self::Event;
    fn general(&self) -> &Self::General;
    fn system(&self) -> &Self::System;
}
