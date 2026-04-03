# Congress Scraper (Rust)

A Rust application that scrapes, parses, and stores U.S. congressional vote and bill data into PostgreSQL. It uses a Python subprocess to sync raw data files, then processes them in parallel using Tokio, with Redis cache invalidation on writes.

## Source Files

### `src/main.rs`

Entry point. Initializes structured JSON logging via `tracing`, loads configuration, opens the database pool, and orchestrates the full scraper run. Iterates over congress sessions (101 to current), conditionally running vote and bill processing. Clears the Redis API cache if any writes occurred, and reports final statistics.

This file also demonstrates **Rust's module system** — every other `.rs` file in the project must be declared here with `mod`. Without a `mod` line, the compiler simply doesn't know the file exists:

```rust
mod bills;
mod config;
mod db;
// ...
```

Think of this as Rust's version of `import` in Python, but required at the crate level to even compile the file. This is different from Python or JavaScript where any file is automatically importable.

### `src/config.rs`

Configuration management. Loads settings from environment variables (`CONGRESS_DIR`, `POSTGRES_URI`, `REDIS_URL`, etc.). Provides helpers like `current_congress()` (calculates from the current year) and `env_enabled()` (parses boolean env vars).

This module shows how Rust handles configuration without a framework — environment variables are read with `std::env::var()`, which returns `Result<String, VarError>`. That `Result` forces you to decide what to do when a variable is missing: provide a default, or fail with a clear error message. There is no silent `undefined` like in JavaScript.

### `src/votes.rs`

Vote processing pipeline. Calls a Python subprocess to sync vote data, then collects and parses vote JSON files in parallel. Uses a semaphore-gated `JoinSet` for concurrent parsing (up to 64 workers), offloads CPU-heavy JSON deserialization to `spawn_blocking`, and writes results to PostgreSQL with a second semaphore limiting DB concurrency to 4. Tracks file hashes to skip unchanged votes.

**Why two semaphores?** The parsing semaphore (64) matches the number of CPU cores available for crunching through JSON. The write semaphore (4) matches the database connection pool size — even though parsing is fast on many cores, the database can only handle so many concurrent writes before performance degrades. This two-tier design keeps both the CPU and database optimally loaded without overwhelming either.

### `src/bills.rs`

Bill processing pipeline. Handles 8 bill types (`s`, `hr`, `hconres`, `hjres`, `hres`, `sconres`, `sjres`, `sres`) across congresses 93 to present. Parses both XML (new and legacy schemas) and JSON formats. Same parallel architecture as votes: semaphore-gated parsing with `spawn_blocking`, followed by semaphore-gated transactional DB writes that upsert bills, actions, cosponsors, committees, and subjects.

**Why both XML and JSON?** The upstream data source changed formats over time. Older congresses use a legacy XML schema, newer ones use a different XML schema, and vote data comes as JSON. Rather than trying to normalize everything upstream, the scraper handles each format with its own set of serde structs and maps them all into the same database tables.

### `src/models.rs`

Data structures for serialization and database insertion. Includes:
- **Insert parameter structs** (`InsertVoteParams`, `InsertBillParams`, etc.) — these are the "shapes" that match database columns. Each field corresponds to a SQL parameter (`$1`, `$2`, etc.).
- **Parsed intermediate types** (`ParsedVote`, `ParsedBill`, `ParsedCommittee`) — these bundle the insert params with related data (like vote members or bill cosponsors) so a single unit can be passed to the database write function.
- **XML deserialization structs** (`BillXmlRootNew`, `BillXmlRootLegacy`, and nested types) — these mirror the XML structure so `serde` can parse it automatically. The `#[serde(rename = "...")]` attributes map XML tag names to Rust field names.
- **Vote JSON deserialization structs** (`VoteJson`, `VoteMemberJson`) — same approach for JSON vote files.

The separation between "deserialization structs" and "insert structs" is intentional. The raw XML/JSON shape is dictated by the data source and can be messy. The insert structs are clean and match what the database expects. A conversion step in between cleans up the data.

### `src/db.rs`

Database operations using `sqlx`. Opens a connection pool (max 4 connections) and provides async functions for inserting/deleting votes, bills, actions, cosponsors, committees, and subjects. All write operations run within transactions using prepared statements.

**Why transactions?** A bill has related data — actions, cosponsors, committees, subjects. If the bill insert succeeds but the cosponsor insert fails, you'd have an incomplete record in the database. By wrapping everything in a transaction, either ALL the data goes in, or NONE of it does. The database stays consistent.

**Why prepared statements (`.bind()`)?** Two reasons: security (prevents SQL injection — user data never gets interpreted as SQL) and performance (the database can reuse the query plan across executions).

### `src/hashes.rs`

File change detection via SHA-256 hashing. `FileHashStore` persists a map of file path to hash (serialized with bincode). Before processing a file, `needs_processing()` computes its hash and compares it against the stored value. Files are re-processed only when their content has changed.

**Why hashing instead of timestamps?** Timestamps can lie — a file can be "touched" (timestamp updated) without its content changing, or copied from another machine with a different timestamp. A SHA-256 hash is a fingerprint of the actual file content. If even one byte changes, the hash will be completely different. This is the same approach Git uses to track file changes.

### `src/redis_cache.rs`

Redis cache invalidation. After writes occur, `clear_api_cache()` uses cursor-based `SCAN` to find all keys matching `csearch:*` and deletes them in batches of 100.

**Why `SCAN` instead of `KEYS *`?** Redis is single-threaded. The `KEYS` command blocks the entire Redis server while it finds all matching keys — on a server with millions of keys, this can freeze Redis for seconds. `SCAN` iterates through keys in small batches using a cursor, so Redis can still serve other clients between batches. It takes a little more code but is much safer in production.

**Why is cache clearing non-fatal?** If Redis is down, the API will serve stale cached data — which is not great, but it's not a data loss scenario. The scraper logs a warning instead of crashing, because the primary job (writing data to PostgreSQL) already succeeded.

### `src/python.rs`

Python subprocess management. `run_congress_task()` spawns a Python process with the appropriate `PYTHONPATH`, pipes stdout/stderr, and streams output to structured logs via two concurrent Tokio tasks.

**Why does a Rust program call Python?** The upstream `congress` library is a Python tool that syncs raw data files from Congress.gov. Instead of rewriting that tool in Rust (which would mean maintaining our own version of a complex data sync protocol), we call it as a subprocess. This is a common pragmatic pattern: port the parts that benefit from Rust (parsing, concurrency, database writes) and keep the parts that work fine as-is.

**Why two Tokio tasks for stdout and stderr?** Operating systems give each stream (stdout, stderr) a fixed-size buffer (typically 64 KB). If the child process writes a lot to stderr and nobody reads it, the buffer fills up and the child process **blocks** — it literally cannot write another byte until someone reads from the pipe. If the parent is only reading stdout, you get a deadlock. Reading both streams concurrently prevents this.

### `src/stats.rs`

Simple counters (`RunStats`) tracking processed, skipped, and failed counts for both bills and votes. The `has_writes()` method checks whether any data was actually written, which determines whether Redis cache invalidation is needed.

This struct derives `Default`, which means `RunStats::default()` creates an instance with all counters at zero — no manual initialization needed.

### `src/util.rs`

Utility functions for date parsing (multiple formats including `YYYY-MM-DD` and RFC 3339), integer parsing, file existence checks, and empty-string-to-`None` conversion.

**Why multiple date formats?** The congressional data sources are inconsistent — some files use `2024-01-15`, others use `2024-01-15T10:30:00Z`, and some use other variations. Rather than failing on unexpected formats, `parse_date` tries each format in sequence and returns the first successful parse. This defensive approach is common when working with real-world data from external sources.

## Tokio Usage

The scraper is built entirely on Tokio's async runtime. Here's how each Tokio feature is used.

### Runtime Initialization

The `#[tokio::main]` macro creates a multi-threaded Tokio runtime at the program entry point. Without this, Rust's `main()` function is synchronous — you can't use `.await`:

```rust
#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!(?err, "scraper failed");
            ExitCode::FAILURE
        }
    }
}
```

**What the macro does behind the scenes:** It creates a Tokio runtime (a thread pool that executes async tasks), runs your async `main` on it, and shuts it down when `main` returns. Without the macro, you'd write this yourself with `tokio::runtime::Runtime::new().unwrap().block_on(async { ... })`.

### Semaphore-Gated Concurrency

Both votes and bills use a two-tier semaphore pattern to limit parallelism. A high-concurrency semaphore gates CPU-bound parsing, and a low-concurrency one gates database writes:

```rust
const WORKER_LIMIT: usize = 64;
const DB_WRITE_CONCURRENCY: usize = 4;

// Parsing phase — up to 64 files parsed simultaneously
let parse_sem = Arc::new(Semaphore::new(WORKER_LIMIT));
let mut tasks = JoinSet::new();

for job in jobs {
    let parse_sem = parse_sem.clone();
    let known_hashes = known_hashes.clone();
    tasks.spawn(async move {
        let _permit = parse_sem.acquire_owned().await?;
        tokio::task::spawn_blocking(move || parse_vote_job(job, &known_hashes)).await?
    });
}

// Write phase — up to 4 database writes simultaneously
let write_sem = Arc::new(Semaphore::new(DB_WRITE_CONCURRENCY));
let mut write_tasks = JoinSet::new();

for changed_vote in changed_votes {
    let write_sem = write_sem.clone();
    let pool = pool.clone();
    write_tasks.spawn(async move {
        let _permit = write_sem.acquire_owned().await?;
        insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
        Ok::<_, anyhow::Error>(changed_vote)
    });
}
```

**How the semaphore works:** Think of it as a counting lock. When you create `Semaphore::new(4)`, there are 4 "permits" available. Each task calls `.acquire_owned()` to take a permit before doing work. If all 4 permits are taken, the next task **waits** until one finishes and drops its permit. The `_permit` variable holds the permit — when it goes out of scope (the task finishes), the permit is automatically returned.

**Why `Arc::clone()` on every loop iteration?** Each spawned task might outlive the current loop iteration. Rust's ownership rules prevent sharing a reference to data that might be dropped. `Arc` (Atomically Reference Counted) lets multiple tasks share the same semaphore/pool by bumping a reference count instead of copying the actual data.

### `spawn_blocking` for CPU-Bound Work

JSON and XML parsing is synchronous and CPU-intensive. To avoid blocking the async executor, it's offloaded to Tokio's blocking thread pool:

```rust
tasks.spawn(async move {
    let _permit = parse_sem.acquire_owned().await?;
    tokio::task::spawn_blocking(move || parse_bill_job(job, &known_hashes)).await?
});
```

**Why can't we just run the parsing directly?** Tokio's async runtime uses a small number of threads (typically equal to CPU cores) that cooperatively multitask by switching between tasks at `.await` points. A CPU-heavy function like JSON parsing never hits an `.await` — it just crunches data for milliseconds or more. During that time, the thread is monopolized and no other async tasks (like database queries or Redis calls) can run on it. `spawn_blocking` moves the heavy work to a separate, dedicated thread pool where blocking is expected.

### `JoinSet` for Task Collection

`JoinSet` manages groups of spawned tasks and collects their results. Think of it as a "bag of futures" — you put tasks in, then drain results out one at a time:

```rust
let mut write_tasks = JoinSet::new();
// ... spawn tasks ...

while let Some(result) = write_tasks.join_next().await {
    match result {
        Ok(Ok(changed_vote)) => {
            hashes.mark_processed(&changed_vote.path, changed_vote.hash);
            stats.votes_processed += 1;
        }
        Ok(Err(err)) => {
            warn!(?err, "vote write failed");
            stats.votes_failed += 1;
        }
        Err(err) => {
            warn!(?err, "vote write task panicked");
            stats.votes_failed += 1;
        }
    }
}
```

**Understanding the double `Result`:** This is the trickiest part for newcomers. There are two layers of failure:

| Pattern | What happened |
|---|---|
| `Ok(Ok(changed_vote))` | The task ran to completion AND the work inside succeeded |
| `Ok(Err(err))` | The task ran to completion BUT the work returned an error (e.g., a DB insert failed) |
| `Err(err)` | The task itself failed — it panicked, was cancelled, or something went wrong at the Tokio level |

The outer `Result` comes from `JoinSet` (did the task finish normally?). The inner `Result` comes from your task's return value (did the business logic succeed?). You need to handle both.

### `tokio::spawn` for Concurrent I/O Streaming

When running the Python subprocess, two tasks are spawned to concurrently stream stdout and stderr without blocking the main wait on process exit:

```rust
let stdout_task = tokio::spawn(stream_output("stdout", stdout));
let stderr_task = tokio::spawn(stream_output("stderr", stderr));

let status = child.wait().await?;
stdout_task.await??;
stderr_task.await??;
```

**What's `??` (double question mark)?** Each `?` unwraps one layer of `Result`. `stdout_task.await` returns `Result<Result<(), anyhow::Error>, JoinError>` — the outer `Result` from the task join, the inner from your function. The first `?` unwraps the `JoinError`, the second unwraps the `anyhow::Error`. It's the same double-Result pattern as `JoinSet`, just written more concisely.

### Async Buffered Line Reading

Python subprocess output is read line-by-line using Tokio's async I/O. This feeds each line into structured logging so Python output shows up in the same JSON log stream as Rust output:

```rust
use tokio::io::{AsyncBufReadExt, BufReader};

async fn stream_output(source: &str, reader: impl AsyncRead + Unpin) -> anyhow::Result<()> {
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        info!(stream = source, output = line, "python");
    }
    Ok(())
}
```

**What's `impl AsyncRead + Unpin`?** This is a trait bound — it says "I accept any type that implements both `AsyncRead` (can be read asynchronously) and `Unpin` (safe to move in memory)." Both `ChildStdout` and `ChildStderr` satisfy these traits, so the same function works for both streams.

### Async Database Operations (sqlx)

All database interactions are non-blocking. The connection pool is configured with the tokio-rustls runtime, which means TLS encryption and database communication happen asynchronously — the program can do other work while waiting for the database to respond:

```rust
// Cargo.toml
// sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "postgres", "chrono"] }

let pool = PgPoolOptions::new()
    .max_connections(DB_WRITE_CONCURRENCY)
    .connect(&cfg.postgres_dsn())
    .await?;
```

Transactions are started and committed asynchronously:

```rust
let mut tx = pool.begin().await?;

sqlx::query("INSERT INTO votes (...) VALUES (...) ON CONFLICT (...) DO UPDATE SET ...")
    .bind(&params.vote_id)
    // ...
    .execute(&mut *tx)
    .await?;

tx.commit().await?;
```

**What's `&mut *tx`?** This is a double dereference + re-borrow. `tx` is a `Transaction<Postgres>` which wraps a connection. `*tx` dereferences to the inner connection, and `&mut` re-borrows it mutably. SQLx's `.execute()` expects a mutable reference to a connection-like type, and this is how you get one from a transaction. It looks odd at first but becomes muscle memory.

### Async Redis Operations

Redis uses a multiplexed async connection with cursor-based scanning. "Multiplexed" means a single TCP connection handles multiple commands concurrently — this is more efficient than opening a new connection for each command:

```rust
let mut connection = client.get_multiplexed_async_connection().await?;

loop {
    let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
        .cursor_arg(cursor)
        .arg("MATCH")
        .arg("csearch:*")
        .arg("COUNT")
        .arg(100)
        .query_async(&mut connection)
        .await?;

    if !keys.is_empty() {
        let removed: i64 = redis::cmd("DEL")
            .arg(&keys)
            .query_async(&mut connection)
            .await?;
    }

    cursor = next_cursor;
    if cursor == 0 { break; }
}
```

**How the cursor works:** Redis `SCAN` is paginated. You start with cursor `0`, and each response gives you (next_cursor, batch_of_keys). You keep calling SCAN with the returned cursor until you get cursor `0` back, which means you've iterated through the entire keyspace. The `COUNT 100` is a hint (not a guarantee) for how many keys to return per batch.

### Concurrency Architecture Summary

```
main()
  |
  +-- run()
       |
       +-- For each congress session:
       |     |
       |     +-- update_votes()  --> tokio::spawn Python subprocess
       |     |                        +-- tokio::spawn(stream stdout)
       |     |                        +-- tokio::spawn(stream stderr)
       |     |
       |     +-- process_votes()
       |     |     +-- collect_changed_votes()
       |     |     |     +-- JoinSet: up to 64 tasks (Semaphore)
       |     |     |           +-- spawn_blocking(parse_vote_job)
       |     |     |
       |     |     +-- JoinSet: up to 4 DB writes (Semaphore)
       |     |           +-- insert_parsed_vote() [async sqlx tx]
       |     |
       |     +-- update_bills()  --> tokio::spawn Python subprocess
       |     |
       |     +-- process_bills()
       |           +-- collect_changed_bills()
       |           |     +-- JoinSet: up to 64 tasks (Semaphore)
       |           |           +-- spawn_blocking(parse_bill_job)
       |           |
       |           +-- JoinSet: up to 4 DB writes (Semaphore)
       |                 +-- insert_parsed_bill() [async sqlx tx]
       |
       +-- clear_api_cache()  --> async Redis SCAN + DEL
```

**Reading this diagram:** The scraper runs **sequentially** at the top level (one congress at a time, votes before bills). Within each step, work fans out **concurrently** — up to 64 files are parsed in parallel, then up to 4 database writes happen in parallel. This design keeps the overall flow simple and predictable while still getting high throughput where it matters most.
