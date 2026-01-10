use core::cell::Cell;

use crate::bmca::{
    BestForeignSnapshot, BestMasterClockAlgorithm, ClockDS, SortedForeignClockRecords,
};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::log::PortLog;
use crate::port::{DomainNumber, DomainPort, PhysicalPort, PortNumber, TimerHost};
use crate::portstate::PortState;
use crate::profile::PortProfile;
use crate::timestamping::TxTimestamping;

pub struct OrdinaryClock<C: SynchronizableClock> {
    local_clock: LocalClock<C>,
    default_ds: ClockDS,
    domain_number: DomainNumber,
    port_number: PortNumber,
    foreign_candidates: Cell<BestForeignSnapshot>,
}

impl<C: SynchronizableClock> OrdinaryClock<C> {
    pub fn new(
        local_clock: LocalClock<C>,
        default_ds: ClockDS,
        domain_number: DomainNumber,
        port_number: PortNumber,
    ) -> Self {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);

        OrdinaryClock {
            local_clock,
            default_ds,
            domain_number,
            port_number,
            foreign_candidates,
        }
    }

    pub fn local_clock(&self) -> &LocalClock<C> {
        &self.local_clock
    }

    pub fn domain_number(&self) -> DomainNumber {
        self.domain_number
    }

    pub fn port_number(&self) -> PortNumber {
        self.port_number
    }

    pub fn port<'a, T, TS, S, L>(
        &'a self,
        physical_port: &'a dyn PhysicalPort,
        timer_host: T,
        timestamping: TS,
        log: L,
        sorted_foreign_clock_records: S,
    ) -> PortState<'a, DomainPort<'a, C, T, TS, L>, S>
    where
        T: TimerHost,
        TS: TxTimestamping,
        S: SortedForeignClockRecords,
        L: PortLog,
    {
        let domain_port = DomainPort::new(
            &self.local_clock,
            physical_port,
            timer_host,
            timestamping,
            log,
            self.domain_number,
            self.port_number,
        );

        let bmca = BestMasterClockAlgorithm::new(
            &self.default_ds,
            &self.foreign_candidates,
            self.port_number,
        );

        PortProfile::default().initializing(domain_port, bmca, sorted_foreign_clock_records)
    }
}
