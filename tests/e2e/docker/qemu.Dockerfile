# --- chef ---
FROM rust:1.89-trixie AS chef
WORKDIR /app
ARG MANIFEST_DIR
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=rptp-cargo-registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git,id=rptp-cargo-git,sharing=shared \
    cargo install cargo-chef --locked
RUN rustup target add thumbv7m-none-eabi
COPY crates/rptp/Cargo.toml ./crates/rptp/Cargo.toml
COPY crates/rptp-embedded-demo/Cargo.toml ./crates/rptp-embedded-demo/Cargo.toml
COPY ${MANIFEST_DIR}/ ${MANIFEST_DIR}/

RUN mkdir -p crates/rptp/src  \
    && : > crates/rptp/src/lib.rs

RUN cargo chef prepare --recipe-path recipe.json --manifest-path ${MANIFEST_DIR}/Cargo.toml

# --- deps ---
FROM rust:trixie AS deps
WORKDIR /app
ENV CARGO_TARGET_DIR=/app/target
RUN rustup target add thumbv7m-none-eabi
COPY --from=chef /usr/local/cargo/bin/cargo-chef /usr/local/cargo/bin/
COPY --from=chef /app/recipe.json recipe.json

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=rptp-cargo-registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git,id=rptp-cargo-git,sharing=shared \
    --mount=type=cache,target=/app/target,id=rptp-target,sharing=locked \
    cargo chef cook --release --recipe-path recipe.json

# --- build ---
FROM rust:1.89-trixie AS builder
WORKDIR /app
ENV CARGO_TARGET_DIR=/app/target
ARG MANIFEST_DIR
RUN rustup target add thumbv7m-none-eabi
COPY crates/rptp ./crates/rptp
COPY ${MANIFEST_DIR}/ ${MANIFEST_DIR}/
WORKDIR /app/${MANIFEST_DIR}
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=rptp-cargo-registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git,id=rptp-cargo-git,sharing=shared \
    --mount=type=cache,target=/app/target,id=rptp-target,sharing=locked \
    cargo build --release --target thumbv7m-none-eabi \
    && mkdir -p /out \
    && cp /app/target/thumbv7m-none-eabi/release/rptp-embedded-demo /out/

# --- runtime ---
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates qemu-system-arm iproute2 \
    bridge-utils iputils-ping ca-certificates
RUN rm -rf /var/lib/apt/lists/*
WORKDIR /app

COPY --from=builder --chown=app:app /out/* /app/

RUN cat >/usr/local/bin/run-qemu <<'EOF' && chmod +x /usr/local/bin/run-qemu
#!/bin/sh
set -e

TAP=qtap0
BRIDGE=br0
MAC=02:00:00:00:00:01

# Create tap and bridge, attach eth0 so qemu is on same L2 as the container.
ip tuntap add dev "${TAP}" mode tap
ip link set "${TAP}" up
ip link add name "${BRIDGE}" type bridge || true
ip link set "${TAP}" master "${BRIDGE}"
ip link set eth0 master "${BRIDGE}"
ip link set "${BRIDGE}" up

exec qemu-system-arm \
  -M lm3s6965evb \
  -cpu cortex-m3 \
  -kernel /app/rptp-embedded-demo \
  -nographic \
  -semihosting \
  -nic tap,ifname="${TAP}",script=no,downscript=no,model=stellaris_enet,mac="${MAC}" \
  "$@"
EOF

ENTRYPOINT ["/usr/local/bin/run-qemu"]
CMD []
