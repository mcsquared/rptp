FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates linuxptp \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
