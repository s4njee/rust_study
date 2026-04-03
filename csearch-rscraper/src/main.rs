// ============================================================================
// main.rs — Entry point for the congress scraper
// ============================================================================
//
// In Rust, `mod` declarations tell the compiler which other source files
// belong to this project (called a "crate"). Think of each `mod` line
// like an `import` in Python or `require` in Node — it makes that file's
// public items available under `crate::module_name`.
//
// Unlike Python/JS, Rust doesn't auto-discover files. If you add a new
// file `src/foo.rs`, you must add `mod foo;` here for it to compile.
// ============================================================================

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

// `use` brings specific items into scope so you don't have to write the
// full path every time. Like `from x import y` in Python.
use std::process::ExitCode;
use std::time::Instant;

// `tracing` is Rust's structured logging library — similar to Python's
// `logging` module or `winston`/`pino` in Node. `info!`, `warn!`, `error!`
// are macros (indicated by the `!`) that emit structured log events.
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

// `crate::` refers to the root of this project, like a relative import.
use crate::config::Config;
use crate::hashes::FileHashStore;
use crate::stats::RunStats;

// ============================================================================
// #[tokio::main] — The async runtime entry point
// ============================================================================
//
// Rust doesn't have a built-in event loop like Node.js or Python's asyncio.
// Instead, you bring your own async runtime — Tokio is the most popular one.
//
// `#[tokio::main]` is a macro that:
//   1. Creates a multi-threaded Tokio runtime (like calling `asyncio.run()`)
//   2. Wraps your `async fn main()` so it runs inside that runtime
//
// Without this macro, you can't use `.await` in main — Rust's `main()`
// is synchronous by default. The macro expands to roughly:
//
//   fn main() -> ExitCode {
//       tokio::runtime::Runtime::new().unwrap().block_on(async { ... })
//   }
//
// `ExitCode` is Rust's way of returning a process exit code (0 or 1),
// similar to `sys.exit()` in Python or `process.exit()` in Node.
// ============================================================================
#[tokio::main]
async fn main() -> ExitCode {
    // Load .env file if present (like `dotenv` in Node or `python-dotenv`).
    // The `let _ =` discards the result — we don't care if .env is missing.
    let _ = dotenvy::dotenv();
    init_tracing();

    // `match` is Rust's pattern matching — like a switch/case but far more
    // powerful. Here we match on whether `run()` succeeded or failed.
    //
    // `run().await` — the `.await` keyword suspends this function until the
    // async `run()` completes. Same concept as `await` in JS/Python, but
    // placed AFTER the expression instead of before it.
    match run().await {
        // `Ok(())` means success with no return value. `()` is Rust's "unit"
        // type — equivalent to `None`/`undefined`/`void` in other languages.
        Ok(()) => ExitCode::SUCCESS,
        // `Err(err)` means something went wrong. The `error = %err` syntax
        // is tracing's structured logging format — `%` means "display format".
        Err(err) => {
            error!(error = %err, "scraper run failed");
            ExitCode::FAILURE
        }
    }
}

/// Sets up structured JSON logging.
///
/// This configures the `tracing` framework to output JSON-formatted log lines
/// (useful for log aggregation in Kubernetes). The log level can be controlled
/// via the `RUST_LOG` or `LOG_LEVEL` environment variables.
fn init_tracing() {
    // Try to read filter from RUST_LOG env var first, then fall back to
    // LOG_LEVEL, then default to "info". This chain of `.or_else()` calls
    // is like: `RUST_LOG || LOG_LEVEL || "info"` in JS.
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| {
            let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
            EnvFilter::try_new(level)
        })
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // Build the tracing subscriber (the thing that actually processes log events).
    // `.json()` makes it output JSON lines instead of human-readable text.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(false)
        .with_span_list(false)
        .init();
}

/// Main orchestration function — runs the full scraper pipeline.
///
/// `anyhow::Result<()>` is a convenience type that means "either Ok with no
/// value, or an Error with a nice error message chain". `anyhow` is Rust's
/// most popular error handling library — it lets you use `?` (the try operator)
/// to propagate errors up the call stack without writing try/catch blocks.
///
/// The `?` operator at the end of expressions like `Config::load()?` is
/// shorthand for: "if this returns Err, return that error immediately from
/// this function." It's like `throw` in JS or letting an exception propagate
/// in Python, but it's explicit at every call site.
async fn run() -> anyhow::Result<()> {
    // `Instant::now()` captures a monotonic timestamp for measuring elapsed time.
    // Like `time.monotonic()` in Python or `performance.now()` in Node.
    let started_at = Instant::now();

    // Load configuration from environment variables.
    // The `?` propagates any error (e.g., missing required env vars).
    let cfg = Config::load()?;

    // `RunStats::default()` creates a zeroed-out stats struct.
    // `Default` is a Rust trait (like an interface) that provides a default
    // constructor. For numeric fields, the default is 0.
    //
    // `mut` means this variable is mutable — in Rust, variables are
    // IMMUTABLE by default (opposite of most languages). You must opt-in
    // to mutability with `mut`. This helps prevent accidental modifications.
    let mut stats = RunStats::default();

    // Structured log: key=value pairs become fields in the JSON output.
    info!(
        run_votes = cfg.run_votes,
        run_bills = cfg.run_bills,
        "scraper run starting"
    );

    // Open a PostgreSQL connection pool (max 4 connections).
    // `.await` suspends until the pool is connected. `?` propagates errors.
    let pool = db::open_pool(&cfg).await?;

    // Build file paths for the hash stores. `.join()` is like `os.path.join()`
    // in Python or `path.join()` in Node — it concatenates path segments.
    let vote_hash_path = cfg
        .congress_dir
        .join("data")
        .join("voteHashes.rscraper.bin");
    let bill_hash_path = cfg
        .congress_dir
        .join("data")
        .join("fileHashes.rscraper.bin");

    // Load previously computed file hashes from disk (for change detection).
    // `mut` because we'll update them as we process new files.
    let mut vote_hashes = FileHashStore::load(vote_hash_path)?;
    let mut bill_hashes = FileHashStore::load(bill_hash_path)?;

    // Conditionally run vote processing pipeline.
    if cfg.run_votes {
        // Step 1: Sync vote data from Congress.gov using Python subprocess.
        votes::update_votes(&cfg).await?;
        // Step 2: Parse changed vote files and write to database.
        // `&mut` passes a mutable reference — the function can modify
        // vote_hashes and stats, but doesn't take ownership of them.
        // Think of `&mut` as passing by reference with write permission.
        votes::process_votes(&pool, &cfg, &mut vote_hashes, &mut stats).await?;
    }

    // Same pipeline for bills.
    if cfg.run_bills {
        bills::update_bills(&cfg).await?;
        bills::process_bills(&pool, &cfg, &mut bill_hashes, &mut stats).await?;
    }

    // If any data was written to PostgreSQL, invalidate the Redis API cache
    // so the API serves fresh data.
    if stats.has_writes() {
        match redis_cache::clear_api_cache(&cfg).await {
            Ok(deleted) => info!(
                redis_url = cfg.redis_url,
                keys_deleted = deleted,
                "redis api cache cleared"
            ),
            // `warn!` instead of `error!` — cache clearing failure is
            // non-fatal. The API will still work, just with stale cache.
            Err(err) => warn!(
                redis_url = cfg.redis_url,
                error = %err,
                "unable to clear redis api cache"
            ),
        }
    } else {
        info!(
            reason = "no new postgres writes",
            "redis api cache clear skipped"
        );
    }

    // Log final statistics. `started_at.elapsed()` returns the wall-clock
    // time since `Instant::now()` was called at the start.
    info!(
        bills_processed = stats.bills_processed,
        bills_skipped = stats.bills_skipped,
        bills_failed = stats.bills_failed,
        votes_processed = stats.votes_processed,
        votes_skipped = stats.votes_skipped,
        votes_failed = stats.votes_failed,
        duration_s = started_at.elapsed().as_secs_f64(),
        "scraper run complete"
    );

    Ok(())
}
