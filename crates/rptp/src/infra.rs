pub mod infra_support {
    use std::rc::Rc;

    use crate::bmca::{ForeignClockRecord, SortedForeignClockRecords};
    use crate::clock::{Clock, FakeClock, SynchronizableClock};
    use crate::port::PortIdentity;
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

        fn update_record<F>(&mut self, source_port_identity: &PortIdentity, update: F) -> bool
        where
            F: FnOnce(&mut ForeignClockRecord),
        {
            if let Some(record) = self
                .records
                .iter_mut()
                .find(|r| r.same_source_as(source_port_identity))
            {
                update(record);
                self.sort_records();
                true
            } else {
                false
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

        fn update_record<F>(&mut self, source_port_identity: &PortIdentity, update: F) -> bool
        where
            F: FnOnce(&mut ForeignClockRecord),
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
        use crate::message::AnnounceMessage;
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

            records.insert(ForeignClockRecord::new(
                high_port_id,
                AnnounceMessage::new(0.into(), high_clock),
            ));
            records.insert(ForeignClockRecord::new(
                low_port_id,
                AnnounceMessage::new(0.into(), low_clock),
            ));
            records.insert(ForeignClockRecord::new(
                mid_port_id,
                AnnounceMessage::new(0.into(), mid_clock),
            ));

            records.update_record(&high_port_id, |record| {
                record.consider(AnnounceMessage::new(1.into(), high_clock));
            });
            records.update_record(&low_port_id, |record| {
                record.consider(AnnounceMessage::new(1.into(), low_clock));
            });
            records.update_record(&mid_port_id, |record| {
                record.consider(AnnounceMessage::new(1.into(), mid_clock));
            });

            let best_clock = records.first().and_then(|record| record.clock());
            assert_eq!(best_clock, Some(&high_clock));
        }
    }
}
