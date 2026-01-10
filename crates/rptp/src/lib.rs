#![cfg_attr(not(any(test, feature = "std")), no_std)]

pub mod bmca;
pub mod clock;
pub mod e2e;
pub mod log;
pub mod message;
pub mod ordinary;
pub mod port;
pub mod portstate;
pub mod profile;
pub mod result;
pub mod servo;
pub mod time;
pub mod timestamping;
pub mod wire;

mod faulty;
mod initializing;
mod listening;
mod master;
mod premaster;
mod slave;
mod uncalibrated;

#[cfg(feature = "std")]
pub mod infra;

#[cfg(feature = "heapless-storage")]
pub mod heapless;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
