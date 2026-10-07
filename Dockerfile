# syntax=docker/dockerfile:1.6
# ---------- build stage ----------
FROM rust:1.75-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY uwa-core ./uwa-core
COPY crates ./crates
RUN cargo build --release -p uwa-bin --features metrics --locked 2>/dev/null \
    || cargo build --release -p uwa-bin --features metrics

# ---------- runtime stage ----------
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
      chromium \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/uwa /usr/local/bin/uwa
COPY crates/uwa-bin/config.example.toml /etc/uwa/uwa.toml

RUN mkdir -p /var/lib/uwa && chmod 755 /var/lib/uwa

ENV UWA_CONFIG=/etc/uwa/uwa.toml
ENV RUST_LOG=info,uwa=debug
EXPOSE 8080 8081

ENTRYPOINT ["/usr/local/bin/uwa"]
CMD ["--config", "/etc/uwa/uwa.toml"]