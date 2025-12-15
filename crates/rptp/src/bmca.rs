//! Best Master Clock Algorithm (BMCA) implementation.
//!
//! This module contains the data structures and logic required to run the
//! IEEE 1588-2019 Best Master Clock Algorithm (BMCA).  It models both the
//! local clock data sets and the set of _foreign_ clocks discovered via
//! Announce messages, and produces high‑level master/slave/passive decisions
//! for a [`Port`].
//!
//! The types here are intentionally low‑level and closely follow the wording
//! of the standard (for example, `DefaultDS`, `ForeignClockDS` and the
//! various priority fields).  Higher‑level code is expected to:
//!
//! - feed received Announce information into a [`Bmca`] implementation using
//!   [`Bmca::consider`];
//! - query the current decision using [`Bmca::decision`];
//! - transition the [`PortState`] based on the returned [`BmcaDecision`].

use core::cell::Cell;
use core::ops::Range;

use crate::clock::{ClockIdentity, ClockQuality, LocalClock, StepsRemoved, SynchronizableClock};
use crate::log::PortLog;
use crate::message::{AnnounceMessage, SequenceId, TimeScale};
use crate::port::{ParentPortIdentity, Port, PortIdentity};
use crate::portstate::PortState;
use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

/// Core BMCA trait implemented by BMCA strategies.
///
/// A BMCA implementation receives foreign clock information via
/// [`Bmca::consider`] and exposes its current decision through
/// [`Bmca::decision`].
pub trait Bmca {
    /// Feed a foreign clock data set into the BMCA.
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ForeignClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    );

    /// Return the current BMCA decision for the given local clock.
    ///
    /// Implementations may return [`BmcaDecision::Undecided`] when the
    /// underlying state has not changed or not enough information is
    /// available yet.
    fn decision<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> BmcaDecision;
}

/// A collection of foreign clock records kept in BMCA order.
///
/// Implementations are responsible for maintaining the partial ordering of
/// [`ForeignClockRecord`]s such that the “best” qualified clock (according to
/// BMCA) can be retrieved efficiently with [`SortedForeignClockRecords::first`].
pub trait SortedForeignClockRecords {
    /// Insert a new foreign clock record into the collection.
    fn insert(&mut self, record: ForeignClockRecord);
    /// Update an existing foreign clock record, if present.
    ///
    /// The record is located by `source_port_identity` and then passed to the
    /// `update` closure, which may mutate it and returns a
    /// [`ForeignClockStatus`] describing what changed.
    fn update_record<F>(
        &mut self,
        source_port_identity: &PortIdentity,
        update: F,
    ) -> ForeignClockResult
    where
        F: FnOnce(&mut ForeignClockRecord) -> ForeignClockStatus;
    /// Return the best qualified foreign clock record in the collection, if
    /// any.
    fn first(&self) -> Option<&ForeignClockRecord>;

    /// Prune stale foreign clock records from the collection.
    ///
    /// Returns `true` when at least one record was removed.  This allows BMCA
    /// wrappers such as [`IncrementalBmca`] to treat the removal of stale
    /// records as an update that may change the overall decision even when the
    /// most recent [`AnnounceMessage`] only refreshed an existing record or
    /// created a still‑unqualified one.
    fn prune_stale(&mut self, now: Instant) -> bool;
}

/// BMCA implementation that returns an actual decision only if the underlying
/// state changed between calls. If no updates occur between calls to decision,
/// it returns [`BmcaDecision::Undecided`].
///
/// This is the basic BMCA role that tells the port if something genuinely changed
/// in the BMCA state, and is used as a building block for more complex BMCA roles
/// such as local master or parent tracking.
pub struct IncrementalBmca<S: SortedForeignClockRecords> {
    inner: BestMasterClockAlgorithm<S>,
    dirty: Cell<bool>,
}

impl<S: SortedForeignClockRecords> IncrementalBmca<S> {
    /// Create a new incremental BMCA around the given sorted record store.
    pub fn new(sorted_clock_records: S) -> Self {
        Self {
            inner: BestMasterClockAlgorithm::new(sorted_clock_records),
            dirty: Cell::new(false),
        }
    }
}

impl<S: SortedForeignClockRecords> Bmca for IncrementalBmca<S> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ForeignClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        let status = self.inner.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
        if status == ForeignClockStatus::Updated {
            self.dirty.set(true);
        }
    }

    fn decision<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> BmcaDecision {
        if self.dirty.get() {
            self.dirty.set(false);
            self.inner.decision(local_clock)
        } else {
            BmcaDecision::Undecided
        }
    }
}

/// BMCA decorator that tracks local master changes and only returns decisions when
/// either the local clock actually changes, or the local clock loses BMCA against a
/// foreign clock. Returns [`BmcaDecision::Undecided`] otherwise.
pub struct LocalMasterTrackingBmca<B: Bmca> {
    inner: B,
}

impl<B: Bmca> LocalMasterTrackingBmca<B> {
    /// Create a new local‑master‑tracking BMCA wrapper.
    pub fn new(inner: B) -> Self {
        Self { inner }
    }

    /// Consume the wrapper and return the inner BMCA.
    pub(crate) fn into_inner(self) -> B {
        self.inner
    }
}

impl<B: Bmca> Bmca for LocalMasterTrackingBmca<B> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ForeignClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.inner.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    fn decision<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> BmcaDecision {
        let decision = self.inner.decision(local_clock);

        match decision {
            BmcaDecision::Master(_) => BmcaDecision::Undecided,
            _ => decision,
        }
    }
}

/// BMCA decorator that tracks a particular foreign clock and produces decision
/// only when the actual parent changes, or the local clock wins the BMCA against
/// the current parent.
///
/// As such, it represents the current selected parent/foreign master clock
/// for a port.
///
/// Returns [`BmcaDecision::Undecided`] when the parent/master clock remains
/// the same between calls to [`Bmca::decision`].
pub struct ParentTrackingBmca<B: Bmca> {
    inner: B,
    parent_port_identity: Cell<ParentPortIdentity>,
}

impl<B: Bmca> ParentTrackingBmca<B> {
    /// Create a new parent‑tracking BMCA wrapper with the initial parent.
    pub fn new(inner: B, parent_port_identity: ParentPortIdentity) -> Self {
        Self {
            inner,
            parent_port_identity: Cell::new(parent_port_identity),
        }
    }

    /// Return the currently tracked parent port identity.
    pub(crate) fn parent(&self) -> ParentPortIdentity {
        self.parent_port_identity.get()
    }

    /// Return `true` if the given source port identity matches the tracked
    /// parent.
    pub(crate) fn matches_parent(&self, source: &PortIdentity) -> bool {
        self.parent_port_identity.get().matches(source)
    }

    /// Consume the wrapper and return the inner BMCA.
    pub(crate) fn into_inner(self) -> B {
        self.inner
    }
}

impl<B: Bmca> Bmca for ParentTrackingBmca<B> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ForeignClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.inner.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    fn decision<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> BmcaDecision {
        let decision = self.inner.decision(local_clock);

        match decision {
            BmcaDecision::Slave(slave_decision) => {
                if !slave_decision.matches_parent(self.parent_port_identity.get()) {
                    decision
                } else {
                    BmcaDecision::Undecided
                }
            }
            _ => decision,
        }
    }
}

/// Best Master Clock Algorithm as defined in IEEE 1588-2019 section 9.3
///
/// Implemented over an abstract sorted collection of foreign clock records. [`consider`] is
/// used to feed in new foreign clock data sets, while [`decision`] produces the current BMCA
/// decision. [`decision`] is pure, i.e., it does not mutate internal state and returns the
/// same result when called multiple times without intervening calls to [`consider`].
///
/// Private to the module; client code shall use the BMCA in terms of defined context-sensitive
/// decorators implementing the `Bmca` trait. In terms of the design philosophy, the BMCA steps
/// into different roles depending on the port state (e.g., local master tracking in pre-master,
/// parent tracking in slave, etc.), and those roles are implemented as decorators around this
/// core BMCA.
struct BestMasterClockAlgorithm<S: SortedForeignClockRecords> {
    sorted_clock_records: S,
}

impl<S: SortedForeignClockRecords> BestMasterClockAlgorithm<S> {
    /// Create a new full BMCA.
    fn new(sorted_clock_records: S) -> Self {
        Self {
            sorted_clock_records,
        }
    }

    /// Let the BMCA consider a foreign clock data set.
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ForeignClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) -> ForeignClockStatus {
        let pruned = self.sorted_clock_records.prune_stale(now);

        let result = self
            .sorted_clock_records
            .update_record(&source_port_identity, |record| {
                record.consider(foreign_clock_ds, log_announce_interval, now)
            });

        let status = match result {
            ForeignClockResult::NotFound => {
                self.sorted_clock_records.insert(ForeignClockRecord::new(
                    source_port_identity,
                    foreign_clock_ds,
                    log_announce_interval,
                    now,
                ));
                ForeignClockStatus::Unchanged
            }
            ForeignClockResult::Status(status) => status,
        };

        if pruned {
            ForeignClockStatus::Updated
        } else {
            status
        }
    }

    // IEEE 1588-2019 Section 9.3.3 - BMCA State Decision Algorithm
    // Note: variable names correspond to those in the spec for easier reference.
    fn decision<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> BmcaDecision {
        let d_0 = local_clock;
        let Some(e_rbest_record) = self.sorted_clock_records.first() else {
            return BmcaDecision::Undecided;
        };
        let Some(e_rbest) = e_rbest_record.dataset() else {
            return BmcaDecision::Undecided;
        };

        if d_0.is_grandmaster_capable() {
            Self::d0_better_or_better_by_topology_than_e_rbest(d_0, e_rbest)
        } else {
            Self::d0_better_or_better_by_topology_than_e_best(
                d_0,
                e_rbest, // TODO: once multi-port is supported, introduce e_best here
                e_rbest,
                e_rbest_record.source_port_identity(),
            )
        }
    }

    fn d0_better_or_better_by_topology_than_e_rbest<C: SynchronizableClock>(
        d_0: &LocalClock<C>,
        e_rbest: &ForeignClockDS,
    ) -> BmcaDecision {
        if d_0.better_than(e_rbest) {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0), // steps removed to zero as per IEEE 1588-2019 Section 9.3.5, Table 13
            ))
        } else {
            BmcaDecision::Passive // Passive decision point P1
        }
    }

    fn d0_better_or_better_by_topology_than_e_best<C: SynchronizableClock>(
        d_0: &LocalClock<C>,
        e_best: &ForeignClockDS,
        _e_rbest: &ForeignClockDS,
        source_port_identity: PortIdentity,
    ) -> BmcaDecision {
        if d_0.better_than(e_best) {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0), // steps removed to zero as per IEEE 1588-2019 Section 9.3.5, Table 13
            ))
        } else {
            // TODO: as the implementation supports only a single port at the
            // moment, we can directly recommend slave here, which corresponds to
            // slave decision point S1. In a multi-port implementation, we would
            // need to compare against e_best. When e_rbest == e_best, we'd had
            // slave decision point S1, we'd compare e_best against e_rbest and
            // decide between master decision point M3 and passive decision point P2.
            BmcaDecision::Slave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(source_port_identity),
                e_best.steps_removed().increment(),
            ))
        }
    }
}

/// BMCA implementation that never produces a decision.
///
/// This is primarily useful for testing or when an application wants to
/// manage port states manually.
#[allow(dead_code)]
pub(crate) struct NoopBmca;

impl Bmca for NoopBmca {
    fn consider(
        &mut self,
        _source_port_identity: crate::port::PortIdentity,
        _foreign_clock_ds: ForeignClockDS,
        _log_announce_interval: LogInterval,
        _now: Instant,
    ) {
    }

    fn decision<C: crate::clock::SynchronizableClock>(
        &self,
        _local_clock: &LocalClock<C>,
    ) -> BmcaDecision {
        BmcaDecision::Undecided
    }
}

/// Master decision point as defined in IEEE 1588-2019 Section 9.3.1 (Figure 26).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BmcaMasterDecisionPoint {
    /// Decision point M1.
    M1,
    /// Decision point M2.
    M2,
    /// Decision point M3.
    #[allow(dead_code)]
    M3,
}

/// High‑level BMCA outcome for a given port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BmcaDecision {
    /// No new decision is available yet.
    Undecided,
    /// The port should act as a master.
    Master(BmcaMasterDecision),
    /// The port should act as a slave, following the given parent.
    Slave(BmcaSlaveDecision),
    /// The port should be passive.
    Passive,
}

/// BMCA decision for entering a master/pre‑master state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BmcaMasterDecision {
    decision_point: BmcaMasterDecisionPoint,
    steps_removed: StepsRemoved,
}

impl BmcaMasterDecision {
    /// Create a new master decision for the given decision point and
    /// `stepsRemoved`.
    pub(crate) fn new(
        decision_point: BmcaMasterDecisionPoint,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            decision_point,
            steps_removed,
        }
    }

    /// Apply this master decision to a [`Port`], producing a new
    /// [`PortState`] in the pre‑master state.
    pub(crate) fn apply<F, P, B, L>(&self, new_port_state: F) -> PortState<P, B, L>
    where
        P: Port,
        B: Bmca,
        L: PortLog,
        F: FnOnce(QualificationTimeoutPolicy, StepsRemoved) -> PortState<P, B, L>,
    {
        let qualification_timeout_policy =
            QualificationTimeoutPolicy::new(self.decision_point, self.steps_removed);

        new_port_state(qualification_timeout_policy, self.steps_removed)
    }
}

/// Qualification timeout policy as defined in IEEE 1588-2019 Section 9.2.6.10.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QualificationTimeoutPolicy {
    master_decision_point: BmcaMasterDecisionPoint,
    steps_removed: StepsRemoved,
}

impl QualificationTimeoutPolicy {
    /// Create a new qualification timeout policy for the given decision point
    /// and `stepsRemoved` value.
    pub fn new(
        master_decision_point: BmcaMasterDecisionPoint,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            master_decision_point,
            steps_removed,
        }
    }

    /// Compute the qualification timeout duration for a given log announce
    /// interval.
    pub fn duration(&self, log_announce_interval: LogInterval) -> Duration {
        match self.master_decision_point {
            BmcaMasterDecisionPoint::M1 => Duration::from_secs(0),
            BmcaMasterDecisionPoint::M2 => Duration::from_secs(0),
            BmcaMasterDecisionPoint::M3 => {
                let n = self.steps_removed.as_u16() as u64 + 1;
                log_announce_interval.duration().saturating_mul(n)
            }
        }
    }
}

/// BMCA decision for entering a slave state, including the selected parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BmcaSlaveDecision {
    parent_port_identity: ParentPortIdentity,
    steps_removed: StepsRemoved,
}

impl BmcaSlaveDecision {
    /// Create a new slave decision with the given parent and `stepsRemoved`.
    pub(crate) fn new(
        parent_port_identity: ParentPortIdentity,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            parent_port_identity,
            steps_removed,
        }
    }

    /// Return `true` if the given source port identity matches the chosen parent.
    pub(crate) fn matches_parent(&self, parent: ParentPortIdentity) -> bool {
        self.parent_port_identity == parent
    }

    /// Apply this slave decision to a [`Port`], producing a new [`PortState`]
    /// in the uncalibrated state.
    pub(crate) fn apply<F, P, B, L>(self, next_port_state: F) -> PortState<P, B, L>
    where
        P: Port,
        B: Bmca,
        L: PortLog,
        F: FnOnce(ParentPortIdentity, StepsRemoved) -> PortState<P, B, L>,
    {
        next_port_state(self.parent_port_identity, self.steps_removed)
    }
}

/// A single foreign clock record tracked by the BMCA. Represents a foreign master clock
/// candidate discovered via Announce messages along with its source port identity and
/// qualification state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClockRecord {
    source_port_identity: PortIdentity,
    foreign_clock_ds: ForeignClockDS,
    qualification: SlidingWindowQualification,
}

impl ForeignClockRecord {
    /// Create a record for a new foreign clock with an initial validation
    /// count of one Announce.
    pub(crate) fn new(
        source_port_identity: PortIdentity,
        foreign_clock_ds: ForeignClockDS,
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
    pub(crate) fn qualified(
        source_port_identity: PortIdentity,
        foreign_clock_ds: ForeignClockDS,
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

    /// Consider a fresh Announce for this foreign clock.
    ///
    /// The validation counter is incremented (saturating), and the underlying
    /// data set is updated when it changes.  The return value describes
    /// whether the record became qualified or changed in a way that may
    /// affect the overall BMCA outcome.
    pub(crate) fn consider(
        &mut self,
        foreign_clock_ds: ForeignClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) -> ForeignClockStatus {
        let window = ForeignMasterTimeWindow::new(log_announce_interval).duration();

        let was_qualified = self.qualification.is_qualified();
        self.qualification.qualify(now, window);
        let is_qualified = self.qualification.is_qualified();

        let data_set_changed = self.foreign_clock_ds != foreign_clock_ds;
        if data_set_changed {
            self.foreign_clock_ds = foreign_clock_ds;
        }

        if was_qualified != is_qualified || (is_qualified && data_set_changed) {
            ForeignClockStatus::Updated
        } else {
            ForeignClockStatus::Unchanged
        }
    }

    /// Return the underlying data set if this record has become qualified as
    /// a foreign master candidate, or `None` otherwise.
    pub(crate) fn dataset(&self) -> Option<&ForeignClockDS> {
        if self.qualification.is_qualified() {
            Some(&self.foreign_clock_ds)
        } else {
            None
        }
    }

    /// Return the source port identity that originated this record.
    pub(crate) fn source_port_identity(&self) -> PortIdentity {
        self.source_port_identity
    }

    /// Return `true` if this record is stale at the given time.
    pub(crate) fn is_stale(&self, now: Instant) -> bool {
        self.qualification.is_stale(now)
    }
}

impl Ord for ForeignClockRecord {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        use core::cmp::Ordering::*;

        let ord = match (self.dataset(), other.dataset()) {
            (Some(&ds1), Some(&ds2)) => {
                if ds1.better_than(&ds2) {
                    Less
                } else if ds2.better_than(&ds1) {
                    Greater
                } else {
                    Equal
                }
            }
            (Some(_), None) => Less,
            (None, Some(_)) => Greater,
            (None, None) => Equal,
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

/// Return value of [`SortedForeignClockRecords::update_record`].
pub enum ForeignClockResult {
    /// No existing record matched the given source port identity.
    NotFound,
    /// A record was found and updated, with the given status.
    Status(ForeignClockStatus),
}

/// Result of considering a foreign clock record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignClockStatus {
    /// The record remained unqualified or unchanged.
    Unchanged,
    /// The record became newly qualified or its data set changed.
    Updated,
}

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

// Simple sliding‑window based qualification state machine for foreign clocks. It implies
// FOREIGN_MASTER_THRESHOLD = 2 as per IEEE 1588-2019. Support for higher thresholds needs
// a different implementation, but the surface of SlidingWindowQualification should be quite
// stable and future proof, so that a different implementation shall not ripple into
// ForeignClockRecord or into other BMCA code.
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

    fn qualify(&mut self, now: Instant, window: Duration) {
        self.prev = Some(self.last);
        self.last = now;
        self.window = window;
    }

    fn is_qualified(&self) -> bool {
        let delta = match self.prev {
            Some(prev) => self.last.checked_sub(prev),
            None => None,
        };

        delta.is_some_and(|delta| delta <= self.window)
    }

    fn is_stale(&self, now: Instant) -> bool {
        let delta = now.checked_sub(self.last);
        delta.is_none_or(|delta| delta > self.window)
    }
}

/// Local clock default data set used when participating in BMCA.
///
/// This is the local node’s view of its own clock quality and priorities, as
/// opposed to [`ForeignClockDS`], which represents remote candidates.
pub struct DefaultDS {
    identity: ClockIdentity,
    priority1: Priority1,
    priority2: Priority2,
    quality: ClockQuality,
    timescale: TimeScale,
}

impl DefaultDS {
    /// Construct a new local default data set.
    pub fn new(
        identity: ClockIdentity,
        priority1: Priority1,
        priority2: Priority2,
        quality: ClockQuality,
        timescale: TimeScale,
    ) -> Self {
        Self {
            identity,
            priority1,
            priority2,
            quality,
            timescale,
        }
    }

    /// Return the local clock identity.
    pub(crate) fn identity(&self) -> &ClockIdentity {
        &self.identity
    }

    /// Return whether the local clock is grandmaster‑capable.
    pub(crate) fn is_grandmaster_capable(&self) -> bool {
        self.quality.is_grandmaster_capable()
    }

    /// Compare the local clock against a foreign candidate.
    ///
    /// `steps_removed` is the local `stepsRemoved` value; this method returns
    /// `true` when the local clock is strictly better than the foreign one.
    pub(crate) fn better_than(
        &self,
        foreign: &ForeignClockDS,
        steps_removed: &StepsRemoved,
    ) -> bool {
        foreign.worse_than(&BmcaRank {
            identity: &self.identity,
            priority1: &self.priority1,
            priority2: &self.priority2,
            quality: &self.quality,
            steps_removed,
        })
    }

    /// Create an [`AnnounceMessage`] advertising this clock’s data set.
    pub(crate) fn announce(
        &self,
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        steps_removed: StepsRemoved,
    ) -> AnnounceMessage {
        AnnounceMessage::new(
            sequence_id,
            log_message_interval,
            ForeignClockDS::new(
                self.identity,
                self.priority1,
                self.priority2,
                self.quality,
                steps_removed,
            ),
            self.timescale,
        )
    }
}

/// Data set describing a foreign master candidate.
///
/// This corresponds closely to the fields carried in Announce messages and
/// used by the BMCA when comparing local and foreign clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClockDS {
    identity: ClockIdentity,
    priority1: Priority1,
    priority2: Priority2,
    quality: ClockQuality,
    steps_removed: StepsRemoved,
}

impl ForeignClockDS {
    const PRIORITY1_OFFSET: usize = 0;
    const QUALITY_RANGE: Range<usize> = 1..5;
    const PRIORITY2_OFFSET: usize = 5;
    const IDENTITY_RANGE: Range<usize> = 6..14;
    const STEPS_REMOVED_OFFSET: Range<usize> = 14..16;

    /// Construct a new foreign clock data set from individual fields.
    pub(crate) fn new(
        identity: ClockIdentity,
        priority1: Priority1,
        priority2: Priority2,
        quality: ClockQuality,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            identity,
            priority1,
            priority2,
            quality,
            steps_removed,
        }
    }

    /// Return the number of steps this clock is removed from the grandmaster.
    pub(crate) fn steps_removed(&self) -> StepsRemoved {
        self.steps_removed
    }

    /// Parse a `ForeignClockDS` from the binary representation used on the
    /// wire (16‑byte BMCA comparison tuple).
    pub(crate) fn from_wire(buf: &[u8; 16]) -> Self {
        Self {
            identity: ClockIdentity::new(&[
                buf[Self::IDENTITY_RANGE.start],
                buf[Self::IDENTITY_RANGE.start + 1],
                buf[Self::IDENTITY_RANGE.start + 2],
                buf[Self::IDENTITY_RANGE.start + 3],
                buf[Self::IDENTITY_RANGE.start + 4],
                buf[Self::IDENTITY_RANGE.start + 5],
                buf[Self::IDENTITY_RANGE.start + 6],
                buf[Self::IDENTITY_RANGE.start + 7],
            ]),
            priority1: Priority1::new(buf[ForeignClockDS::PRIORITY1_OFFSET]),
            priority2: Priority2::new(buf[ForeignClockDS::PRIORITY2_OFFSET]),
            quality: ClockQuality::from_wire(&[
                buf[ForeignClockDS::QUALITY_RANGE.start],
                buf[ForeignClockDS::QUALITY_RANGE.start + 1],
                buf[ForeignClockDS::QUALITY_RANGE.start + 2],
                buf[ForeignClockDS::QUALITY_RANGE.start + 3],
            ]),
            steps_removed: StepsRemoved::new(u16::from_be_bytes([
                buf[ForeignClockDS::STEPS_REMOVED_OFFSET.start],
                buf[ForeignClockDS::STEPS_REMOVED_OFFSET.start + 1],
            ])),
        }
    }

    /// Return `true` if this foreign clock is strictly better than `other`
    /// according to BMCA ranking rules.
    fn better_than(&self, other: &ForeignClockDS) -> bool {
        let a = BmcaRank {
            identity: &self.identity,
            priority1: &self.priority1,
            quality: &self.quality,
            priority2: &self.priority2,
            steps_removed: &self.steps_removed,
        };
        let b = BmcaRank {
            identity: &other.identity,
            priority1: &other.priority1,
            quality: &other.quality,
            priority2: &other.priority2,
            steps_removed: &other.steps_removed,
        };

        a.better_than(&b)
    }

    fn worse_than(&self, other: &BmcaRank) -> bool {
        let own_rank = BmcaRank {
            identity: &self.identity,
            priority1: &self.priority1,
            quality: &self.quality,
            priority2: &self.priority2,
            steps_removed: &self.steps_removed,
        };

        other.better_than(&own_rank)
    }

    /// Serialize this data set into the 16‑byte BMCA comparison tuple
    /// representation.
    pub(crate) fn to_wire(self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[Self::PRIORITY1_OFFSET] = self.priority1.as_u8();
        bytes[Self::QUALITY_RANGE].copy_from_slice(&self.quality.to_wire());
        bytes[Self::PRIORITY2_OFFSET] = self.priority2.as_u8();
        bytes[Self::IDENTITY_RANGE].copy_from_slice(self.identity.as_bytes());
        bytes[Self::STEPS_REMOVED_OFFSET.start..Self::STEPS_REMOVED_OFFSET.end]
            .copy_from_slice(&self.steps_removed.to_be_bytes());
        bytes
    }
}

/// `defaultDS.priority1` as defined in IEEE 1588.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority1(u8);

impl Priority1 {
    /// Create a new `Priority1` from a raw `u8` value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Return the underlying `u8` value.
    pub(crate) fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for Priority1 {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

/// `defaultDS.priority2` as defined in IEEE 1588.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority2(u8);

impl Priority2 {
    /// Create a new `Priority2` from a raw `u8` value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Return the underlying `u8` value.
    pub(crate) fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for Priority2 {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

/// Internal helper representing the tuple used by the BMCA to compare clocks.
///
/// This combines a clock identity, its priorities, quality and the number of
/// steps removed into a single ordering, matching IEEE 1588’s rules for
/// selecting the best grandmaster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BmcaRank<'a> {
    identity: &'a ClockIdentity,
    priority1: &'a Priority1,
    quality: &'a ClockQuality,
    priority2: &'a Priority2,
    steps_removed: &'a StepsRemoved,
}

impl BmcaRank<'_> {
    /// Return `true` if `self` is strictly better than `other`.
    ///
    /// When both ranks refer to the same clock identity, the comparison is
    /// based solely on `steps_removed`; otherwise it follows the BMCA tuple
    /// ordering (priority1, clock quality, priority2, identity).
    fn better_than(&self, other: &BmcaRank) -> bool {
        if self.identity == other.identity {
            return self.steps_removed < other.steps_removed;
        }

        let a = (
            &self.priority1,
            &self.quality,
            &self.priority2,
            &self.identity,
        );
        let b = (
            &other.priority1,
            &other.quality,
            &other.priority2,
            &other.identity,
        );

        a < b
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use crate::clock::ClockAccuracy;
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NOOP_CLOCK_METRICS;
    use crate::port::PortNumber;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::FakeClock;
    use crate::time::{Duration, Instant};

    const CLK_ID_HIGH: ClockIdentity =
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]);
    const CLK_ID_MID: ClockIdentity =
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]);
    const CLK_ID_LOW: ClockIdentity =
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x03]);
    const CLK_ID_GM: ClockIdentity =
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x04]);

    const CLK_QUALITY_HIGH: ClockQuality =
        ClockQuality::new(248, ClockAccuracy::Within250ns, 0xFFFF);
    const CLK_QUALITY_MID: ClockQuality =
        ClockQuality::new(250, ClockAccuracy::Within100us, 0xFFFF);
    const CLK_QUALITY_LOW: ClockQuality = ClockQuality::new(255, ClockAccuracy::Within1ms, 0xFFFF);
    const CLK_QUALITY_GM: ClockQuality = ClockQuality::new(100, ClockAccuracy::Within100ns, 0xFFFF);

    impl ForeignClockDS {
        pub(crate) fn gm_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_GM,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_GM,
                StepsRemoved::new(0),
            )
        }

        pub(crate) fn high_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_HIGH,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_HIGH,
                StepsRemoved::new(0),
            )
        }

        pub(crate) fn mid_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_MID,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_MID,
                StepsRemoved::new(0),
            )
        }

        pub(crate) fn low_grade_test_clock() -> ForeignClockDS {
            ForeignClockDS::new(
                CLK_ID_LOW,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_LOW,
                StepsRemoved::new(0),
            )
        }
    }

    impl DefaultDS {
        pub(crate) fn high_grade_test_clock() -> DefaultDS {
            DefaultDS::new(
                CLK_ID_HIGH,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_HIGH,
                TimeScale::Ptp,
            )
        }

        pub(crate) fn mid_grade_test_clock() -> DefaultDS {
            DefaultDS::new(
                CLK_ID_MID,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_MID,
                TimeScale::Ptp,
            )
        }

        pub(crate) fn low_grade_test_clock() -> DefaultDS {
            DefaultDS::new(
                CLK_ID_LOW,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_LOW,
                TimeScale::Ptp,
            )
        }

        pub(crate) fn gm_grade_test_clock() -> DefaultDS {
            DefaultDS::new(
                CLK_ID_GM,
                Priority1::new(127),
                Priority2::new(127),
                CLK_QUALITY_GM,
                TimeScale::Ptp,
            )
        }
    }

    #[test]
    fn sliding_window_qualification_requires_two_fast_announces() {
        let t0 = Instant::from_secs(0);
        let mut record = ForeignClockRecord::new(
            PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1)),
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            t0,
        );

        // First Announce: record is created, still unqualified.
        assert!(record.dataset().is_none());

        // Second Announce within the window makes it qualified.
        record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        assert!(record.dataset().is_some());
    }

    #[test]
    fn sliding_window_qualification_drops_on_slow_announces() {
        let t0 = Instant::from_secs(0);
        let mut record = ForeignClockRecord::new(
            PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1)),
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            t0,
        );

        // A second fast announce to qualify.
        record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        assert!(record.dataset().is_some());

        // After a long gap, a single announce is not enough to stay qualified.
        record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        assert!(record.dataset().is_none());
    }

    #[test]
    fn sliding_window_never_qualifies_with_one_annonce_per_window() {
        let t0 = Instant::from_secs(0);
        let mut record = ForeignClockRecord::new(
            PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1)),
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            t0,
        );

        // Announce just slower than foreignMasterTimeWindow; density never reaches 2
        // within any sliding window.
        record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(5),
        );
        assert!(record.dataset().is_none());

        record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        assert!(record.dataset().is_none());

        record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(15),
        );
        assert!(record.dataset().is_none());
    }

    #[test]
    fn qualified_dataset_change_reports_updated() {
        let t0 = Instant::from_secs(0);
        let mut record = ForeignClockRecord::new(
            PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1)),
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            t0,
        );

        // First fast Announce qualifies the record (two samples: t0 and t1).
        record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        assert!(record.dataset().is_some());

        // Change dataset while still qualified.
        let status = record.consider(
            ForeignClockDS::low_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(2),
        );
        assert_eq!(status, ForeignClockStatus::Updated);
        assert_eq!(
            record.dataset(),
            Some(&ForeignClockDS::low_grade_test_clock())
        );
    }

    #[test]
    fn unqualified_dataset_change_does_not_report_updated() {
        let t0 = Instant::from_secs(0);
        let mut record = ForeignClockRecord::new(
            PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1)),
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            t0,
        );

        // First slow Announce: still unqualified (spacing >= window).
        let _ = record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        assert!(record.dataset().is_none());

        // Second slow Announce with changed dataset: still unqualified and should report Unchanged.
        let status = record.consider(
            ForeignClockDS::low_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(20),
        );
        assert_eq!(status, ForeignClockStatus::Unchanged);
        assert!(record.dataset().is_none());
    }

    #[test]
    fn sliding_window_edge_drops_qualification_but_not_stale() {
        let t0 = Instant::from_secs(0);
        let mut record = ForeignClockRecord::new(
            PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1)),
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            t0,
        );

        // Qualify with a fast Announce.
        record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        assert!(record.dataset().is_some());

        // Next Announce spaced just beyond the window drops qualification
        // but the record is not stale yet.
        let now = Instant::from_nanos(5_000_000_001);
        let _ = record.consider(
            ForeignClockDS::high_grade_test_clock(),
            LogInterval::new(0),
            now,
        );
        assert!(record.dataset().is_none());
        assert!(!record.is_stale(now));
    }

    #[test]
    fn test_foreign_clock_ordering() {
        let high = ForeignClockDS::high_grade_test_clock();
        let mid = ForeignClockDS::mid_grade_test_clock();
        let low = ForeignClockDS::low_grade_test_clock();

        assert!(high.better_than(&mid));
        assert!(!mid.better_than(&high));

        assert!(high.better_than(&low));
        assert!(!low.better_than(&high));

        assert!(mid.better_than(&low));
        assert!(!low.better_than(&mid));
    }

    #[test]
    fn test_foreign_clock_equality() {
        let c1 = ForeignClockDS::high_grade_test_clock();
        let c2 = ForeignClockDS::high_grade_test_clock();

        assert!(!c1.better_than(&c2));
        assert!(!c2.better_than(&c1));
        assert_eq!(c1, c2);
    }

    #[test]
    fn foreign_clock_ds_compares_priority1_before_quality() {
        // a has lower priority1 but worse quality; still better than b.
        let a = ForeignClockDS::new(
            CLK_ID_LOW,
            Priority1::new(10),
            Priority2::new(127),
            CLK_QUALITY_LOW,
            StepsRemoved::new(0),
        );
        let b = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(100),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_compares_priority2_after_quality() {
        // Same p1 and quality; lower p2 wins.
        let a = ForeignClockDS::new(
            CLK_ID_LOW,
            Priority1::new(127),
            Priority2::new(10),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );
        let b = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(20),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_tiebreaks_on_identity_last() {
        // All fields equal except identity; lower identity better than higher.
        let a = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );
        let b = ForeignClockDS::new(
            CLK_ID_MID,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_prefers_lower_steps_removed_when_identity_equal() {
        let a = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(1),
        );
        let b = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(127),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(3),
        );

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn bmca_prunes_stale_foreign_clocks_on_next_announce_reception() {
        let stale_records = vec![
            ForeignClockRecord::qualified(
                PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1)),
                ForeignClockDS::high_grade_test_clock(),
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::qualified(
                PortIdentity::new(CLK_ID_MID, PortNumber::new(1)),
                ForeignClockDS::mid_grade_test_clock(),
                LogInterval::new(0),
                Instant::from_secs(1),
            ),
            ForeignClockRecord::qualified(
                PortIdentity::new(CLK_ID_MID, PortNumber::new(1)),
                ForeignClockDS::low_grade_test_clock(),
                LogInterval::new(0),
                Instant::from_secs(2),
            ),
        ];
        let mut sorted_records = SortedForeignClockRecordsVec::from_records(&stale_records);

        let mut bmca = BestMasterClockAlgorithm::new(&mut sorted_records);

        // Consider a new announce from a different foreign clock.
        let status = bmca.consider(
            PortIdentity::new(CLK_ID_GM, PortNumber::new(1)),
            ForeignClockDS::gm_grade_test_clock(),
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        assert_eq!(status, ForeignClockStatus::Updated);

        assert_eq!(
            sorted_records.len(),
            1,
            "stale foreign clock records should have been pruned"
        );
        assert_eq!(
            sorted_records.first(),
            Some(&ForeignClockRecord::new(
                PortIdentity::new(CLK_ID_GM, PortNumber::new(1)),
                ForeignClockDS::gm_grade_test_clock(),
                LogInterval::new(0),
                Instant::from_secs(10),
            ))
        );
    }

    #[test]
    fn bmca_gm_capable_local_with_no_qualified_foreign_is_undecided() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn bmca_gm_capable_local_loses_tuple_returns_passive() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let mut bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        // Foreign uses lower priority1 so it is better, even though clock class is worse.
        let foreign_strong = ForeignClockDS::new(
            CLK_ID_HIGH,
            Priority1::new(1),
            Priority2::new(127),
            CLK_QUALITY_HIGH,
            StepsRemoved::new(0),
        );
        let port_id = PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1));

        bmca.consider(
            port_id,
            foreign_strong,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id,
            foreign_strong,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Passive);
    }

    #[test]
    fn bmca_gm_capable_local_better_than_foreign_returns_master_m1() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let mut bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        // Foreign is worse (higher priority1), so local GM-capable should become Master(M1).
        let foreign = ForeignClockDS::mid_grade_test_clock();
        let port_id = PortIdentity::new(CLK_ID_MID, PortNumber::new(1));

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(&local_clock),
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0)
            ))
        );
    }

    #[test]
    fn bmca_non_gm_local_better_than_foreign_returns_master_m2() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let mut bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        // Foreign is slightly worse quality; local should be better and take M2.
        let foreign = ForeignClockDS::low_grade_test_clock();
        let port_id = PortIdentity::new(CLK_ID_LOW, PortNumber::new(1));

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(&local_clock),
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0)
            ))
        );
    }

    #[test]
    fn bmca_non_gm_local_loses_tuple_returns_slave() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let mut bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        let foreign = ForeignClockDS::high_grade_test_clock();
        let port_id = PortIdentity::new(CLK_ID_HIGH, PortNumber::new(1));

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(&local_clock),
            BmcaDecision::Slave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(port_id),
                foreign.steps_removed().increment()
            )),
        );
    }

    #[test]
    fn bmca_recommends_slave_from_interleaved_announce_sequence() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::low_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let mut bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();

        let port_id_high = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let port_id_mid = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );

        bmca.consider(
            port_id_high,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_mid,
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_high,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_mid,
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        let decision = bmca.decision(&local_clock);

        assert_eq!(
            decision,
            BmcaDecision::Slave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(port_id_high),
                foreign_high.steps_removed().increment()
            )),
        );
    }

    #[test]
    fn bmca_recommends_slave_from_non_interleaved_announce_sequence() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::low_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let mut bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();

        let port_id_high = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let port_id_mid = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );

        bmca.consider(
            port_id_high,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_high,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_mid,
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_mid,
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        let decision = bmca.decision(&local_clock);

        assert_eq!(
            decision,
            BmcaDecision::Slave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(port_id_high),
                foreign_high.steps_removed().increment()
            ))
        );
    }

    #[test]
    fn bmca_undecided_when_no_announces_yet() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn bmca_undecided_when_no_qualified_clock_records_yet() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let mut bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();

        bmca.consider(
            PortIdentity::fake(),
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn bmca_undecided_when_only_single_announces_each() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let mut bmca = BestMasterClockAlgorithm::new(SortedForeignClockRecordsVec::new());

        let foreign_high = ForeignClockDS::high_grade_test_clock();
        let foreign_mid = ForeignClockDS::mid_grade_test_clock();
        let foreign_low = ForeignClockDS::low_grade_test_clock();

        bmca.consider(
            PortIdentity::new(CLK_ID_HIGH, PortNumber::new(0)),
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            PortIdentity::new(CLK_ID_MID, PortNumber::new(0)),
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            PortIdentity::new(CLK_ID_LOW, PortNumber::new(0)),
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(&local_clock), BmcaDecision::Undecided);
    }

    #[test]
    fn qualification_timeout_policy_duration_m1_is_zero() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(5));

        let qualification_timeout_interval = policy.duration(LogInterval::new(4));

        assert_eq!(qualification_timeout_interval, Duration::from_secs(0));
    }

    #[test]
    fn qualification_timeout_policy_duration_m2_is_zero() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M2, StepsRemoved::new(5));

        let qualification_timeout_interval = policy.duration(LogInterval::new(4));

        assert_eq!(qualification_timeout_interval, Duration::from_secs(0));
    }

    #[test]
    fn qualification_timeout_policy_duration_m3_scales_with_steps_removed() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(0));
        let qualification_timeout_interval = policy.duration(LogInterval::new(0));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(1));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(1));
        let qualification_timeout_interval = policy.duration(LogInterval::new(0));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(2));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(2));
        let qualification_timeout_interval = policy.duration(LogInterval::new(1));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(6));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(3));
        let qualification_timeout_interval = policy.duration(LogInterval::new(2));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(16));
    }

    #[test]
    fn qualification_timeout_policy_duration_m3_large_inputs() {
        let policy = QualificationTimeoutPolicy::new(
            BmcaMasterDecisionPoint::M3,
            StepsRemoved::new(u16::MAX),
        );

        let qualification_timeout_interval = policy.duration(LogInterval::new(10));

        assert_eq!(
            qualification_timeout_interval,
            Duration::from_secs(67_108_864)
        );
    }
}
