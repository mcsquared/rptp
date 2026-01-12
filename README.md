# rptp

[![CI](https://github.com/mcsquared/rptp/actions/workflows/ci.yml/badge.svg)](https://github.com/mcsquared/rptp/actions/workflows/ci.yml)

*An Object-Oriented and Domain-Driven PTPv2 core in Rust*

---

## Introduction

`rptp` is a **work in progress**: an early, exploratory, object-oriented, and domain-driven approach on IEEE 1588-2019, better known as Precision Time Protocol.

Expect incomplete protocol coverage, rough edges, and frequent refactors as domain knowledge and understanding deepens, the model matures and tests grow.

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
- A **test-heavy, portable core**: most tests live next to the domain logic in the `rptp` crate, and the core runs without async/runtime dependencies so it can sit behind different runtimes and timestamping backends (Tokio today, embedded/PHC or bare metal tomorrow).  

### What this project is not (yet)

- A complete implementation of all PTPv2 features and profiles.  
- A hardened, production-grade daemon with hardware timestamping or PHC integration.  
- A performance-tuned stack with stable public APIs, configuration formats, and operational tooling.  
- A drop-in replacement for existing PTP daemons.  

Expect **0.x** semantics: the design and APIs will evolve as the model matures.

---

## Status & Agenda

This is a **work-in-progress, early-stage, experimental project**, with a deliberate focus on a testable **domain core first**:

- APIs are **unstable** and may change in 0.x releases.
- The design is actively evolving as domain understanding deepens.
- The core model is ahead of the runtime: `rptp` evolves first, `rptp-daemon` follows.
- It has **not been audited for security** and currently implements **no IEEE 1588/PTP security mechanisms** (authentication, certificates, encryption, secure profiles, …).

You **must not** treat `rptp` or `rptp-daemon` as a hardened, production-ready PTP stack.

- Use it only in **controlled test environments**, labs, or experiments.
- Do not expose the daemon directly to **untrusted or hostile networks**.

### Current Status

- Core PTPv2 message flows: BMCA, port state machine, and end-to-end delay mechanism for a single-port, single-domain topology.  
- A rich, **core-centered test suite: 200+** unit and integration tests around the domain logic; a small Tokio/e2e layer that is just large enough to drive the core.  
- Virtual clocks and software timestamping for simulation & tests.  
- A deliberately rough but effective **bare-metal QEMU demo harness** (`crates/rptp-embedded-demo`) that runs as grandmaster on a 12 MHz Cortex‑M target, still fits within a 64‑KiB MCU budget, and is exercised by Dockerized e2e tests against `ptp4l`. It exists to keep the embedded trajectory honest, not as production firmware yet.  
- Thin, Tokio-driven integration that wires the core to UDP multicast sockets and system timers for tests and experiments.  

### Near‑Term Focus (0.x)

These are concrete areas of work planned for the near term, before the project claims broader feature coverage or stability:

- **Broader protocol field coverage**  
  Improve evaluation of message fields, including coarse origin timestamps in Announce and two‑step Sync messages, and other currently underused fields.

- **Timescale and UTC handling**  
  Make the PTP timescale handling explicit and correct, including leap seconds, UTC offset fields, and related flags.

- **Multi-domain and multi-port setups**  
  Extend support beyond single-domain / single-port topologies to validate and exercise the `DomainMessage` ↔ `PortMap` collaboration in more realistic deployments, including multi-port configurations and BMCA behavior for boundary clock ports.

- **PI servo tuning and constraints**  
  Tune the PI servo (`kp`/`ki`) based on the Sync log message interval, clamp drift in the PI servo, and derive parameters such as `min_delta` from `ki` rather than ad‑hoc choices.

- **FaultyPort transitions, error handling and recovery**  
  At the moment, FaultyPort is a dead end stub in the state machine. The implementation shall support graceful fault transitions, error logging and recovery paths soon.  

- **Stronger acceptance tests criteria.**  
  Tighten interoperability and acceptance criteria against `ptp4l`, including timescale awareness and correctness, not just message flow compatibility.

### On the Agenda

These are planned directions and areas of interest, not yet hard commitments; progress will depend on available time and funding.

- Deeper interoperability work and long-running scenarios beyond the near-term acceptance tests against `ptp4l`.  
- Evaluation and processing of the correction field and other profile-specific fields once the core flows are stable.  
- OS-level and hardware timestamping paths, including PHC-backed system and embedded clock backends for constrained or specialized targets.  
- Expanded protocol coverage and support for additional PTP profiles beyond the default.  
- Stronger runtime and infrastructure abstractions, configuration surfaces, and observability as `rptp-daemon` evolves from a thin test harness into a production-grade daemon.  
- Systematic benchmarking and profiling to understand performance envelopes and trade-offs.  
- A richer and more aggressive testing strategy: property-based testing (e.g. `proptest`), fuzzing, mutation testing, and pcap replay support for real-world captures (e.g. from Wireshark), covering edge cases, failure modes, packet loss/reordering, and other conditions beyond the initial interoperability and timescale checks.  

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
# build the whole workspace
cargo build

# Run the full test suite for the workspace
cargo test

# Format the code (optional, but recommended)
cargo fmt

# Run clippy lints (if installed)
cargo clippy --all-targets --all-features
```

### See it in Action

There are currently two ways to run the code: to run the e2e acceptance tests, or to run the grafana-prometheus demo.

#### End-to-End Acceptence ("Smoke") Tests

From the repository root:
```sh
# To speed up the test run, it's recommended to build the docker images once first
docker build -f tests/e2e/docker/Dockerfile \
  --build-arg MANIFEST_DIR=tests/e2e/scenarios -t rptp-e2e-tests .
docker build -f tests/e2e/docker/qemu.Dockerfile \
  --build-arg MANIFEST_DIR=crates/rptp-embedded-demo -t rptp-e2e-test:qemu .
docker build -f tests/e2e/docker/ptp4l.Dockerfile -t rptp-e2e-test:ptp4l tests/e2e/docker

# Run the e2e acceptance tests
cargo test -p rptp-e2e-tests -- --nocapture --ignored
```

#### Grafana & Prometheus Demo

This demo was introduced as a fast and easy first way to provide a graphical live view of clock sync and servo behaviour.
It sets up two ordinary clocks connected through a software loopback, everything in-process and in-memory. This demo can be seen as an early feedback tool too, besides the other tests.

```sh
cd crates/grafana-prometheus-demo

# Start up the Demo
docker compose up --build

# Open http://localhost:3000/. After logging in with admin-admin you should
# see a Grafana dashboard with a "Offset From Master" live graph

# Shutdown the Demo (from the same directory)
docker compose down
```

After starting the demo, a Grafana dashboard should be available on localhost (`http://localhost:3000`, admin:admin). The dashboard has a pre-configured graph view for the master offset. After approximately 90s the demo simulates two consecutive clock steps. The first one big enough, to force a hard clock step on the slave side too. The second step is below the step-threshold, resulting in clock slewing behaviour.


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
- and a clean, maintainable PTP implementation,
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
