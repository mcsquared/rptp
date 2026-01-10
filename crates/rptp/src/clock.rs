//! Clock-related domain concepts.
//!
//! This module models the “clock” vocabulary used throughout the `rptp` domain core:
//!
//! - **Identity**: [`ClockIdentity`] is the 8-byte identity used in PTP datasets and port identities.
//! - **Quality**: [`ClockClass`], [`ClockAccuracy`], and [`ClockQuality`] represent the fields used
//!   by BMCA comparisons.
//! - **Discipline boundary**: [`LocalClock`] composes a platform clock backend ([`SynchronizableClock`])
//!   with a [`Servo`] strategy. Port roles feed samples into that boundary and observe the resulting
//!   [`ServoState`] (locked/calibrating/unlocked).
//!
//! The traits in this module are intentionally small adapter surfaces: infrastructure can implement
//! them for system clocks, virtual clocks used in tests, or embedded clock backends.

use core::fmt::{Display, Formatter};
use core::ops::Range;

use crate::{
    servo::{Servo, ServoSample, ServoState},
    time::TimeStamp,
};

/// A PTP clock identity (IEEE 1588-2019 `clockIdentity`).
///
/// In PTP, clocks are identified by an 8-byte identity that appears in datasets and message
/// headers. `rptp` treats it as an opaque identifier with a stable ordering for comparisons and
/// map keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockIdentity {
    id: [u8; 8],
}

impl ClockIdentity {
    /// Create a new clock identity from its raw 8 bytes.
    pub const fn new(id: &[u8; 8]) -> Self {
        Self { id: *id }
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 8] {
        &self.id
    }
}

impl Display for ClockIdentity {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:02x}{:02x}{:02x}.{:02x}{:02x}.{:02x}{:02x}{:02x}",
            self.id[0],
            self.id[1],
            self.id[2],
            self.id[3],
            self.id[4],
            self.id[5],
            self.id[6],
            self.id[7]
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TimeScalePolicy {
    Mandatory(TimeScale),
    Any,
}

/// PTP clock class (`clockClass`).
///
/// This is a domain representation of the IEEE 1588-2019 clock class enumeration. Unknown or
/// reserved values are preserved as `Reserved(u8)` so they can round-trip and still participate in
/// deterministic ordering for BMCA comparisons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockClass {
    Reserved(u8),
    PrimaryReference,
    PrimaryHoldover,
    ApplicationSpecific,
    ApplicationHoldover,
    PrimaryHoldoverDegradedA,
    ApplicationHoldoverDegradedA,
    PrimaryHoldoverDegradedB,
    ApplicationHoldoverDegradedB,
    Default,
    V1Compatibility,
    SlaveOnly,
    AlternateProfile(u8),
}

impl ClockClass {
    /// Interpret a raw IEEE 1588 `clockClass` value.
    pub const fn new(value: u8) -> Self {
        match value {
            6 => Self::PrimaryReference,
            7 => Self::PrimaryHoldover,
            13 => Self::ApplicationSpecific,
            14 => Self::ApplicationHoldover,
            52 => Self::PrimaryHoldoverDegradedA,
            58 => Self::ApplicationHoldoverDegradedA,
            68..=122 | 133..=170 | 216..=232 => Self::AlternateProfile(value),
            187 => Self::PrimaryHoldoverDegradedB,
            193 => Self::ApplicationHoldoverDegradedB,
            248 => Self::Default,
            251 => Self::V1Compatibility,
            255 => Self::SlaveOnly,
            _ => Self::Reserved(value),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn time_scale_policy(&self) -> TimeScalePolicy {
        match self {
            Self::PrimaryReference
            | Self::PrimaryHoldover
            | Self::PrimaryHoldoverDegradedA
            | Self::PrimaryHoldoverDegradedB => TimeScalePolicy::Mandatory(TimeScale::Ptp),
            Self::ApplicationSpecific
            | Self::ApplicationHoldover
            | Self::ApplicationHoldoverDegradedA
            | Self::ApplicationHoldoverDegradedB => TimeScalePolicy::Mandatory(TimeScale::Arb),
            Self::Default
            | Self::V1Compatibility
            | Self::SlaveOnly
            | Self::Reserved(_)
            | Self::AlternateProfile(_) => TimeScalePolicy::Any,
        }
    }

    /// Return true if this clock class represents an authoritative clock. The spec talks a lot
    /// about this implicitly, but never names it explicitly, keeps it underdetermined in a way.
    ///
    /// So we introduce the term "authoritative" here to mean a clock class in the range 1..=127,
    /// which represents clocks that represent external time sources, primary references, or
    /// holdover states, clocks representing an atomic clock, GPS receivers, or other high-quality,
    /// high-authority time sources.
    ///
    /// See IEEE 1588-2019 §9.3.3; Fig. 33, where autoritative clocks are implicitly defined in the
    /// path to decision points M1 and P1 (D_0 is class 1..127).
    pub(crate) fn is_authoritative(&self) -> bool {
        (1..=127).contains(&self.as_u8())
    }

    /// Return the raw IEEE 1588 `clockClass` value.
    pub(crate) const fn as_u8(&self) -> u8 {
        match self {
            Self::Reserved(v) | Self::AlternateProfile(v) => *v,
            Self::PrimaryReference => 6,
            Self::PrimaryHoldover => 7,
            Self::ApplicationSpecific => 13,
            Self::ApplicationHoldover => 14,
            Self::PrimaryHoldoverDegradedA => 52,
            Self::ApplicationHoldoverDegradedA => 58,
            Self::PrimaryHoldoverDegradedB => 187,
            Self::ApplicationHoldoverDegradedB => 193,
            Self::Default => 248,
            Self::V1Compatibility => 251,
            Self::SlaveOnly => 255,
        }
    }
}

impl Ord for ClockClass {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let a = self.as_u8();
        let b = other.as_u8();
        a.cmp(&b)
    }
}

impl PartialOrd for ClockClass {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// PTP clock accuracy (`clockAccuracy`).
///
/// This is a domain representation of the IEEE 1588 clock accuracy enumeration. Unknown or
/// reserved values are preserved to keep ordering and round-tripping predictable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockAccuracy {
    Reserved(u8),
    Within25ns,
    Within100ns,
    Within250ns,
    Within1us,
    Within2_5us,
    Within10us,
    Within25us,
    Within100us,
    Within250us,
    Within1ms,
    Within2_5ms,
    Within10ms,
    Within25ms,
    Within100ms,
    Within250ms,
    Within1s,
    Within10s,
    GreaterThan10s,
    AlternateProfile(u8),
    Unknown,
}

impl ClockAccuracy {
    /// Interpret a raw IEEE 1588 `clockAccuracy` value.
    pub const fn new(value: u8) -> Self {
        match value {
            0x00..=0x1F => Self::Reserved(value),
            0x20 => Self::Within25ns,
            0x21 => Self::Within100ns,
            0x22 => Self::Within250ns,
            0x23 => Self::Within1us,
            0x24 => Self::Within2_5us,
            0x25 => Self::Within10us,
            0x26 => Self::Within25us,
            0x27 => Self::Within100us,
            0x28 => Self::Within250us,
            0x29 => Self::Within1ms,
            0x2A => Self::Within2_5ms,
            0x2B => Self::Within10ms,
            0x2C => Self::Within25ms,
            0x2D => Self::Within100ms,
            0x2E => Self::Within250ms,
            0x2F => Self::Within1s,
            0x30 => Self::Within10s,
            0x31 => Self::GreaterThan10s,
            0x32..=0x7F => Self::Reserved(value),
            0x80..=0xFD => Self::AlternateProfile(value),
            0xFE => Self::Unknown,
            0xFF => Self::Reserved(value),
        }
    }

    /// Return the raw IEEE 1588 `clockAccuracy` value.
    pub(crate) const fn as_u8(&self) -> u8 {
        match self {
            ClockAccuracy::Reserved(v) | ClockAccuracy::AlternateProfile(v) => *v,
            ClockAccuracy::Within25ns => 0x20,
            ClockAccuracy::Within100ns => 0x21,
            ClockAccuracy::Within250ns => 0x22,
            ClockAccuracy::Within1us => 0x23,
            ClockAccuracy::Within2_5us => 0x24,
            ClockAccuracy::Within10us => 0x25,
            ClockAccuracy::Within25us => 0x26,
            ClockAccuracy::Within100us => 0x27,
            ClockAccuracy::Within250us => 0x28,
            ClockAccuracy::Within1ms => 0x29,
            ClockAccuracy::Within2_5ms => 0x2A,
            ClockAccuracy::Within10ms => 0x2B,
            ClockAccuracy::Within25ms => 0x2C,
            ClockAccuracy::Within100ms => 0x2D,
            ClockAccuracy::Within250ms => 0x2E,
            ClockAccuracy::Within1s => 0x2F,
            ClockAccuracy::Within10s => 0x30,
            ClockAccuracy::GreaterThan10s => 0x31,
            ClockAccuracy::Unknown => 0xFE,
        }
    }
}

impl Ord for ClockAccuracy {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let a: u8 = self.as_u8();
        let b: u8 = other.as_u8();
        a.cmp(&b)
    }
}

impl PartialOrd for ClockAccuracy {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// PTP clock quality (`clockQuality`).
///
/// BMCA compares clocks based on `(clockClass, clockAccuracy, offsetScaledLogVariance)` and then
/// additional dataset fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockQuality {
    clock_class: ClockClass,
    clock_accuracy: ClockAccuracy,
    offset_scaled_log_variance: u16,
}

impl ClockQuality {
    const CLOCK_CLASS_OFFSET: usize = 0;
    const CLOCK_ACCURACY_OFFSET: usize = 1;
    const OFFSET_SCALED_LOG_VARIANCE_OFFSET: Range<usize> = 2..4;

    /// Create a new clock quality tuple.
    ///
    /// `offset_scaled_log_variance` is the raw IEEE 1588 field. `rptp` currently treats it as an
    /// ordered scalar for BMCA comparisons.
    pub const fn new(
        clock_class: ClockClass,
        clock_accuracy: ClockAccuracy,
        offset_scaled_log_variance: u16,
    ) -> Self {
        Self {
            clock_class,
            clock_accuracy,
            offset_scaled_log_variance,
        }
    }

    pub(crate) fn is_authoritative(&self) -> bool {
        self.clock_class.is_authoritative()
    }

    pub(crate) fn from_wire(buf: &[u8; 4]) -> Self {
        Self {
            clock_class: ClockClass::new(buf[Self::CLOCK_CLASS_OFFSET]),
            clock_accuracy: ClockAccuracy::new(buf[Self::CLOCK_ACCURACY_OFFSET]),
            offset_scaled_log_variance: u16::from_be_bytes([
                buf[Self::OFFSET_SCALED_LOG_VARIANCE_OFFSET.start],
                buf[Self::OFFSET_SCALED_LOG_VARIANCE_OFFSET.end - 1],
            ]),
        }
    }

    pub(crate) fn to_wire(self) -> [u8; 4] {
        let mut bytes = [0u8; 4];
        bytes[Self::CLOCK_CLASS_OFFSET] = self.clock_class.as_u8();
        bytes[Self::CLOCK_ACCURACY_OFFSET] = self.clock_accuracy.as_u8();
        bytes[Self::OFFSET_SCALED_LOG_VARIANCE_OFFSET]
            .copy_from_slice(&self.offset_scaled_log_variance.to_be_bytes());
        bytes
    }
}

impl Ord for ClockQuality {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let a = (
            &self.clock_class,
            &self.clock_accuracy,
            &self.offset_scaled_log_variance,
        );
        let b = (
            &other.clock_class,
            &other.clock_accuracy,
            &other.offset_scaled_log_variance,
        );

        a.cmp(&b)
    }
}

impl PartialOrd for ClockQuality {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A read-only view of a clock.
///
/// Infrastructure implements this trait for clocks that can provide the current PTP time and its
/// [`TimeScale`]. Ingress timestamping uses [`TimeStamp`] throughout the domain core.
pub trait Clock {
    /// Return the current clock time in its [`TimeScale`].
    fn now(&self) -> TimeStamp;
    /// Return the clock's time scale (PTP vs. arbitrary/application-defined).
    fn time_scale(&self) -> TimeScale;
}

/// A clock that can be disciplined (stepped or rate-adjusted).
///
/// This is the infrastructure boundary that servos operate against. A servo may:
/// - **step** the clock to a specific [`TimeStamp`], or
/// - **adjust** the clock rate by a multiplicative factor (`1.0` means “nominal rate”).
pub trait SynchronizableClock: Clock {
    /// Step (discontinuously set) the clock to `to`.
    fn step(&self, to: TimeStamp);
    /// Adjust the clock rate by a multiplicative factor.
    ///
    /// Implementations are expected to interpret `rate=1.0` as “no adjustment”, values greater than
    /// `1.0` as “run faster”, and values less than `1.0` as “run slower”.
    fn adjust(&self, rate: f64);
}

/// The local clock in a PTP node: identity + backend + servo.
///
/// `LocalClock` composes:
/// - a platform clock backend (`C: [`SynchronizableClock`]),
/// - a stable [`ClockIdentity`], and
/// - a [`Servo`] strategy that turns observed offset samples into clock actions.
///
/// Port roles feed samples into the clock discipline boundary via `discipline()` and then use the
/// returned [`ServoState`] in their state machine decisions.
pub struct LocalClock<C: SynchronizableClock> {
    clock: C,
    identity: ClockIdentity,
    servo: Servo,
}

impl<C: SynchronizableClock> LocalClock<C> {
    /// Create a new local clock wrapper around a backend clock and a servo strategy.
    pub fn new(clock: C, identity: ClockIdentity, servo: Servo) -> Self {
        Self {
            clock,
            identity,
            servo,
        }
    }

    pub fn identity(&self) -> &ClockIdentity {
        &self.identity
    }

    /// Return the current time reported by the backend clock.
    pub fn now(&self) -> TimeStamp {
        self.clock.now()
    }

    /// Return the time scale used by the backend clock.
    pub fn time_scale(&self) -> TimeScale {
        self.clock.time_scale()
    }

    /// Feed a new servo sample into the discipline pipeline.
    pub(crate) fn discipline(&self, sample: ServoSample) -> ServoState {
        self.servo.feed(&self.clock, sample)
    }
}

/// The PTP `stepsRemoved` dataset field.
///
/// This counts the number of communication paths between the local clock and the grandmaster. It
/// participates in BMCA comparisons and is carried in Announce messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StepsRemoved(u16);

impl StepsRemoved {
    /// Create a `StepsRemoved` value from the raw counter.
    pub fn new(steps_removed: u16) -> Self {
        Self(steps_removed)
    }

    pub(crate) fn as_u16(&self) -> u16 {
        self.0
    }

    pub(crate) fn to_be_bytes(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }
}

/// The time scale used by a clock.
///
/// PTP distinguishes between the PTP timescale and an arbitrary/application-defined timescale.
/// `rptp` carries this flag through Announce messages and clock backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeScale {
    Ptp,
    Arb,
}
