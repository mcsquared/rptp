# rptp-embedded-demo

An experimental bare‑metal harness for the `rptp` core. It targets the
`thumbv7m-none-eabi` Cortex‑M profile and wires the PTP port state machine to a
`smoltcp` UDP stack plus a SysTick‑driven timer loop. The current QEMU setup
models a 12 MHz, 64‑KiB‑class Cortex‑M3 (`lm3s6965evb`). The same binary is
used both for local QEMU experiments and for the Docker‑based end‑to‑end tests
in `tests/e2e`.

## Purpose

This crate is intentionally _not_ a production‑ready embedded application or a
reference design. It is a testing tool: just polished enough to answer two
questions:

1. Does the `rptp` core currently fit into a small MCU‑class target?
2. Does a simple “smoke test” setup still behave like a viable PTP grandmaster?

From a TDD perspective, this is “just enough” implementation to provide those
signals. It is expected to be rough around the edges, and its design and APIs
are not meant as a blueprint for real‑world firmware.

## Non-goals

This crate deliberately does **not** try to:

- Provide production‑quality firmware, safety guarantees, or fault handling.
- Be portable across a wide range of MCUs or evaluation boards.
- Offer a stable, well‑factored embedded API surface or HAL abstraction.
- Serve as a recommended architecture or template for shipping applications.

If you are looking for a starting point for a real product, treat this crate as
a source of inspiration for what is *possible*, not as something to copy
unchanged.

## Building

Prerequisites:

- Rust toolchain with `rustup`
- Target `thumbv7m-none-eabi` installed
- Optionally, an `arm-none-eabi-*` toolchain for inspecting binary size

From the repository root:

```bash
rustup target add thumbv7m-none-eabi
cargo build -p rptp-embedded-demo --release --target thumbv7m-none-eabi
```

Or from inside this crate:

```bash
cd crates/rptp-embedded-demo
cargo build --release --target thumbv7m-none-eabi
```

You can check the size of the generated binary to see whether it fits a
64‑KiB‑RAM‑class MCU:

```bash
arm-none-eabi-size target/thumbv7m-none-eabi/release/rptp-embedded-demo
```

The output should look roughly like this:

```bash
   text	   data	    bss	    dec	    hex	filename
  48056	   7488	    416	  55960	   da98	target/thumbv7m-none-eabi/release/rptp-embedded-demo
```

## Running under QEMU (locally)

The memory layout matches QEMU's `lm3s6965evb` board. With a release build
available, you can run the demo directly under QEMU. On macOS, using the
`vmnet-host` network backend:

```bash
qemu-system-arm \
  -M lm3s6965evb -cpu cortex-m3 \
  -kernel target/thumbv7m-none-eabi/release/rptp-embedded-demo \
  -nographic -semihosting \
  -nic vmnet-host,model=stellaris_enet,mac=02:00:00:00:00:01
```

The `vmnet-host` network backend is specific to macOS. On Linux, you can use
`tap` or bridge‑based networking instead; the e2e QEMU container uses this
approach (see `tests/e2e/docker/qemu.Dockerfile` for a concrete example).

Once QEMU is running, you can:

- Capture multicast/IGMP and PTP traffic (announce, sync, follow‑up) with
  Wireshark on the corresponding interface.
- Observe the port state machine and BMCA behaviour via semihosting logs on
  QEMU’s stdout.

## How it is used in e2e tests

This bare‑metal demo is exercised by the end‑to‑end tests under `tests/e2e`.
Those tests build two Docker images:

- A `qemu` image that embeds this `rptp-embedded-demo` binary and runs it under
  QEMU with a TAP/bridge setup so it shares L2 connectivity with other
  containers.
- A `ptp4l` image that runs LinuxPTP’s `ptp4l` daemon.

The main scenario for this crate is
`tests/e2e/tests/ptp4l_slave_syncs_to_qemu_master.rs`, which:

1. Builds the `qemu` image using `tests/e2e/docker/qemu.Dockerfile` (with
   `MANIFEST_DIR=crates/rptp-embedded-demo`).
2. Builds the `ptp4l` image using `tests/e2e/docker/ptp4l.Dockerfile`.
3. Creates a dedicated Docker network and starts the QEMU container.
4. Starts the `ptp4l` container on the same network and waits for it to report
   `UNCALIBRATED to SLAVE on MASTER_CLOCK_SELECTED`, indicating that it has
   synchronized to the embedded grandmaster.

To run this end‑to‑end test from the project root (it is `#[ignore]` by
default and expects a local Docker daemon with sufficient privileges):

```bash
cargo test -p rptp-e2e-tests ptp4l_slave_syncs_to_qemu_master -- --nocapture --ignored
```

This will build the necessary Docker images, start QEMU and `ptp4l`, stream
their logs, and verify that `ptp4l` successfully locks to the QEMU‑hosted
embedded master.
