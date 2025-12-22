use tokio::sync::mpsc;

use rptp::{
    bmca::IncrementalBmca,
    clock::{LocalClock, SynchronizableClock},
    infra::infra_support::SortedForeignClockRecordsVec,
    message::SystemMessage,
    ordinary::OrdinaryClock,
    port::{DomainNumber, DomainPort, PortIdentity, PortNumber},
    portstate::PortState,
    timestamping::TxTimestamping,
};

use crate::log::TracingPortLog;
use crate::net::NetworkSocket;
use crate::node::{TokioPhysicalPort, TokioTimerHost};

pub type TokioPort<'a, C, TS> = PortState<
    DomainPort<'a, C, TokioTimerHost, TS, TracingPortLog>,
    IncrementalBmca<SortedForeignClockRecordsVec>,
>;

pub struct OrdinaryTokioClock<C: SynchronizableClock> {
    ordinary_clock: OrdinaryClock<C>,
}

impl<C: SynchronizableClock> OrdinaryTokioClock<C> {
    pub fn new(
        local_clock: LocalClock<C>,
        domain_number: DomainNumber,
        port_number: PortNumber,
    ) -> Self {
        OrdinaryTokioClock {
            ordinary_clock: OrdinaryClock::new(local_clock, domain_number, port_number),
        }
    }

    pub fn domain_number(&self) -> DomainNumber {
        self.ordinary_clock.domain_number()
    }

    pub fn port_number(&self) -> PortNumber {
        self.ordinary_clock.port_number()
    }

    pub fn port<'a, N, T>(
        &'a self,
        physical_port: &'a TokioPhysicalPort<N>,
        system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
        timestamping: T,
    ) -> TokioPort<'a, C, T>
    where
        N: NetworkSocket,
        T: TxTimestamping,
    {
        self.ordinary_clock.port(
            physical_port,
            TokioTimerHost::new(self.ordinary_clock.domain_number(), system_tx.clone()),
            timestamping,
            TracingPortLog::new(PortIdentity::new(
                *self.ordinary_clock.local_clock().identity(),
                self.ordinary_clock.port_number(),
            )),
            SortedForeignClockRecordsVec::new(),
        )
    }
}
