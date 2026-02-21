#!/usr/bin/env python3
"""
One-time migration: SQLite → PostgreSQL.

Reads from the existing SQLite database and writes to PostgreSQL.
Preserves all data from: markets, features, trades, resolutions, signals, events.

Usage:
    export DATABASE_URL="postgresql://user:pass@localhost:5432/polymarket"
    python3 scripts/migrate_sqlite_to_pg.py --sqlite ../polymarket-launch-edge-trader/data/trading.db
"""

import argparse
import os
import sqlite3
import sys

import psycopg2
from psycopg2.extras import execute_batch

# Tables to migrate (in dependency order)
TABLES = [
    "markets",
    "features",
    "trades",
    "resolutions",
    "signals",
    "events",
]


def get_sqlite_conn(path: str) -> sqlite3.Connection:
    if not os.path.exists(path):
        print(f"ERROR: SQLite file not found: {path}")
        sys.exit(1)
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    return conn


def get_pg_conn(url: str):
    return psycopg2.connect(url)


def get_columns(sqlite_conn: sqlite3.Connection, table: str) -> list[str]:
    cursor = sqlite_conn.execute(f"PRAGMA table_info({table})")
    return [row["name"] for row in cursor.fetchall()]


def migrate_table(
    sqlite_conn: sqlite3.Connection,
    pg_conn,
    table: str,
) -> int:
    columns = get_columns(sqlite_conn, table)
    if not columns:
        print(f"  SKIP {table}: no columns found in SQLite")
        return 0

    # Remove 'id' column — PostgreSQL uses BIGSERIAL
    non_id_columns = [c for c in columns if c != "id"]

    # Read all rows from SQLite
    cursor = sqlite_conn.execute(f"SELECT {', '.join(non_id_columns)} FROM {table}")
    rows = cursor.fetchall()

    if not rows:
        print(f"  SKIP {table}: 0 rows")
        return 0

    # Build INSERT for PG
    placeholders = ", ".join(["%s"] * len(non_id_columns))
    col_list = ", ".join(non_id_columns)
    insert_sql = f"INSERT INTO {table} ({col_list}) VALUES ({placeholders}) ON CONFLICT DO NOTHING"

    pg_cursor = pg_conn.cursor()

    # Convert Row objects to tuples
    data = [tuple(row[c] for c in non_id_columns) for row in rows]

    execute_batch(pg_cursor, insert_sql, data, page_size=500)
    pg_conn.commit()

    print(f"  OK {table}: {len(data)} rows migrated")
    return len(data)


def reset_sequences(pg_conn):
    """Reset PostgreSQL sequences to max(id) + 1 for each table."""
    pg_cursor = pg_conn.cursor()

    for table in TABLES:
        try:
            pg_cursor.execute(f"SELECT MAX(id) FROM {table}")
            max_id = pg_cursor.fetchone()[0]
            if max_id:
                seq_name = f"{table}_id_seq"
                pg_cursor.execute(f"SELECT setval('{seq_name}', {max_id})")
                print(f"  SEQ {seq_name} → {max_id}")
        except Exception as e:
            print(f"  WARN: could not reset sequence for {table}: {e}")
            pg_conn.rollback()

    pg_conn.commit()


def verify_counts(sqlite_conn: sqlite3.Connection, pg_conn):
    """Verify row counts match between SQLite and PostgreSQL."""
    pg_cursor = pg_conn.cursor()
    all_match = True

    for table in TABLES:
        sqlite_cursor = sqlite_conn.execute(f"SELECT COUNT(*) FROM {table}")
        sqlite_count = sqlite_cursor.fetchone()[0]

        pg_cursor.execute(f"SELECT COUNT(*) FROM {table}")
        pg_count = pg_cursor.fetchone()[0]

        status = "OK" if pg_count >= sqlite_count else "MISMATCH"
        if pg_count < sqlite_count:
            all_match = False

        print(f"  {status} {table}: SQLite={sqlite_count}, PG={pg_count}")

    return all_match


def main():
    parser = argparse.ArgumentParser(description="Migrate SQLite to PostgreSQL")
    parser.add_argument(
        "--sqlite",
        default="../polymarket-launch-edge-trader/data/trading.db",
        help="Path to SQLite database",
    )
    parser.add_argument(
        "--pg-url",
        default=os.environ.get("DATABASE_URL", ""),
        help="PostgreSQL connection URL",
    )
    args = parser.parse_args()

    if not args.pg_url:
        print("ERROR: DATABASE_URL not set. Set via env or --pg-url")
        sys.exit(1)

    print(f"Source: {args.sqlite}")
    print(f"Target: {args.pg_url.split('@')[-1] if '@' in args.pg_url else 'PostgreSQL'}")
    print()

    sqlite_conn = get_sqlite_conn(args.sqlite)
    pg_conn = get_pg_conn(args.pg_url)

    print("Migrating tables...")
    total = 0
    for table in TABLES:
        total += migrate_table(sqlite_conn, pg_conn, table)

    print(f"\nTotal rows migrated: {total}")

    print("\nResetting sequences...")
    reset_sequences(pg_conn)

    print("\nVerifying counts...")
    ok = verify_counts(sqlite_conn, pg_conn)

    sqlite_conn.close()
    pg_conn.close()

    if ok:
        print("\nMigration complete!")
    else:
        print("\nWARNING: Some counts don't match — check above.")
        sys.exit(1)


if __name__ == "__main__":
    main()
