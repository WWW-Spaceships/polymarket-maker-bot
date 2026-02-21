# --- Build stage ---
FROM debian:bookworm AS builder

WORKDIR /app

# Install build deps + rustup
RUN apt-get update && apt-get install -y \
    curl build-essential pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Install Rust 1.93 via rustup (Docker Hub rust images lag behind)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.93.0
ENV PATH="/root/.cargo/bin:${PATH}"

# Cache dependency build — copy manifests first
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src

# Copy source and build for real
COPY src/ src/
COPY config/ config/
COPY migrations/ migrations/
RUN cargo build --release

# --- Runtime stage ---
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/polymarket-maker-bot /app/polymarket-maker-bot
COPY config/ config/
COPY migrations/ migrations/

# Railway injects PORT env var for web services
ENV RUST_LOG=info
ENV PM__LOGGING__JSON_OUTPUT=true

EXPOSE 8080

CMD ["./polymarket-maker-bot"]
