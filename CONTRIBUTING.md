# Contributing to `rptp`

Welcome, and thanks for your interest in contributing to `rptp`.

This project aims to implement IEEE 1588/PTP in a deliberate **domain-driven, object-rich, and test-guided** fashion. If that excites you, you’re in the right place.

For an overview on the general architecture, see `docs/architecture-overview.md`. For the more philosophical *why* behind the architecture, see `docs/design-philosophy.md`.  
This document focuses on the *how*: practical expectations for code and collaboration. The design philosophy is optional; contribution expectations live here.

For build and test instructions, see the Getting Started section in `README.md`.

---

## A Note from the Maintainer

This project is my first open-source project, my first serious Rust project, and my first time running a public PR-based workflow. I learned Rust through this project, and I'm still learning.

That means:

- Reviews might be a bit slow while I’m learning how to run them well.
- I may ask a lot of questions to understand your changes and their impact.
- I’m very open to suggestions on both code and process (how we review, how we structure issues/PRs, etc.).
- Please assume good intent — and I’ll do the same. See `CODE_OF_CONDUCT.md`.

If you’re experienced with open-source workflows, your patience and gentle guidance are very welcome. If you’re new as well, we’ll be learning together.

---

## Quick Checklist (Before Opening a PR)

- `cargo test`
- `cargo fmt`
- `cargo clippy --all-targets --all-features`

If your change affects infrastructure, integration, or interoperability, consider exercising one of these too (when available in your environment):

- Grafana demo: `crates/grafana-prometheus-demo` (see `README.md`)
- End-to-end smoke tests: `tests/e2e` (see `README.md`)

---

## How We Model the Domain

- **Behavior first, structure second**  
  Start from behavior: what does this object do, in which context, and which messages does it send or receive? Data and structure emerge from that.

- **Roles and responsibilities**  
  Model things in terms of roles (Clock, Port, Message, BMCA decision, …) with clear responsibilities. Prefer small, cohesive objects over big things, that do too much on their own.

- **Collaborating objects, not managers**  
  Avoid generic *Manager*, *Handler*, *Controller*, *Util* types. Let domain objects talk to each other through meaningful methods instead. When a command-and-control pattern really fits the domain (e.g. a state machine), it’s fine—but it should be the exception, not the default.

- **Domain vs. infrastructure**  
  Keep the *core* free from async runtimes, OS APIs, hardware details, and similar concerns. Those live at the edges (e.g. the Tokio-based daemon). The domain layer should be easy to test in isolation.

- **Composition over reuse**  
  Design for composability and clarity, not clever reusability. A small, well-named type that fits its context is better than a vague “generic” helper.

If you’re unsure where something belongs, err on the side of:

- Making domain behavior explicit and testable, and  
- Keeping infrastructure concerns at the boundaries.

---

## Tests and TDD-ish Expectations

This project is strongly influenced by *Growing Object-Oriented Software, Guided by Tests* and TDD in general. You don’t have to be a TDD purist, but:

- **Prefer test-first or test-guided changes**  
  New behavior should come with new tests. Bug fixes should include tests that fail before the fix and pass afterwards.

- **Keep tests close to the behavior**  
  Unit tests should exercise small objects and sharp boundaries. Integration and end-to-end tests should focus on meaningful PTP flows and scenarios (including interop with `ptp4l` when relevant).

- **Readable, intention-revealing tests**  
  Tests should tell a story about the domain, not just verify implementation details. It should be clear what scenario is being modeled and what outcome is expected.

If you’re not sure how to test something in this architecture, open an issue or draft PR—discussion is welcome.

---

## Rust Style and Structure

This project is also a learning journey in Rust. Idiomatic suggestions are welcome, as long as they don’t compromise the domain model.

General expectations:

- **Prefer expressive names over abbreviations**  
  Name types and methods in the language of the PTP domain and IEEE 1588, not in terms of implementation details.

- **Keep functions and methods small**  
  If you find yourself scrolling, consider extracting behavior into another object or method.

- **Encapsulation matters**  
  Use visibility and modules to protect invariants. Expose what collaborators genuinely need, not everything “just in case”.

- **Consistency with existing code**  
  Match the current layout, naming conventions, and error-handling patterns unless there’s a strong reason to refactor—and then, refactor in focused, well-tested steps.

---

## Making a Change

1. **Discuss first (recommended for larger changes)**  
   - Open a GitHub issue or draft PR describing the domain behavior you want to add or change.  
   - Tie it back to the PTP domain and, if relevant, to points from `docs/architecture-overview.md` or `docs/design-philosophy.md`.

2. **Model the behavior**  
   - Identify the objects and messages involved.  
   - Decide which layer they belong to (core domain vs. runtime/infrastructure).

3. **Write or extend tests**  
   - Add or adjust tests that express the new behavior.  
   - Run `cargo test` and keep the suite green.

4. **Implement the behavior**  
   - Keep changes focused and cohesive.  
   - Prefer several small, clear commits over a single large, opaque one.

5. **Polish and document**  
   - Update any relevant docs or inline explanations when you change behavior.  
   - Run `cargo fmt` and `cargo clippy --all-targets --all-features` when possible.
   - Mention design trade-offs or open questions in the PR description.

**If your change touches security or you suspect a vulnerability, please see SECURITY.md for responsible disclosure guidelines.**

---

## Pull Requests

When you open a PR, please include:

- **What** you changed (at a high level).  
- **Why**—preferably in domain terms (what part of PTP behavior this affects).  
- **How** the change aligns with the projects design goals (e.g., clearer roles, better encapsulation, improved testability).  
- **How to exercise it**, including which tests are most relevant.

It’s perfectly fine to open a **draft PR** early for feedback, especially if you’re exploring the model or unsure about where a piece belongs.

---

## Tone and Collaboration

The philosophical stance in this project is strong, but the collaboration style should be welcoming:

- Curiosity and questions are encouraged.  
- Disagreement about design is fine; we ground it in domain understanding, not personal taste.  
- It’s okay to say “I don’t know yet”—this codebase is an experiment, not a finished cathedral.
- Respect the project's `CODE_OF_CONDUCT.md`.

If you’re trying to work in the spirit of the design philosophy and you care about PTP and good software, you’re very welcome here.
