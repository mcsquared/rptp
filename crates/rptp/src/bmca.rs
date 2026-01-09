use core::cell::Cell;
use core::ops::Range;

use crate::clock::{ClockIdentity, ClockQuality, StepsRemoved, TimeScale};
use crate::message::{AnnounceMessage, SequenceId};
use crate::port::{ParentPortIdentity, Port, PortIdentity, PortNumber};
use crate::portstate::PortState;
use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

pub trait Bmca {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    );

    fn decision(&self) -> Option<BmcaDecision>;
}

pub trait SortedForeignClockRecords {
    fn remember(&mut self, record: ForeignClockRecord);
    fn best_qualified(&self) -> Option<&ForeignClockRecord>;
    fn prune_stale(&mut self, now: Instant) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BestForeignDataset<'a> {
    Qualified {
        ds: &'a ClockDS,
        source_port_identity: &'a PortIdentity,
    },
    Empty,
}

impl<'a> BestForeignDataset<'a> {
    fn grandmaster_id(&self) -> Option<&ClockIdentity> {
        match self {
            BestForeignDataset::Qualified { ds, .. } => Some(ds.identity()),
            BestForeignDataset::Empty => None,
        }
    }

    fn better_than(&self, other: &BestForeignDataset) -> bool {
        match (self, other) {
            (
                BestForeignDataset::Qualified { ds: ds1, .. },
                BestForeignDataset::Qualified { ds: ds2, .. },
            ) => ds1.better_than(ds2),
            (BestForeignDataset::Qualified { .. }, BestForeignDataset::Empty) => true,
            (BestForeignDataset::Empty, BestForeignDataset::Qualified { .. }) => false,
            (BestForeignDataset::Empty, BestForeignDataset::Empty) => false,
        }
    }

    fn common_parent(self, other: BestForeignDataset) -> Option<ParentPortIdentity> {
        match (self, other) {
            (
                BestForeignDataset::Qualified {
                    source_port_identity: id1,
                    ..
                },
                BestForeignDataset::Qualified {
                    source_port_identity: id2,
                    ..
                },
            ) if id1 == id2 => Some(ParentPortIdentity::new(*id1)),
            _ => None,
        }
    }

    fn snapshot(&self) -> BestForeignSnapshot {
        match self {
            BestForeignDataset::Qualified {
                ds,
                source_port_identity,
            } => BestForeignSnapshot::Qualified {
                ds: **ds,
                source_port_identity: **source_port_identity,
            },
            BestForeignDataset::Empty => BestForeignSnapshot::Empty,
        }
    }
}

pub struct BestForeignRecord<S: SortedForeignClockRecords> {
    sorted_clock_records: S,
}

impl<S: SortedForeignClockRecords> BestForeignRecord<S> {
    pub fn new(sorted_clock_records: S) -> Self {
        Self {
            sorted_clock_records,
        }
    }

    pub(crate) fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.sorted_clock_records.prune_stale(now);
        self.sorted_clock_records.remember(ForeignClockRecord::new(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        ));
    }

    fn dataset<'a>(&'a self) -> BestForeignDataset<'a> {
        match self.sorted_clock_records.best_qualified() {
            Some(record) => BestForeignDataset::Qualified {
                ds: record
                    .qualified_ds()
                    .expect("best_qualified must return a qualified record"),
                source_port_identity: record.source_port_identity(),
            },
            None => BestForeignDataset::Empty,
        }
    }
}

pub(crate) struct ListeningBmca<'a, S: SortedForeignClockRecords> {
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
}

impl<'a, S: SortedForeignClockRecords> ListeningBmca<'a, S> {
    /// Create a new incremental BMCA around the given sorted record store.
    pub fn new(bmca: BestMasterClockAlgorithm<'a>, best_foreign: BestForeignRecord<S>) -> Self {
        Self { bmca, best_foreign }
    }

    pub(crate) fn into_parent_tracking(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> ParentTrackingBmca<'a, S> {
        ParentTrackingBmca::new(self.bmca, self.best_foreign, parent_port_identity)
    }

    pub(crate) fn into_grandmaster_tracking(
        self,
        grandmaster_id: ClockIdentity,
    ) -> GrandMasterTrackingBmca<'a, S> {
        GrandMasterTrackingBmca::new(self.bmca, self.best_foreign, grandmaster_id)
    }

    pub(crate) fn into_current_grandmaster_tracking(self) -> GrandMasterTrackingBmca<'a, S> {
        let current_grandmaster_id = self.bmca.using_grandmaster(|gm| *gm.identity());
        self.into_grandmaster_tracking(current_grandmaster_id)
    }
}

impl<'a, S: SortedForeignClockRecords> Bmca for ListeningBmca<'a, S> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.best_foreign.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    fn decision(&self) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.dataset();

        // In listening mode, only non-empty foreign datasets are considered for BMCA decisions.
        // See IEEE 1588-2019 Section 9.3.3, figure 26.
        match e_rbest {
            BestForeignDataset::Qualified { .. } => Some(self.bmca.decision(e_rbest)),
            BestForeignDataset::Empty => None,
        }
    }
}

pub struct GrandMaster<'a> {
    ds: &'a ClockDS,
}

impl<'a> GrandMaster<'a> {
    fn new(ds: &'a ClockDS) -> Self {
        Self { ds }
    }

    pub fn identity(&self) -> &ClockIdentity {
        self.ds.identity()
    }

    pub fn announce(
        &self,
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        time_scale: TimeScale,
    ) -> AnnounceMessage {
        AnnounceMessage::new(sequence_id, log_message_interval, *self.ds, time_scale)
    }
}

pub(crate) struct GrandMasterTrackingBmca<'a, S: SortedForeignClockRecords> {
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
    grandmaster_id: ClockIdentity,
}

impl<'a, S: SortedForeignClockRecords> GrandMasterTrackingBmca<'a, S> {
    pub fn new(
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        grandmaster_id: ClockIdentity,
    ) -> Self {
        Self {
            bmca,
            best_foreign,
            grandmaster_id,
        }
    }

    pub(crate) fn using_grandmaster<R>(&self, f: impl FnOnce(GrandMaster<'_>) -> R) -> R {
        self.bmca.using_grandmaster(f)
    }

    pub(crate) fn with_grandmaster_id(self, grandmaster_id: ClockIdentity) -> Self {
        Self {
            bmca: self.bmca,
            best_foreign: self.best_foreign,
            grandmaster_id,
        }
    }

    pub(crate) fn into_parent_tracking(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> ParentTrackingBmca<'a, S> {
        ParentTrackingBmca::new(self.bmca, self.best_foreign, parent_port_identity)
    }
}

impl<'a, S: SortedForeignClockRecords> Bmca for GrandMasterTrackingBmca<'a, S> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.best_foreign.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    fn decision(&self) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.dataset();
        let decision = self.bmca.decision(e_rbest);

        match decision {
            BmcaDecision::Master(master_decision) => {
                if master_decision.grandmaster_id() == self.grandmaster_id {
                    None
                } else {
                    Some(decision)
                }
            }
            BmcaDecision::Slave(_) | BmcaDecision::Passive => Some(decision),
        }
    }
}

pub(crate) struct ParentTrackingBmca<'a, S: SortedForeignClockRecords> {
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
    parent_port_identity: ParentPortIdentity,
}

impl<'a, S: SortedForeignClockRecords> ParentTrackingBmca<'a, S> {
    pub fn new(
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        parent_port_identity: ParentPortIdentity,
    ) -> Self {
        Self {
            bmca,
            best_foreign,
            parent_port_identity,
        }
    }

    pub(crate) fn matches_parent(&self, source: &PortIdentity) -> bool {
        self.parent_port_identity.matches(source)
    }

    pub(crate) fn with_parent(self, parent: ParentPortIdentity) -> Self {
        Self {
            bmca: self.bmca,
            best_foreign: self.best_foreign,
            parent_port_identity: parent,
        }
    }

    pub(crate) fn into_grandmaster_tracking(
        self,
        grandmaster_id: ClockIdentity,
    ) -> GrandMasterTrackingBmca<'a, S> {
        GrandMasterTrackingBmca::new(self.bmca, self.best_foreign, grandmaster_id)
    }

    pub(crate) fn into_current_grandmaster_tracking(self) -> GrandMasterTrackingBmca<'a, S> {
        let current_grandmaster_id = self.bmca.using_grandmaster(|gm| *gm.identity());
        self.into_grandmaster_tracking(current_grandmaster_id)
    }
}

impl<'a, S: SortedForeignClockRecords> Bmca for ParentTrackingBmca<'a, S> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.best_foreign.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    fn decision(&self) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.dataset();
        let decision = self.bmca.decision(e_rbest);

        match decision {
            BmcaDecision::Slave(parent) => {
                if self.parent_port_identity != parent {
                    Some(decision)
                } else {
                    None
                }
            }
            _ => Some(decision),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BestForeignSnapshot {
    Qualified {
        ds: ClockDS,
        source_port_identity: PortIdentity,
    },
    Empty,
}

impl BestForeignSnapshot {
    fn as_best_foreign_dataset(&self) -> BestForeignDataset<'_> {
        match self {
            BestForeignSnapshot::Qualified {
                ds,
                source_port_identity,
            } => BestForeignDataset::Qualified {
                ds,
                source_port_identity,
            },
            BestForeignSnapshot::Empty => BestForeignDataset::Empty,
        }
    }
}

pub trait ForeignGrandMasterCandidates {
    fn remember(&self, port: PortNumber, snapshot: BestForeignSnapshot);
    fn best(&self) -> BestForeignSnapshot;
}

// Simple in-memory implementation of ForeignGrandMasterCandidates for single-port use cases.
impl ForeignGrandMasterCandidates for Cell<BestForeignSnapshot> {
    fn remember(&self, port: PortNumber, snapshot: BestForeignSnapshot) {
        debug_assert!(port == PortNumber::new(1)); // only single-port use cases supported
        self.set(snapshot);
    }

    fn best(&self) -> BestForeignSnapshot {
        self.get()
    }
}

pub trait LocalGrandMasterCandidate {
    fn snapshot(&self) -> ClockDS;
}

impl LocalGrandMasterCandidate for ClockDS {
    fn snapshot(&self) -> ClockDS {
        *self
    }
}

pub(crate) struct BestMasterClockAlgorithm<'a> {
    local_candidate: &'a dyn LocalGrandMasterCandidate,
    foreign_candidates: &'a dyn ForeignGrandMasterCandidates,
    port_number: PortNumber,
}

impl<'a> BestMasterClockAlgorithm<'a> {
    pub fn new(
        local_candidate: &'a dyn LocalGrandMasterCandidate,
        foreign_candidates: &'a dyn ForeignGrandMasterCandidates,
        port_number: PortNumber,
    ) -> Self {
        Self {
            local_candidate,
            foreign_candidates,
            port_number,
        }
    }

    pub(crate) fn using_grandmaster<R>(&self, f: impl FnOnce(GrandMaster<'_>) -> R) -> R {
        // TODO: once multi-port is supported, this should return the grandmaster, whether local or
        // foreign. With only single port support atm, it's always local d_0.
        let d_0 = self.local_candidate.snapshot();
        f(GrandMaster::new(&d_0))
    }

    pub(crate) fn decision(&self, e_rbest: BestForeignDataset) -> BmcaDecision {
        self.foreign_candidates
            .remember(self.port_number, e_rbest.snapshot());

        let d_0 = self.local_candidate.snapshot();
        if d_0.is_authoritative() {
            Self::d0_better_or_better_by_topology_than_e_rbest(&d_0, e_rbest)
        } else {
            let best_snapshot = self.foreign_candidates.best();
            let e_best = best_snapshot.as_best_foreign_dataset();
            Self::d0_better_or_better_by_topology_than_e_best(&d_0, e_best, e_rbest)
        }
    }

    fn d0_better_or_better_by_topology_than_e_rbest(
        d_0: &ClockDS,
        e_rbest: BestForeignDataset,
    ) -> BmcaDecision {
        if d_0.better_than_foreign(e_rbest) {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0), // steps removed to zero as per IEEE 1588-2019 Section 9.3.5, Table 13
                *d_0.identity(),
            ))
        } else {
            BmcaDecision::Passive // Passive decision point P1
        }
    }

    fn d0_better_or_better_by_topology_than_e_best(
        d_0: &ClockDS,
        e_best: BestForeignDataset,
        e_rbest: BestForeignDataset,
    ) -> BmcaDecision {
        if d_0.better_than_foreign(e_best) {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0), // steps removed to zero as per IEEE 1588-2019 Section 9.3.5, Table 13
                *d_0.identity(),
            ))
        } else if let Some(parent) = e_best.common_parent(e_rbest) {
            BmcaDecision::Slave(parent) // Slave decision point S1
        } else {
            // Only a qualified e_best can reach this point, see IEEE 1588-2019 Section 9.3.3., figure 26.
            debug_assert!(matches!(e_best, BestForeignDataset::Qualified { .. }));
            Self::e_best_better_by_topology_than_e_rbest(e_best, e_rbest)
        }
    }

    fn e_best_better_by_topology_than_e_rbest(
        e_best: BestForeignDataset,
        e_rbest: BestForeignDataset,
    ) -> BmcaDecision {
        // TODO: this comparison shall be topology-only, as the method name suggests.
        if e_best.better_than(&e_rbest) {
            BmcaDecision::Passive // Passive decision point P2
        } else {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M3,
                StepsRemoved::new(0),
                // unwrap is safe here, because only a qualified e_best can win against d_0, see above.
                e_best.grandmaster_id().copied().unwrap(),
            ))
        }
    }
}

#[allow(dead_code)]
pub(crate) struct NoopBmca;

impl Bmca for NoopBmca {
    fn consider(
        &mut self,
        _source_port_identity: crate::port::PortIdentity,
        _foreign_clock_ds: ClockDS,
        _log_announce_interval: LogInterval,
        _now: Instant,
    ) {
    }

    fn decision(&self) -> Option<BmcaDecision> {
        None
    }
}

/// Master decision point as defined in IEEE 1588-2019 Section 9.3.1 (Figure 26).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BmcaMasterDecisionPoint {
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
    /// The port should act as a master.
    Master(BmcaMasterDecision),
    /// The port should act as a slave, following the given parent.
    Slave(ParentPortIdentity),
    /// The port should be passive.
    Passive,
}

/// BMCA decision for entering a master/pre‑master state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BmcaMasterDecision {
    decision_point: BmcaMasterDecisionPoint,
    steps_removed: StepsRemoved,
    grandmaster_id: ClockIdentity,
}

impl BmcaMasterDecision {
    /// Create a new master decision for the given decision point and
    /// `stepsRemoved`.
    pub(crate) fn new(
        decision_point: BmcaMasterDecisionPoint,
        steps_removed: StepsRemoved,
        grandmaster_id: ClockIdentity,
    ) -> Self {
        Self {
            decision_point,
            steps_removed,
            grandmaster_id,
        }
    }

    pub fn grandmaster_id(&self) -> ClockIdentity {
        self.grandmaster_id
    }

    /// Apply this master decision to a [`Port`], producing a new
    /// [`PortState`] in the pre‑master state.
    pub(crate) fn apply<'a, F, P, S>(&self, new_port_state: F) -> PortState<'a, P, S>
    where
        P: Port,
        S: SortedForeignClockRecords,
        F: FnOnce(QualificationTimeoutPolicy, ClockIdentity) -> PortState<'a, P, S>,
    {
        let qualification_timeout_policy =
            QualificationTimeoutPolicy::new(self.decision_point, self.steps_removed);

        new_port_state(qualification_timeout_policy, self.grandmaster_id)
    }
}

/// Qualification timeout policy as defined in IEEE 1588-2019 Section 9.2.6.10.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QualificationTimeoutPolicy {
    master_decision_point: BmcaMasterDecisionPoint,
    steps_removed: StepsRemoved,
}

impl QualificationTimeoutPolicy {
    pub fn new(
        master_decision_point: BmcaMasterDecisionPoint,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            master_decision_point,
            steps_removed,
        }
    }

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

/// A single foreign clock record tracked by the BMCA. Represents a foreign master clock
/// candidate discovered via Announce messages along with its source port identity and
/// qualification state.
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
    pub(crate) fn qualified(
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

    /// Return the underlying data set if this record has become qualified as
    /// a foreign master candidate, or `None` otherwise.
    pub(crate) fn qualified_ds(&self) -> Option<&ClockDS> {
        if self.qualification.is_qualified() {
            Some(&self.foreign_clock_ds)
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

/// Data set describing a foreign master candidate.
///
/// This corresponds closely to the fields carried in Announce messages and
/// used by the BMCA when comparing local and foreign clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDS {
    identity: ClockIdentity,
    priority1: Priority1,
    priority2: Priority2,
    quality: ClockQuality,
    steps_removed: StepsRemoved,
}

impl ClockDS {
    const PRIORITY1_OFFSET: usize = 0;
    const QUALITY_RANGE: Range<usize> = 1..5;
    const PRIORITY2_OFFSET: usize = 5;
    const IDENTITY_RANGE: Range<usize> = 6..14;
    const STEPS_REMOVED_OFFSET: Range<usize> = 14..16;

    /// Construct a new foreign clock data set from individual fields.
    pub fn new(
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

    /// Return the clock identity of this foreign clock.
    pub fn identity(&self) -> &ClockIdentity {
        &self.identity
    }

    pub(crate) fn is_authoritative(&self) -> bool {
        self.quality.is_authoritative()
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
            priority1: Priority1::new(buf[ClockDS::PRIORITY1_OFFSET]),
            priority2: Priority2::new(buf[ClockDS::PRIORITY2_OFFSET]),
            quality: ClockQuality::from_wire(&[
                buf[ClockDS::QUALITY_RANGE.start],
                buf[ClockDS::QUALITY_RANGE.start + 1],
                buf[ClockDS::QUALITY_RANGE.start + 2],
                buf[ClockDS::QUALITY_RANGE.start + 3],
            ]),
            steps_removed: StepsRemoved::new(u16::from_be_bytes([
                buf[ClockDS::STEPS_REMOVED_OFFSET.start],
                buf[ClockDS::STEPS_REMOVED_OFFSET.start + 1],
            ])),
        }
    }

    /// Return `true` if this foreign clock is strictly better than `other`
    /// according to BMCA ranking rules.
    pub(crate) fn better_than(&self, other: &ClockDS) -> bool {
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

    pub(crate) fn better_than_foreign(&self, other: BestForeignDataset) -> bool {
        match other {
            BestForeignDataset::Qualified { ds: other_ds, .. } => self.better_than(other_ds),
            BestForeignDataset::Empty => true,
        }
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use crate::clock::LocalClock;
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NOOP_CLOCK_METRICS;
    use crate::port::PortNumber;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, TestClockCatalog};
    use crate::time::{Duration, Instant};

    #[test]
    fn grandmaster_tracking_bmca_gates_when_grandmaster_id_matches() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_high_grade().default_ds();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
            *local_clock.identity(),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn grandmaster_tracking_bmca_emits_when_grandmaster_id_differs() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_high_grade().default_ds();

        let other_grandmaster_id = TestClockCatalog::gps_grandmaster().clock_identity();
        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
            other_grandmaster_id,
        );

        assert!(matches!(bmca.decision(), Some(BmcaDecision::Master(_))));
    }

    #[test]
    fn grandmaster_tracking_bmca_does_not_gate_passive_decisions() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let now = Instant::from_secs(0);

        let default_ds = TestClockCatalog::gps_grandmaster().default_ds();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let better = TestClockCatalog::atomic_grandmaster();
        let better_port_id = PortIdentity::new(better.clock_identity(), PortNumber::new(1));
        let better_ds = better.foreign_ds(StepsRemoved::new(0));
        let records = [ForeignClockRecord::qualified(
            better_port_id,
            better_ds,
            LogInterval::new(0),
            now,
        )];

        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::from_records(&records)),
            *local_clock.identity(),
        );

        assert_eq!(bmca.decision(), Some(BmcaDecision::Passive));
    }

    #[test]
    fn grandmaster_tracking_bmca_does_not_gate_slave_decisions() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let now = Instant::from_secs(0);

        let default_ds = TestClockCatalog::default_low_grade_slave_only().default_ds();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let better = TestClockCatalog::default_high_grade();
        let better_port_id = PortIdentity::new(better.clock_identity(), PortNumber::new(1));
        let better_ds = better.foreign_ds(StepsRemoved::new(0));
        let records = [ForeignClockRecord::qualified(
            better_port_id,
            better_ds,
            LogInterval::new(0),
            now,
        )];

        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::from_records(&records)),
            *local_clock.identity(),
        );

        assert!(matches!(bmca.decision(), Some(BmcaDecision::Slave(_))));
    }

    #[test]
    fn sliding_window_qualification_requires_two_fast_announces() {
        let t0 = Instant::from_secs(0);
        let high = TestClockCatalog::default_high_grade();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // First Announce: record is created, still unqualified.
        assert!(record.qualified_ds().is_none());

        // Second Announce within the window makes it qualified.
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified_ds().is_some());
    }

    #[test]
    fn sliding_window_qualification_drops_on_slow_announces() {
        let t0 = Instant::from_secs(0);
        let high = TestClockCatalog::default_high_grade();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
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
        assert!(record.qualified_ds().is_some());

        // After a long gap, a single announce is not enough to stay qualified.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());
    }

    #[test]
    fn sliding_window_never_qualifies_with_one_annonce_per_window() {
        let t0 = Instant::from_secs(0);
        let high = TestClockCatalog::default_high_grade();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
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
        assert!(record.qualified_ds().is_none());

        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());

        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(15),
        );
        record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());
    }

    #[test]
    fn qualified_dataset_change_reports_updated() {
        let t0 = Instant::from_secs(0);
        let high = TestClockCatalog::default_high_grade();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
        let foreign_low =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));
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
        assert!(record.qualified_ds().is_some());

        // Change dataset while still qualified.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(2),
        );
        let status = record.update_from(&updated_record);
        assert_eq!(status, ForeignClockStatus::Updated);
        assert_eq!(record.qualified_ds(), Some(&foreign_low));
    }

    #[test]
    fn unqualified_dataset_change_does_not_report_updated() {
        let t0 = Instant::from_secs(0);
        let high = TestClockCatalog::default_high_grade();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
        let foreign_low =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));
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
        assert!(record.qualified_ds().is_none());

        // Second slow Announce with changed dataset: still unqualified and should report Unchanged.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(20),
        );
        let status = record.update_from(&updated_record);
        assert_eq!(status, ForeignClockStatus::Unchanged);
        assert!(record.qualified_ds().is_none());
    }

    #[test]
    fn sliding_window_edge_drops_qualification_but_not_stale() {
        let t0 = Instant::from_secs(0);
        let high = TestClockCatalog::default_high_grade();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
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
        assert!(record.qualified_ds().is_some());

        // Next Announce spaced just beyond the window drops qualification
        // but the record is not stale yet.
        let now = Instant::from_nanos(5_000_000_001);
        let updated_record =
            ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), now);
        let _ = record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());
        assert!(!record.is_stale(now));
    }

    #[test]
    fn test_foreign_clock_ordering() {
        let high = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let mid = TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0));
        let low = TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));

        assert!(high.better_than(&mid));
        assert!(!mid.better_than(&high));

        assert!(high.better_than(&low));
        assert!(!low.better_than(&high));

        assert!(mid.better_than(&low));
        assert!(!low.better_than(&mid));
    }

    #[test]
    fn test_foreign_clock_equality() {
        let c1 = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let c2 = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));

        assert!(!c1.better_than(&c2));
        assert!(!c2.better_than(&c1));
        assert_eq!(c1, c2);
    }

    #[test]
    fn foreign_clock_ds_compares_priority1_before_quality() {
        let a = TestClockCatalog::default_high_grade()
            .with_priority1(Priority1::new(10))
            .foreign_ds(StepsRemoved::new(0));
        let b = TestClockCatalog::atomic_grandmaster()
            .with_priority1(Priority1::new(20))
            .foreign_ds(StepsRemoved::new(0));

        // Atomic grandmaster looses againt default high grade because of lower priority1.
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_compares_priority2_after_quality() {
        let a = TestClockCatalog::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x00; 8]))
            .with_priority2(Priority2::new(10))
            .foreign_ds(StepsRemoved::new(0));
        let b = TestClockCatalog::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x01; 8]))
            .with_priority2(Priority2::new(20))
            .foreign_ds(StepsRemoved::new(0));

        // Same p1 and quality; lower p2 wins.
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_tiebreaks_on_identity_last() {
        let a = TestClockCatalog::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x00; 8]))
            .foreign_ds(StepsRemoved::new(0));
        let b = TestClockCatalog::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x01; 8]))
            .foreign_ds(StepsRemoved::new(0));

        // All fields equal except identity; lower identity better than higher.
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_prefers_lower_steps_removed_when_identity_equal() {
        let a = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(1));
        let b = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(3));

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn listening_bmca_prunes_stale_foreign_clocks_on_next_announce_reception() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let high = TestClockCatalog::default_high_grade();
        let mid = TestClockCatalog::default_mid_grade();
        let low = TestClockCatalog::default_low_grade_slave_only();
        let gm = TestClockCatalog::gps_grandmaster();

        let stale_records = vec![
            ForeignClockRecord::qualified(
                PortIdentity::new(high.clock_identity(), PortNumber::new(1)),
                high.foreign_ds(StepsRemoved::new(0)),
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::qualified(
                PortIdentity::new(mid.clock_identity(), PortNumber::new(1)),
                mid.foreign_ds(StepsRemoved::new(0)),
                LogInterval::new(0),
                Instant::from_secs(1),
            ),
            ForeignClockRecord::qualified(
                PortIdentity::new(mid.clock_identity(), PortNumber::new(1)),
                low.foreign_ds(StepsRemoved::new(0)),
                LogInterval::new(0),
                Instant::from_secs(2),
            ),
        ];
        let sorted_records = SortedForeignClockRecordsVec::from_records(&stale_records);

        let gm_port_id = PortIdentity::new(gm.clock_identity(), PortNumber::new(1));
        let gm_foreign = gm.foreign_ds(StepsRemoved::new(0));

        // Consider a new announce from a different foreign clock.
        let default_ds = TestClockCatalog::default_high_grade().default_ds();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(sorted_records),
        );
        bmca.consider(
            gm_port_id,
            gm_foreign,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        // Second announce qualifies the record.
        bmca.consider(
            gm_port_id,
            gm_foreign,
            LogInterval::new(0),
            Instant::from_secs(10),
        );

        assert_eq!(
            bmca.best_foreign.sorted_clock_records.len(),
            1,
            "stale foreign clock records should have been pruned"
        );
        let best = bmca
            .best_foreign
            .sorted_clock_records
            .best_qualified()
            .expect("record should be qualified after two announces");
        assert_eq!(best.source_port_identity(), &gm_port_id);
        assert_eq!(best.qualified_ds(), Some(&gm_foreign));
    }

    #[test]
    fn listening_bmca_gm_capable_local_with_no_qualified_foreign_is_undecided() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::gps_grandmaster().default_ds();
        let bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn listening_bmca_gm_capable_local_loses_tuple_returns_passive() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::gps_grandmaster().default_ds();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        // Foreign uses lower priority1 so it is better, even though clock class is worse.
        let foreign =
            TestClockCatalog::default_low_grade_slave_only().with_priority1(Priority1::new(1));
        let port_id = PortIdentity::new(foreign.clock_identity(), PortNumber::new(1));
        let foreign_strong = foreign.foreign_ds(StepsRemoved::new(0));

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

        assert_eq!(bmca.decision(), Some(BmcaDecision::Passive));
    }

    #[test]
    fn listening_bmca_gm_capable_local_better_than_foreign_returns_master_m1() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::gps_grandmaster().default_ds();
        let local_identity = *default_ds.identity();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        // Foreign is worse (higher priority1), so local GM-capable should become Master(M1).
        let foreign = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let port_id = PortIdentity::new(
            TestClockCatalog::default_high_grade().clock_identity(),
            PortNumber::new(1),
        );

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(),
            Some(BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0),
                local_identity,
            )))
        );
    }

    #[test]
    fn listening_bmca_non_gm_local_better_than_foreign_returns_master_m2() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_mid_grade().default_ds();
        let local_identity = *default_ds.identity();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        // Foreign is slightly worse quality; local should be better and take M2.
        let foreign =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));
        let port_id = PortIdentity::new(
            TestClockCatalog::default_low_grade_slave_only().clock_identity(),
            PortNumber::new(1),
        );

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(),
            Some(BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0),
                local_identity,
            )))
        );
    }

    #[test]
    fn listening_bmca_non_gm_local_loses_tuple_returns_slave() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_mid_grade().default_ds();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        let foreign = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let port_id = PortIdentity::new(
            TestClockCatalog::default_high_grade().clock_identity(),
            PortNumber::new(1),
        );

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(),
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id))),
        );
    }

    #[test]
    fn listening_bmca_recommends_slave_from_interleaved_announce_sequence() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_low_grade().default_ds();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        let high = TestClockCatalog::default_high_grade();
        let mid = TestClockCatalog::default_mid_grade();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
        let foreign_mid = mid.foreign_ds(StepsRemoved::new(0));

        let port_id_high = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let port_id_mid = PortIdentity::new(mid.clock_identity(), PortNumber::new(1));

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

        let decision = bmca.decision();

        assert_eq!(
            decision,
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id_high))),
        );
    }

    #[test]
    fn listening_bmca_recommends_slave_from_non_interleaved_announce_sequence() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_low_grade().default_ds();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        let high = TestClockCatalog::default_high_grade();
        let mid = TestClockCatalog::default_mid_grade();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
        let foreign_mid = mid.foreign_ds(StepsRemoved::new(0));

        let port_id_high = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let port_id_mid = PortIdentity::new(mid.clock_identity(), PortNumber::new(1));

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

        let decision = bmca.decision();

        assert_eq!(
            decision,
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id_high)))
        );
    }

    #[test]
    fn listening_bmca_undecided_when_no_announces_yet() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_mid_grade().default_ds();
        let bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn listening_bmca_undecided_when_no_qualified_clock_records_yet() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_mid_grade().default_ds();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        let foreign_high = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));

        bmca.consider(
            PortIdentity::fake(),
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn listening_bmca_undecided_when_only_single_announces_each() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockCatalog::default_mid_grade().default_ds();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
        );

        let high = TestClockCatalog::default_high_grade();
        let mid = TestClockCatalog::default_mid_grade();
        let low = TestClockCatalog::default_low_grade_slave_only();
        let foreign_high = high.foreign_ds(StepsRemoved::new(0));
        let foreign_mid = mid.foreign_ds(StepsRemoved::new(0));
        let foreign_low = low.foreign_ds(StepsRemoved::new(0));

        bmca.consider(
            PortIdentity::new(high.clock_identity(), PortNumber::new(0)),
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            PortIdentity::new(mid.clock_identity(), PortNumber::new(0)),
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            PortIdentity::new(low.clock_identity(), PortNumber::new(0)),
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(), None);
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
