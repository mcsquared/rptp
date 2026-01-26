//! Heapless storage helpers for `no_std` / embedded use.
//!
//! This module is available when the crate feature `heapless-storage` is enabled (it is part of
//! the default feature set). It provides ready-made, fixed-capacity implementations of
//! domain-facing storage traits used by the core.
//!
//! The goal is to let embedded adopters wire `rptp` without pulling in a heap allocator, while
//! keeping the domain core independent from concrete collection types.
//! If you prefer a different storage strategy (arrays, custom lists, arena allocators, …), you can
//! ignore these helpers and implement the relevant traits yourself.
//!
//! Currently this module provides:
//! - [`HeaplessForeignClockRecords`]: a bounded [`ForeignClockRecords`] implementation used by BMCA
//!   to track and qualify foreign master candidates from Announce messages.

use heapless::Vec;

use crate::bmca::{
    ForeignClockRecord, ForeignClockRecords, ForeignClockStatus, QualifiedForeignClockRecord,
};

/// Heapless implementation of [`ForeignClockRecords`] backed by a bounded
/// `heapless::Vec`.
///
/// This type is intended for embedded adopters that want a ready‑made fixed‑capacity
/// foreign clock store. Applications that prefer to supply their own storage can
/// ignore this type and implement [`ForeignClockRecords`] on their own.
///
/// ## Ordering and selection
///
/// Internally, records are kept sorted so that `records[0]` is the *best* candidate according to
/// [`ForeignClockRecord`]'s ordering (qualification first, then dataset ranking, then identity).
/// [`ForeignClockRecords::best_qualified`] therefore returns `self.records.first()` if it is
/// qualified.
///
/// ## Capacity and replacement behaviour
///
/// This store has a hard capacity `N`. When capacity is exceeded, it may drop an existing record
/// to make room for the new candidate:
///
/// - The replacement decision is made **by dataset quality only** (ignoring qualification).
/// - A new record that is *better than at least one existing record* replaces the worst record
///   that it beats.
/// - A new record that is worse than all existing records is dropped.
///
/// This intentionally allows a “better but not yet qualified” candidate to enter the set and
/// become qualified later, instead of being permanently excluded by capacity pressure.
pub struct HeaplessForeignClockRecords<const N: usize> {
    records: Vec<ForeignClockRecord, N>,
    removal_policy: RemovalPolicy,
}

impl<const N: usize> HeaplessForeignClockRecords<N> {
    /// Create an empty store with capacity `N`.
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            removal_policy: RemovalPolicy,
        }
    }

    /// Create a store from pre-seeded records (tests/support only).
    ///
    /// The resulting store is sorted according to the record ordering so that “best first” holds.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn from_records(records: &[ForeignClockRecord]) -> Self {
        let mut vec = Vec::from_slice(records).unwrap_or_default();
        vec.sort_unstable();
        Self {
            records: vec,
            removal_policy: RemovalPolicy,
        }
    }
}

impl Default for HeaplessForeignClockRecords<0> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ForeignClockRecords for HeaplessForeignClockRecords<N> {
    /// Remember (insert or update) a record.
    ///
    /// If capacity is exceeded, [`RemovalPolicy`] may choose a record to remove.
    fn remember(&mut self, record: ForeignClockRecord) {
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|r| r.same_source_as(record.source_port_identity()))
        {
            if let ForeignClockStatus::Updated = existing.update_from(&record) {
                self.records.sort_unstable();
            }
            return;
        }

        let record = match self.records.push(record) {
            Ok(()) => {
                self.records.sort_unstable();
                return;
            }
            Err(record) => record,
        };

        // Capacity exceeded, determine if there is a record to remove by policy
        if let Some(index) = self.removal_policy.candidate_index(&self.records, &record) {
            self.records.remove(index);
            let result = self.records.push(record);
            assert!(result.is_ok());

            self.records.sort_unstable();
        }
    }

    fn best_qualified<'a>(&'a self) -> Option<QualifiedForeignClockRecord<'a>> {
        self.records.first().and_then(|record| record.qualified())
    }

    fn prune_stale(&mut self, now: crate::time::Instant) -> bool {
        let before = self.records.len();
        self.records.retain(|record| !record.is_stale(now));
        self.records.len() != before
    }
}

// Replacement policy to determine which record to possibly remove when capacity is
// exceeded to make room for a new, possibly better record, even if unqualified.
//
// The current policy:
// - ignores qualification (considers only dataset ranking),
// - scans from worst → best, and
// - replaces the worst record that the candidate beats.
struct RemovalPolicy;

impl RemovalPolicy {
    /// Return the index of a record to remove to make room for `candidate`.
    ///
    /// The returned index points into `current` (which is assumed to be sorted best→worst).
    /// The policy chooses the worst record that is still worse than `candidate` by dataset ranking.
    fn candidate_index(
        &self,
        current: &[ForeignClockRecord],
        candidate: &ForeignClockRecord,
    ) -> Option<usize> {
        current.iter().rev().enumerate().find_map(|(i, record)| {
            if candidate.better_by_dataset_than(record) {
                Some(current.len() - 1 - i)
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ClockIdentity;
    use crate::port::{PortIdentity, PortNumber};
    use crate::test_support::TestClockDS;
    use crate::time::{Instant, LogInterval};

    fn new_port_identity(last_octet: u8) -> PortIdentity {
        PortIdentity::new(
            ClockIdentity::new(&[0, 1, 2, 3, 4, 5, 6, last_octet]),
            PortNumber::new(1),
        )
    }

    #[test]
    fn heapless_maintains_best_record_first() {
        let high_clock = TestClockDS::default_high_grade().dataset();
        let mid_clock = TestClockDS::default_mid_grade().dataset();
        let low_clock = TestClockDS::default_low_grade_slave_only().dataset();

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

        let records = HeaplessForeignClockRecords::<4>::from_records(&[
            ForeignClockRecord::new_qualified(
                high_port_id,
                high_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::new(
                low_port_id,
                low_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::new(
                mid_port_id,
                mid_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
        ]);

        let best_clock = records.best_qualified();
        assert_eq!(
            best_clock,
            Some(QualifiedForeignClockRecord::new(&high_clock, &high_port_id))
        );
    }

    fn same_dataset(a: &ForeignClockRecord, b: &ForeignClockRecord) -> bool {
        !a.better_by_dataset_than(b) && !b.better_by_dataset_than(a)
    }

    #[test]
    fn heapless_replacement_keeps_best_records_on_overflow() {
        let high_clock = TestClockDS::default_high_grade().dataset();
        let mid_clock = TestClockDS::default_mid_grade().dataset();
        let low_clock = TestClockDS::default_low_grade_slave_only().dataset();

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

        // Start with mid and low, both qualified.
        let mut records = HeaplessForeignClockRecords::<2>::from_records(&[
            ForeignClockRecord::new_qualified(
                mid_port_id,
                mid_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::new_qualified(
                low_port_id,
                low_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
        ]);

        // Insert a better clock; capacity is exceeded, so low should be removed.
        records.remember(ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        ));

        assert_eq!(records.records.len(), 2);

        let template_high = ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        let template_mid = ForeignClockRecord::new(
            mid_port_id,
            mid_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        let template_low = ForeignClockRecord::new(
            low_port_id,
            low_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        let has_high = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_high));
        let has_mid = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_mid));
        let has_low = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_low));

        assert!(has_high);
        assert!(has_mid);
        assert!(!has_low);
    }

    #[test]
    fn heapless_replacement_does_not_replace_when_candidate_is_worse() {
        let high_clock = TestClockDS::default_high_grade().dataset();
        let mid_clock = TestClockDS::default_mid_grade().dataset();
        let low_clock = TestClockDS::default_low_grade_slave_only().dataset();

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

        // Start with high and mid, both qualified.
        let mut records = HeaplessForeignClockRecords::<2>::from_records(&[
            ForeignClockRecord::new_qualified(
                high_port_id,
                high_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::new_qualified(
                mid_port_id,
                mid_clock,
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
        ]);

        // Insert a worse clock; capacity is exceeded, but no record should be replaced.
        records.remember(ForeignClockRecord::new(
            low_port_id,
            low_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        ));

        assert_eq!(records.records.len(), 2);

        let template_high = ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        let template_mid = ForeignClockRecord::new(
            mid_port_id,
            mid_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        let template_low = ForeignClockRecord::new(
            low_port_id,
            low_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        let has_high = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_high));
        let has_mid = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_mid));
        let has_low = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_low));

        assert!(has_high);
        assert!(has_mid);
        assert!(!has_low);
    }

    #[test]
    fn heapless_prune_stale_returns_true_when_records_removed() {
        let high_clock = TestClockDS::default_high_grade().dataset();
        let high_port_id = new_port_identity(1);

        let mut records = HeaplessForeignClockRecords::<4>::new();
        records.remember(ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        ));

        // With logInterval = 0, the foreign master time window is 4 seconds.
        // Advancing well beyond that should mark the record as stale.
        let pruned = records.prune_stale(Instant::from_secs(10));

        assert!(pruned, "prune_stale should report removal of stale records");
        assert!(
            records.records.is_empty(),
            "all stale records should have been removed"
        );
    }

    #[test]
    fn heapless_prune_stale_returns_false_when_no_records_are_stale() {
        let high_clock = TestClockDS::default_high_grade().dataset();
        let high_port_id = new_port_identity(1);

        let mut records = HeaplessForeignClockRecords::<4>::new();
        records.remember(ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        ));

        // Within the 4-second window, the record is not yet stale.
        let pruned = records.prune_stale(Instant::from_secs(2));

        assert!(
            !pruned,
            "prune_stale should report no removals when nothing is stale"
        );
        assert_eq!(records.records.len(), 1);
    }
}
