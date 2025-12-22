use crate::bmca::{IncrementalBmca, SortedForeignClockRecords};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::log::PortLog;
use crate::port::{DomainNumber, DomainPort, PhysicalPort, PortNumber, TimerHost};
use crate::portstate::{PortProfile, PortState};
use crate::timestamping::TxTimestamping;

pub struct OrdinaryClock<'a, C: SynchronizableClock> {
    local_clock: &'a LocalClock<C>,
    domain_number: DomainNumber,
    port_number: PortNumber,
}

impl<'a, C: SynchronizableClock> OrdinaryClock<'a, C> {
    pub fn new(
        local_clock: &'a LocalClock<C>,
        domain_number: DomainNumber,
        port_number: PortNumber,
    ) -> Self {
        OrdinaryClock {
            local_clock,
            domain_number,
            port_number,
        }
    }

    pub fn local_clock(&self) -> &'a LocalClock<C> {
        self.local_clock
    }

    pub fn domain_number(&self) -> DomainNumber {
        self.domain_number
    }

    pub fn port_number(&self) -> PortNumber {
        self.port_number
    }

    pub fn port<T, TS, S, L>(
        &self,
        physical_port: &'a dyn PhysicalPort,
        timer_host: T,
        timestamping: TS,
        sorted_foreign_clock_records: S,
        log: L,
    ) -> PortState<DomainPort<'a, C, T, TS>, IncrementalBmca<S>, L>
    where
        T: TimerHost,
        TS: TxTimestamping,
        S: SortedForeignClockRecords,
        L: PortLog,
    {
        let domain_port = DomainPort::new(
            self.local_clock,
            physical_port,
            timer_host,
            timestamping,
            self.domain_number,
            self.port_number,
        );

        let bmca = IncrementalBmca::new(sorted_foreign_clock_records);

        PortProfile::default().initializing(domain_port, bmca, log)
    }
}
