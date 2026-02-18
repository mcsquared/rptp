# AGENTS.md

Repository-level agent instructions for this project.

## Purpose

Work in this repo should stay close to the PTP domain and its language.
Prefer problem-space modeling and behaviour discovery before implementation details.

## Source of truth

Use these docs first when reasoning about architecture and style:

- `README.md`
- `CONTRIBUTING.md`
- `docs/architecture-overview.md`
- `docs/design-philosophy.md`

If there is tension between quick implementation and these docs, prefer the docs and explain tradeoffs.

## Abstract Object Discovery Story

- Treat tests as a practical application of "simulation drives object discovery"
- Try to identify hidden concepts in the domain in a DDD-sense and make them explicit (e.g. ListeningBmca)

## Core modeling stance

- Model behaviour-rich, collaborating objects.
- Prefer domain roles and messages over procedural command-and-control flows.
- Keep domain language explicit and intention-revealing.
- Keep domain core (`crates/rptp`) runtime- and infrastructure-agnostic.
- Keep infrastructure concerns at edges (`crates/rptp-daemon`, demos, adapters).

## Problem-space-first workflow

Before writing code for non-trivial changes:

1. State the domain behaviour to change in PTP terms.
2. Identify collaborators, responsibilities, and boundaries.
3. Define the smallest tests that express the intended behaviour.
4. Implement the minimal cohesive change.
5. Re-check if design stayed in domain language and avoided accidental infrastructure coupling.

Do not jump straight into framework or low-level implementation detail unless explicitly requested.

## Object and design guidelines

- Prefer small, focused objects with clear responsibilities.
- Prefer composition over broad inheritance-style abstractions.
- Prefer composable wrappers/decorators for orthogonal behaviour.
- Avoid generic `Manager`, `Handler`, `Controller`, `Util`, `Helper` types unless they are true domain concepts.
- Keep methods short and cohesive; split when a method mixes responsibilities.
- Express invariants through types and module boundaries where possible.

## Encapsulation and state

- Avoid trivial getters/setters that only expose or mutate internal state without domain meaning.
- Expose behaviour, not internal representation.
- Do not publish fields "just in case"; expose only what collaborators need.
- Avoid anemic data bags plus external procedures.

## Construction and dependencies

- Prefer constructor injection for required collaborators.
- Keep constructors code-light and side-effect free when possible.
- Create valid, ready-to-use objects; avoid multi-step init sequences.
- Ask for confirmation before adding new production dependencies.

## Rust-specific boundaries

- Keep the `crates/rptp` domain core crate independent of async runtimes, synchronization primitves and OS specifics.
- Do not pull Tokio/runtime details into the domain core.
- Avoid global mutable state; prefer explicit collaborator wiring.

## Code size & verbosity bias

- Prefer the smallest change that satisfies behaviour and tests.
- Avoid speculative abstractions.
- Do not introduce additional layers/types unless they reduce domain complexity.
- If naming drifts into technical/framework language, propose domain-aligned alternatives.

Refactorings should be:
- behaviour-preserving
- justified in domain/design terms
- incremental rather than sweeping

## Testing expectations

- New behaviour should come with tests.
- Bug fixes should include a test that fails before and passes after.
- Keep tests close to behaviour when possible.
- Prefer tests that describe domain scenarios, not implementation trivia.

After relevant code changes, run:

- `cargo fmt`
- `cargo clippy --all-targets --all-features`
- `cargo test`

If full runs are too heavy, run the narrowest relevant subset first and state what was not run.

## Change control

Ask for confirmation before:

- adding new production dependencies
- broad cross-crate refactors
- changing public APIs in ways that affect multiple crates
- introducing new architectural patterns that conflict with current docs

## Communication style for Codex

- Be concise, direct, and technical.
- Explain reasoning in terms of domain behaviour and tradeoffs.
- Highlight assumptions and uncertainty early.
- If requirements are ambiguous, ask focused clarification questions.

## Definition of done for a typical change

- Behaviour is clear in domain terms.
- Design keeps strong encapsulation and object collaboration.
- Tests cover the new/changed behaviour.
- Formatting/lints/tests were run (or explicitly scoped with rationale).
- Notes include tradeoffs and follow-up risks if any.
