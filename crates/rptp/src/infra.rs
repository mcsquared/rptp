#[cfg(feature = "std")]
pub mod infra_support {
    use std::rc::Rc;

    use crate::bmca::{ForeignClockRecord, ForeignClockStatus, SortedForeignClockRecords};
    use crate::clock::{Clock, LocalClock, SynchronizableClock, TimeScale};
    use crate::log::PortEvent;
    use crate::message::{EventMessage, GeneralMessage, SystemMessage};
    use crate::port::{Port, SendResult};
    use crate::time::TimeStamp;

    impl Clock for Rc<dyn SynchronizableClock> {
        fn now(&self) -> TimeStamp {
            self.as_ref().now()
        }

        fn time_scale(&self) -> TimeScale {
            self.as_ref().time_scale()
        }
    }

    impl<P: Port> Port for Box<P> {
        type Clock = P::Clock;
        type Timeout = P::Timeout;

        fn local_clock(&self) -> &LocalClock<Self::Clock> {
            self.as_ref().local_clock()
        }

        fn send_event(&self, msg: EventMessage) -> SendResult {
            self.as_ref().send_event(msg)
        }

        fn send_general(&self, msg: GeneralMessage) -> SendResult {
            self.as_ref().send_general(msg)
        }

        fn timeout(&self, msg: SystemMessage) -> Self::Timeout {
            self.as_ref().timeout(msg)
        }

        fn log(&self, event: PortEvent) {
            self.as_ref().log(event)
        }
    }

    pub struct SortedForeignClockRecordsVec {
        records: Vec<ForeignClockRecord>,
    }

    impl Default for SortedForeignClockRecordsVec {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SortedForeignClockRecordsVec {
        pub fn new() -> Self {
            Self {
                records: Vec::new(),
            }
        }

        #[cfg(any(test, feature = "test-support"))]
        pub fn from_records(records: &[ForeignClockRecord]) -> Self {
            let mut vec = Self {
                records: records.to_vec(),
            };
            vec.sort_records();
            vec
        }

        #[cfg(test)]
        fn is_empty(&self) -> bool {
            self.records.is_empty()
        }

        #[cfg(test)]
        pub(crate) fn len(&self) -> usize {
            self.records.len()
        }

        fn sort_records(&mut self) {
            self.records.sort();
        }
    }

    impl SortedForeignClockRecords for SortedForeignClockRecordsVec {
        fn remember(&mut self, record: ForeignClockRecord) {
            if let Some(existing) = self
                .records
                .iter_mut()
                .find(|r| r.same_source_as(record.source_port_identity()))
            {
                if let ForeignClockStatus::Updated = existing.update_from(&record) {
                    self.sort_records();
                }
            } else {
                self.records.push(record);
                self.sort_records();
            }
        }

        fn best_qualified(&self) -> Option<&ForeignClockRecord> {
            self.records
                .first()
                .filter(|record| record.qualified_ds().is_some())
        }

        fn prune_stale(&mut self, now: crate::time::Instant) -> bool {
            let before = self.records.len();
            self.records.retain(|record| !record.is_stale(now));
            self.records.len() != before
        }
    }

    impl<S: SortedForeignClockRecords> SortedForeignClockRecords for &mut S {
        fn remember(&mut self, record: ForeignClockRecord) {
            (*self).remember(record);
        }

        fn best_qualified(&self) -> Option<&ForeignClockRecord> {
            (**self).best_qualified()
        }

        fn prune_stale(&mut self, now: crate::time::Instant) -> bool {
            (*self).prune_stale(now)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::clock::ClockIdentity;
        use crate::clock::StepsRemoved;
        use crate::port::{PortIdentity, PortNumber};
        use crate::test_support::TestClockCatalog;
        use crate::time::{Instant, LogInterval};

        #[test]
        fn sorted_foreign_vec_maintains_best_record_first() {
            let mut records = SortedForeignClockRecordsVec::new();

            let high_clock =
                TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
            let mid_clock = TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0));
            let low_clock =
                TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));

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

            records.remember(ForeignClockRecord::new(
                high_port_id,
                high_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ));
            records.remember(ForeignClockRecord::new(
                low_port_id,
                low_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ));
            records.remember(ForeignClockRecord::new(
                mid_port_id,
                mid_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ));

            // Qualify by remembering a second announce (threshold=2).
            records.remember(ForeignClockRecord::new(
                high_port_id,
                high_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ));
            records.remember(ForeignClockRecord::new(
                low_port_id,
                low_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ));
            records.remember(ForeignClockRecord::new(
                mid_port_id,
                mid_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ));

            let best_clock = records
                .best_qualified()
                .and_then(|record| record.qualified_ds());
            assert_eq!(best_clock, Some(&high_clock));
        }

        #[test]
        fn sorted_foreign_vec_prune_stale_returns_true_when_records_removed() {
            let high_clock =
                TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
            let high_port_id = PortIdentity::new(
                ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, 9]),
                PortNumber::new(1),
            );

            let mut records = SortedForeignClockRecordsVec::new();
            records.remember(ForeignClockRecord::new(
                high_port_id,
                high_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ));

            // With logInterval = 0, the foreign master time window is 4 seconds.
            let pruned = records.prune_stale(Instant::from_secs(10));

            assert!(pruned, "prune_stale should report removal of stale records");
            assert!(records.is_empty());
        }

        #[test]
        fn sorted_foreign_vec_prune_stale_returns_false_when_no_records_are_stale() {
            let high_clock =
                TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
            let high_port_id = PortIdentity::new(
                ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, 10]),
                PortNumber::new(1),
            );

            let mut records = SortedForeignClockRecordsVec::new();
            records.remember(ForeignClockRecord::new(
                high_port_id,
                high_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ));

            let pruned = records.prune_stale(Instant::from_secs(2));

            assert!(
                !pruned,
                "prune_stale should report no removals when nothing is stale"
            );
            assert_eq!(records.len(), 1);
        }
    }
}
