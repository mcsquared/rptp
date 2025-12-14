use std::rc::Rc;

use tokio::sync::mpsc;

use rptp::{
    clock::Clock,
    message::{EventMessage, SystemMessage, TimestampMessage},
    port::DomainNumber,
    time::TimeStamp,
    timestamping::TxTimestamping,
};

pub trait RxTimestamping {
    fn ingress_stamp(&self) -> TimeStamp;
}

pub struct ClockTimestamping<C: Clock> {
    clock: C,
    system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
    domain: DomainNumber,
}

impl<C: Clock> ClockTimestamping<C> {
    pub fn new(
        clock: C,
        system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
        domain: DomainNumber,
    ) -> Self {
        Self {
            clock,
            system_tx,
            domain,
        }
    }
}

impl<C: Clock> TxTimestamping for ClockTimestamping<C> {
    fn stamp_egress(&self, msg: EventMessage) {
        let system_msg = SystemMessage::Timestamp(TimestampMessage::new(msg, self.clock.now()));
        let _ = self.system_tx.send((self.domain, system_msg));
    }
}

impl<C: Clock> RxTimestamping for ClockTimestamping<C> {
    fn ingress_stamp(&self) -> TimeStamp {
        self.clock.now()
    }
}

impl<C: Clock> RxTimestamping for &ClockTimestamping<C> {
    fn ingress_stamp(&self) -> TimeStamp {
        (*self).ingress_stamp()
    }
}

impl<C: Clock> RxTimestamping for Rc<ClockTimestamping<C>> {
    fn ingress_stamp(&self) -> TimeStamp {
        (**self).ingress_stamp()
    }
}
