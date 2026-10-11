# syntax=docker/dockerfile:1.7

# ============================================================================
# Stage 1 — build the Rust binary and the UI bundle
# ============================================================================
FROM rust:1.82-bookworm AS rust-build

WORKDIR /src

# Cache dependencies: copy manifests first, build a stub, then copy sources.
COPY Cargo.toml Cargo.lock ./
COPY uwa-core/Cargo.toml uwa-core/
COPY crates/uwa-config/Cargo.toml crates/uwa-config/
COPY crates/uwa-browser/Cargo.toml crates/uwa-browser/
COPY crates/uwa-stealth/Cargo.toml crates/uwa-stealth/
COPY crates/uwa-extract/Cargo.toml crates/uwa-extract/
COPY crates/uwa-providers/Cargo.toml crates/uwa-providers/
COPY crates/uwa-session/Cargo.toml crates/uwa-session/
COPY crates/uwa-resilience/Cargo.toml crates/uwa-resilience/
COPY crates/uwa-lifecycle/Cargo.toml crates/uwa-lifecycle/
COPY crates/uwa-history/Cargo.toml crates/uwa-history/
COPY crates/uwa-tools/Cargo.toml crates/uwa-tools/
COPY crates/uwa-api/Cargo.toml crates/uwa-api/
COPY crates/uwa-bin/Cargo.toml crates/uwa-bin/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    for d in uwa-core crates/uwa-config crates/uwa-browser crates/uwa-stealth \
             crates/uwa-extract crates/uwa-providers crates/uwa-session \
             crates/uwa-resilience crates/uwa-lifecycle crates/uwa-history \
             crates/uwa-tools crates/uwa-api crates/uwa-bin; do \
      mkdir -p "$d/src"; \
      echo "" > "$d/src/lib.rs"; \
      echo "fn main() {}" > "$d/src/main.rs"; \
    done && \
    cargo build --release --bin uwa 2>/dev/null || true && \
    rm -rf target/release/.fingerprint/uwa-* target/release/deps/uwa*

# Real sources.
COPY uwa-core uwa-core
COPY crates crates
COPY rust-toolchain.toml ./

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --bin uwa

# ---------------------------------------------------------------------------
FROM node:20-bookworm-slim AS ui-build
WORKDIR /ui
COPY crates/uwa-api/static/package.json crates/uwa-api/static/package-lock.json* ./
RUN npm ci --silent
COPY crates/uwa-api/static ./
RUN npm run --silent build

# ============================================================================
# Stage 2 — runtime: Debian slim + Chromium + static uwa binary
# ============================================================================
FROM debian:bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    UWA_HOME=/var/lib/uwa \
    UWA_CONFIG=/etc/uwa/uwa.toml \
    UWA_STATIC_DIR=/usr/share/uwa/static \
    UWA_CHROMIUM_WS=http://127.0.0.1:9222

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        chromium \
        dumb-init \
        fontconfig \
        fonts-liberation \
        libasound2 \
        libnss3 \
        libx11-xcb1 \
        libxcomposite1 \
        libxdamage1 \
        libxrandr2 \
        xdg-utils \
    && rm -rf /var/lib/apt/lists/*

# Non-root user.
RUN useradd --create-home --shell /usr/sbin/nologin uwa \
 && mkdir -p /etc/uwa /usr/share/uwa/static /var/lib/uwa \
 && chown -R uwa:uwa /var/lib/uwa /etc/uwa /usr/share/uwa

# Binary + UI bundle.
COPY --from=rust-build /src/target/release/uwa /usr/local/bin/uwa
COPY --from=ui-build /ui/dist/ /usr/share/uwa/static/dist/
COPY --from=ui-build /ui/index.html /usr/share/uwa/static/index.html
COPY --from=ui-build /ui/styles.css /usr/share/uwa/static/styles.css

# Example config the entrypoint copies on first run.
COPY crates/uwa-bin/config.example.toml /etc/uwa/config.example.toml

COPY docker/entrypoint.sh /usr/local/bin/uwa-entrypoint
RUN chmod +x /usr/local/bin/uwa-entrypoint

USER uwa
WORKDIR /home/uwa
EXPOSE 8080

HEALTHCHECK --interval=15s --timeout=3s --start-period=30s --retries=5 \
  CMD ["uwa", "daemon", "status"] || exit 1

ENTRYPOINT ["dumb-init", "--", "/usr/local/bin/uwa-entrypoint"]
CMD ["--config", "/etc/uwa/uwa.toml"]