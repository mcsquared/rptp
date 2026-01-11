---
name: Bug Report
about: Report a bug or unexpected behavior
title: '[BUG] '
labels: bug
assignees: ''
---

## Bug Description

A clear and concise description of what the bug is.

## Steps to Reproduce

1. Step 1...
2. Step 2...
3. Step 3...

## Expected Behavior

What you expected to happen.

## Actual Behavior

What actually happened.

## Environment

- **rptp version:** (e.g., 0.1.0, commit SHA, or branch name)
- **Rust version:** (output of `rustc --version`)
- **Operating System:** (e.g., Ubuntu 22.04, macOS 13, etc.)
- **Target:** (e.g., x86_64-unknown-linux-gnu, thumbv7m-none-eabi)
- **Runtime:** (e.g., rptp-daemon with Tokio, embedded demo, custom)

## Additional Context

Add any other context about the problem here:
- Logs or error messages
- Network topology or configuration
- Relevant code snippets
- Links to related issues

## Domain Context (Optional)

If applicable, describe the bug in terms of PTP/IEEE 1588 behavior:
- Which port state or BMCA scenario is involved?
- What message flows are affected?
- Does this violate a specific part of the IEEE 1588 spec?

## Note

If you believe this might be a security vulnerability, please **do not** open a public issue. Instead, follow the guidance in [SECURITY.md](../../SECURITY.md).
