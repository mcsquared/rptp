use std::rc::Rc;

use tokio::sync::mpsc;

use rptp::{
    bmca::IncrementalBmca,
    clock::{LocalClock, SynchronizableClock},
    infra::infra_support::SortedForeignClockRecordsVec,
    message::SystemMessage,
    port::{DomainNumber, DomainPort, PortIdentity, PortNumber},
    portstate::PortState,
    timestamping::TxTimestamping,
};

use crate::log::TracingPortLog;
use crate::net::NetworkSocket;
use crate::node::{TokioPhysicalPort, TokioTimerHost};

pub type TokioPort<'a, C, NS, TS> = PortState<
    DomainPort<'a, C, TokioPhysicalPort<NS>, TokioTimerHost, TS>,
    IncrementalBmca<SortedForeignClockRecordsVec>,
    TracingPortLog,
>;

pub struct OrdinaryTokioClock<'a, C: SynchronizableClock> {
    ordinary_clock: rptp::ordinary::OrdinaryClock<'a, C>,
}

impl<'a, C: SynchronizableClock> OrdinaryTokioClock<'a, C> {
    pub fn new(
        local_clock: &'a LocalClock<C>,
        domain_number: DomainNumber,
        port_number: PortNumber,
    ) -> Self {
        OrdinaryTokioClock {
            ordinary_clock: rptp::ordinary::OrdinaryClock::new(
                local_clock,
                domain_number,
                port_number,
            ),
        }
    }

    pub fn domain_number(&self) -> DomainNumber {
        self.ordinary_clock.domain_number()
    }

    pub fn port_number(&self) -> PortNumber {
        self.ordinary_clock.port_number()
    }

    pub fn port<N, T>(
        &self,
        event_socket: Rc<N>,
        general_socket: Rc<N>,
        system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
        timestamping: T,
    ) -> TokioPort<'a, C, N, T>
    where
        N: NetworkSocket,
        T: TxTimestamping,
    {
        self.ordinary_clock.port(
            TokioPhysicalPort::new(event_socket.clone(), general_socket.clone()),
            TokioTimerHost::new(self.ordinary_clock.domain_number(), system_tx.clone()),
            timestamping,
            SortedForeignClockRecordsVec::new(),
            TracingPortLog::new(PortIdentity::new(
                *self.ordinary_clock.local_clock().identity(),
                self.ordinary_clock.port_number(),
            )),
        )
    }
}
