// ============================================================================
// votes.rs — Vote data processing pipeline
// ============================================================================
//
// This module handles:
//   1. Syncing vote data files from Congress.gov (via Python subprocess)
//   2. Detecting which vote files have changed (via SHA-256 hashing)
//   3. Parsing changed vote JSON files in parallel (up to 64 concurrent)
//   4. Writing parsed votes to PostgreSQL in parallel (up to 4 concurrent)
//
// The architecture uses a two-phase pipeline:
//   Phase 1 (collect): Parse files on the blocking thread pool (CPU-bound)
//   Phase 2 (write):   Insert into PostgreSQL on the async runtime (I/O-bound)
//
// ============================================================================

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
// `Arc` = Atomic Reference Count. It's Rust's way of sharing data between
// async tasks (or threads). Like Python objects (which are ref-counted by
// default) or `std::shared_ptr` in C++.
//
// Normal Rust ownership means only ONE variable owns a value at a time.
// `Arc` wraps a value so MULTIPLE owners can share it. When the last `Arc`
// clone is dropped, the value is freed.
//
// Arc is needed here because `tokio::spawn` tasks may run on different
// threads, and each needs access to the shared hash map and semaphore.
use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::PgPool;
// `Semaphore` limits how many tasks can run concurrently — like a counting
// semaphore in any language. `acquire()` blocks if the limit is reached,
// and the permit is released when dropped (RAII pattern).
use tokio::sync::Semaphore;
// `JoinSet` is a collection of spawned async tasks that you can await
// together. Think of it as an enhanced `Promise.allSettled()` in JS or
// `asyncio.TaskGroup` in Python — but you can drain results one at a time
// with `join_next()` instead of waiting for all to finish.
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::config::{Config, current_congress};
use crate::db;
use crate::hashes::{FileHashStore, sha256_file};
use crate::models::{
    InsertVoteMemberParams, InsertVoteParams, ParsedVote, VoteJson, VoteMemberJson,
};
use crate::python::run_congress_task;
use crate::stats::RunStats;
use crate::util::{file_exists, option_string, parse_date_value};

/// Maximum number of concurrent file-parsing tasks.
/// Set to 64 to parallelize CPU-bound JSON parsing across threads.
const WORKER_LIMIT: usize = 64;

/// Maximum number of concurrent database write tasks.
/// Set to 4 to match the database connection pool size.
const DB_WRITE_CONCURRENCY: usize = 4;

/// A pending vote file to be processed.
/// This is a simple container struct — like a Python dataclass or JS object.
struct VoteJob {
    path: PathBuf,
}

/// A vote that was parsed and determined to have changed since the last run.
/// Carries the parsed data, the file path, and the computed hash (so we can
/// update the hash store after successful DB write).
struct ChangedVote {
    parsed_vote: ParsedVote,
    path: PathBuf,
    hash: String,
}

/// Aggregated results from the collection (parsing) phase.
struct VoteCollectResult {
    changed_votes: Vec<ChangedVote>,
    skipped: u32,
    missing: u32,
    failed: u32,
}

/// Outcome of parsing a single vote file.
///
/// `enum` in Rust is a "tagged union" or "algebraic data type" — much more
/// powerful than enums in Python/JS. Each variant can hold different data:
///   - `Changed(ChangedVote)` — the file changed and was parsed successfully
///   - `Skipped` — the file hasn't changed since last run
///   - `Missing` — the expected file doesn't exist
///
/// This is like TypeScript's discriminated unions:
///   type VoteParseOutcome =
///     | { kind: "changed", data: ChangedVote }
///     | { kind: "skipped" }
///     | { kind: "missing" }
enum VoteParseOutcome {
    Changed(ChangedVote),
    Skipped,
    Missing,
}

// ============================================================================
// Public API: update_votes and process_votes
// ============================================================================

/// Runs the Python congress sync tool to download latest vote data.
///
/// If the sync fails (e.g., network error), we log a warning but DON'T
/// abort — we'll still process whatever data we already have on disk.
pub async fn update_votes(cfg: &Config) -> Result<()> {
    let congress = current_congress();
    if let Err(err) = run_congress_task(cfg, &["votes", &format!("--congress={congress}")]).await {
        warn!(congress, error = %err, "vote sync skipped");
    }
    Ok(())
}

/// Processes vote data for all congress sessions from 101 to the current.
///
/// For each congress:
///   1. Discover all vote files on disk
///   2. Parse changed files in parallel (Phase 1)
///   3. Write parsed votes to the database in parallel (Phase 2)
///   4. Save updated file hashes to disk
///
/// `&mut FileHashStore` and `&mut RunStats` are mutable references — the
/// function can modify these values (update hashes, increment counters)
/// but doesn't own them. The caller retains ownership.
pub async fn process_votes(
    pool: &PgPool,
    cfg: &Config,
    hashes: &mut FileHashStore,
    stats: &mut RunStats,
) -> Result<()> {
    // `101..=current_congress()` is an inclusive range (101, 102, ..., 119).
    // `..=` means inclusive end; `..` would be exclusive.
    // Like Python's `range(101, current_congress() + 1)`.
    for congress in 101..=current_congress() {
        // Discover all vote files for this congress.
        let jobs = match vote_jobs_for_congress(cfg, congress) {
            Ok(jobs) => jobs,
            Err(err) => {
                warn!(congress, error = %err, "skipping vote congress");
                stats.votes_failed += 1;
                // `continue` skips to the next iteration of the for loop.
                continue;
            }
        };

        info!(
            congress,
            candidates = jobs.len(),
            "processing vote congress"
        );
        let vote_candidates = jobs.len() as u32;

        // Phase 1: Parse all changed vote files in parallel.
        let collected = collect_changed_votes(jobs, hashes, congress).await;
        let changed_candidates = collected.changed_votes.len() as u32;
        stats.votes_skipped += u64::from(collected.skipped);
        stats.votes_failed += u64::from(collected.failed);

        // ====================================================================
        // Phase 2: Write parsed votes to the database
        // ====================================================================
        //
        // We create a semaphore with 4 permits (matching the DB pool size)
        // and spawn an async task for each vote. The semaphore ensures at
        // most 4 DB writes happen concurrently.
        //
        // `Arc::new(...)` wraps the semaphore in an atomic reference count
        // so it can be shared across spawned tasks. Each `write_sem.clone()`
        // creates a new reference to the SAME semaphore (cheap, just bumps
        // a counter).
        // ====================================================================
        let write_sem = Arc::new(Semaphore::new(DB_WRITE_CONCURRENCY));
        let mut write_tasks = JoinSet::new();

        for changed_vote in collected.changed_votes {
            // Clone the pool and semaphore for the spawned task.
            // `pool.clone()` is cheap — PgPool is internally Arc'd.
            let pool = pool.clone();
            let write_sem = write_sem.clone();

            // `write_tasks.spawn(async move { ... })` creates a new async
            // task and adds it to the JoinSet.
            //
            // `async move { ... }` is an async closure that TAKES OWNERSHIP
            // of captured variables (pool, write_sem, changed_vote). The
            // `move` keyword is needed because the task may outlive the
            // current scope — Rust needs to know the data will still be
            // valid when the task runs.
            //
            // This is like:
            //   asyncio.create_task(insert_vote(pool, vote))
            // in Python, or:
            //   promises.push(insertVote(pool, vote))
            // in JS.
            write_tasks.spawn(async move {
                // Acquire a semaphore permit. This `.await` suspends until
                // a permit is available (at most 4 tasks can hold permits).
                // `_permit` keeps the permit alive; it's released when this
                // variable is dropped (when the async block ends).
                let _permit = write_sem.acquire_owned().await?;
                insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
                // `Ok::<_, anyhow::Error>(...)` — explicit type annotation
                // needed because Rust can't always infer the error type
                // inside an async block. This says "the Ok variant carries
                // a ChangedVote, and the Err variant is an anyhow::Error."
                Ok::<_, anyhow::Error>(changed_vote)
            });
        }

        // ====================================================================
        // Collect results from all write tasks
        // ====================================================================
        //
        // `join_next().await` returns the next completed task's result.
        // The double-Result pattern handles two levels of failure:
        //
        //   Ok(Ok(vote))   — task completed, DB write succeeded
        //   Ok(Err(err))   — task completed, but the DB write failed
        //   Err(err)        — task itself panicked (runtime error)
        //
        // This is like checking both "did the promise resolve?" and
        // "did the async function throw?" in JS Promise.allSettled().
        // ====================================================================
        let mut congress_processed = 0u32;
        let mut congress_failed = 0u32;
        while let Some(result) = write_tasks.join_next().await {
            match result {
                Ok(Ok(changed_vote)) => {
                    // Success! Record the file hash so we skip it next time.
                    hashes.mark_processed(&changed_vote.path, changed_vote.hash);
                    stats.votes_processed += 1;
                    congress_processed += 1;
                }
                Ok(Err(err)) => {
                    warn!(error = %err, "unable to insert vote");
                    stats.votes_failed += 1;
                    congress_failed += 1;
                }
                Err(err) => {
                    warn!(error = %err, "vote write task failed");
                    stats.votes_failed += 1;
                    congress_failed += 1;
                }
            }
        }

        info!(
            congress,
            candidates = vote_candidates,
            changed = changed_candidates,
            skipped = collected.skipped,
            missing = collected.missing,
            processed = congress_processed,
            failed = congress_failed,
            "congress votes done"
        );

        // Persist the updated hash store to disk after each congress.
        // This way, if the scraper crashes mid-run, we don't re-process
        // votes from already-completed congresses.
        hashes
            .save()
            .with_context(|| format!("persist vote hashes for congress {congress}"))?;
    }

    Ok(())
}

// ============================================================================
// File discovery
// ============================================================================

/// Discovers all vote data.json files for a given congress session.
///
/// Walks the directory structure:
///   {congress_dir}/congress/data/{congress}/votes/{year}/{vote_id}/data.json
///
/// Returns a Vec of VoteJob structs, each pointing to a vote file.
fn vote_jobs_for_congress(cfg: &Config, congress: i32) -> Result<Vec<VoteJob>> {
    let root = cfg
        .congress_dir
        .join("congress")
        .join("data")
        .join(congress.to_string())
        .join("votes");
    // `fs::read_dir()` returns an iterator over directory entries.
    // Like `os.listdir()` in Python or `fs.readdirSync()` in Node.
    let years = fs::read_dir(root)?;

    let mut jobs = Vec::new();
    for year in years {
        // `year?` unwraps the Result — directory iteration can fail
        // (e.g., permission denied), so each entry is a Result.
        let year = year?;
        let year_dir = year.path();
        let votes = match fs::read_dir(&year_dir) {
            Ok(votes) => votes,
            Err(err) => {
                warn!(year = %year.file_name().to_string_lossy(), error = %err, "skipping vote year");
                continue;
            }
        };

        for vote in votes {
            let vote = vote?;
            jobs.push(VoteJob {
                path: year_dir.join(vote.file_name()).join("data.json"),
            });
        }
    }

    Ok(jobs)
}

// ============================================================================
// Phase 1: Parallel file parsing
// ============================================================================

/// Parses vote files in parallel, returning only those that have changed.
///
/// This function demonstrates several key Tokio patterns:
///
/// 1. **Arc for shared data**: The hash map is wrapped in `Arc` so all
///    spawned tasks can read it concurrently without copying.
///
/// 2. **Semaphore for concurrency limiting**: At most 64 tasks parse files
///    simultaneously, preventing resource exhaustion.
///
/// 3. **spawn_blocking for CPU work**: JSON parsing is CPU-intensive and
///    synchronous. `spawn_blocking` runs it on Tokio's dedicated blocking
///    thread pool so it doesn't block the async executor.
///
/// 4. **JoinSet for task collection**: Tasks are spawned into a JoinSet
///    and results are drained with `join_next()`.
async fn collect_changed_votes(
    jobs: Vec<VoteJob>,
    hashes: &FileHashStore,
    congress: i32,
) -> VoteCollectResult {
    // Wrap the hash snapshot in Arc for sharing across tasks.
    // `.snapshot()` clones the HashMap — each task gets read-only access
    // via the shared Arc, with no locking needed (because it's immutable).
    let known_hashes = Arc::new(hashes.snapshot());
    let parse_sem = Arc::new(Semaphore::new(WORKER_LIMIT));
    let mut tasks = JoinSet::new();

    for job in jobs {
        // Clone the Arc handles — this is cheap (just bumps ref counts).
        // The `move` in the async block below needs to own these clones.
        let known_hashes = known_hashes.clone();
        let parse_sem = parse_sem.clone();

        tasks.spawn(async move {
            // Acquire a parsing permit (wait if 64 tasks are already parsing).
            let _permit = parse_sem.acquire_owned().await?;

            // ================================================================
            // tokio::task::spawn_blocking — bridging sync and async
            // ================================================================
            //
            // `parse_vote_job` is a synchronous function that does CPU-heavy
            // work (file I/O + JSON parsing). If we ran it directly in an
            // async task, it would BLOCK the Tokio worker thread, preventing
            // other async tasks from running.
            //
            // `spawn_blocking` moves the work to a dedicated thread pool
            // designed for blocking operations. It returns a Future that
            // resolves when the blocking work is done.
            //
            // Analogy:
            //   - Python: `await loop.run_in_executor(None, parse_vote_job, ...)`
            //   - Node: `await new Promise(resolve => worker.postMessage(...))`
            //
            // The `move` keyword transfers ownership of `job` and
            // `known_hashes` into the blocking closure.
            // ================================================================
            tokio::task::spawn_blocking(move || parse_vote_job(job, &known_hashes)).await?
        });
    }

    // Drain results from the JoinSet one at a time.
    let mut changed_votes = Vec::new();
    let mut skipped = 0u32;
    let mut missing = 0u32;
    let mut failed = 0u32;
    while let Some(result) = tasks.join_next().await {
        match result {
            // Nested pattern matching: unwrap JoinSet result, then parse outcome.
            Ok(Ok(VoteParseOutcome::Changed(changed_vote))) => changed_votes.push(changed_vote),
            Ok(Ok(VoteParseOutcome::Skipped)) => skipped += 1,
            Ok(Ok(VoteParseOutcome::Missing)) => missing += 1,
            Ok(Err(err)) => {
                warn!(congress, error = %err, "unable to parse vote");
                failed += 1;
            }
            Err(err) => {
                warn!(congress, error = %err, "vote parse task failed");
                failed += 1;
            }
        }
    }

    VoteCollectResult {
        changed_votes,
        skipped,
        missing,
        failed,
    }
}

/// Synchronous function that checks if a vote file has changed and parses it.
///
/// This runs on Tokio's blocking thread pool (via `spawn_blocking`).
/// It's NOT async — no `.await` calls, just regular sequential code.
fn parse_vote_job(
    job: VoteJob,
    known_hashes: &HashMap<String, String>,
) -> Result<VoteParseOutcome> {
    // Skip if the file doesn't exist.
    if !file_exists(&job.path) {
        return Ok(VoteParseOutcome::Missing);
    }

    // Compute SHA-256 hash and compare to the stored hash.
    let hash =
        sha256_file(&job.path).with_context(|| format!("hash vote {}", job.path.display()))?;
    let key = job.path.to_string_lossy();
    if known_hashes.get(key.as_ref()) == Some(&hash) {
        // File hasn't changed — skip it.
        return Ok(VoteParseOutcome::Skipped);
    }

    // File is new or changed — parse it.
    let parsed_vote = parse_vote(&job.path)?;
    Ok(VoteParseOutcome::Changed(ChangedVote {
        parsed_vote,
        path: job.path,
        hash,
    }))
}

// ============================================================================
// Vote JSON parsing
// ============================================================================

/// Parses a vote data.json file into a `ParsedVote`.
///
/// The JSON structure is:
/// ```json
/// {
///   "vote_id": "s47-110.2008",
///   "bill": { "congress": 110, "number": 70, "type": "sconres" },
///   "votes": {
///     "Yea": [{ "id": "S001", "display_name": "...", ... }, "VP"],
///     "Nay": [{ "id": "S002", ... }]
///   }
/// }
/// ```
///
/// Note: The `votes` values contain mixed types (objects AND strings like "VP"),
/// which is why we parse them as `serde_json::Value` and filter manually.
fn parse_vote(path: &Path) -> Result<ParsedVote> {
    let data = fs::read_to_string(path)?;
    // `serde_json::from_str` deserializes JSON into the `VoteJson` struct.
    // Like `json.loads()` in Python or `JSON.parse()` in JS, but it also
    // validates the structure against the struct definition.
    let vote_json: VoteJson = serde_json::from_str(&data)?;

    // Handle two possible locations for bill_type (top-level vs nested).
    let bill_type = if vote_json.bill.bill_type.is_empty() {
        vote_json.bill_type.clone()
    } else {
        vote_json.bill.bill_type.clone()
    };

    let voted_at = parse_date_value(&vote_json.votedate)
        .with_context(|| format!("parse vote date for {}", path.display()))?;

    // Handle zero values as None (0 means "not present" in this data).
    // `.then_some(value)` returns `Some(value)` if the condition is true, `None` otherwise.
    let congress = if vote_json.congress == 0 {
        (vote_json.bill.congress != 0).then_some(vote_json.bill.congress)
    } else {
        Some(vote_json.congress)
    };

    let vote = InsertVoteParams {
        voteid: vote_json.vote_id.clone(),
        bill_type: option_string(bill_type),
        bill_number: (vote_json.bill.number != 0).then_some(vote_json.bill.number),
        congress,
        votenumber: (vote_json.number != 0).then_some(vote_json.number),
        votedate: voted_at,
        question: option_string(vote_json.question),
        result: option_string(vote_json.result),
        votesession: option_string(vote_json.session),
        chamber: option_string(vote_json.chamber),
        source_url: option_string(vote_json.source_url),
        votetype: option_string(vote_json.votetype),
    };

    // Parse member votes from the "votes" HashMap.
    // Each key is a position ("Yea", "Nay", etc.) and each value is an
    // array of member objects (with some null/string values mixed in).
    let mut members = Vec::new();
    for (key, items) in vote_json.votes {
        let parsed_members = parse_vote_members(items)
            .with_context(|| format!("parse vote members for {}/{}", path.display(), key))?;
        let position = normalize_position(&key);

        for item in parsed_members {
            // Skip entries without a bioguide ID (e.g., "VP" entries).
            if item.id.is_empty() {
                continue;
            }
            members.push(InsertVoteMemberParams {
                voteid: vote_json.vote_id.clone(),
                bioguide_id: item.id,
                display_name: option_string(item.display_name),
                party: option_string(item.party),
                state: option_string(item.state),
                position: position.clone(),
            });
        }
    }

    Ok(ParsedVote { vote, members })
}

/// Parses vote member entries, filtering out non-object values.
///
/// The JSON array can contain mixed types:
///   - Objects: `{"id": "S001", "display_name": "...", ...}` — real members
///   - Strings: `"VP"` — the Vice President (not a regular member)
///   - Nulls: `null` — padding entries
///
/// We skip strings and nulls, only keeping valid member objects.
///
/// `Vec<serde_json::Value>` is a vector of untyped JSON values — like
/// `list[Any]` in Python or `any[]` in TypeScript.
fn parse_vote_members(items: Vec<serde_json::Value>) -> Result<Vec<VoteMemberJson>> {
    // `Vec::with_capacity(items.len())` pre-allocates memory for the
    // expected number of elements. Like `[None] * n` in Python, but without
    // filling the slots. This avoids repeated memory reallocations as the
    // vector grows. An optimization you don't need in Python/JS.
    let mut members = Vec::with_capacity(items.len());
    for item in items {
        match item {
            // Skip null and string values (they're not member objects).
            serde_json::Value::Null | serde_json::Value::String(_) => continue,
            // For everything else (objects), try to parse as VoteMemberJson.
            // `serde_json::from_value` converts an untyped Value into a typed struct.
            value => members.push(serde_json::from_value(value)?),
        }
    }

    Ok(members)
}

/// Normalizes vote position strings to a consistent format.
///
/// The source data uses various labels ("Yea", "Aye", "Not Voting", etc.)
/// that we normalize to lowercase, no-space versions for consistency.
fn normalize_position(key: &str) -> String {
    match key {
        "Yea" | "Aye" => "yea",
        "Nay" | "No" => "nay",
        "Not Voting" => "notvoting",
        "Present" => "present",
        "Guilty" => "guilty",
        "Not Guilty" => "notguilty",
        // `_` is the wildcard/default pattern — like `default:` in a switch.
        _ => key,
    }
    .to_string()
}

// ============================================================================
// Database insertion
// ============================================================================

/// Inserts a parsed vote and all its members into the database in a
/// single transaction.
///
/// A transaction ensures atomicity: either ALL writes succeed, or NONE do.
/// If any query fails, the transaction is rolled back automatically when
/// `tx` is dropped (Rust's RAII pattern — no try/finally needed).
///
/// Steps:
///   1. Upsert the vote record
///   2. Delete existing member votes (clean slate)
///   3. Insert all member votes
///   4. Commit the transaction
async fn insert_parsed_vote(pool: &PgPool, parsed_vote: &ParsedVote) -> Result<()> {
    // `pool.begin()` starts a new transaction and returns a Transaction object.
    // All queries executed on `tx` are part of this transaction.
    let mut tx = pool.begin().await?;

    db::insert_vote(&mut tx, &parsed_vote.vote)
        .await
        .with_context(|| format!("InsertVote failed for {}", parsed_vote.vote.voteid))?;

    // Delete existing members first, then re-insert — ensures we always
    // have the latest data even if the member list changed.
    db::delete_vote_members(&mut tx, &parsed_vote.vote.voteid)
        .await
        .with_context(|| format!("DeleteVoteMembers failed for {}", parsed_vote.vote.voteid))?;

    for member in &parsed_vote.members {
        db::insert_vote_member(&mut tx, member)
            .await
            .with_context(|| {
                format!(
                    "InsertVoteMember failed for {}/{}",
                    parsed_vote.vote.voteid, member.bioguide_id
                )
            })?;
    }

    // Commit the transaction. If we don't call this (e.g., because an
    // error caused an early return via `?`), the transaction is rolled
    // back when `tx` is dropped.
    tx.commit().await?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================
#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_vote_skips_legacy_string_markers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.json");
        // Note: the "Yea" array contains "VP" (a string) AND a member object.
        // The parser should skip "VP" and only include the real member.
        let payload = r#"{
          "bill": {"congress": 110, "number": 70, "type": "sconres"},
          "number": 47,
          "congress": 110,
          "question": "Question",
          "result": "Passed",
          "chamber": "s",
          "date": "2008-03-13T12:38:00-04:00",
          "session": "2008",
          "source_url": "https://example.com/vote.xml",
          "type": "On the Motion",
          "vote_id": "s47-110.2008",
          "votes": {
            "Yea": [
              "VP",
              {
                "display_name": "Example Senator (D-IL)",
                "id": "S999",
                "party": "D",
                "state": "IL"
              }
            ],
            "Nay": []
          }
        }"#;

        fs::write(&path, payload).unwrap();

        let parsed = parse_vote(&path).unwrap();
        assert_eq!(parsed.vote.voteid, "s47-110.2008");
        // Only 1 member (the "VP" string was filtered out).
        assert_eq!(parsed.members.len(), 1);
        assert_eq!(parsed.members[0].bioguide_id, "S999");
        assert_eq!(parsed.members[0].position, "yea");
    }
}
