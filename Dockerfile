# Build the daemon only: `uwa` is the whole runtime surface.
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release -p uwa-bin --features uwa-bin/mcp-http

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        chromium ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/uwa /usr/local/bin/uwa

ENV UWA_CONFIG=/etc/uwa/uwa.toml \
    UWA_CHROMIUM_WS=http://127.0.0.1:9222 \
    RUST_LOG=info,uwa=debug
EXPOSE 8080 8081
ENTRYPOINT ["/usr/local/bin/uwa"]
