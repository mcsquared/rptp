FROM rust:1.89 AS builder

WORKDIR /app

# Copy the entire workspace so Cargo can resolve dependencies.
COPY . .

# Build the grafana-prometheus-demo binary in release mode.
RUN cargo build --release -p grafana-prometheus-demo

FROM debian:bookworm-slim

RUN useradd -m appuser
USER appuser

WORKDIR /app

# Copy the built binary from the builder stage.
COPY --from=builder /app/target/release/grafana-prometheus-demo /usr/local/bin/grafana-prometheus-demo

EXPOSE 9898

ENV RUST_LOG=info

CMD ["grafana-prometheus-demo"]

