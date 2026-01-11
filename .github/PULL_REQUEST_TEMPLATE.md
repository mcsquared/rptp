# Pull Request

## What Changed

<!-- Provide a high-level summary of the changes in this PR -->

## Why

<!-- Explain the motivation for these changes, preferably in domain terms -->
<!-- Example: "Adds support for two-step Sync messages to complete the E2E delay mechanism" -->

## How This Aligns with Design Goals

<!-- Explain how this change aligns with the project's architecture and design philosophy -->
<!-- Consider: -->
<!-- - Does it improve the domain model (clearer roles, better encapsulation, improved testability)? -->
<!-- - Does it maintain separation between core and infrastructure? -->
<!-- - Does it follow behavior-first, constructor injection, or other design principles? -->
<!-- See CONTRIBUTING.md and architecture documents under docs/ for guidance -->

## How to Exercise It

<!-- Which tests are most relevant? -->
<!-- Are there new test scenarios? -->
<!-- How can reviewers verify the behavior? -->

```bash
# Example commands to run relevant tests
cargo test -p rptp test_name
```

## Checklist

Before submitting this PR, please ensure:

- [ ] `cargo test` passes
- [ ] `cargo fmt` has been run
- [ ] `cargo clippy --all-targets --all-features` passes with no warnings
- [ ] New behavior has tests (or this is a documentation or refactoring change)
- [ ] Public API changes have doc comments
- [ ] Changes are documented in relevant docs (if applicable)

## Additional Context

<!-- Any other information that would help reviewers -->
<!-- - Design trade-offs considered -->
<!-- - Open questions or areas where feedback is especially wanted -->
<!-- - Related issues or PRs -->

## Type of Change

<!-- Check all that apply -->

- [ ] Bug fix (non-breaking change which fixes an issue)
- [ ] New feature (non-breaking change which adds functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality to not work as expected)
- [ ] Refactoring (no functional changes, improves code structure)
- [ ] Documentation update
- [ ] Test improvements
- [ ] Infrastructure/tooling changes

---

**Note:** If this PR addresses a security vulnerability, please ensure you've followed the process in [SECURITY.md](../SECURITY.md) before opening a public PR.
