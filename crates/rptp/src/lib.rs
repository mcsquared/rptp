#![cfg_attr(not(any(test, feature = "std")), no_std)]

pub mod bmca;
pub mod buffer;
pub mod clock;
pub mod faulty;
pub mod initializing;
pub mod listening;
pub mod log;
pub mod master;
pub mod message;
pub mod port;
pub mod portstate;
pub mod premaster;
pub mod result;
pub mod slave;
pub mod sync;
pub mod time;
pub mod timestamping;
pub mod uncalibrated;

#[cfg(feature = "std")]
pub mod infra;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
