# Security Policy

## Project status and intended use

`rptp` is an **experimental, early-stage project** aiming at the implementation of IEEE 1588-2019/PTP.

- It is **not ready for production use**.
- It has **not been audited** for security.
- It currently implements **no PTP security mechanisms** (such as authentication, certificates, or encryption as described in IEEE 1588 and related profiles).

Until those mechanisms are designed, implemented, and reviewed, you should assume:

- Traffic processed by `rptp` **can be spoofed or manipulated** by an attacker on the network.
- The daemon **must not be exposed to untrusted or hostile networks**.
- The crate and daemon are suitable only for **experiments, learning, and controlled test environments**.

If you need a hardened, production-ready PTP stack today, you should use an established implementation instead.

## Supported versions

At this stage the project follows **0.x semantics**:

- There are **no supported or LTS branches**.
- Breaking changes may occur at any time.
- Security fixes will be applied on a **best-effort** basis to the latest `main` branch and most recent release.

## Reporting a vulnerability

If you believe you have found a security issue in `rptp` or `rptp-daemon`:

1. **Do not open a public GitHub issue with exploit details.**
2. Instead, please contact the maintainer privately at:

   - **Email:** `sec-rptp@posteo.com`  
   - Or use GitHub’s “Report a vulnerability” feature if available for this repository.  

When you report, please include:

- A description of the issue and its potential impact.
- Steps to reproduce (if possible).
- Any relevant environment details (OS, Rust version, configuration).

I will:

- Acknowledge receipt as soon as reasonably possible.
- Investigate the issue.
- Coordinate a fix, mitigation, and, if appropriate, a public advisory.

Because this is a one-person, experimental project, there are **no formal response-time guarantees**, but security reports are taken seriously.

## Non-security issues

For non-security bugs, design questions, or feature requests:

- Please use the regular **GitHub Issues** on the repository.
- When in doubt, you can open an issue and mark it clearly if you think it *might* have security implications; I can help route it appropriately.

## Future security work

Security features for IEEE 1588/PTP (such as authentication, certificates, and encryption) are on the **long-term roadmap**, but are **not implemented yet**.

Contributions, discussions, and design proposals around:

- threat models and deployment assumptions,
- secure profile support,
- and integration with existing PTP security mechanisms

are very welcome — please open an issue to start that conversation.
