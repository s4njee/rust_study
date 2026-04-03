# Congress Scraper (Rust)

A Rust application that scrapes, parses, and stores U.S. congressional vote and bill data into PostgreSQL. It uses a Python subprocess to sync raw data files, then processes them in parallel using Tokio, with Redis cache invalidation on writes.

## Source Files

### `src/main.rs`

Entry point. Initializes structured JSON logging via `tracing`, loads configuration, opens the database pool, and orchestrates the full scraper run. Iterates over congress sessions (101 to current), conditionally running vote and bill processing. Clears the Redis API cache if any writes occurred, and reports final statistics.

### `src/config.rs`

Configuration management. Loads settings from environment variables (`CONGRESS_DIR`, `POSTGRES_URI`, `REDIS_URL`, etc.). Provides helpers like `current_congress()` (calculates from the current year) and `env_enabled()` (parses boolean env vars).

### `src/votes.rs`

Vote processing pipeline. Calls a Python subprocess to sync vote data, then collects and parses vote JSON files in parallel. Uses a semaphore-gated `JoinSet` for concurrent parsing (up to 64 workers), offloads CPU-heavy JSON deserialization to `spawn_blocking`, and writes results to PostgreSQL with a second semaphore limiting DB concurrency to 4. Tracks file hashes to skip unchanged votes.

### `src/bills.rs`

Bill processing pipeline. Handles 8 bill types (`s`, `hr`, `hconres`, `hjres`, `hres`, `sconres`, `sjres`, `sres`) across congresses 93 to present. Parses both XML (new and legacy schemas) and JSON formats. Same parallel architecture as votes: semaphore-gated parsing with `spawn_blocking`, followed by semaphore-gated transactional DB writes that upsert bills, actions, cosponsors, committees, and subjects.

### `src/models.rs`

Data structures for serialization and database insertion. Includes:
- Insert parameter structs (`InsertVoteParams`, `InsertBillParams`, etc.)
- Parsed intermediate types (`ParsedVote`, `ParsedBill`, `ParsedCommittee`)
- XML deserialization structs (`BillXmlRootNew`, `BillXmlRootLegacy`, and nested types)
- Vote JSON deserialization structs (`VoteJson`, `VoteMemberJson`)

### `src/db.rs`

Database operations using `sqlx`. Opens a connection pool (max 4 connections) and provides async functions for inserting/deleting votes, bills, actions, cosponsors, committees, and subjects. All write operations run within transactions using prepared statements.

### `src/hashes.rs`

File change detection via SHA-256 hashing. `FileHashStore` persists a map of file path to hash (serialized with bincode). Before processing a file, `needs_processing()` computes its hash and compares it against the stored value. Files are re-processed only when their content has changed.

### `src/redis_cache.rs`

Redis cache invalidation. After writes occur, `clear_api_cache()` uses cursor-based `SCAN` to find all keys matching `csearch:*` and deletes them in batches of 100.

### `src/python.rs`

Python subprocess management. `run_congress_task()` spawns a Python process with the appropriate `PYTHONPATH`, pipes stdout/stderr, and streams output to structured logs via two concurrent Tokio tasks.

### `src/stats.rs`

Simple counters (`RunStats`) tracking processed, skipped, and failed counts for both bills and votes.

### `src/util.rs`

Utility functions for date parsing (multiple formats including `YYYY-MM-DD` and RFC 3339), integer parsing, file existence checks, and empty-string-to-`None` conversion.

## Tokio Usage

The scraper is built entirely on Tokio's async runtime. Here's how each Tokio feature is used.

### Runtime Initialization

The `#[tokio::main]` macro creates a multi-threaded Tokio runtime at the program entry point:

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

### Semaphore-Gated Concurrency

Both votes and bills use a two-tier semaphore pattern to limit parallelism. A high-concurrency semaphore gates CPU-bound parsing, and a low-concurrency one gates database writes:

```rust
const WORKER_LIMIT: usize = 64;
const DB_WRITE_CONCURRENCY: usize = 4;

// Parsing phase
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

// Write phase
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

### `spawn_blocking` for CPU-Bound Work

JSON and XML parsing is synchronous and CPU-intensive. To avoid blocking the async executor, it's offloaded to Tokio's blocking thread pool:

```rust
tasks.spawn(async move {
    let _permit = parse_sem.acquire_owned().await?;
    tokio::task::spawn_blocking(move || parse_bill_job(job, &known_hashes)).await?
});
```

### `JoinSet` for Task Collection

`JoinSet` manages groups of spawned tasks and collects their results:

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

The double `Result` pattern (`Ok(Ok(...))` / `Ok(Err(...))` / `Err(...)`) handles both task-level panics (outer `Result` from `JoinSet`) and application-level errors (inner `Result` from the task body).

### `tokio::spawn` for Concurrent I/O Streaming

When running the Python subprocess, two tasks are spawned to concurrently stream stdout and stderr without blocking the main wait on process exit:

```rust
let stdout_task = tokio::spawn(stream_output("stdout", stdout));
let stderr_task = tokio::spawn(stream_output("stderr", stderr));

let status = child.wait().await?;
stdout_task.await??;
stderr_task.await??;
```

### Async Buffered Line Reading

Python subprocess output is read line-by-line using Tokio's async I/O:

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

### Async Database Operations (sqlx)

All database interactions are non-blocking. The connection pool is configured with the tokio-rustls runtime:

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

### Async Redis Operations

Redis uses a multiplexed async connection with cursor-based scanning:

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
