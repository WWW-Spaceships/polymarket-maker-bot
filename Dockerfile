# --- Build stage ---
FROM rust:1.83-bookworm AS builder

WORKDIR /app

# Install native TLS deps
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

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
