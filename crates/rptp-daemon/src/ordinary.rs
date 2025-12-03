use std::rc::Rc;

use tokio::sync::mpsc;

use rptp::bmca::IncrementalBmca;
use rptp::clock::{LocalClock, SynchronizableClock};
use rptp::infra::infra_support::SortedForeignClockRecordsVec;
use rptp::message::SystemMessage;
use rptp::port::{DomainNumber, DomainPort, PortIdentity, PortNumber, PortTimingPolicy};
use rptp::portstate::PortState;
use rptp::timestamping::TxTimestamping;

use crate::log::TracingPortLog;
use crate::net::NetworkSocket;
use crate::node::{TokioPhysicalPort, TokioTimerHost};

pub type OrdinaryBmca = IncrementalBmca<SortedForeignClockRecordsVec>;

pub type OrdinaryPort<'a, C, N, TS> = DomainPort<'a, C, TokioPhysicalPort<N>, TokioTimerHost, TS>;

pub type OrdinaryPortState<'a, C, N, TS> =
    PortState<Box<OrdinaryPort<'a, C, N, TS>>, OrdinaryBmca, TracingPortLog>;

pub type OrdinaryPortMap<'a, C, N, TS> =
    rptp::port::SingleDomainPortMap<Box<OrdinaryPort<'a, C, N, TS>>, OrdinaryBmca, TracingPortLog>;

pub fn ordinary_clock_port<'a, C, N, TS: TxTimestamping>(
    local_clock: &'a LocalClock<C>,
    domain_number: DomainNumber,
    event_socket: Rc<N>,
    general_socket: Rc<N>,
    system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
    port_number: PortNumber,
    timestamping: TS,
) -> OrdinaryPortState<'a, C, N, TS>
where
    C: SynchronizableClock,
    N: NetworkSocket,
{
    let physical_port = TokioPhysicalPort::new(event_socket, general_socket);

    let bmca = OrdinaryBmca::new(SortedForeignClockRecordsVec::new());
    let timer_host = TokioTimerHost::new(domain_number, system_tx);

    let domain_port = Box::new(DomainPort::new(
        local_clock,
        physical_port,
        timer_host,
        timestamping,
        domain_number,
        port_number,
    ));

    let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
    let log = TracingPortLog::new(port_identity);

    PortState::initializing(domain_port, bmca, log, PortTimingPolicy::default())
}
