#![cfg_attr(not(any(test, feature = "std")), no_std)]
//! `rptp` is an early-stage, domain-driven IEEE 1588-2019 / PTP core.
//!
//! This crate is a **work in progress**: protocol coverage is incomplete, and the public API may
//! change at any time during 0.x development.
//!
//! The focus is a portable, testable domain model: ports, state transitions, BMCA decisions, and
//! clock/servo behavior. Runtime and platform concerns (async runtimes, sockets, timers, hardware
//! timestamping) are intentionally kept at the edges.
//!
//! ## Where to start
//!
//! - Message ingress and dispatch: [`message::MessageIngress`]
//! - Port state machine (IEEE 1588 state chart as objects): [`portstate::PortState`]
//! - BMCA logic and foreign master tracking: [`bmca`]
//! - Building an ordinary clock: [`ordinary::OrdinaryClock`]
//! - Time and intervals: [`time`]
//!
//! For an end-to-end reference wiring, see the `rptp-daemon` crate in this repository.
//!
//! ## `no_std`
//!
//! The core supports `no_std` when the `std` feature is disabled.
//! For example, depending on `rptp` with `default-features = false` builds the core as `no_std`,
//! and `heapless-storage` can be enabled for constrained targets.
//! In Cargo.toml this typically looks like:
//! `rptp = { version = "...", default-features = false, features = ["heapless-storage"] }`
//!
//! # Feature flags
//!
//! - `std` (default): enables standard-library support in the core and `std`-based adapters.
//! - `heapless-storage` (default): enables heapless storage helpers for constrained targets.
//! - `test-support`: enables extra test helpers (fake clocks/ports, etc.).

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
mod passive;
mod premaster;
mod slave;
mod uncalibrated;

#[cfg(feature = "std")]
pub mod infra;

#[cfg(feature = "heapless-storage")]
pub mod heapless;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
