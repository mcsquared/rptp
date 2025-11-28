use crate::message::EventMessage;

pub trait TxTimestamping {
    fn stamp_egress(&self, msg: EventMessage);
}
