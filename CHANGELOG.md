# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with 0.x semantics (breaking changes may occur at any time).

## [Unreleased]

### Initial Implementation

This is the initial public release of rptp, an early-stage, domain-driven IEEE 1588/PTP implementation in Rust.

#### Added

**Core Domain (`crates/rptp`):**
- Domain-rich port state machine with distinct types for each state (Listening, Master, Slave, Uncalibrated, PreMaster, Faulty, Initializing)
- Best Master Clock Algorithm (BMCA) with role-based decision tracking (ListeningBmca, ParentTrackingBmca, GrandMasterTrackingBmca)
- End-to-end delay mechanism with sync and delay exchange tracking
- Servo implementations: SteppingServo and PiServo with staged calibration
- Clock discipline pipeline: message exchanges → servo samples → clock adjustments
- PTP message parsing and serialization (Announce, Sync, FollowUp, DelayReq, DelayResp)
- Port profile abstraction for timing policies and port state assembly
- Virtual and fake clock implementations for deterministic testing
- no_std support with heapless storage option for embedded targets
- 245+ unit and integration tests

**Infrastructure Layers:**
- `rptp-daemon`: Tokio-based runtime adapter with UDP multicast support
- `rptp-embedded-demo`: Bare-metal Cortex-M demo targeting thumbv7m-none-eabi (12 MHz, 64 KiB MCU)
- `grafana-prometheus-demo`: Live visualization of clock sync behavior with Grafana dashboard
- End-to-end acceptance tests with ptp4l interoperability (Docker-based)
- QEMU-based embedded testing

**Documentation:**
- Comprehensive README with project status, getting started guide, and roadmap
- Architecture overview explaining hexagonal architecture and domain collaborators
- Design philosophy document on Object Thinking, DDD, and test-guided design
- Contributing guidelines with modeling expectations and TDD guidance
- Security policy with vulnerability reporting process
- Code of Conduct (Rust CoC)

**Project Infrastructure:**
- Dual MIT/Apache-2.0 licensing
- CI workflow with tests, clippy, and no_std builds
- Issue and PR templates
- CHANGELOG (this file)

#### Known Limitations

**Protocol Coverage:**
- Single-domain, single-port topologies (multi-port foundations in place but incomplete)
- Basic PTPv2 message flows only (BMCA, port state machine, E2E delay)
- No correction field processing
- No PTP security mechanisms (authentication, encryption)
- No hardware timestamping or PHC integration
- FaultyPort state is a stub (no recovery paths)

**Servo and Time Handling:**
- Coarse origin timestamp handling in Announce and two-step Sync messages
- Limited UTC/timescale handling (leap seconds, UTC offset fields)
- PI servo tuning is basic (kp/ki not adjusted for Sync interval)
- No drift clamping in PI servo

**Infrastructure:**
- Software timestamping only (no hardware/PHC support)
- Tokio daemon is a thin test harness, not production-ready
- No operational tooling or configuration management
- Limited observability beyond basic logging and Grafana demo

See README.md "What this project is not (yet)" for detailed scope.

#### Design Decisions

- **Domain-first approach:** Core PTP model is runtime-agnostic, portable (no_std), and infrastructure-independent
- **Behavior-rich objects:** Port states, BMCA roles, and servo strategies are modeled as distinct collaborating types rather than procedures on data
- **Constructor injection:** Objects are fully initialized at construction time with all required collaborators
- **Fail-fast core, fail-safe boundaries:** Domain core enforces invariants loudly; infrastructure gracefully handles malformed input
- **Test-guided design:** 245+ tests drive the domain model; tests live alongside domain code
- **Ports as roles:** Port state machine modeled as role changes rather than mutable state fields

#### Security Warnings

⚠️ **This is experimental software. Do not use in production.**

- Not security-audited
- No PTP security mechanisms implemented
- Must not be exposed to untrusted networks
- Suitable only for controlled test environments and experiments

See SECURITY.md for vulnerability reporting.

---

<!-- Template for future releases:

## [X.Y.Z] - YYYY-MM-DD

### Added
- New features

### Changed
- Changes in existing functionality

### Deprecated
- Soon-to-be removed features

### Removed
- Removed features

### Fixed
- Bug fixes

### Security
- Security fixes

-->
