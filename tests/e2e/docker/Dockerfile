# --- chef ---
FROM rust:1.89-trixie AS chef
WORKDIR /app
ARG MANIFEST_DIR
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=rptp-cargo-registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git,id=rptp-cargo-git,sharing=shared \
    cargo install cargo-chef --locked
COPY crates/rptp/Cargo.toml ./crates/rptp/Cargo.toml
COPY crates/rptp-daemon/Cargo.toml ./crates/rptp-daemon/Cargo.toml
COPY ${MANIFEST_DIR}/ ${MANIFEST_DIR}/

RUN mkdir -p crates/rptp/src crates/rptp-daemon/src \
    && : > crates/rptp/src/lib.rs \
    && : > crates/rptp-daemon/src/lib.rs

RUN cargo chef prepare --recipe-path recipe.json --workspace --manifest-path ${MANIFEST_DIR}/Cargo.toml

# --- deps ---
FROM rust:trixie AS deps
WORKDIR /app
ENV CARGO_TARGET_DIR=/app/target
COPY --from=chef /usr/local/cargo/bin/cargo-chef /usr/local/cargo/bin/
COPY --from=chef /app/recipe.json recipe.json

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=rptp-cargo-registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git,id=rptp-cargo-git,sharing=shared \
    --mount=type=cache,target=/app/target,id=rptp-target,sharing=locked \
    cargo chef cook --release --recipe-path recipe.json

# --- build ---
FROM rust:1.89-trixie AS builder
WORKDIR /app
ARG MANIFEST_DIR
ENV CARGO_TARGET_DIR=/app/target
COPY crates ./crates
COPY ${MANIFEST_DIR}/ ${MANIFEST_DIR}/

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=rptp-cargo-registry,sharing=shared \
    --mount=type=cache,target=/usr/local/cargo/git,id=rptp-cargo-git,sharing=shared \
    --mount=type=cache,target=/app/target,id=rptp-target,sharing=locked \
    cargo build --release --workspace --bins --manifest-path ${MANIFEST_DIR}/Cargo.toml \
    && mkdir -p /out \
    && find /app/target/release -maxdepth 1 -type f -executable -exec cp {} /out/ \;

# --- runtime ---
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates
RUN rm -rf /var/lib/apt/lists/*
RUN useradd -r -u 1000 app
USER app
WORKDIR /app

COPY --from=builder --chown=app:app /out/* /app/
