//! SQL query functions for all tables.

use sqlx::PgPool;
use super::models::*;

// ── Markets ──────────────────────────────────────────────────────

pub async fn insert_market(
    pool: &PgPool,
    condition_id: &str,
    asset: &str,
    timeframe: &str,
    start_time: f64,
    end_time: f64,
) -> anyhow::Result<i64> {
    let row = sqlx::query_scalar::<_, i64>(
        "INSERT INTO markets (condition_id, asset, timeframe, start_time, end_time)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (condition_id) DO UPDATE SET end_time = EXCLUDED.end_time
         RETURNING id"
    )
    .bind(condition_id)
    .bind(asset)
    .bind(timeframe)
    .bind(start_time)
    .bind(end_time)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn get_market_by_condition_id(
    pool: &PgPool,
    condition_id: &str,
) -> anyhow::Result<Option<DbMarket>> {
    let row = sqlx::query_as::<_, DbMarket>(
        "SELECT * FROM markets WHERE condition_id = $1"
    )
    .bind(condition_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn resolve_market(
    pool: &PgPool,
    condition_id: &str,
    outcome: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE markets SET resolved = true, outcome = $1 WHERE condition_id = $2")
        .bind(outcome)
        .bind(condition_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ── Trades ───────────────────────────────────────────────────────

pub async fn insert_trade(
    pool: &PgPool,
    market_id: i64,
    condition_id: &str,
    side: &str,
    entry_price: f64,
    size: f64,
    entry_time: f64,
    direction: Option<&str>,
    dislocation_bps: Option<f64>,
    tier: Option<i32>,
    order_id: Option<&str>,
    token_id: Option<&str>,
    signal_type: Option<&str>,
) -> anyhow::Result<i64> {
    let row = sqlx::query_scalar::<_, i64>(
        "INSERT INTO trades (market_id, condition_id, side, entry_price, size, entry_time,
         direction, dislocation_bps, tier, order_id, token_id, signal_type)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         RETURNING id"
    )
    .bind(market_id)
    .bind(condition_id)
    .bind(side)
    .bind(entry_price)
    .bind(size)
    .bind(entry_time)
    .bind(direction)
    .bind(dislocation_bps)
    .bind(tier)
    .bind(order_id)
    .bind(token_id)
    .bind(signal_type)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn get_recent_trades(pool: &PgPool, limit: i64) -> anyhow::Result<Vec<DbTrade>> {
    let rows = sqlx::query_as::<_, DbTrade>(
        "SELECT * FROM trades ORDER BY entry_time DESC LIMIT $1"
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_signal_type_stats(
    pool: &PgPool,
) -> anyhow::Result<Vec<(String, i64, i64)>> {
    let rows = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT COALESCE(signal_type, 'unknown'), COUNT(*), SUM(CASE WHEN pnl > 0 THEN 1 ELSE 0 END)
         FROM trades WHERE pnl IS NOT NULL GROUP BY signal_type"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ── Resolutions ──────────────────────────────────────────────────

pub async fn insert_resolution(
    pool: &PgPool,
    market_id: i64,
    condition_id: &str,
    outcome: &str,
    final_price: f64,
) -> anyhow::Result<i64> {
    let row = sqlx::query_scalar::<_, i64>(
        "INSERT INTO resolutions (market_id, condition_id, outcome, final_price)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (market_id) DO UPDATE SET outcome = EXCLUDED.outcome, final_price = EXCLUDED.final_price
         RETURNING id"
    )
    .bind(market_id)
    .bind(condition_id)
    .bind(outcome)
    .bind(final_price)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

// ── Signals ──────────────────────────────────────────────────────

pub async fn insert_signal(
    pool: &PgPool,
    market_id: Option<i64>,
    timestamp: f64,
    direction: Option<&str>,
    signal_strength: Option<f64>,
    binance_delta_1s: Option<f64>,
    coinbase_delta_1s: Option<f64>,
    poly_mid: Option<f64>,
    seconds_left: Option<f64>,
    traded: bool,
) -> anyhow::Result<i64> {
    let row = sqlx::query_scalar::<_, i64>(
        "INSERT INTO signals (market_id, timestamp, direction, signal_strength,
         binance_delta_1s, coinbase_delta_1s, poly_mid, seconds_left, traded)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id"
    )
    .bind(market_id)
    .bind(timestamp)
    .bind(direction)
    .bind(signal_strength)
    .bind(binance_delta_1s)
    .bind(coinbase_delta_1s)
    .bind(poly_mid)
    .bind(seconds_left)
    .bind(traded)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn get_recent_signals(pool: &PgPool, limit: i64) -> anyhow::Result<Vec<DbSignal>> {
    let rows = sqlx::query_as::<_, DbSignal>(
        "SELECT * FROM signals ORDER BY timestamp DESC LIMIT $1"
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ── Events ───────────────────────────────────────────────────────

pub async fn record_event(
    pool: &PgPool,
    ts: f64,
    event_type: &str,
    market_id: Option<&str>,
    payload: &serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO events (ts, event_type, market_id, payload) VALUES ($1, $2, $3, $4)"
    )
    .bind(ts)
    .bind(event_type)
    .bind(market_id)
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(())
}

// ── Maker Orders ─────────────────────────────────────────────────

pub async fn insert_maker_order(
    pool: &PgPool,
    order_id: &str,
    market_id: i64,
    condition_id: &str,
    token_id: &str,
    side: &str,
    price: f64,
    size: f64,
    order_type: &str,
    posted_at: f64,
    strategy: &str,
) -> anyhow::Result<i64> {
    let row = sqlx::query_scalar::<_, i64>(
        "INSERT INTO maker_orders (order_id, market_id, condition_id, token_id, side,
         price, size, order_type, posted_at, strategy)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id"
    )
    .bind(order_id)
    .bind(market_id)
    .bind(condition_id)
    .bind(token_id)
    .bind(side)
    .bind(price)
    .bind(size)
    .bind(order_type)
    .bind(posted_at)
    .bind(strategy)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn update_maker_order_status(
    pool: &PgPool,
    order_id: &str,
    status: &str,
    fill_price: Option<f64>,
    fill_size: Option<f64>,
) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    sqlx::query(
        "UPDATE maker_orders SET status = $1, fill_price = $2, fill_size = $3,
         filled_at = CASE WHEN $1 = 'filled' THEN $4 ELSE filled_at END,
         cancelled_at = CASE WHEN $1 = 'cancelled' THEN $4 ELSE cancelled_at END
         WHERE order_id = $5"
    )
    .bind(status)
    .bind(fill_price)
    .bind(fill_size)
    .bind(now)
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_live_orders(pool: &PgPool) -> anyhow::Result<Vec<DbMakerOrder>> {
    let rows = sqlx::query_as::<_, DbMakerOrder>(
        "SELECT * FROM maker_orders WHERE status IN ('posted', 'partially_filled') ORDER BY posted_at DESC"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ── Inventory ────────────────────────────────────────────────────

pub async fn upsert_inventory(
    pool: &PgPool,
    token_id: &str,
    condition_id: &str,
    side: &str,
    shares: f64,
    avg_entry_price: f64,
    updated_at: f64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO inventory (token_id, condition_id, side, shares, avg_entry_price, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (token_id)
         DO UPDATE SET shares = EXCLUDED.shares, avg_entry_price = EXCLUDED.avg_entry_price, updated_at = EXCLUDED.updated_at"
    )
    .bind(token_id)
    .bind(condition_id)
    .bind(side)
    .bind(shares)
    .bind(avg_entry_price)
    .bind(updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_all_inventory(pool: &PgPool) -> anyhow::Result<Vec<DbInventory>> {
    let rows = sqlx::query_as::<_, DbInventory>(
        "SELECT * FROM inventory WHERE shares > 0 ORDER BY updated_at DESC"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
