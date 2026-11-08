# =============================================================================
# Stage 1: Builder
# Compiles the Rust binary inside a Linux container so the output is a proper
# Linux ELF executable — no cross-compilation hassle on Windows.
# =============================================================================
FROM rust:1.82-slim AS builder

WORKDIR /app

# Install OS-level build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# ---- Dependency caching layer ------------------------------------------------
# Copy manifests first. Docker caches this layer and only re-downloads/recompiles
# crates when Cargo.toml or Cargo.lock actually change.
COPY Cargo.toml Cargo.lock ./

# Build a stub binary so all dependencies are compiled and cached as a layer
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release
# Remove the stub so the real source build picks up changes correctly
RUN rm -f target/release/miko_scraper target/release/deps/miko_scraper*

# ---- Real build --------------------------------------------------------------
COPY src        ./src
COPY migrations ./migrations
COPY frontend   ./frontend

RUN cargo build --release

# =============================================================================
# Stage 2: Runtime image
# Tiny Debian image — only the compiled binary + runtime shared libraries.
# =============================================================================
FROM debian:bookworm-slim AS runtime

# Install only the shared libraries the binary links at runtime
RUN apt-get update && apt-get install -y \
    libssl3 \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/miko_scraper ./miko_scraper

# Copy static assets served by Axum's ServeDir (must be at the same relative
# path as in development — the binary looks for ./frontend/)
COPY --from=builder /app/frontend    ./frontend

# Copy SQL migration files (auto-run at startup via src/db/migrations.rs)
COPY --from=builder /app/migrations  ./migrations

# /app/data is the mount point for the SQLite database volume.
# The existing miko_scraper.db must be copied to ./data/ on the host
# before the first `docker compose up` (see DOCKER.md).
RUN mkdir -p /app/data

EXPOSE 3000

# Health-check: ping root every 30 s, fail after 3 misses
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -f http://localhost:3000/ || exit 1

CMD ["./miko_scraper"]
