use std::rc::Rc;

use tokio::sync::mpsc;

use rptp::bmca::FullBmca;
use rptp::clock::{LocalClock, SynchronizableClock};
use rptp::infra::infra_support::SortedForeignClockRecordsVec;
use rptp::message::SystemMessage;
use rptp::port::{DomainPort, PortNumber};
use rptp::portstate::PortState;

use crate::net::NetworkSocket;
use crate::node::{TokioPhysicalPort, TokioTimerHost};

pub fn ordinary_clock_port<'a, C, N>(
    local_clock: &'a LocalClock<C>,
    domain_number: u8,
    event_socket: Rc<N>,
    general_socket: Rc<N>,
    system_tx: mpsc::UnboundedSender<(u8, SystemMessage)>,
    port_number: PortNumber,
) -> PortState<
    DomainPort<
        'a,
        C,
        FullBmca<SortedForeignClockRecordsVec>,
        TokioPhysicalPort<'a, C, N>,
        TokioTimerHost,
    >,
>
where
    C: SynchronizableClock,
    N: NetworkSocket,
{
    let physical_port = TokioPhysicalPort::new(
        local_clock,
        domain_number,
        event_socket,
        general_socket,
        system_tx.clone(),
    );

    let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
    let timer_host = TokioTimerHost::new(domain_number, system_tx);

    let domain_port = DomainPort::new(
        local_clock,
        bmca,
        physical_port,
        timer_host,
        domain_number,
        port_number,
    );

    PortState::initializing(domain_port)
}
