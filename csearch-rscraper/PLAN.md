# csearch-rscraper — Porting Plan

Step-by-step plan for rewriting the csearch Go scraper in Rust. Each step is self-contained and testable before moving on.

---

## Prerequisites

- Rust toolchain (rustup, cargo)
- Local Postgres instance with the csearch schema loaded
- Local Redis instance
- A copy of the congress data directory (or access to the production `CONGRESSDIR` volume)
- The existing Go scraper running as a reference for expected output

---

## Step 0 — Project Scaffold

Initialize the Rust project and establish the dependency baseline.

```bash
cargo init csearch-rscraper
```

**Cargo.toml dependencies:**

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "chrono", "macros"] }
redis = { version = "0.27", features = ["aio", "tokio-comp"] }
quick-xml = { version = "0.37", features = ["serialize"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
bincode = "1"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
chrono = { version = "0.4", features = ["serde"] }
dotenvy = "0.15"
```

**Module layout:**

```
src/
  main.rs          # entrypoint, feature flags, orchestration
  config.rs        # environment variable loading
  hashes.rs        # SHA-256 file hash cache (bincode persistence)
  db.rs            # Postgres connection pool, query functions
  models.rs        # all structs (bill, vote, parsed results, XML shapes)
  bills.rs         # bill discovery, XML/JSON parsing, insertion
  votes.rs         # vote discovery, JSON parsing, insertion
  redis_cache.rs   # cache invalidation (SCAN + DEL)
  python.rs        # subprocess runner for congress/run.py
  stats.rs         # atomic run statistics
```

**Acceptance:** `cargo build` succeeds with all dependencies resolved.

---

## Step 1 — Configuration Loading

Port `runtime.go:loadConfig` and environment variable handling.

**What to do:**

1. Create a `Config` struct mirroring `appConfig`:
   ```rust
   pub struct Config {
       pub congress_dir: PathBuf,
       pub postgres_uri: String,
       pub redis_url: String,
       pub db_user: String,
       pub db_password: String,
       pub db_name: String,
       pub db_port: u16,
       pub run_votes: bool,
       pub run_bills: bool,
       pub log_level: String,
   }
   ```

2. Load from environment using `dotenvy` (reads `.env` if present) and `std::env::var` with defaults.

3. Validate that `congress_dir` exists and `postgres_uri` is non-empty; return `anyhow::Result<Config>`.

4. Build the Postgres DSN: `postgres://{user}:{password}@{uri}:{port}/{name}`

5. Derive `congress_runtime_dir` with the same fallback logic: check `{congress_dir}/congress/run.py`, else `/opt/csearch/congress`.

**Test:** Write a unit test that sets env vars, calls `Config::load()`, and asserts field values. Write a test that omits `CONGRESSDIR` and asserts an error.

---

## Step 2 — Structured Logging

Port the `slog` JSON logger to `tracing`.

**What to do:**

1. In `main.rs`, initialize a `tracing_subscriber` with JSON formatting and an `EnvFilter` driven by `config.log_level`:
   ```rust
   tracing_subscriber::fmt()
       .json()
       .with_env_filter(EnvFilter::new(&config.log_level))
       .init();
   ```

2. Replace all Go `slog.Info(...)` / `slog.Error(...)` patterns with `tracing::info!(...)` / `tracing::error!(...)` using structured fields:
   ```rust
   tracing::info!(
       congress = congress,
       bills_processed = stats.bills_processed.load(Ordering::Relaxed),
       "congress complete"
   );
   ```

**Test:** Run the binary, verify JSON lines appear on stdout with expected keys.

---

## Step 3 — File Hash Cache

Port `hashes.go` — the GOB-based SHA-256 deduplication cache.

**What to do:**

1. Define the store:
   ```rust
   pub struct FileHashStore {
       path: PathBuf,
       hashes: HashMap<String, String>,  // file path -> SHA-256 hex
   }
   ```

2. **Persistence format:** Use `bincode` to serialize/deserialize the `HashMap<String, String>`. This replaces Go's GOB encoding. The file format will be different from the Go version — this is intentional; the Rust scraper maintains its own cache files.

3. Implement:
   - `FileHashStore::load(path) -> Result<Self>` — read file if it exists, else return empty store
   - `FileHashStore::needs_processing(&self, path) -> Result<(String, bool)>` — compute SHA-256 of the file, compare with stored hash, return `(hash, needs_update)`
   - `FileHashStore::mark_processed(&mut self, path, hash)` — insert/update entry
   - `FileHashStore::save(&self) -> Result<()>` — write to disk with `bincode`
   - `sha256_file(path) -> Result<String>` — read file, compute SHA-256, return hex string

4. **Thread safety:** Unlike Go's `sync.RWMutex` approach, the Rust version does not need interior mutability for the port. The hash store will be accessed from a single task that collects changed files, then passed into the write phase. If concurrent access is later needed, wrap in `Arc<RwLock<FileHashStore>>`.

**Test:** Create a temp file, hash it, mark processed, save, reload, verify `needs_processing` returns false. Modify the file, verify it returns true.

---

## Step 4 — Database Connection and Query Layer

Port `runtime.go:openQueries` and the sqlc-generated query functions.

**What to do:**

1. Create a Postgres connection pool with `sqlx::PgPool`:
   ```rust
   let pool = PgPoolOptions::new()
       .max_connections(4)  // matches Go's dbWriteConcurrency
       .connect(&dsn)
       .await?;
   ```

2. Define all query functions as standalone async functions that take `&PgPool` or `&mut PgConnection` (for transactions). Use `sqlx::query!` macros for compile-time SQL checking where possible, or `sqlx::query_as!` for typed returns.

3. Port each sqlc query from `query.sql`. Key queries:

   **Bills:**
   ```rust
   pub async fn insert_bill(pool: &PgPool, bill: &InsertBillParams) -> Result<()>
   pub async fn insert_bill_action(pool: &PgPool, action: &InsertBillActionParams) -> Result<i64>
   pub async fn update_bill_latest_action(pool: &PgPool, params: &UpdateBillLatestActionParams) -> Result<()>
   pub async fn clear_bill_latest_action(pool: &PgPool, params: &ClearBillLatestActionParams) -> Result<()>
   pub async fn delete_bill_actions(pool: &PgPool, billtype: &str, billnumber: i32, congress: i32) -> Result<()>
   pub async fn insert_bill_cosponsor(pool: &PgPool, cosponsor: &InsertBillCosponsorParams) -> Result<()>
   pub async fn delete_bill_cosponsors(pool: &PgPool, billtype: &str, billnumber: i32, congress: i32) -> Result<()>
   pub async fn insert_committee(pool: &PgPool, params: &InsertCommitteeParams) -> Result<()>
   pub async fn insert_bill_committee(pool: &PgPool, params: &InsertBillCommitteeParams) -> Result<()>
   pub async fn delete_bill_committees(pool: &PgPool, billtype: &str, billnumber: i32, congress: i32) -> Result<()>
   pub async fn insert_bill_subject(pool: &PgPool, params: &InsertBillSubjectParams) -> Result<()>
   pub async fn delete_bill_subjects(pool: &PgPool, billtype: &str, billnumber: i32, congress: i32) -> Result<()>
   ```

   **Votes:**
   ```rust
   pub async fn insert_vote(pool: &PgPool, vote: &InsertVoteParams) -> Result<()>
   pub async fn insert_vote_member(pool: &PgPool, member: &InsertVoteMemberParams) -> Result<()>
   pub async fn delete_vote_members(pool: &PgPool, voteid: &str) -> Result<()>
   ```

4. Port `ensureSchemaCompatibility` — run a simple `SELECT 1 FROM bills LIMIT 0` to verify the schema exists.

5. Use `sqlx::query_file!` or `sqlx::query!` with the SQL from `query.sql` directly. All the Go queries use `ON CONFLICT ... DO UPDATE` (upserts) and `ON CONFLICT ... DO NOTHING` — these work identically in sqlx.

**Test:** Connect to a test database, insert a bill, read it back, verify fields. Test upsert by inserting twice with different values.

---

## Step 5 — Model Structs

Port all Go structs from `bills.go`, `votes.go`, and the sqlc parameter types.

**What to do:**

1. **XML bill structs** — define with `serde` + `quick-xml` deserialization. The Go scraper handles two XML formats (pre-114th Congress and 114+). In Rust, use a single struct with `Option<T>` fields for the differences:

   ```rust
   #[derive(Debug, Deserialize)]
   #[serde(rename = "bill")]
   pub struct BillXml {
       // Pre-114 uses "billNumber", 114+ uses "number"
       #[serde(alias = "billNumber", alias = "number")]
       pub number: String,
       #[serde(rename = "billType")]
       pub bill_type: String,
       // 114+ also has "type" field — alias handles both
       #[serde(alias = "type")]
       pub type_field: Option<String>,
       #[serde(rename = "introducedDate")]
       pub introduced_at: String,
       // ... remaining fields
   }
   ```

2. **JSON vote structs** — straightforward `serde_json` deserialization:
   ```rust
   #[derive(Debug, Deserialize)]
   pub struct VoteJson {
       pub bill: Option<VoteBillRef>,
       pub number: i32,
       pub congress: i32,
       pub question: String,
       pub result: String,
       pub chamber: String,
       pub date: String,
       pub session: String,
       pub source_url: Option<String>,
       #[serde(rename = "type")]
       pub vote_type: String,
       pub vote_id: String,
       pub votes: HashMap<String, Vec<serde_json::Value>>,
   }
   ```

3. **Legacy JSON bill structs** — for congresses < ~113 that use `data.json` instead of XML.

4. **Parsed intermediate structs** — `ParsedBill`, `ParsedVote` that hold the normalized data ready for DB insertion.

5. **DB parameter structs** — mirror the sqlc `InsertXxxParams` types. Use `Option<T>` instead of Go's `sql.NullString` / `sql.NullTime`:
   ```rust
   pub struct InsertBillParams {
       pub bill_id: Option<String>,
       pub bill_number: i32,
       pub bill_type: String,
       pub introduced_at: Option<NaiveDate>,
       pub congress: i32,
       pub summary_date: Option<String>,
       pub summary_text: Option<String>,
       pub sponsor_bioguide_id: Option<String>,
       pub sponsor_name: Option<String>,
       pub sponsor_state: Option<String>,
       pub sponsor_party: Option<String>,
       pub origin_chamber: Option<String>,
       pub policy_area: Option<String>,
       pub update_date: Option<NaiveDateTime>,
       pub latest_action_date: Option<NaiveDate>,
       pub bill_status: String,
       pub status_at: NaiveDateTime,
       pub short_title: Option<String>,
       pub official_title: Option<String>,
   }
   ```

**Test:** Deserialize a real `fdsys_billstatus.xml` from the congress data directory into `BillXml`, print it, verify all fields populated. Do the same for a vote `data.json`.

---

## Step 6 — Bill Parsing

Port `bills.go` parsing logic — the largest and most complex file.

**What to do:**

1. **Port helper functions:**
   - `parse_date_value(value: &str) -> Result<NaiveDate>` — handle the multiple date formats the Go code accepts (`2006-01-02`, `2006-01-02T15:04:05Z`, etc.)
   - `parse_i32_value(value: &str) -> Result<i32>`
   - `official_title(titles: &TitlesXml) -> Option<String>` — find first "Official Title as Introduced"
   - `derive_bill_status(latest_action_text: &str) -> String` — keyword matching on action text
   - `normalize_bill_status(raw_status: &str, latest_action_text: &str) -> String`
   - `bill_status_from_sidecar(path: &Path) -> Option<String>` — read data.json alongside XML for status field

2. **Port `parse_bill_xml`:**
   - Read file bytes
   - Deserialize with `quick_xml::de::from_str` into `BillXmlRoot`
   - Extract fields into `ParsedBill`
   - Handle the pre-114 vs 114+ format differences (the `number` vs `billNumber` field, etc.)
   - Parse all nested collections: actions, cosponsors, committees, subjects, titles

3. **Port `parse_bill_json`:**
   - Read file bytes
   - Deserialize with `serde_json::from_str` into `BillJson`
   - Convert to `ParsedBill` with the same normalized output shape

4. **Port `build_parsed_bill`:**
   - Shared builder that both XML and JSON paths converge on
   - Normalize bill type to lowercase
   - Construct `bill_id` as `{billtype}{billnumber}-{congress}`
   - Parse and set all optional fields

5. **Port `bill_jobs_for_table`:**
   - Walk `{congress_dir}/congress/data/{congress}/bills/{billtype}/`
   - For each bill directory, check for `fdsys_billstatus.xml` first, fall back to `data.json`
   - Return a `Vec<BillJob>` with path and format indicator

**Test:** Parse 5-10 real bill XMLs and JSONs from different congresses (93, 110, 114, 118). Compare field-by-field output against the Go scraper by inserting into a test table and diffing.

---

## Step 7 — Vote Parsing

Port `votes.go` parsing logic.

**What to do:**

1. **Port `normalize_position`:**
   ```rust
   fn normalize_position(key: &str) -> &str {
       match key {
           "Yea" | "Aye" => "Yea",
           "Nay" | "No" => "Nay",
           "Not Voting" | "Present" => key,
           _ => "Not Voting",
       }
   }
   ```

2. **Port `parse_vote`:**
   - Read `data.json`
   - Deserialize into `VoteJson`
   - Parse the `votes` map — each key is a position string, each value is an array of member objects (which can be either objects or bare strings)
   - Build `ParsedVote` with `InsertVoteParams` and `Vec<InsertVoteMemberParams>`
   - Construct `vote_id` from the JSON's `vote_id` field

3. **Port `parse_vote_members`:**
   - Handle the mixed array: each element is either `{"display_name": ..., "id": ..., ...}` or a bare string `"VP"` (Vice President tiebreaker)
   - Use `serde_json::Value` to detect the type and parse accordingly
   - Skip bare string entries (no bioguide ID)

4. **Port `vote_jobs_for_congress`:**
   - Walk `{congress_dir}/congress/data/{congress}/votes/{year}/{votenumber}/`
   - Return `Vec<VoteJob>` with paths to `data.json`
   - Filter to congresses 101 through current

**Test:** Parse 5-10 real vote JSONs. Verify member counts, positions, and bill references match Go output.

---

## Step 8 — Bill Insertion with Transactions

Port `bills.go:insertParsedBill` — the transactional write logic.

**What to do:**

1. Begin a transaction: `let mut tx = pool.begin().await?;`

2. Execute the full insertion sequence within the transaction:
   ```
   1. insert_bill(&mut *tx, &parsed.bill)
   2. clear_bill_latest_action(&mut *tx, ...)
   3. delete_bill_actions(&mut *tx, ...)
   4. delete_bill_cosponsors(&mut *tx, ...)
   5. delete_bill_committees(&mut *tx, ...)
   6. delete_bill_subjects(&mut *tx, ...)
   7. For each action: insert_bill_action(&mut *tx, ...) → get returned ID
   8. Find the action matching latest_action_date → update_bill_latest_action with that ID
   9. For each cosponsor: insert_bill_cosponsor(&mut *tx, ...)
   10. For each committee: insert_committee(&mut *tx, ...) then insert_bill_committee(&mut *tx, ...)
   11. For each subject: insert_bill_subject(&mut *tx, ...)
   ```

3. Commit: `tx.commit().await?;`

4. On any error, the transaction auto-rolls back when `tx` is dropped (Rust's RAII).

**Test:** Insert a parsed bill, query the database to verify all related rows (actions, cosponsors, committees, subjects) exist. Insert again with modified data, verify upsert behavior.

---

## Step 9 — Vote Insertion with Transactions

Port `votes.go:insertParsedVote`.

**What to do:**

1. Begin transaction
2. Execute:
   ```
   1. delete_vote_members(&mut *tx, &vote_id)
   2. insert_vote(&mut *tx, &parsed.vote)
   3. For each member: insert_vote_member(&mut *tx, &member)
   ```
3. Commit

**Test:** Insert a parsed vote, verify vote row and all member rows exist. Re-insert with changed result, verify upsert.

---

## Step 10 — Concurrent Processing Pipeline

Port the concurrency model from goroutines to tokio tasks.

**What to do:**

1. **File discovery + hash filtering (replaces collectChangedVotes / bill loop):**
   - Collect all file jobs for a congress
   - Use `tokio::task::spawn_blocking` for SHA-256 computation (CPU-bound)
   - Use a `tokio::sync::Semaphore` with 64 permits for parse concurrency
   - Collect results into a `Vec<ParsedBill>` or `Vec<ParsedVote>`

   ```rust
   let sem = Arc::new(Semaphore::new(64));
   let mut tasks = JoinSet::new();

   for job in jobs {
       let sem = sem.clone();
       let hashes = hashes.clone(); // Arc<RwLock<FileHashStore>>
       tasks.spawn(async move {
           let _permit = sem.acquire().await?;
           // Hash check + parse (spawn_blocking for CPU work)
           let result = tokio::task::spawn_blocking(move || {
               let (hash, needs) = hashes.read().unwrap().needs_processing(&job.path)?;
               if !needs { return Ok(None); }
               let parsed = parse_bill_xml(&job.path)?;
               Ok(Some((parsed, hash, job.path)))
           }).await??;
           Ok(result)
       });
   }

   let mut changed = Vec::new();
   while let Some(result) = tasks.join_next().await {
       if let Ok(Ok(Some((parsed, hash, path)))) = result {
           changed.push((parsed, hash, path));
       }
   }
   ```

2. **DB write concurrency (replaces the 4-goroutine semaphore):**
   - Use a `Semaphore` with 4 permits matching the pool's `max_connections`
   - Spawn a task per insertion, each acquiring a permit before calling insert

   ```rust
   let write_sem = Arc::new(Semaphore::new(4));
   let mut write_tasks = JoinSet::new();

   for (parsed, hash, path) in changed {
       let pool = pool.clone();
       let write_sem = write_sem.clone();
       let hashes = hashes.clone();
       write_tasks.spawn(async move {
           let _permit = write_sem.acquire().await?;
           insert_parsed_bill(&pool, &parsed).await?;
           hashes.write().unwrap().mark_processed(&path, &hash);
           Ok::<_, anyhow::Error>(())
       });
   }

   while let Some(result) = write_tasks.join_next().await {
       match result {
           Ok(Ok(())) => stats.bills_processed.fetch_add(1, Ordering::Relaxed),
           _ => stats.bills_failed.fetch_add(1, Ordering::Relaxed),
       };
   }
   ```

3. **Per-congress loop with hash persistence:**
   - After completing all writes for a congress, call `hashes.save()`
   - This matches the Go pattern of persisting hashes between congress batches

**Test:** Process a full congress worth of bills (e.g., 118th). Verify:
- Correct number of bills inserted
- Hash cache file exists and is non-empty
- Re-running skips all bills (stats show all skipped)
- Modifying one file causes only that file to be reprocessed

---

## Step 11 — Python Subprocess Runner

Port `runtime.go:runCongressTask` for invoking the Python data fetcher.

**What to do:**

1. Use `tokio::process::Command` to run the Python subprocess:
   ```rust
   pub async fn run_congress_task(cfg: &Config, args: &[&str]) -> Result<()> {
       let runtime_dir = congress_runtime_dir(cfg);
       let python_path = python_path_for_congress_dir(&cfg.congress_dir);

       let mut cmd = tokio::process::Command::new("python3");
       cmd.arg(runtime_dir.join("run.py"));
       cmd.args(args);
       cmd.current_dir(&runtime_dir);
       cmd.env("PYTHONPATH", &python_path);
       cmd.stdout(Stdio::piped());
       cmd.stderr(Stdio::piped());

       let mut child = cmd.spawn()?;

       // Stream stdout and stderr concurrently
       let stdout = child.stdout.take().unwrap();
       let stderr = child.stderr.take().unwrap();

       let stdout_task = tokio::spawn(stream_output("stdout", stdout));
       let stderr_task = tokio::spawn(stream_output("stderr", stderr));

       let status = child.wait().await?;
       stdout_task.await?;
       stderr_task.await?;

       if !status.success() {
           anyhow::bail!("python exited with {}", status);
       }
       Ok(())
   }
   ```

2. Implement `stream_output` using `tokio::io::BufReader` and `AsyncBufReadExt::lines()`:
   ```rust
   async fn stream_output(source: &str, reader: impl AsyncRead + Unpin) {
       let mut lines = BufReader::new(reader).lines();
       while let Ok(Some(line)) = lines.next_line().await {
           tracing::info!(stream = source, output = %line, "python");
       }
   }
   ```

3. Port the two invocation sites:
   - `update_bills`: `run_congress_task(cfg, &["govinfo", "--bulkdata=BILLSTATUS", &format!("--congress={}", current_congress())]).await?`
   - `update_votes`: `run_congress_task(cfg, &["votes", &format!("--congress={}", congress)]).await?`

**Test:** Run with a real congress directory. Verify Python output streams to tracing logs. Test failure case (bad args) returns error.

---

## Step 12 — Redis Cache Invalidation

Port `runtime.go:clearAPICache`.

**What to do:**

1. Connect to Redis:
   ```rust
   let client = redis::Client::open(cfg.redis_url.as_str())?;
   let mut conn = client.get_multiplexed_async_connection().await?;
   ```

2. SCAN + DEL loop:
   ```rust
   pub async fn clear_api_cache(cfg: &Config) -> Result<usize> {
       let client = redis::Client::open(cfg.redis_url.as_str())?;
       let mut conn = client.get_multiplexed_async_connection().await?;

       redis::cmd("PING").query_async::<String>(&mut conn).await?;

       let mut cursor: u64 = 0;
       let mut total_deleted: usize = 0;

       loop {
           let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
               .arg(cursor)
               .arg("MATCH")
               .arg("csearch:*")
               .arg("COUNT")
               .arg(100)
               .query_async(&mut conn)
               .await?;

           if !keys.is_empty() {
               let deleted: usize = redis::cmd("DEL")
                   .arg(&keys)
                   .query_async(&mut conn)
                   .await?;
               total_deleted += deleted;
           }

           cursor = next_cursor;
           if cursor == 0 { break; }
       }

       Ok(total_deleted)
   }
   ```

3. Call conditionally from `main` — only if `bills_processed > 0 || votes_processed > 0`.

**Test:** Seed Redis with a few `csearch:*` keys. Run `clear_api_cache`. Verify keys are gone and count matches.

---

## Step 13 — Run Statistics

Port the atomic `runStats` struct.

**What to do:**

```rust
use std::sync::atomic::{AtomicU64, Ordering};

pub struct RunStats {
    pub bills_processed: AtomicU64,
    pub bills_skipped: AtomicU64,
    pub bills_failed: AtomicU64,
    pub votes_processed: AtomicU64,
    pub votes_skipped: AtomicU64,
    pub votes_failed: AtomicU64,
}

impl RunStats {
    pub fn new() -> Self { /* all zeros */ }

    pub fn any_processed(&self) -> bool {
        self.bills_processed.load(Ordering::Relaxed) > 0
            || self.votes_processed.load(Ordering::Relaxed) > 0
    }

    pub fn log_summary(&self, duration: std::time::Duration) {
        tracing::info!(
            bills_processed = self.bills_processed.load(Ordering::Relaxed),
            bills_skipped = self.bills_skipped.load(Ordering::Relaxed),
            bills_failed = self.bills_failed.load(Ordering::Relaxed),
            votes_processed = self.votes_processed.load(Ordering::Relaxed),
            votes_skipped = self.votes_skipped.load(Ordering::Relaxed),
            votes_failed = self.votes_failed.load(Ordering::Relaxed),
            duration_secs = duration.as_secs(),
            "scraper run complete"
        );
    }
}
```

**Test:** Increment from multiple tokio tasks, verify final counts are correct.

---

## Step 14 — Main Orchestrator

Wire everything together in `main.rs`, matching the exact Go `main()` flow.

**What to do:**

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // 1. Load config (exits on failure)
    let config = Config::load()?;

    // 2. Initialize tracing with JSON output
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(&config.log_level)
        .init();

    let start = Instant::now();
    let stats = Arc::new(RunStats::new());

    tracing::info!(
        run_votes = config.run_votes,
        run_bills = config.run_bills,
        "scraper run starting"
    );

    // 3. Connect to Postgres
    let pool = db::connect(&config).await?;

    // 4. Load hash caches
    let vote_hashes = Arc::new(RwLock::new(
        FileHashStore::load(config.congress_dir.join("data/voteHashes.bin"))?
    ));
    let bill_hashes = Arc::new(RwLock::new(
        FileHashStore::load(config.congress_dir.join("data/fileHashes.bin"))?
    ));

    // 5. Votes
    if config.run_votes {
        update_votes(&config).await?;          // Python subprocess
        process_votes(&pool, &config, &vote_hashes, &stats).await?;
    }

    // 6. Bills
    if config.run_bills {
        update_bills(&config).await?;          // Python subprocess
        process_bills(&pool, &config, &bill_hashes, &stats).await?;
    }

    // 7. Cache invalidation
    if stats.any_processed() {
        match clear_api_cache(&config).await {
            Ok(n) => tracing::info!(keys_deleted = n, "api cache cleared"),
            Err(e) => tracing::warn!(error = %e, "failed to clear api cache"),
        }
    } else {
        tracing::info!("cache clear skipped, no changes");
    }

    // 8. Summary
    stats.log_summary(start.elapsed());
    Ok(())
}
```

**Test:** Run end-to-end against the real congress data directory and database. Compare final stats with a Go scraper run on the same data. Verify:
- Same number of bills/votes processed on a clean database
- Same number skipped on a second run
- Redis keys cleared
- Hash cache files written

---

## Step 15 — Validation and Parity Testing

Verify the Rust scraper produces identical database state to the Go scraper.

**What to do:**

1. **Schema parity:** Run both scrapers against a clean database (one at a time). Dump each table with `pg_dump --data-only --column-inserts` and diff the output. Focus on:
   - Bill counts per congress
   - Action counts per bill
   - Cosponsor counts per bill
   - Vote member counts per vote
   - Null vs non-null field alignment

2. **Edge cases to verify:**
   - Congress 93 (oldest, JSON format)
   - Congress 110 (transition era)
   - Congress 114 (XML format change boundary)
   - Congress 118/119 (latest, most data)
   - Bills with no cosponsors
   - Bills with no summary
   - Votes with VP tiebreaker entries
   - Votes with missing bill references

3. **Performance comparison:** Time both scrapers on a full run (clean DB). The Rust version should be faster on the parsing phase; DB write speed will be similar since Postgres is the bottleneck.

4. **Hash cache isolation:** The Rust scraper uses `.bin` (bincode) files, not `.gob` files. Both scrapers can coexist in the same `CONGRESSDIR` without conflicts.

---

## Step 16 — Dockerfile and Deployment

Package the Rust scraper for Kubernetes deployment.

**What to do:**

1. **Multi-stage Dockerfile:**
   ```dockerfile
   # Build stage
   FROM rust:1.83-slim AS builder
   RUN apt-get update && apt-get install -y pkg-config libssl-dev
   WORKDIR /app
   COPY Cargo.toml Cargo.lock ./
   COPY src/ src/
   RUN cargo build --release

   # Runtime stage
   FROM python:3.11-slim
   # Install the congress Python tool (same as Go scraper)
   COPY --from=builder /app/target/release/csearch-rscraper /usr/local/bin/
   COPY congress/ /opt/csearch/congress/
   CMD ["csearch-rscraper"]
   ```

2. **K8s CronJob manifest:** Same structure as the existing Go scraper CronJob, just changing the image reference.

3. **Rollout plan:**
   - Deploy Rust scraper as a separate CronJob on a different schedule (e.g., offset by 12 hours)
   - Compare database state after each run
   - Once confident, disable Go scraper CronJob
   - Remove Go scraper deployment

---

## Summary of Go → Rust Mappings

| Go | Rust |
|----|------|
| `encoding/gob` | `bincode` |
| `encoding/xml` | `quick-xml` with serde |
| `encoding/json` | `serde_json` |
| `database/sql` + `sqlc` | `sqlx` with `query!` macros |
| `sql.NullString` / `sql.NullTime` | `Option<String>` / `Option<NaiveDate>` |
| `sync.WaitGroup` + goroutines | `tokio::task::JoinSet` |
| `chan struct{}` semaphore | `tokio::sync::Semaphore` |
| `sync.RWMutex` | `std::sync::RwLock` (or `tokio::sync::RwLock`) |
| `sync/atomic` | `std::sync::atomic` |
| `log/slog` | `tracing` + `tracing-subscriber` |
| `github.com/spf13/viper` | `dotenvy` + `std::env` |
| `github.com/lib/pq` | `sqlx` postgres feature |
| `github.com/redis/go-redis/v9` | `redis` crate with `aio` feature |
| `os/exec.Command` | `tokio::process::Command` |
| `bufio.Scanner` | `tokio::io::BufReader` + `AsyncBufReadExt` |
| `crypto/sha256` | `sha2` crate |
| `fmt.Errorf` / error wrapping | `anyhow::Context` / `thiserror` |
| `os.Exit(1)` on fatal | `?` propagation to `main` → process exit |
