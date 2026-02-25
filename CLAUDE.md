# CLAUDE.md — Project Instructions

## Deployment
- Deployed on **Railway** (background worker, no public HTTP needed)
- Railway Postgres plugin provides `DATABASE_URL`
- Logs must be **plain text** (not JSON) — Railway's log viewer doesn't let you copy-paste JSON structured logs. Keep `PM__LOGGING__JSON_OUTPUT=false`.

## Build
- Rust 1.93+ required (alloy crate needs ≥1.91)
- Dockerfile uses rustup to install Rust (Docker Hub images lag behind)
- `cargo check` to verify, `cargo build --release` for production

## Architecture
- Polymarket maker bot: two-sided GTC quoting + article endgame strategy
- Makers pay 0% fees and earn USDC rebates (post-Feb 2026 rules)
- 50ms cancel/replace hot loop per market
- Binance + Coinbase oracle feeds, Polymarket WS orderbook
- PostgreSQL for persistence (preserves legacy trades/signals/resolutions)

## Key env vars (set in Railway dashboard)
- `DATABASE_URL` — Postgres connection string (use `${{Postgres.DATABASE_URL}}`)
- `PRIVATE_KEY` — Polymarket magic link private key
- `FUNDER_ADDRESS` — Polygon wallet address
- `CLOB_API_KEY`, `CLOB_SECRET`, `CLOB_PASSPHRASE` — CLOB API credentials
- `PM__TRADING__PAPER_MODE` — `true` for paper, `false` for live
