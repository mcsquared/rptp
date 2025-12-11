# rptp

[![CI](https://github.com/mcsquared/rptp/actions/workflows/ci.yml/badge.svg)](https://github.com/mcsquared/rptp/actions/workflows/ci.yml)

*A domain-driven IEEE 1588/PTP core in Rust.*

---

## Introduction

`rptp` is an early domain-driven implementation of IEEE 1588‑2019 / PTP in Rust, exploratory and growing.

This repository contains:

- `crates/rptp` – the core PTP domain model (no async/runtime dependencies). 
- `crates/rptp-daemon` – a thin, Tokio-based infrastructure layer that currently serves as a test harness and playground for the core: it wires `rptp` to UDP multicast sockets, system timers and virtual clocks, and is expected to evolve into a full daemon as the model stabilizes.
- `crates/rptp-embedded-demo` – an experimental bare-metal harness for `rptp` targeting `thumbv7m-none-eabi` and a 12 MHz, 64‑KiB‑class Cortex‑M MCU; see `crates/rptp-embedded-demo/README.md` for building, QEMU usage, and Docker-based e2e tests.
- `tests/e2e` – end-to-end acceptance "smoke" test scenarios, including QEMU + `ptp4l` interoperability.

### How it started

This project began as:

- an exploration of **Object Thinking**, **Domain-Driven Design** and **Test-Driven Development** in a systems- and embedded domain  
- a personal **first serious Rust learning project**  
- a question: *what if PTP were modeled as collaborating objects, not procedures acting on anemic data bags and shared state?*

### How it’s going

- A growing **domain-driven model** of PTP: behavior-rich objects (clocks, ports, BMCA roles, servos, delay mechanisms) that talk in the language of the IEEE 1588 standard.  
- A **test-heavy core**: the vast majority of tests live in the `rptp` crate (clocks, BMCA, port state machine, sync/delay, servo). The Tokio layer has only the minimal tests needed to drive and probe the core.  
- A **portable foundation**: the core runs without async/runtime dependencies and is designed to sit behind different runtimes and timestamping backends (Tokio today, embedded/PHC or bare metal tomorrow).  
- A first **bare-metal embedded demo** (`crates/rptp-embedded-demo`) that compiles for `thumbv7m-none-eabi`, currently fits comfortably into a 12 MHz, 64‑KiB‑RAM‑class Cortex‑M MCU, and passes a QEMU + Docker-based smoke test against `ptp4l` (embedded node as grandmaster, `ptp4l` as slave). This is an early but important milestone for the embedded trajectory of the project.  
- An intentionally **thin Tokio adapter**: `crates/rptp-daemon` is, for now, a test harness and playground for the domain core: it exists to exercise the model end-to-end and drive experiments, not as a production daemon yet. It is expected to grow into a full daemon as the domain model and the Rust/Tokio integration mature.  

### What this project is not (yet)

- A complete implementation of all PTPv2 features and profiles.  
- A hardened, production-grade daemon with hardware timestamping or PHC integration.  
- A performance-tuned stack with stable public APIs, configuration formats, and operational tooling.  
- A drop-in replacement for existing PTP daemons.  

Expect **0.x** semantics: the design and APIs will evolve as the model matures.

---

## Status & Agenda

This is an **early-stage, experimental project**, with a deliberate focus on a testable **domain core first**:

- APIs are **unstable** and may change in 0.x releases.
- The design is actively evolving as domain understanding deepens.
- The core model is ahead of the runtime: `rptp` evolves first, `rptp-daemon` follows.
- It has **not been audited for security** and currently implements **no IEEE 1588/PTP security mechanisms** (authentication, certificates, encryption, secure profiles, …).

You **must not** treat `rptp` or `rptp-daemon` as a hardened, production-ready PTP stack.

- Use it only in **controlled test environments**, labs, or experiments.
- Do not expose the daemon directly to **untrusted or hostile networks**.

### Current Status

- Core PTPv2 message flows: BMCA, port state machine, and end-to-end delay mechanism for a single-port, single-domain topology.  
- A rich, **core-centered test suite**: 200+ unit and integration tests around the domain logic; a small Tokio/e2e layer that is just large enough to drive the core.  
- Virtual clocks and software timestamping for simulation & tests.  
- A deliberately rough but effective **bare-metal QEMU demo harness** (`crates/rptp-embedded-demo`) that runs as grandmaster on a 12 MHz Cortex‑M target, still fits within a 64‑KiB MCU budget, and is exercised by Dockerized e2e tests against `ptp4l`. It exists to keep the embedded trajectory honest, not as production firmware yet.  
- Thin, Tokio-driven integration that wires the core to UDP multicast sockets and system timers for tests and experiments.  
- Promising early locality/performance behavior in the core, with the hot paths kept free from unnecessary allocation, infrastructure or framework concerns.  

### On the Agenda

These are planned directions and areas of interest, not yet hard commitments; progress will depend on available time and funding.

- End-to-end interoperability and testing against `ptp4l`  
- Exploration of OS-level and hardware timestamping paths  
- Expanded protocol coverage and profile support  
- Broader testing for edge cases, failure modes, and error conditions  
- Stronger runtime and infrastructure abstractions  
- Evolving `rptp-daemon` from a thin test harness into a production-grade daemon as the model and understanding solidify  
- Embedded and system/PHC clock backends  
- Systematic benchmarking & profiling  

For details on how to report potential vulnerabilities or security-sensitive issues, see `SECURITY.md`.

---

## Getting Started

### Prerequisites

- A recent Rust 2024 edition toolchain (stable).  
- A Unix-like environment for running the daemon examples and tests.
- A docker environment for end-to-end acceptance testing

### Build and test the core

From the repository root:

```bash
# Run the full test suite for the workspace
cargo test

# Format the code (optional, but recommended)
cargo fmt

# Run clippy lints (if installed)
cargo clippy --all-targets --all-features
```

### Run the Tokio-based daemon

`rptp-daemon` currently wires the core to:

- a virtual clock (for experimentation),
- UDP multicast sockets for PTP event and general messages,
- a small Tokio event loop.

To run it:

```bash
cargo run -p rptp-daemon
```

By design this is still a **playground**:

- It is useful for experimenting with message flows and time synchronization behavior.
- It is **not** a production-grade daemon, and its configuration and feature set are intentionally limited for now.

---

## Design & Architecture

If you’re interested in the modeling approach:

- `docs/architecture-overview.md` – how clocks, ports, port states, BMCA, and messages fit together, and how the daemon talks to the core.
- `docs/design-philosophy.md` – the underlying ideas from Object Thinking, DDD, and test-guided design that shape the code.

A few high-level points:

- The core is modeled as **collaborating objects** (ports, states, clocks, BMCA roles) rather than procedures acting on shared state.
- The **port state machine** follows IEEE 1588 state diagrams but is expressed in terms of explicit state types and transitions.
- The runtime/infra layer (Tokio, sockets, timestamping backends) lives at the edges, so the domain model remains **portable and testable**.

---

## Support & Collaboration

Building a production-ready, clean, portable PTP implementation is a serious project.

This work may be especially relevant if your organization operates in:

- professional broadcasting environments  
- industrial automation  
- navigation, nautics, avionics  
- telecom or time-sensitive networking  

If you see potential in this approach, collaboration, sponsorship, or employment can meaningfully accelerate progress.

I’m open to:

- joint experiments and proof-of-concept work  
- funded feature development and roadmap partnerships  
- long-term collaboration or roles where **object thinking, domain-driven, and test-driven design** in systems or embedded development is a shared commitment  

If you think this aligns with your needs or interests, please get in touch via the usual issue/PR channels or directly.

---

## Contributing

Contributions, feedback, and design discussions are very welcome.

- Please read `CONTRIBUTING.md` for:
  - modeling guidelines (behavior-first, roles, and responsibilities),
  - expectations around tests and refactoring,
  - how to frame changes in domain terms.
- The project follows the Rust Code of Conduct as documented in `CODE_OF_CONDUCT.md`.
- For security-sensitive issues, please follow the guidance in `SECURITY.md` instead of opening a public issue.

If you’re excited about:

- domain-driven, object-rich modeling,
- test-guided design in systems / embedded contexts,
- and clean, maintainable PTP implementation,
you’re very much invited to participate.

---

## Trademarks

“IEEE” and “IEEE 1588” are trademarks of the Institute of Electrical and Electronics Engineers, Inc.  
This project is not affiliated with, endorsed by, or sponsored by IEEE.

All other product names, trademarks, and registered trademarks are the property of their respective owners. Use of them here is for identification and reference only.

---

## License

This project is licensed under either of

- the [MIT license](LICENSE-MIT), or
- the [Apache License, Version 2.0](LICENSE-APACHE),

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
