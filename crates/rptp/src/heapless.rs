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

use crate::foreign::{ForeignClockRecord, ForeignClockRecords};
use crate::port::PortIdentity;

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
    receiver: PortIdentity,
}

impl<const N: usize> HeaplessForeignClockRecords<N> {
    /// Create an empty store with capacity `N`.
    ///
    /// `receiver` identifies the port that will receive these foreign clock records.
    pub fn new(receiver: PortIdentity) -> Self {
        Self {
            records: Vec::new(),
            removal_policy: RemovalPolicy,
            receiver,
        }
    }

    /// Create a store from pre-seeded records (tests/support only).
    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn from_records(receiver: PortIdentity, records: &[ForeignClockRecord]) -> Self {
        let vec = Vec::from_slice(records).unwrap_or_default();
        Self {
            records: vec,
            removal_policy: RemovalPolicy,
            receiver,
        }
    }
}

impl Default for HeaplessForeignClockRecords<0> {
    fn default() -> Self {
        Self::new(PortIdentity::new(
            crate::clock::ClockIdentity::new(&[0; 8]),
            crate::port::PortNumber::new(1),
        ))
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
            existing.update_from(&record);
            return;
        }

        if self.records.push(record).is_ok() {
            return;
        }

        // Capacity exceeded, determine if there is a record to remove by policy
        if let Some(index) =
            self.removal_policy
                .candidate_index(&self.records, &record, &self.receiver)
        {
            self.records.remove(index);
            let result = self.records.push(record);
            assert!(result.is_ok());
        }
    }

    /// Iterate over all stored records.
    fn iter(&self) -> impl Iterator<Item = &ForeignClockRecord> {
        self.records.iter()
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
// - ignores qualification (considers only dataset ranking with topology), and
// - replaces the worst record that is worse than the candidate by dataset + topology ranking.
struct RemovalPolicy;

impl RemovalPolicy {
    /// Return the index of a record to remove to make room for `candidate`.
    ///
    /// The policy scans all records and chooses the worst one that is still worse
    /// than `candidate` by dataset ranking with topology. This ensures we always evict the
    /// lowest-quality record to make room for a better candidate.
    fn candidate_index(
        &self,
        current: &[ForeignClockRecord],
        candidate: &ForeignClockRecord,
        receiver: &PortIdentity,
    ) -> Option<usize> {
        let mut worst_index: Option<usize> = None;

        for (i, record) in current.iter().enumerate() {
            if candidate.is_better_than(record, receiver) {
                match worst_index {
                    None => worst_index = Some(i),
                    Some(w) => {
                        // If current record is worse than the previously found worst,
                        // update the worst index.
                        if record.is_better_than(&current[w], receiver) {
                            // current[w] is still the worst
                        } else {
                            // current record is worse or equal
                            worst_index = Some(i);
                        }
                    }
                }
            }
        }

        worst_index
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
    fn heapless_stores_and_iterates_records() {
        let receiver = new_port_identity(99);
        let mut records = HeaplessForeignClockRecords::<4>::new(receiver);

        let high_clock = TestClockDS::default_high_grade().dataset();
        let mid_clock = TestClockDS::default_mid_grade().dataset();
        let low_clock = TestClockDS::default_low_grade_slave_only().dataset();

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

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

        // All three records should be stored.
        assert_eq!(records.records.len(), 3);

        // Iteration should return all records with exact match.
        let all_records: heapless::Vec<_, 4> = records.iter().collect();
        assert!(all_records.contains(&&ForeignClockRecord::new(
            high_port_id,
            high_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        )));
        assert!(all_records.contains(&&ForeignClockRecord::new(
            low_port_id,
            low_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        )));
        assert!(all_records.contains(&&ForeignClockRecord::new(
            mid_port_id,
            mid_clock,
            LogInterval::new(0),
            Instant::from_secs(0),
        )));
    }

    fn same_dataset(
        a: &ForeignClockRecord,
        b: &ForeignClockRecord,
        receiver: &PortIdentity,
    ) -> bool {
        !a.is_better_than(b, receiver) && !b.is_better_than(a, receiver)
    }

    #[test]
    fn heapless_replacement_keeps_best_records_on_overflow() {
        let receiver = new_port_identity(99);
        let high_clock = TestClockDS::default_high_grade().dataset();
        let mid_clock = TestClockDS::default_mid_grade().dataset();
        let low_clock = TestClockDS::default_low_grade_slave_only().dataset();

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

        // Start with mid and low, both qualified.
        let mut records = HeaplessForeignClockRecords::<2>::from_records(
            receiver,
            &[
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
            ],
        );

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
            .any(|r| same_dataset(r, &template_high, &receiver));
        let has_mid = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_mid, &receiver));
        let has_low = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_low, &receiver));

        assert!(has_high);
        assert!(has_mid);
        assert!(!has_low);
    }

    #[test]
    fn heapless_replacement_does_not_replace_when_candidate_is_worse() {
        let receiver = new_port_identity(99);
        let high_clock = TestClockDS::default_high_grade().dataset();
        let mid_clock = TestClockDS::default_mid_grade().dataset();
        let low_clock = TestClockDS::default_low_grade_slave_only().dataset();

        let high_port_id = new_port_identity(1);
        let mid_port_id = new_port_identity(2);
        let low_port_id = new_port_identity(3);

        // Start with high and mid, both qualified.
        let mut records = HeaplessForeignClockRecords::<2>::from_records(
            receiver,
            &[
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
            ],
        );

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
            .any(|r| same_dataset(r, &template_high, &receiver));
        let has_mid = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_mid, &receiver));
        let has_low = records
            .records
            .iter()
            .any(|r| same_dataset(r, &template_low, &receiver));

        assert!(has_high);
        assert!(has_mid);
        assert!(!has_low);
    }

    #[test]
    fn heapless_prune_stale_returns_true_when_records_removed() {
        let receiver = new_port_identity(99);
        let high_clock = TestClockDS::default_high_grade().dataset();
        let high_port_id = new_port_identity(1);

        let mut records = HeaplessForeignClockRecords::<4>::new(receiver);
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
        let receiver = new_port_identity(99);
        let high_clock = TestClockDS::default_high_grade().dataset();
        let high_port_id = new_port_identity(1);

        let mut records = HeaplessForeignClockRecords::<4>::new(receiver);
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
