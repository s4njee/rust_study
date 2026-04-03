# Services And Configuration

This doc covers the code that talks to databases, Redis, environment variables,
logs, and subprocesses.

## SQLx For Database Work

In plain English: SQLx lets you talk to Postgres without giving up plain SQL.
You still write SQL yourself, but the Rust side manages connections, parameter
binding, and async execution. Unlike ORMs that hide the SQL, SQLx keeps it
visible — which makes debugging and performance tuning much easier.

### Connection Pooling

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
query is wasteful — each connection involves a TCP handshake, TLS negotiation,
and authentication. A pool keeps a reusable set of connections ready and hands
them out to tasks on demand.

Key details:

- `max_connections(4)` caps the pool size to match the write semaphore (4
  concurrent DB operations)
- The pool is created **once** at startup and shared via `&PgPool` references
  (or `Arc<PgPool>` across tasks)
- If all connections are in use, the next `.acquire()` call will wait until
  one becomes available

## Parameter Binding

In plain English: `.bind(...)` is how you safely pass real values into SQL
without building fragile SQL strings by hand. This prevents SQL injection and
handles type conversion automatically.

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

Each `.bind()` call corresponds to a `$N` placeholder in the SQL. The types
are checked at compile time when using `sqlx::query!()` (with the `!`), or at
runtime when using `sqlx::query()` (without the `!`).

**Why not string formatting?** Compare:

```rust
// DANGEROUS — never do this:
let sql = format!("SELECT * FROM votes WHERE voteid = '{}'", user_input);

// SAFE — SQLx escapes and types the value for you:
sqlx::query("SELECT * FROM votes WHERE voteid = $1").bind(&user_input)
```

The first approach lets attackers inject arbitrary SQL. The second sends the
value as a separate parameter, so the database never interprets it as SQL.

## Transactions

In plain English: a transaction is an all-or-nothing bundle of DB changes.
Either every statement commits, or none of them do. This guarantees consistency
in multi-step operations.

```rust
let mut tx = pool.begin().await?;
db::insert_vote(&mut tx, &parsed_vote.vote).await?;
db::delete_vote_members(&mut tx, &parsed_vote.vote.voteid).await?;
db::insert_vote_members(&mut tx, &parsed_vote.vote.voteid, &parsed_vote.members).await?;
tx.commit().await?;
```

If any step fails before `commit`, the changes roll back. That is exactly what
you want when several records need to stay in sync — you never end up with a
vote in the database but no members, or members without a vote.

The RAII pattern is at work here too: if the `tx` variable is dropped without
calling `.commit()`, the transaction is **automatically rolled back**. This
means early returns via `?` are always safe.

```rust
// This is safe — if any ? triggers, tx is dropped and rolled back
let mut tx = pool.begin().await?;
do_first_thing(&mut tx).await?;   // rolls back if this fails
do_second_thing(&mut tx).await?;  // rolls back if this fails
tx.commit().await?;               // only commits if everything succeeded
```

## Redis Async Operations

In plain English: Redis is used here like a fast side cache. The code connects,
checks that Redis is alive, then scans and clears matching cache keys when new
data is written to Postgres.

### Connecting and verifying

```rust
let client = redis::Client::open(cfg.redis_url.clone())?;
let mut connection = client.get_multiplexed_async_connection().await?;

// Ping to verify the connection is alive
let _: String = redis::cmd("PING").query_async(&mut connection).await?;
```

`get_multiplexed_async_connection()` creates a single TCP connection that can
handle multiple concurrent Redis commands. This is more efficient than opening
a new connection for each command.

### Cursor-based SCAN

The code uses `SCAN` instead of `KEYS *` because `SCAN` is safer for a live
server. `KEYS *` blocks Redis while it finds all matching keys. `SCAN` walks
the keyspace in chunks, returning a cursor for pagination:

```rust
let mut cursor = 0_u64;
let mut deleted = 0_usize;

loop {
    // SCAN returns (next_cursor, list_of_matching_keys)
    let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
        .cursor_arg(cursor)
        .arg("MATCH")
        .arg(format!("{REDIS_CACHE_KEY_PREFIX}*"))
        .arg("COUNT")
        .arg(100)  // hint: process ~100 keys per iteration
        .query_async(&mut connection)
        .await?;

    if !keys.is_empty() {
        let removed: i64 = redis::cmd("DEL")
            .arg(&keys)
            .query_async(&mut connection)
            .await?;
        deleted += removed as usize;
    }

    cursor = next_cursor;
    if cursor == 0 {
        break;  // cursor == 0 means the full scan is complete
    }
}
```

See `csearch-rscraper/src/redis_cache.rs` for the full implementation.

**Important note:** The cache invalidation is **non-fatal**. If Redis is down
or the clear fails, the scraper logs a warning and continues. The API will
serve stale cached data until Redis is cleared, but the data pipeline is not
blocked:

```rust
match redis_cache::clear_api_cache(&cfg).await {
    Ok(deleted) => info!(keys_deleted = deleted, "redis api cache cleared"),
    Err(err) => warn!(error = %err, "unable to clear redis api cache"),
}
```

## Structured Logging With `tracing`

In plain English: `tracing` logs are meant for machines and people. Instead of
writing one big string, you attach named fields that can be parsed, filtered,
and searched later.

### Basic logging

```rust
info!(
    run_votes = cfg.run_votes,
    run_bills = cfg.run_bills,
    "scraper run starting"
);
```

That produces a JSON log line like:

```json
{"level":"INFO","run_votes":true,"run_bills":true,"message":"scraper run starting"}
```

A helpful mindset:

- `println!` is for simple local output (user-facing CLI messages)
- `tracing` is for real application events (what happened, where, why)

### Log level selection

Different levels communicate different urgency:

```rust
// Operational success
info!(keys_deleted = deleted, "redis api cache cleared");

// Non-fatal issue — things still work, but something is degraded
warn!(error = %err, "unable to clear redis api cache");

// Fatal failure
error!(error = %err, "scraper run failed");
```

### Setting up the subscriber

```rust
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| {
            let level = std::env::var("LOG_LEVEL")
                .unwrap_or_else(|_| "info".to_string());
            EnvFilter::try_new(level)
        })
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .json()                     // output JSON lines
        .with_env_filter(filter)    // respect RUST_LOG or LOG_LEVEL
        .with_current_span(false)   // omit span details
        .with_span_list(false)      // omit span hierarchy
        .init();
}
```

The JSON format is chosen because the scraper runs in Kubernetes, where log
aggregation tools (like Loki or CloudWatch) work best with structured JSON.

## Error Handling With `anyhow`

In plain English: `anyhow` is a practical way to say, "I care about good error
messages more than building a giant custom error hierarchy right now." It is the
go-to error handling library for applications (as opposed to libraries, which
often use `thiserror`).

Patterns used across the repo:

```rust
// ? propagates the error up to the caller
let cfg = Config::load()?;

// bail! creates and returns an error immediately
bail!("CONGRESSDIR does not exist: {}", congress_dir.display());

// .context() adds a human-readable explanation to an existing error
let pool = PgPoolOptions::new()
    .connect(&cfg.postgres_dsn())
    .await
    .context("connect to postgres")?;
```

Why it is useful:

- errors stay easy to propagate — `?` works with any error type
- messages gain **context** as they bubble upward, creating a chain:
  `"connect to postgres: Connection refused (os error 111)"`
- command-line failures become much easier to diagnose

**`anyhow` vs `thiserror`:**

| Library | Best for | How it works |
|---|---|---|
| `anyhow` | Applications | Wraps any error into a single unified type |
| `thiserror` | Libraries | Generates custom error enums with `#[derive(Error)]` |

This repo uses `anyhow` everywhere because these are all applications, not
reusable libraries consumed by other crates.

## Environment Variables And `.env`

In plain English: configuration is mostly loaded from the environment so the
same binary can run in different places without editing code. This is the
[twelve-factor app](https://12factor.net/config) approach.

```rust
let _ = dotenvy::dotenv();
```

That line says, "if a `.env` file exists, load it." The `let _ =` discards the
result — it is fine if no `.env` file exists (like in production, where env vars
come from the container runtime).

The repo shows three configuration patterns:

**Required variables with custom errors:**

```rust
let congress_dir = env::var("CONGRESSDIR")
    .map(PathBuf::from)
    .map_err(|_| anyhow::anyhow!("missing CONGRESSDIR"))?;
```

**Optional variables with sensible defaults:**

```rust
let redis_url = env::var("REDIS_URL")
    .unwrap_or_else(|_| "redis://localhost:6379".to_string());
```

**Parsing numbers and booleans from strings:**

```rust
// Number parsing with chained Option operations
let db_port: u16 = env::var("DB_PORT")
    .ok()                              // Result → Option (discard error)
    .and_then(|val| val.parse().ok())  // try to parse → Option<u16>
    .unwrap_or(5432);                  // fall back to default

// Boolean parsing using the let-else pattern
pub fn env_enabled(name: &str, default_value: bool) -> bool {
    let Ok(value) = env::var(name) else {
        return default_value;
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default_value,
    }
}
```

The `let ... else` pattern is worth studying: it says "if this destructuring
works, bind the result; otherwise, run the else block (which must diverge — in
this case, `return`)."

## Async Subprocess Management

In plain English: sometimes the Rust program still needs to kick off another
tool, like the Python Congress scraper. The repo runs those child processes
without freezing the rest of the app.

```rust
let mut child = command.spawn()
    .with_context(|| format!("spawn {}", run_py.display()))?;
```

Then it reads stdout and stderr **concurrently** so neither pipe fills up and
blocks the child process. This is a real-world gotcha: if you only read stdout,
the child's stderr buffer can fill up, causing the child to block on its next
write, creating a deadlock.

```rust
// Read both pipes concurrently using tokio's async I/O
let stdout_handle = tokio::spawn(async move {
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        info!(stream = "stdout", "{line}");
    }
    Ok::<_, anyhow::Error>(())
});

let stderr_handle = tokio::spawn(async move {
    let reader = BufReader::new(stderr);
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        warn!(stream = "stderr", "{line}");
    }
    Ok::<_, anyhow::Error>(())
});

// Wait for the process to finish
let status = child.wait().await?;
```

## Module Organization

In plain English: Rust likes explicit structure. If a file is a module, you say
so with `mod`. This is different from Python (where any `.py` file is
automatically importable) or Node (where any file can be `require()`d).

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

Each `mod` declaration is doing three things:

1. Telling the compiler that `src/bills.rs` exists and should be compiled
2. Making its `pub` items available under `crate::bills`
3. Establishing a visibility boundary — private items in `bills.rs` are not
   accessible from other modules

Then `use` brings specific items into scope:

```rust
// Bring items from the standard library
use std::process::ExitCode;
use std::time::Instant;

// Bring items from external crates
use tracing::{error, info, warn};

// Bring items from this project's modules
use crate::config::Config;
use crate::hashes::FileHashStore;
```

That can feel verbose at first, but it makes project boundaries easy to see.
You can always tell where something comes from by looking at the `use`
statements at the top of the file.

## Mental Shortcut

Use this rule of thumb:

| Tool | What it manages | When to use it |
|---|---|---|
| **SQLx** | Durable data | Anything that must survive restarts |
| **Redis** | Fast temporary cache | Data that is expensive to compute but OK to lose |
| **`tracing`** | What happened | Application events, metrics, debugging |
| **`anyhow`** | Why it failed | Error messages with context chains |
| **env vars** | How the app is configured | Per-environment settings |
| **child processes** | Call out to external tools | When porting is not yet complete |

## Good Next Step

Read [`05-cli-files-and-media.md`](05-cli-files-and-media.md) next if you want
to see the patterns used in the smaller, self-contained Phase 1 projects.
