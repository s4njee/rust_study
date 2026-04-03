// ============================================================================
// db.rs — Database operations using sqlx (async PostgreSQL)
// ============================================================================
//
// This module contains all SQL queries for inserting/updating/deleting data
// in PostgreSQL. It uses `sqlx`, an async database library for Rust.
//
// Key differences from Python/JS database libraries:
//
//   - **Async by default**: Every query returns a Future that must be `.await`ed.
//     Like `await pool.query(...)` in Node's `pg` or `await conn.execute(...)`
//     in Python's `asyncpg`.
//
//   - **Compile-time safety**: sqlx can verify SQL queries against your actual
//     database schema at compile time (though we use runtime mode here).
//
//   - **Transactions via ownership**: A `Transaction` object represents an
//     open transaction. The transaction is rolled back automatically if the
//     `Transaction` is dropped (goes out of scope) without `.commit()`.
//     This is Rust's RAII pattern — no need for try/finally blocks.
//
//   - **Prepared statements**: `.bind()` safely parameterizes queries,
//     preventing SQL injection. Like `$1, $2` placeholders in node-postgres.
//
// ============================================================================

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Transaction};

use crate::config::Config;
use crate::models::{
    InsertBillActionParams, InsertBillCosponsorParams, InsertBillParams, InsertBillSubjectParams,
    InsertCommitteeParams, InsertVoteMemberParams, InsertVoteParams,
};

/// Maximum number of concurrent database connections in the pool.
/// `u32` because sqlx's pool API expects an unsigned 32-bit int.
const DB_WRITE_CONCURRENCY: u32 = 4;

/// SQL to ensure the `committees` table exists and is populated.
///
/// `r#"..."#` is a raw string literal — backslashes and quotes inside
/// don't need escaping. Like Python's `r"..."` or JS template literals,
/// but for multi-line strings with special characters.
///
/// This runs on startup to handle schema migrations gracefully. The
/// `DO $$ ... END $$` block is PL/pgSQL (PostgreSQL's procedural language)
/// that conditionally migrates data from `bill_committees` to `committees`.
const ENSURE_SCHEMA_COMPATIBILITY_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS committees (
    committee_code text PRIMARY KEY,
    committee_name text,
    chamber        text
);

CREATE INDEX IF NOT EXISTS committees_chamber_idx
    ON committees (chamber);

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = 'public'
          AND table_name = 'bill_committees'
          AND column_name = 'committee_name'
    ) THEN
        INSERT INTO committees (committee_code, committee_name, chamber)
        SELECT DISTINCT ON (committee_code)
            committee_code,
            NULLIF(committee_name, ''),
            NULLIF(chamber, '')
        FROM bill_committees
        WHERE committee_code IS NOT NULL
        ORDER BY committee_code, committee_name NULLS LAST, chamber NULLS LAST
        ON CONFLICT (committee_code) DO UPDATE SET
            committee_name = COALESCE(excluded.committee_name, committees.committee_name),
            chamber = COALESCE(excluded.chamber, committees.chamber);
    END IF;
END $$;
"#;

/// Creates and returns a PostgreSQL connection pool.
///
/// `PgPool` is a pool of reusable database connections — like `asyncpg.Pool`
/// in Python or `pg.Pool` in Node. Connection pooling avoids the overhead
/// of establishing a new TCP connection for every query.
///
/// The pool is cloneable and thread-safe — you can share it across async
/// tasks with `pool.clone()`. Internally it's reference-counted (like
/// Python objects), so cloning is cheap (just increments a counter).
pub async fn open_pool(cfg: &Config) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(DB_WRITE_CONCURRENCY)
        .connect(&cfg.postgres_dsn())
        .await
        // `.context(...)` adds a human-readable message to errors.
        // Without it, you'd get a raw TCP/SSL error. With it, you get:
        // "connect to postgres: connection refused" — much more debuggable.
        .context("connect to postgres")?;

    // Run schema migration on startup.
    ensure_schema_compatibility(&pool).await?;
    Ok(pool)
}

/// Runs the schema compatibility migration SQL.
async fn ensure_schema_compatibility(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(ENSURE_SCHEMA_COMPATIBILITY_SQL)
        .execute(pool)
        .await
        .context("ensure schema compatibility")?;
    Ok(())
}

// ============================================================================
// Vote database operations
// ============================================================================

/// Inserts or updates a vote in the `votes` table.
///
/// `tx: &mut Transaction<'_, Postgres>` — this takes a mutable reference to
/// an open transaction. The `'_` is a lifetime parameter (explained below).
///
/// **Lifetimes** are Rust's way of tracking how long references are valid.
/// `'_` means "the compiler will figure out the lifetime automatically."
/// In Python/JS, the garbage collector handles this. In Rust, lifetimes
/// prevent dangling references (use-after-free bugs) at compile time.
///
/// `&mut Transaction` means:
///   - `&` = this is a reference (not owned)
///   - `mut` = we can modify the transaction (execute queries on it)
///   - The transaction is NOT consumed — the caller still owns it
///
/// `ON CONFLICT ... DO UPDATE SET` is PostgreSQL's UPSERT syntax —
/// if the vote already exists (by voteid), update it instead of failing.
pub async fn insert_vote(
    tx: &mut Transaction<'_, Postgres>,
    vote: &InsertVoteParams,
) -> Result<()> {
    sqlx::query(
        r#"
INSERT INTO votes (
    voteid, bill_type, bill_number, congress,
    votenumber, votedate, question, result,
    votesession, chamber, source_url, votetype
) VALUES (
    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12
) ON CONFLICT (voteid) DO UPDATE SET
    bill_type   = excluded.bill_type,
    bill_number = excluded.bill_number,
    congress    = excluded.congress,
    votenumber  = excluded.votenumber,
    votedate    = excluded.votedate,
    question    = excluded.question,
    result      = excluded.result,
    votesession = excluded.votesession,
    chamber     = excluded.chamber,
    source_url  = excluded.source_url,
    votetype    = excluded.votetype
        "#,
    )
    // `.bind(...)` sets the value for each `$N` placeholder. This is how
    // you prevent SQL injection — never use string formatting for SQL values!
    // Like `cursor.execute("... WHERE id = %s", (vote_id,))` in Python.
    //
    // `&vote.voteid` — the `&` borrows the field (passes by reference).
    // For `String` fields, we pass `&` to avoid cloning the string.
    // For `i32`/`NaiveDate`/`Option<T>`, we pass the value directly
    // (small types are cheaper to copy than to reference).
    .bind(&vote.voteid)
    .bind(&vote.bill_type)
    .bind(vote.bill_number)
    .bind(vote.congress)
    .bind(vote.votenumber)
    .bind(vote.votedate)
    .bind(&vote.question)
    .bind(&vote.result)
    .bind(&vote.votesession)
    .bind(&vote.chamber)
    .bind(&vote.source_url)
    .bind(&vote.votetype)
    // `&mut **tx` — the double dereference (`**`) is needed because `tx`
    // is a `&mut Transaction`, and `Transaction` wraps an inner connection.
    // The `**` unwraps both layers. This is a sqlx API quirk.
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deletes all member votes for a given vote ID (before re-inserting).
pub async fn delete_vote_members(tx: &mut Transaction<'_, Postgres>, voteid: &str) -> Result<()> {
    sqlx::query("DELETE FROM vote_members WHERE voteid = $1")
        .bind(voteid)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Inserts a single vote member record.
/// `ON CONFLICT ... DO NOTHING` skips duplicates silently.
pub async fn insert_vote_member(
    tx: &mut Transaction<'_, Postgres>,
    member: &InsertVoteMemberParams,
) -> Result<()> {
    sqlx::query(
        r#"
INSERT INTO vote_members (voteid, bioguide_id, display_name, party, state, position)
VALUES ($1, $2, $3, $4, $5, $6)
ON CONFLICT ON CONSTRAINT vote_members_pkey DO NOTHING
        "#,
    )
    .bind(&member.voteid)
    .bind(&member.bioguide_id)
    .bind(&member.display_name)
    .bind(&member.party)
    .bind(&member.state)
    .bind(&member.position)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ============================================================================
// Bill database operations
// ============================================================================

/// Inserts or updates a bill in the `bills` table.
/// Uses a composite unique key: (billtype, billnumber, congress).
pub async fn insert_bill(
    tx: &mut Transaction<'_, Postgres>,
    bill: &InsertBillParams,
) -> Result<()> {
    sqlx::query(
        r#"
INSERT INTO bills (
    billid, billnumber, billtype, introducedat, congress,
    summary_date, summary_text,
    sponsor_bioguide_id, sponsor_name, sponsor_state, sponsor_party,
    origin_chamber, policy_area, update_date,
    latest_action_date, bill_status, statusat, shorttitle, officialtitle
) VALUES (
    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19
) ON CONFLICT (billtype, billnumber, congress) DO UPDATE SET
    summary_date        = excluded.summary_date,
    summary_text        = excluded.summary_text,
    sponsor_bioguide_id = excluded.sponsor_bioguide_id,
    sponsor_name        = excluded.sponsor_name,
    sponsor_state       = excluded.sponsor_state,
    sponsor_party       = excluded.sponsor_party,
    origin_chamber      = excluded.origin_chamber,
    policy_area         = excluded.policy_area,
    update_date         = excluded.update_date,
    latest_action_date  = excluded.latest_action_date,
    bill_status         = excluded.bill_status,
    statusat            = excluded.statusat,
    shorttitle          = excluded.shorttitle,
    officialtitle       = excluded.officialtitle
        "#,
    )
    .bind(&bill.billid)
    .bind(bill.billnumber)
    .bind(&bill.billtype)
    .bind(bill.introducedat)
    .bind(bill.congress)
    .bind(&bill.summary_date)
    .bind(&bill.summary_text)
    .bind(&bill.sponsor_bioguide_id)
    .bind(&bill.sponsor_name)
    .bind(&bill.sponsor_state)
    .bind(&bill.sponsor_party)
    .bind(&bill.origin_chamber)
    .bind(&bill.policy_area)
    .bind(bill.update_date)
    .bind(bill.latest_action_date)
    .bind(&bill.bill_status)
    .bind(bill.statusat)
    .bind(&bill.shorttitle)
    .bind(&bill.officialtitle)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Clears the latest_action_id for a bill (used before re-inserting actions).
pub async fn clear_bill_latest_action(
    tx: &mut Transaction<'_, Postgres>,
    billtype: &str,
    billnumber: i32,
    congress: i32,
    latest_action_date: Option<chrono::NaiveDate>,
) -> Result<()> {
    sqlx::query(
        r#"
UPDATE bills
SET latest_action_id = NULL,
    latest_action_date = $4
WHERE billtype = $1
  AND billnumber = $2
  AND congress = $3
        "#,
    )
    .bind(billtype)
    .bind(billnumber)
    .bind(congress)
    .bind(latest_action_date)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deletes all actions for a bill (before re-inserting from fresh data).
pub async fn delete_bill_actions(
    tx: &mut Transaction<'_, Postgres>,
    billtype: &str,
    billnumber: i32,
    congress: i32,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM bill_actions WHERE billtype = $1 AND billnumber = $2 AND congress = $3",
    )
    .bind(billtype)
    .bind(billnumber)
    .bind(congress)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Inserts a bill action and returns its auto-generated ID.
///
/// `query_scalar` + `RETURNING id` retrieves the inserted row's ID.
/// This is like Python's `cursor.execute(...); return cursor.fetchone()[0]`
/// or `RETURNING id` in any PostgreSQL client.
pub async fn insert_bill_action(
    tx: &mut Transaction<'_, Postgres>,
    action: &InsertBillActionParams,
) -> Result<i64> {
    let id = sqlx::query_scalar(
        r#"
INSERT INTO bill_actions (billtype, billnumber, congress, acted_at, action_text, action_type, action_code, source_system_code)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
RETURNING id
        "#,
    )
    .bind(&action.billtype)
    .bind(action.billnumber)
    .bind(action.congress)
    .bind(action.acted_at)
    .bind(&action.action_text)
    .bind(&action.action_type)
    .bind(&action.action_code)
    .bind(&action.source_system_code)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

/// Updates the latest_action_id and latest_action_date on a bill.
/// Called after inserting all actions, once we know which one is the latest.
pub async fn update_bill_latest_action(
    tx: &mut Transaction<'_, Postgres>,
    billtype: &str,
    billnumber: i32,
    congress: i32,
    latest_action_id: Option<i64>,
    latest_action_date: Option<chrono::NaiveDate>,
) -> Result<()> {
    sqlx::query(
        r#"
UPDATE bills
SET latest_action_id = $4,
    latest_action_date = $5
WHERE billtype = $1
  AND billnumber = $2
  AND congress = $3
        "#,
    )
    .bind(billtype)
    .bind(billnumber)
    .bind(congress)
    .bind(latest_action_id)
    .bind(latest_action_date)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deletes all cosponsors for a bill (before re-inserting).
pub async fn delete_bill_cosponsors(
    tx: &mut Transaction<'_, Postgres>,
    billtype: &str,
    billnumber: i32,
    congress: i32,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM bill_cosponsors WHERE billtype = $1 AND billnumber = $2 AND congress = $3",
    )
    .bind(billtype)
    .bind(billnumber)
    .bind(congress)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Inserts a bill cosponsor. Skips duplicates silently.
pub async fn insert_bill_cosponsor(
    tx: &mut Transaction<'_, Postgres>,
    cosponsor: &InsertBillCosponsorParams,
) -> Result<()> {
    sqlx::query(
        r#"
INSERT INTO bill_cosponsors (billtype, billnumber, congress, bioguide_id, full_name, state, party, sponsorship_date, is_original_cosponsor)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
ON CONFLICT ON CONSTRAINT bill_cosponsors_pkey DO NOTHING
        "#,
    )
    .bind(&cosponsor.billtype)
    .bind(cosponsor.billnumber)
    .bind(cosponsor.congress)
    .bind(&cosponsor.bioguide_id)
    .bind(&cosponsor.full_name)
    .bind(&cosponsor.state)
    .bind(&cosponsor.party)
    .bind(cosponsor.sponsorship_date)
    .bind(cosponsor.is_original_cosponsor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Inserts or updates a committee in the `committees` reference table.
pub async fn insert_committee(
    tx: &mut Transaction<'_, Postgres>,
    committee: &InsertCommitteeParams,
) -> Result<()> {
    sqlx::query(
        r#"
INSERT INTO committees (committee_code, committee_name, chamber)
VALUES ($1, $2, $3)
ON CONFLICT (committee_code) DO UPDATE SET
    committee_name = excluded.committee_name,
    chamber = excluded.chamber
        "#,
    )
    .bind(&committee.committee_code)
    .bind(&committee.committee_name)
    .bind(&committee.chamber)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deletes all committee associations for a bill.
pub async fn delete_bill_committees(
    tx: &mut Transaction<'_, Postgres>,
    billtype: &str,
    billnumber: i32,
    congress: i32,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM bill_committees WHERE billtype = $1 AND billnumber = $2 AND congress = $3",
    )
    .bind(billtype)
    .bind(billnumber)
    .bind(congress)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Links a bill to a committee in the `bill_committees` join table.
pub async fn insert_bill_committee(
    tx: &mut Transaction<'_, Postgres>,
    billtype: &str,
    billnumber: i32,
    congress: i32,
    committee_code: &str,
) -> Result<()> {
    sqlx::query(
        r#"
INSERT INTO bill_committees (billtype, billnumber, congress, committee_code)
VALUES ($1, $2, $3, $4)
ON CONFLICT ON CONSTRAINT bill_committees_pkey DO NOTHING
        "#,
    )
    .bind(billtype)
    .bind(billnumber)
    .bind(congress)
    .bind(committee_code)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deletes all subjects for a bill.
pub async fn delete_bill_subjects(
    tx: &mut Transaction<'_, Postgres>,
    billtype: &str,
    billnumber: i32,
    congress: i32,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM bill_subjects WHERE billtype = $1 AND billnumber = $2 AND congress = $3",
    )
    .bind(billtype)
    .bind(billnumber)
    .bind(congress)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Inserts a bill subject. Skips duplicates silently.
pub async fn insert_bill_subject(
    tx: &mut Transaction<'_, Postgres>,
    subject: &InsertBillSubjectParams,
) -> Result<()> {
    sqlx::query(
        r#"
INSERT INTO bill_subjects (billtype, billnumber, congress, subject)
VALUES ($1, $2, $3, $4)
ON CONFLICT ON CONSTRAINT bill_subjects_pkey DO NOTHING
        "#,
    )
    .bind(&subject.billtype)
    .bind(subject.billnumber)
    .bind(subject.congress)
    .bind(&subject.subject)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
