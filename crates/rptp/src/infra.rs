pub mod infra_support {
    use std::rc::Rc;

    use crate::bmca::{
        ForeignClockRecord, ForeignClockResult, ForeignClockStatus, SortedForeignClockRecords,
    };
    use crate::clock::{Clock, FakeClock, LocalClock, SynchronizableClock};
    use crate::message::{EventMessage, GeneralMessage, SystemMessage};
    use crate::port::{Port, PortIdentity};
    use crate::time::TimeStamp;

    impl Clock for Rc<dyn SynchronizableClock> {
        fn now(&self) -> TimeStamp {
            self.as_ref().now()
        }
    }

    impl SynchronizableClock for Rc<dyn SynchronizableClock> {
        fn synchronize(&self, to: TimeStamp) {
            self.as_ref().synchronize(to);
        }
    }

    impl Clock for Rc<FakeClock> {
        fn now(&self) -> TimeStamp {
            self.as_ref().now()
        }
    }

    impl<P: Port> Port for Box<P> {
        type Clock = P::Clock;
        type PhysicalPort = P::PhysicalPort;
        type Timeout = P::Timeout;

        fn local_clock(&self) -> &LocalClock<Self::Clock> {
            self.as_ref().local_clock()
        }

        fn send_event(&self, msg: EventMessage) {
            self.as_ref().send_event(msg)
        }

        fn send_general(&self, msg: GeneralMessage) {
            self.as_ref().send_general(msg)
        }

        fn timeout(&self, msg: SystemMessage, delay: std::time::Duration) -> Self::Timeout {
            self.as_ref().timeout(msg, delay)
        }
    }

    impl SynchronizableClock for Rc<FakeClock> {
        fn synchronize(&self, to: TimeStamp) {
            self.as_ref().synchronize(to);
        }
    }

    pub struct SortedForeignClockRecordsVec {
        records: Vec<ForeignClockRecord>,
    }

    impl SortedForeignClockRecordsVec {
        pub fn new() -> Self {
            Self {
                records: Vec::new(),
            }
        }

        pub fn from_records(records: &[ForeignClockRecord]) -> Self {
            let mut vec = Self {
                records: records.to_vec(),
            };
            vec.sort_records();
            vec
        }

        fn sort_records(&mut self) {
            self.records.sort();
        }
    }

    impl SortedForeignClockRecords for SortedForeignClockRecordsVec {
        fn insert(&mut self, record: ForeignClockRecord) {
            self.records.push(record);
            self.sort_records();
        }

        fn update_record<F>(
            &mut self,
            source_port_identity: &PortIdentity,
            update: F,
        ) -> ForeignClockResult
        where
            F: FnOnce(&mut ForeignClockRecord) -> ForeignClockStatus,
        {
            if let Some(record) = self
                .records
                .iter_mut()
                .find(|r| r.same_source_as(source_port_identity))
            {
                let status = update(record);
                if let ForeignClockStatus::Updated = status {
                    self.sort_records();
                }
                ForeignClockResult::Status(status)
            } else {
                ForeignClockResult::NotFound
            }
        }

        fn first(&self) -> Option<&ForeignClockRecord> {
            self.records.first()
        }
    }

    impl SortedForeignClockRecords for Box<SortedForeignClockRecordsVec> {
        fn insert(&mut self, record: ForeignClockRecord) {
            self.as_mut().insert(record);
        }

        fn update_record<F>(
            &mut self,
            source_port_identity: &PortIdentity,
            update: F,
        ) -> ForeignClockResult
        where
            F: FnOnce(&mut ForeignClockRecord) -> ForeignClockStatus,
        {
            self.as_mut().update_record(source_port_identity, update)
        }

        fn first(&self) -> Option<&ForeignClockRecord> {
            self.as_ref().first()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::bmca::ForeignClockDS;
        use crate::clock::ClockIdentity;
        use crate::port::{PortIdentity, PortNumber};

        #[test]
        fn sorted_foreign_vec_maintains_best_record_first() {
            let mut records = SortedForeignClockRecordsVec::new();

            let high_clock = ForeignClockDS::high_grade_test_clock();
            let mid_clock = ForeignClockDS::mid_grade_test_clock();
            let low_clock = ForeignClockDS::low_grade_test_clock();

            let high_port_id = PortIdentity::new(
                ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, 1]),
                PortNumber::new(1),
            );
            let mid_port_id = PortIdentity::new(
                ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, 2]),
                PortNumber::new(1),
            );
            let low_port_id = PortIdentity::new(
                ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, 3]),
                PortNumber::new(1),
            );

            records.insert(ForeignClockRecord::new(high_port_id, high_clock));
            records.insert(ForeignClockRecord::new(low_port_id, low_clock));
            records.insert(ForeignClockRecord::new(mid_port_id, mid_clock));

            records.update_record(&high_port_id, |record| record.consider(high_clock));
            records.update_record(&low_port_id, |record| record.consider(low_clock));
            records.update_record(&mid_port_id, |record| record.consider(mid_clock));

            let best_clock = records.first().and_then(|record| record.dataset());
            assert_eq!(best_clock, Some(&high_clock));
        }
    }
}
