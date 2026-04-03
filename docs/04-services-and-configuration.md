# Services And Configuration

This doc covers the code that talks to databases, Redis, environment variables,
logs, and subprocesses.

## SQLx For Database Work

In plain English: SQLx lets you talk to Postgres without giving up plain SQL.
You still write SQL yourself, but the Rust side manages connections, parameter
binding, and async execution.

Connection pool example:

```rust
pub async fn open_pool(cfg: &Config) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(DB_WRITE_CONCURRENCY)
        .connect(&cfg.postgres_dsn())
        .await
        .context("connect to postgres")?;

    ensure_schema_compatibility(&pool).await?;
    Ok(pool)
}
```

Why pooling matters in plain English: opening a fresh DB connection for every
query is wasteful. A pool keeps a reusable set of connections ready.

## Parameter Binding

In plain English: `.bind(...)` is how you safely pass real values into SQL
without building fragile SQL strings by hand.

```rust
sqlx::query(
    r#"
INSERT INTO votes (voteid, bill_type, bill_number, congress, votenumber)
VALUES ($1, $2, $3, $4, $5)
    "#,
)
.bind(&vote.voteid)
.bind(&vote.bill_type)
.bind(vote.bill_number)
.bind(vote.congress)
.bind(vote.votenumber)
.execute(&mut **tx)
.await?;
```

## Transactions

In plain English: a transaction is an all-or-nothing bundle of DB changes.

```rust
let mut tx = pool.begin().await?;
db::insert_vote(&mut tx, &parsed_vote.vote).await?;
db::delete_vote_members(&mut tx, &parsed_vote.vote.voteid).await?;
tx.commit().await?;
```

If any step fails before `commit`, the changes roll back. That is exactly what
you want when several records need to stay in sync.

## Redis Async Operations

In plain English: Redis is used here like a fast side cache. The code connects,
checks that Redis is alive, then scans and clears matching cache keys.

```rust
let mut connection = client.get_multiplexed_async_connection().await?;
let _: String = redis::cmd("PING").query_async(&mut connection).await?;
```

The scanner uses `SCAN` instead of `KEYS *` because `SCAN` is safer for a live
server. It walks the keyspace in chunks instead of trying to stop the world.

## Structured Logging With `tracing`

In plain English: `tracing` logs are meant for machines and people. Instead of
writing one big string, you attach named fields.

```rust
info!(
    run_votes = cfg.run_votes,
    run_bills = cfg.run_bills,
    "scraper run starting"
);
```

That makes the logs easier to filter, search, and analyze later.

A helpful mindset:

- `println!` is for simple local output
- `tracing` is for real application events

## Error Handling With `anyhow`

In plain English: `anyhow` is a practical way to say, "I care about good error
messages more than building a giant custom error hierarchy right now."

Patterns used across the repo:

```rust
let cfg = Config::load()?;
bail!("CONGRESSDIR does not exist: {}", congress_dir.display());
.context("connect to postgres")?;
```

Why it is useful:

- errors stay easy to propagate
- messages gain context as they bubble upward
- command-line failures become much easier to understand

## Environment Variables And `.env`

In plain English: configuration is mostly loaded from the environment so the
same binary can run in different places without editing code.

```rust
let _ = dotenvy::dotenv();
```

That line says, "if a `.env` file exists, load it." It is a convenience for
local development.

The repo also shows:

- required variables with custom errors
- optional variables with sensible defaults
- parsing numbers and booleans from strings

## Async Subprocess Management

In plain English: sometimes the Rust program still needs to kick off another
tool, like the Python Congress scraper. The repo runs those child processes
without freezing the rest of the app.

```rust
let mut child = command.spawn()
    .with_context(|| format!("spawn {}", run_py.display()))?;
```

Then it reads stdout and stderr concurrently so neither pipe fills up and blocks
the child process.

## Module Organization

In plain English: Rust likes explicit structure. If a file is a module, you say
so with `mod`.

```rust
mod bills;
mod config;
mod db;
mod hashes;
mod models;
mod python;
mod redis_cache;
mod stats;
mod util;
mod votes;
```

That can feel verbose at first, but it makes project boundaries easy to see.

## Mental Shortcut

Use this rule of thumb:

- SQLx: durable data
- Redis: fast temporary cache
- `tracing`: what happened
- `anyhow`: why it failed
- env vars: how the app is configured
- child processes: call out to an external tool without blocking everything
