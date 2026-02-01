//! Foreign master tracking and qualification.
//!
//! This module implements the storage and qualification side of the Best Master Clock Algorithm
//! (BMCA): retaining foreign clock candidates learned from Announce messages, qualifying them over
//! a sliding time window, and exposing the best qualified candidate for BMCA decision logic.
//!
//! - **Storage traits** – [`ForeignClockRecords`] (per-port store of foreign records) and
//!   [`ForeignGrandMasterCandidates`] (cross-port aggregation of best foreign snapshots, for
//!   boundary clocks). Implementations can be heap-based, heapless, or custom.
//! - **[`BestForeignSnapshot`]** – Represents `E_best` / `E_rbest` in the spec: either a qualified
//!   foreign [`ClockDS`] plus source port identity, or empty. Used when passing the best foreign
//!   candidate into BMCA and when comparing against the local dataset.
//! - **[`BestForeignRecord`]** – Per-port collection that feeds Announce-derived observations
//!   into a [`ForeignClockRecords`] store and produces a [`BestForeignSnapshot`].
//! - **[`ForeignClockRecord`]** – A single foreign clock candidate with sliding-window
//!   qualification state (qualified once enough Announces arrive within the time window; stale
//!   after silence).
//!
//! Dataset types ([`ClockDS`], etc.) live in [`crate::dataset`]. The BMCA algorithm and roles that
//! consume these snapshots and produce port state decisions live in [`crate::bmca`].

use core::cell::Cell;

use crate::clock::ClockIdentity;
use crate::dataset::ClockDS;
use crate::port::{ParentPortIdentity, PortIdentity, PortNumber};
use crate::time::{Duration, Instant, LogInterval};

/// Storage surface for foreign clock records.
///
/// The BMCA needs to retain foreign clock candidates, update them on subsequent Announce messages,
/// qualify them, and efficiently retrieve the best qualified candidate.
///
/// This is a trait to keep the core portable across `std` and `no_std` environments, and to allow
/// different storage strategies (heap, heapless, fixed arrays, etc.).
pub trait ForeignClockRecords {
    /// Remember (insert or update) a record.
    fn remember(&mut self, record: ForeignClockRecord);
    /// Return the best *qualified* record, if any.
    fn best_qualified<'a>(&'a self) -> Option<QualifiedForeignClockRecord<'a>>;
    /// Prune stale records and return `true` if any were removed.
    fn prune_stale(&mut self, now: Instant) -> bool;
}

/// Foreign grandmaster candidates storage surface to present the cross-port shared best
/// foreign dataset.
///
/// Infrastructure implementations own storage strategy, locking, etc.
pub trait ForeignGrandMasterCandidates {
    /// Remember or update what a port reports (qualified candidate or empty).
    ///
    /// `port` identifies which port is reporting. For `Empty` snapshots, this signals
    /// that the port has no qualified candidates and allows the store to update its
    /// per-port tracking accordingly.
    fn remember(&self, port: PortNumber, snapshot: BestForeignSnapshot);

    /// Return the best qualified foreign candidate across all ports.
    fn best(&self) -> BestForeignSnapshot;
}

// Simple in-memory implementation of ForeignGrandMasterCandidates for single-port use cases.
impl ForeignGrandMasterCandidates for Cell<BestForeignSnapshot> {
    fn remember(&self, port: PortNumber, snapshot: BestForeignSnapshot) {
        debug_assert!(
            port == PortNumber::new(1),
            "only single-port use cases supported"
        );
        debug_assert!(
            match snapshot {
                BestForeignSnapshot::Qualified {
                    received_on_port, ..
                } => received_on_port == PortNumber::new(1),
                BestForeignSnapshot::Empty => true,
            },
            "only single-port use cases supported"
        );
        self.set(snapshot);
    }

    fn best(&self) -> BestForeignSnapshot {
        self.get()
    }
}

/// Snapshot of best foreign datasets.
///
/// Represents `E_best` and `E_rbest` in the IEEE 1588-2019 §9.3.
/// Used for storage, passing between components, and as input to BMCA decision logic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BestForeignSnapshot {
    /// A qualified foreign candidate and the identity of the port that announced it.
    Qualified {
        ds: ClockDS,
        source_port_identity: PortIdentity,
        received_on_port: PortNumber,
    },
    /// No qualified foreign candidate is currently known. The empty dataset in spec language.
    Empty,
}

impl BestForeignSnapshot {
    /// Return the grandmaster identity of this dataset, if non-empty.
    pub(crate) fn grandmaster_id(&self) -> Option<ClockIdentity> {
        match self {
            BestForeignSnapshot::Qualified { ds, .. } => Some(*ds.identity()),
            BestForeignSnapshot::Empty => None,
        }
    }

    /// Return `true` if `self` is better than `other` according to BMCA dataset comparison.
    pub(crate) fn better_than(&self, other: &BestForeignSnapshot) -> bool {
        match (self, other) {
            (
                BestForeignSnapshot::Qualified { ds: ds1, .. },
                BestForeignSnapshot::Qualified { ds: ds2, .. },
            ) => ds1.better_than(ds2),
            (BestForeignSnapshot::Qualified { .. }, BestForeignSnapshot::Empty) => true,
            (BestForeignSnapshot::Empty, BestForeignSnapshot::Qualified { .. }) => false,
            (BestForeignSnapshot::Empty, BestForeignSnapshot::Empty) => false,
        }
    }

    /// Return the parent port identity if this dataset was received on the given port.
    pub(crate) fn parent_if_received_on_port(
        &self,
        port: PortNumber,
    ) -> Option<ParentPortIdentity> {
        match self {
            BestForeignSnapshot::Qualified {
                source_port_identity,
                received_on_port,
                ..
            } => {
                (*received_on_port == port).then(|| ParentPortIdentity::new(*source_port_identity))
            }
            BestForeignSnapshot::Empty => None,
        }
    }
}

/// Notion of the best foreign clock record determined over a collection of foreign clock records
/// per port.
pub struct BestForeignRecord<S: ForeignClockRecords> {
    received_on_port: PortNumber,
    foreign_clock_records: S,
}

impl<S: ForeignClockRecords> BestForeignRecord<S> {
    /// Create a new best foreign record collection.
    ///
    /// `received_on_port` identifies the port this record collection is associated with.
    /// It is used when creating snapshots to identify which port received the foreign clock data.
    pub fn new(received_on_port: PortNumber, foreign_clock_records: S) -> Self {
        Self {
            received_on_port,
            foreign_clock_records,
        }
    }

    /// Consider a new foreign clock record observation.
    pub(crate) fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.foreign_clock_records.prune_stale(now);
        self.foreign_clock_records.remember(ForeignClockRecord::new(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        ));
    }

    /// Return the best qualified foreign dataset snapshot observed so far.
    pub(crate) fn snapshot(&self) -> BestForeignSnapshot {
        match self.foreign_clock_records.best_qualified() {
            Some(qualified) => qualified.snapshot(self.received_on_port),
            None => BestForeignSnapshot::Empty,
        }
    }
}

/// A single foreign clock record tracked by BestForeignRecord and stored in
/// `ForeignClockRecords`. It represents a foreign master clock candidate discovered via
/// Announce messages along with its source port identity and qualification state.
///
/// A record becomes *qualified* once enough Announce messages have been received within the
/// `ForeignMasterTimeWindow`. Until qualified, the record is retained but not considered for
/// `best_qualified` selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClockRecord {
    source_port_identity: PortIdentity,
    foreign_clock_ds: ClockDS,
    qualification: SlidingWindowQualification,
}

impl ForeignClockRecord {
    pub(crate) fn new(
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) -> Self {
        let window = ForeignMasterTimeWindow::new(log_announce_interval).duration();

        Self {
            source_port_identity,
            foreign_clock_ds,
            qualification: SlidingWindowQualification::new_unqualified(now, window),
        }
    }

    /// Create a record for a new foreign clock that is already qualified. This is mainly
    /// useful for tests, where we want to set up qualified foreign clock records directly.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn new_qualified(
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) -> Self {
        let window = ForeignMasterTimeWindow::new(log_announce_interval).duration();

        Self {
            source_port_identity,
            foreign_clock_ds,
            qualification: SlidingWindowQualification {
                last: now,
                prev: Some(now),
                window,
            },
        }
    }

    /// Return `true` if this record originates from `source_port_identity`.
    pub(crate) fn same_source_as(&self, source_port_identity: &PortIdentity) -> bool {
        self.source_port_identity == *source_port_identity
    }

    /// Return `true` if this record is better than `other` purely based on the data sets,
    /// ignoring qualification status.
    pub(crate) fn better_by_dataset_than(&self, other: &ForeignClockRecord) -> bool {
        self.foreign_clock_ds.better_than(&other.foreign_clock_ds)
    }

    /// Update this record from a newly observed record of the same source.
    ///
    /// This potentially:
    /// - advances the qualification window,
    /// - changes qualification status, and/or
    /// - updates the underlying data set.
    pub(crate) fn update_from(&mut self, record: &ForeignClockRecord) -> ForeignClockStatus {
        debug_assert!(self.source_port_identity == record.source_port_identity);

        let was_qualified = self.qualification.is_qualified();
        self.qualification
            .qualify(record.qualification.last, record.qualification.window);
        let is_qualified = self.qualification.is_qualified();

        let data_set_changed = self.foreign_clock_ds != record.foreign_clock_ds;
        if data_set_changed {
            self.foreign_clock_ds = record.foreign_clock_ds;
        }

        if was_qualified != is_qualified || (is_qualified && data_set_changed) {
            ForeignClockStatus::Updated
        } else {
            ForeignClockStatus::Unchanged
        }
    }

    /// Return a qualified foreign clock record if this record has become qualified.
    pub(crate) fn qualified<'a>(&'a self) -> Option<QualifiedForeignClockRecord<'a>> {
        if self.qualification.is_qualified() {
            Some(QualifiedForeignClockRecord::new(
                &self.foreign_clock_ds,
                &self.source_port_identity,
            ))
        } else {
            None
        }
    }

    /// Return the source port identity that originated this record.
    pub(crate) fn source_port_identity(&self) -> &PortIdentity {
        &self.source_port_identity
    }

    /// Return `true` if this record is stale at the given time.
    pub(crate) fn is_stale(&self, now: Instant) -> bool {
        self.qualification.is_stale(now)
    }
}

/// A qualified foreign clock record, guaranteed to have a valid dataset.
#[derive(Debug, PartialEq, Eq)]
pub struct QualifiedForeignClockRecord<'a> {
    dataset: &'a ClockDS,
    source_port_identity: &'a PortIdentity,
}

impl<'a> QualifiedForeignClockRecord<'a> {
    pub(crate) fn new(dataset: &'a ClockDS, source_port_identity: &'a PortIdentity) -> Self {
        Self {
            dataset,
            source_port_identity,
        }
    }

    pub(crate) fn snapshot(&self, port_number: PortNumber) -> BestForeignSnapshot {
        BestForeignSnapshot::Qualified {
            ds: *self.dataset,
            source_port_identity: *self.source_port_identity,
            received_on_port: port_number,
        }
    }
}

impl Ord for ForeignClockRecord {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        use core::cmp::Ordering::*;

        let ord = match (
            self.qualification.is_qualified(),
            other.qualification.is_qualified(),
        ) {
            (true, true) => {
                if self.foreign_clock_ds.better_than(&other.foreign_clock_ds) {
                    Less
                } else if other.foreign_clock_ds.better_than(&self.foreign_clock_ds) {
                    Greater
                } else {
                    Equal
                }
            }
            (true, false) => Less,
            (false, true) => Greater,
            (false, false) => Equal,
        };

        if ord == Equal {
            self.source_port_identity.cmp(&other.source_port_identity)
        } else {
            ord
        }
    }
}

impl PartialOrd for ForeignClockRecord {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Result of considering a foreign clock record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignClockStatus {
    /// The record remained unqualified or unchanged.
    Unchanged,
    /// The record became newly qualified or its data set changed.
    Updated,
}

/// The `ForeignMasterTimeWindow` derived from a port's announce interval.
///
/// The window defines how tightly spaced Announce messages must be to qualify a foreign record and
/// how long a record can remain silent before being considered stale.
///
/// See IEEE 1588-2019 §9.3.2.4.5
struct ForeignMasterTimeWindow {
    log_announce_interval: LogInterval,
}

impl ForeignMasterTimeWindow {
    fn new(log_announce_interval: LogInterval) -> Self {
        Self {
            log_announce_interval,
        }
    }

    fn duration(&self) -> Duration {
        self.log_announce_interval.duration().saturating_mul(4)
    }
}

/// Sliding window qualification policy for foreign master records. Encodes the logic to only
/// consider an observed foreign clock as qualified once enough Announce messages have been received
/// within a given time window.
///
/// This is intentionally a small and self-contained state machine that can later be replaced by a
/// different strategy (e.g. higher thresholds) without rippling into the rest of the module.
///
/// Notes
/// - Current behavior assumes a threshold of `2` Announce messages within the window. As defined in
///   IEEE 1588-2019 §9.3.2.4.5 (FOREIGN_MASTER_THRESHOLD = 2).
///
/// See
/// - IEEE 1588-2019 §9.3.2.4.5
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlidingWindowQualification {
    last: Instant,
    prev: Option<Instant>,
    window: Duration,
}

impl SlidingWindowQualification {
    fn new_unqualified(first: Instant, window: Duration) -> Self {
        Self {
            last: first,
            prev: None,
            window,
        }
    }

    /// Try to qualify this record based on a newly observed announce at `now`.
    fn qualify(&mut self, now: Instant, window: Duration) {
        self.prev = Some(self.last);
        self.last = now;
        self.window = window;
    }

    /// Return `true` if this record is currently qualified.
    fn is_qualified(&self) -> bool {
        let delta = match self.prev {
            Some(prev) => self.last.checked_sub(prev),
            None => None,
        };

        delta.is_some_and(|delta| delta <= self.window)
    }

    /// Return `true` if this record is stale at the given time.
    fn is_stale(&self, now: Instant) -> bool {
        let delta = now.checked_sub(self.last);
        delta.is_none_or(|delta| delta > self.window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::TestClockDS;

    #[test]
    fn qualified_dataset_change_reports_updated() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let foreign_low = TestClockDS::default_low_grade_slave_only().dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // First fast Announce qualifies the record (two samples: t0 and t1).
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified().is_some());

        // Change dataset while still qualified.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(2),
        );
        let status = record.update_from(&updated_record);
        assert_eq!(status, ForeignClockStatus::Updated);
        assert_eq!(
            record.qualified().map(|q| q.snapshot(PortNumber::new(1))),
            Some(BestForeignSnapshot::Qualified {
                ds: foreign_low,
                source_port_identity: port_id,
                received_on_port: PortNumber::new(1),
            })
        );
    }

    #[test]
    fn unqualified_dataset_change_does_not_report_updated() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let foreign_low = TestClockDS::default_low_grade_slave_only().dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // First slow Announce: still unqualified (spacing >= window).
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        let _ = record.update_from(&updated_record);
        assert!(record.qualified().is_none());

        // Second slow Announce with changed dataset: still unqualified and should report Unchanged.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(20),
        );
        let status = record.update_from(&updated_record);
        assert_eq!(status, ForeignClockStatus::Unchanged);
        assert!(record.qualified().is_none());
    }

    #[test]
    fn sliding_window_qualification_requires_two_fast_announces() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // First Announce: record is created, still unqualified.
        assert!(record.qualified().is_none());

        // Second Announce within the window makes it qualified.
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified().is_some());
    }

    #[test]
    fn sliding_window_qualification_drops_on_slow_announces() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // A second fast announce to qualify.
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified().is_some());

        // After a long gap, a single announce is not enough to stay qualified.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        record.update_from(&updated_record);
        assert!(record.qualified().is_none());
    }

    #[test]
    fn sliding_window_never_qualifies_with_one_annonce_per_window() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // Announce just slower than foreignMasterTimeWindow; density never reaches 2
        // within any sliding window.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(5),
        );
        record.update_from(&updated_record);
        assert!(record.qualified().is_none());

        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        record.update_from(&updated_record);
        assert!(record.qualified().is_none());

        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(15),
        );
        record.update_from(&updated_record);
        assert!(record.qualified().is_none());
    }

    #[test]
    fn sliding_window_edge_drops_qualification_but_not_stale() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // Qualify with a fast Announce.
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified().is_some());

        // Next Announce spaced just beyond the window drops qualification
        // but the record is not stale yet.
        let now = Instant::from_nanos(5_000_000_001);
        let updated_record =
            ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), now);
        let _ = record.update_from(&updated_record);
        assert!(record.qualified().is_none());
        assert!(!record.is_stale(now));
    }
}
