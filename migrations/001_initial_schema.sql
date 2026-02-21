-- Initial schema — port of existing 7 tables from SQLite to PostgreSQL.
-- Preserves trades, signals, resolutions, markets, features, events.

CREATE TABLE IF NOT EXISTS markets (
    id              BIGSERIAL PRIMARY KEY,
    condition_id    TEXT NOT NULL UNIQUE,
    asset           TEXT NOT NULL,
    timeframe       TEXT NOT NULL,
    start_time      DOUBLE PRECISION NOT NULL,
    end_time        DOUBLE PRECISION NOT NULL,
    resolved        BOOLEAN NOT NULL DEFAULT FALSE,
    outcome         TEXT
);

CREATE TABLE IF NOT EXISTS features (
    id                  BIGSERIAL PRIMARY KEY,
    market_id           BIGINT NOT NULL REFERENCES markets(id),
    timestamp           DOUBLE PRECISION NOT NULL,
    pm_mid_price        DOUBLE PRECISION,
    pm_best_bid         DOUBLE PRECISION,
    pm_best_ask         DOUBLE PRECISION,
    pm_spread           DOUBLE PRECISION,
    pm_top_bid_size     DOUBLE PRECISION,
    pm_top_ask_size     DOUBLE PRECISION,
    external_price      DOUBLE PRECISION,
    external_return_5s  DOUBLE PRECISION,
    external_return_10s DOUBLE PRECISION,
    pm_implied_prob     DOUBLE PRECISION,
    price_dislocation   DOUBLE PRECISION
);

CREATE TABLE IF NOT EXISTS trades (
    id              BIGSERIAL PRIMARY KEY,
    market_id       BIGINT NOT NULL REFERENCES markets(id),
    condition_id    TEXT NOT NULL,
    side            TEXT NOT NULL,
    entry_price     DOUBLE PRECISION NOT NULL,
    size            DOUBLE PRECISION NOT NULL,
    entry_time      DOUBLE PRECISION NOT NULL,
    exit_price      DOUBLE PRECISION,
    exit_time       DOUBLE PRECISION,
    pnl             DOUBLE PRECISION,
    edge_at_entry   DOUBLE PRECISION,
    direction       TEXT,
    dislocation_bps DOUBLE PRECISION,
    tier            INTEGER,
    order_id        TEXT,
    token_id        TEXT,
    signal_type     TEXT
);

CREATE TABLE IF NOT EXISTS resolutions (
    id              BIGSERIAL PRIMARY KEY,
    market_id       BIGINT NOT NULL UNIQUE REFERENCES markets(id),
    condition_id    TEXT NOT NULL,
    outcome         TEXT NOT NULL,
    final_price     DOUBLE PRECISION NOT NULL
);

CREATE TABLE IF NOT EXISTS signals (
    id                  BIGSERIAL PRIMARY KEY,
    market_id           BIGINT REFERENCES markets(id),
    timestamp           DOUBLE PRECISION NOT NULL,
    binance_price       DOUBLE PRECISION,
    coinbase_price      DOUBLE PRECISION,
    binance_delta_1s    DOUBLE PRECISION,
    coinbase_delta_1s   DOUBLE PRECISION,
    binance_delta_3s    DOUBLE PRECISION,
    coinbase_delta_3s   DOUBLE PRECISION,
    poly_mid            DOUBLE PRECISION,
    seconds_left        DOUBLE PRECISION,
    direction           TEXT,
    signal_strength     DOUBLE PRECISION,
    traded              BOOLEAN,
    result              TEXT
);

CREATE TABLE IF NOT EXISTS events (
    id          BIGSERIAL PRIMARY KEY,
    ts          DOUBLE PRECISION NOT NULL,
    event_type  TEXT NOT NULL,
    market_id   TEXT,
    payload     JSONB NOT NULL DEFAULT '{}'
);

-- Indexes for common queries
CREATE INDEX IF NOT EXISTS idx_markets_condition_id ON markets(condition_id);
CREATE INDEX IF NOT EXISTS idx_trades_condition_id ON trades(condition_id);
CREATE INDEX IF NOT EXISTS idx_trades_entry_time ON trades(entry_time);
CREATE INDEX IF NOT EXISTS idx_trades_signal_type ON trades(signal_type);
CREATE INDEX IF NOT EXISTS idx_signals_timestamp ON signals(timestamp);
CREATE INDEX IF NOT EXISTS idx_signals_market_id ON signals(market_id);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
CREATE INDEX IF NOT EXISTS idx_events_event_type ON events(event_type);
CREATE INDEX IF NOT EXISTS idx_features_market_id ON features(market_id);
CREATE INDEX IF NOT EXISTS idx_features_timestamp ON features(timestamp);
