//! `std`-based infrastructure helpers.
//!
//! This module is enabled behind the crate feature `std` and provides small adapter implementations
//! that make it easier to wire the `rptp` domain core into `std` environments (Tokio daemons, test
//! harnesses, demos).
//!
//! The intent is convenience, not a “one true runtime layer”: infrastructure is expected to supply
//! its own concrete implementations for networking, timers, and timestamping. These helpers exist
//! mainly to avoid repetitive glue for common `std` collection and pointer types.
//!
//! For allocator-free environments, see the `heapless-storage` feature and `crate::heapless`.

#[cfg(feature = "std")]
pub mod infra_support {
    //! Convenience adapters used by `std` integrations and tests.
    //!
    //! This submodule intentionally contains “glue code” only:
    //! - adapters for common pointer/container types (`Rc`, `Box`, `&mut _`),
    //! - and a simple `Vec`-backed [`ForeignClockRecords`] implementation for BMCA.

    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::bmca::{
        BestForeignSnapshot, ForeignClockRecord, ForeignClockRecords, ForeignClockStatus,
        ForeignGrandMasterCandidates,
    };
    use crate::clock::{Clock, LocalClock, SynchronizableClock, TimeScale};
    use crate::log::PortEvent;
    use crate::message::{EventMessage, GeneralMessage, SystemMessage};
    use crate::port::{Port, PortNumber, SendResult};
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

    /// `Vec`-backed implementation of [`ForeignClockRecords`].
    ///
    /// This is the default “easy mode” storage adapter for `std` environments and is used heavily
    /// in tests. Records are kept sorted so that “best first” holds (see [`ForeignClockRecord`]'s
    /// ordering).
    ///
    /// For fixed-capacity / `no_std` environments, see [`crate::heapless::HeaplessForeignClockRecords`].
    pub struct ForeignClockRecordsVec {
        records: Vec<ForeignClockRecord>,
    }

    impl Default for ForeignClockRecordsVec {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ForeignClockRecordsVec {
        /// Create an empty store.
        pub fn new() -> Self {
            Self {
                records: Vec::new(),
            }
        }

        /// Create a store from pre-seeded records (tests/support only).
        ///
        /// The resulting store is sorted according to the record ordering so that “best first”
        /// holds.
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
        /// Return the number of currently stored records (tests only).
        pub(crate) fn len(&self) -> usize {
            self.records.len()
        }

        fn sort_records(&mut self) {
            self.records.sort();
        }
    }

    impl ForeignClockRecords for ForeignClockRecordsVec {
        /// Remember (insert or update) a record and keep the internal list sorted.
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

        /// Return the best qualified record, if any.
        fn best_qualified(&self) -> Option<&ForeignClockRecord> {
            self.records
                .first()
                .filter(|record| record.qualified_ds().is_some())
        }

        /// Prune stale records and report whether any were removed.
        fn prune_stale(&mut self, now: crate::time::Instant) -> bool {
            let before = self.records.len();
            self.records.retain(|record| !record.is_stale(now));
            self.records.len() != before
        }
    }

    /// Blanket impl to allow passing `&mut S` where an owned `S: ForeignClockRecords` is expected.
    impl<S: ForeignClockRecords> ForeignClockRecords for &mut S {
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

    /// Multi-port implementation of [`ForeignGrandMasterCandidates`] for boundary clock scenarios.
    ///
    /// This implementation tracks per-port foreign candidate snapshots and aggregates the best
    /// qualified candidate across all ports for BMCA decision making. It supports boundary clocks
    /// where multiple ports may observe different foreign masters.
    pub struct MultiPortForeignCandidates {
        by_port: RefCell<BTreeMap<PortNumber, BestForeignSnapshot>>,
    }

    impl MultiPortForeignCandidates {
        /// Create a new multi-port foreign candidates store.
        pub fn new() -> Self {
            Self {
                by_port: RefCell::new(BTreeMap::new()),
            }
        }
    }

    impl Default for MultiPortForeignCandidates {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ForeignGrandMasterCandidates for MultiPortForeignCandidates {
        fn remember(&self, port: PortNumber, snapshot: BestForeignSnapshot) {
            match snapshot {
                BestForeignSnapshot::Qualified { .. } => {
                    // Port reports qualified candidate - store it
                    self.by_port.borrow_mut().insert(port, snapshot);
                }
                BestForeignSnapshot::Empty => {
                    // Port explicitly reports empty - remove its entry if it had one
                    // This allows the store to track when a port's candidate goes stale
                    self.by_port.borrow_mut().remove(&port);
                }
            }
        }

        fn best(&self) -> BestForeignSnapshot {
            let by_port = self.by_port.borrow();
            let mut best_snapshot = BestForeignSnapshot::Empty;

            for snapshot in by_port.values().copied() {
                let better = match (snapshot, best_snapshot) {
                    (
                        BestForeignSnapshot::Qualified { ds: ds1, .. },
                        BestForeignSnapshot::Qualified { ds: ds2, .. },
                    ) => ds1.better_than(&ds2),
                    (BestForeignSnapshot::Qualified { .. }, BestForeignSnapshot::Empty) => true,
                    (BestForeignSnapshot::Empty, BestForeignSnapshot::Qualified { .. }) => false,
                    (BestForeignSnapshot::Empty, BestForeignSnapshot::Empty) => false,
                };

                if better {
                    best_snapshot = snapshot;
                }
            }

            best_snapshot
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::bmca::{BestForeignSnapshot, ForeignGrandMasterCandidates};
        use crate::clock::ClockIdentity;
        use crate::port::{PortIdentity, PortNumber};
        use crate::test_support::TestClockDS;
        use crate::time::{Instant, LogInterval};

        #[test]
        fn foreign_vec_maintains_best_record_first() {
            let mut records = ForeignClockRecordsVec::new();

            let high_clock = TestClockDS::default_high_grade().dataset();
            let mid_clock = TestClockDS::default_mid_grade().dataset();
            let low_clock = TestClockDS::default_low_grade_slave_only().dataset();

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
        fn foreign_vec_prune_stale_returns_true_when_records_removed() {
            let high_clock = TestClockDS::default_high_grade().dataset();
            let high_port_id = PortIdentity::new(
                ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, 9]),
                PortNumber::new(1),
            );

            let mut records = ForeignClockRecordsVec::new();
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
        fn foreign_vec_prune_stale_returns_false_when_no_records_are_stale() {
            let high_clock = TestClockDS::default_high_grade().dataset();
            let high_port_id = PortIdentity::new(
                ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, 10]),
                PortNumber::new(1),
            );

            let mut records = ForeignClockRecordsVec::new();
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

        #[test]
        fn multi_port_foreign_candidates_stores_qualified_per_port() {
            let candidates = MultiPortForeignCandidates::new();
            let port1 = PortNumber::new(1);
            let port2 = PortNumber::new(2);

            let high = TestClockDS::default_high_grade();
            let mid = TestClockDS::default_mid_grade();
            let high_ds = high.dataset();
            let mid_ds = mid.dataset();

            let high_port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
            let mid_port_id = PortIdentity::new(mid.clock_identity(), PortNumber::new(1));

            // Port 1 reports high-grade candidate
            candidates.remember(
                port1,
                BestForeignSnapshot::Qualified {
                    ds: high_ds,
                    source_port_identity: high_port_id,
                    received_on_port: port1,
                },
            );

            // Port 2 reports mid-grade candidate
            candidates.remember(
                port2,
                BestForeignSnapshot::Qualified {
                    ds: mid_ds,
                    source_port_identity: mid_port_id,
                    received_on_port: port2,
                },
            );

            // Best should be the high-grade candidate from port 1
            let best = candidates.best();
            match best {
                BestForeignSnapshot::Qualified {
                    ds,
                    received_on_port,
                    ..
                } => {
                    assert_eq!(ds, high_ds);
                    assert_eq!(received_on_port, port1);
                }
                BestForeignSnapshot::Empty => panic!("expected qualified candidate"),
            }
        }

        #[test]
        fn multi_port_foreign_candidates_removes_port_on_empty() {
            let candidates = MultiPortForeignCandidates::new();
            let port1 = PortNumber::new(1);
            let port2 = PortNumber::new(2);

            let high = TestClockDS::default_high_grade();
            let mid = TestClockDS::default_mid_grade();
            let high_ds = high.dataset();
            let mid_ds = mid.dataset();

            let high_port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
            let mid_port_id = PortIdentity::new(mid.clock_identity(), PortNumber::new(1));

            // Both ports report candidates
            candidates.remember(
                port1,
                BestForeignSnapshot::Qualified {
                    ds: high_ds,
                    source_port_identity: high_port_id,
                    received_on_port: port1,
                },
            );
            candidates.remember(
                port2,
                BestForeignSnapshot::Qualified {
                    ds: mid_ds,
                    source_port_identity: mid_port_id,
                    received_on_port: port2,
                },
            );

            // Port 1 reports empty (candidate went stale)
            candidates.remember(port1, BestForeignSnapshot::Empty);

            // Best should now be the mid-grade candidate from port 2
            let best = candidates.best();
            match best {
                BestForeignSnapshot::Qualified {
                    ds,
                    received_on_port,
                    ..
                } => {
                    assert_eq!(ds, mid_ds);
                    assert_eq!(received_on_port, port2);
                }
                BestForeignSnapshot::Empty => panic!("expected qualified candidate from port 2"),
            }
        }

        #[test]
        fn multi_port_foreign_candidates_returns_empty_when_no_qualified() {
            let candidates = MultiPortForeignCandidates::new();
            let port1 = PortNumber::new(1);

            // Port reports empty
            candidates.remember(port1, BestForeignSnapshot::Empty);

            // Best should be empty
            assert_eq!(candidates.best(), BestForeignSnapshot::Empty);
        }

        #[test]
        fn multi_port_foreign_candidates_returns_empty_when_all_ports_empty() {
            let candidates = MultiPortForeignCandidates::new();
            let port1 = PortNumber::new(1);
            let port2 = PortNumber::new(2);

            let high = TestClockDS::default_high_grade();
            let high_ds = high.dataset();
            let high_port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));

            // Port 1 reports qualified
            candidates.remember(
                port1,
                BestForeignSnapshot::Qualified {
                    ds: high_ds,
                    source_port_identity: high_port_id,
                    received_on_port: port1,
                },
            );

            // Both ports report empty
            candidates.remember(port1, BestForeignSnapshot::Empty);
            candidates.remember(port2, BestForeignSnapshot::Empty);

            // Best should be empty
            assert_eq!(candidates.best(), BestForeignSnapshot::Empty);
        }

        #[test]
        fn multi_port_foreign_candidates_selects_best_across_ports() {
            let candidates = MultiPortForeignCandidates::new();
            let port1 = PortNumber::new(1);
            let port2 = PortNumber::new(2);
            let port3 = PortNumber::new(3);

            let high = TestClockDS::default_high_grade();
            let mid = TestClockDS::default_mid_grade();
            let low = TestClockDS::default_low_grade_slave_only();
            let high_ds = high.dataset();
            let mid_ds = mid.dataset();
            let low_ds = low.dataset();

            let high_port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
            let mid_port_id = PortIdentity::new(mid.clock_identity(), PortNumber::new(1));
            let low_port_id = PortIdentity::new(low.clock_identity(), PortNumber::new(1));

            // Ports report in non-optimal order
            candidates.remember(
                port2,
                BestForeignSnapshot::Qualified {
                    ds: mid_ds,
                    source_port_identity: mid_port_id,
                    received_on_port: port2,
                },
            );
            candidates.remember(
                port3,
                BestForeignSnapshot::Qualified {
                    ds: low_ds,
                    source_port_identity: low_port_id,
                    received_on_port: port3,
                },
            );
            candidates.remember(
                port1,
                BestForeignSnapshot::Qualified {
                    ds: high_ds,
                    source_port_identity: high_port_id,
                    received_on_port: port1,
                },
            );

            // Best should be the high-grade candidate from port 1
            let best = candidates.best();
            match best {
                BestForeignSnapshot::Qualified {
                    ds,
                    received_on_port,
                    ..
                } => {
                    assert_eq!(ds, high_ds);
                    assert_eq!(received_on_port, port1);
                }
                BestForeignSnapshot::Empty => panic!("expected qualified candidate"),
            }
        }
    }
}
