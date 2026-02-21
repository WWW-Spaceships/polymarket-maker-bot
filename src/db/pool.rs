//! PostgreSQL connection pool setup and migrations.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tracing::info;

/// Create a PostgreSQL connection pool.
pub async fn create_pool(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    info!("database pool created");
    Ok(pool)
}

/// Run SQL migrations from the migrations/ directory.
pub async fn run_migrations(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    info!("database migrations applied");
    Ok(())
}

/// Health check — verify the database connection is alive.
pub async fn health_check(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}
