use crate::message::EventMessage;

pub trait TxTimestamping {
    fn stamp_egress(&self, msg: EventMessage);
}

impl<Tx: TxTimestamping> TxTimestamping for &Tx {
    fn stamp_egress(&self, msg: EventMessage) {
        (*self).stamp_egress(msg)
    }
}
