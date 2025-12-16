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

pub struct ClockTxTimestamping<C: Clock> {
    clock: C,
    system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
    domain: DomainNumber,
}

impl<C: Clock> ClockTxTimestamping<C> {
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

impl<C: Clock> TxTimestamping for ClockTxTimestamping<C> {
    fn stamp_egress(&self, msg: EventMessage) {
        let system_msg = SystemMessage::Timestamp(TimestampMessage::new(msg, self.clock.now()));
        let _ = self.system_tx.send((self.domain, system_msg));
    }
}

pub struct ClockRxTimestamping<C: Clock> {
    clock: C,
}

impl<C: Clock> ClockRxTimestamping<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }
}

impl<C: Clock> RxTimestamping for ClockRxTimestamping<C> {
    fn ingress_stamp(&self) -> TimeStamp {
        self.clock.now()
    }
}
