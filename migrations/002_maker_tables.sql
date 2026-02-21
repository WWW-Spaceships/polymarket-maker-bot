-- Maker-specific tables: order tracking, rebates, inventory.

CREATE TABLE IF NOT EXISTS maker_orders (
    id              BIGSERIAL PRIMARY KEY,
    order_id        TEXT NOT NULL UNIQUE,
    market_id       BIGINT NOT NULL REFERENCES markets(id),
    condition_id    TEXT NOT NULL,
    token_id        TEXT NOT NULL,
    side            TEXT NOT NULL,
    price           DOUBLE PRECISION NOT NULL,
    size            DOUBLE PRECISION NOT NULL,
    order_type      TEXT NOT NULL DEFAULT 'GTC',
    status          TEXT NOT NULL DEFAULT 'posted',
    posted_at       DOUBLE PRECISION NOT NULL,
    filled_at       DOUBLE PRECISION,
    cancelled_at    DOUBLE PRECISION,
    fill_price      DOUBLE PRECISION,
    fill_size       DOUBLE PRECISION,
    cancel_reason   TEXT,
    strategy        TEXT NOT NULL DEFAULT 'quoting',
    quote_cycle_id  BIGINT
);

CREATE TABLE IF NOT EXISTS rebates (
    id              BIGSERIAL PRIMARY KEY,
    date            DATE NOT NULL UNIQUE,
    amount_usdc     DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    volume_usdc     DOUBLE PRECISION,
    recorded_at     DOUBLE PRECISION NOT NULL
);

CREATE TABLE IF NOT EXISTS inventory (
    id              BIGSERIAL PRIMARY KEY,
    token_id        TEXT NOT NULL UNIQUE,
    condition_id    TEXT NOT NULL,
    side            TEXT NOT NULL,
    shares          DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    avg_entry_price DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    updated_at      DOUBLE PRECISION NOT NULL
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_maker_orders_status ON maker_orders(status);
CREATE INDEX IF NOT EXISTS idx_maker_orders_condition_id ON maker_orders(condition_id);
CREATE INDEX IF NOT EXISTS idx_maker_orders_posted_at ON maker_orders(posted_at);
CREATE INDEX IF NOT EXISTS idx_maker_orders_strategy ON maker_orders(strategy);
CREATE INDEX IF NOT EXISTS idx_inventory_condition_id ON inventory(condition_id);
