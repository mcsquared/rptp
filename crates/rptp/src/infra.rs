pub mod infra_support {
    use std::rc::Rc;

    use crate::bmca::{ForeignClockDS, ForeignClockRecord, SortedForeignClockRecords};
    use crate::clock::{Clock, FakeClock, SynchronizableClock};
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

        fn sort_records(&mut self) {
            self.records.sort();
        }
    }

    impl SortedForeignClockRecords for SortedForeignClockRecordsVec {
        fn insert(&mut self, record: ForeignClockRecord) {
            self.records.push(record);
            self.sort_records();
        }

        fn update_record<F>(&mut self, foreign: &ForeignClockDS, update: F) -> bool
        where
            F: FnOnce(&mut ForeignClockRecord),
        {
            if let Some(record) = self.records.iter_mut().find(|r| r.same_source_as(&foreign)) {
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

        fn update_record<F>(&mut self, foreign: &ForeignClockDS, update: F) -> bool
        where
            F: FnOnce(&mut ForeignClockRecord),
        {
            self.as_mut().update_record(foreign, update)
        }

        fn first(&self) -> Option<&ForeignClockRecord> {
            self.as_ref().first()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::bmca::ForeignClockDS;
        use crate::message::AnnounceMessage;

        #[test]
        fn sorted_foreign_vec_maintains_best_record_first() {
            let mut records = SortedForeignClockRecordsVec::new();

            let high_clock = ForeignClockDS::high_grade_test_clock();
            let mid_clock = ForeignClockDS::mid_grade_test_clock();
            let low_clock = ForeignClockDS::low_grade_test_clock();

            records.insert(ForeignClockRecord::new(AnnounceMessage::new(0, high_clock)));
            records.insert(ForeignClockRecord::new(AnnounceMessage::new(0, low_clock)));
            records.insert(ForeignClockRecord::new(AnnounceMessage::new(0, mid_clock)));

            records.update_record(&high_clock, |record| {
                record.consider(AnnounceMessage::new(1, high_clock));
            });
            records.update_record(&low_clock, |record| {
                record.consider(AnnounceMessage::new(1, low_clock));
            });
            records.update_record(&mid_clock, |record| {
                record.consider(AnnounceMessage::new(1, mid_clock));
            });

            let best_clock = records.first().and_then(|record| record.clock());
            assert_eq!(best_clock, Some(&high_clock));
        }
    }
}
