# Rust Patterns Study Guide

A comprehensive reference of every Rust pattern, idiom, and library used across
this repository. Each section includes real examples from the codebase with file
locations so you can read the full context.

---

## Table of Contents

1. [Project Overview](#project-overview)
2. [Tokio Async Runtime](#tokio-async-runtime)
3. [spawn_blocking for CPU Work](#spawn_blocking-for-cpu-work)
4. [JSON Parsing (serde_json)](#json-parsing-serde_json)
5. [XML Parsing (quick-xml)](#xml-parsing-quick-xml)
6. [SQLx Database Operations](#sqlx-database-operations)
7. [File Hashing (SHA-256)](#file-hashing-sha-256)
8. [Logging (tracing)](#logging-tracing)
9. [Error Handling (anyhow)](#error-handling-anyhow)
10. [CLI Argument Parsing (clap)](#cli-argument-parsing-clap)
11. [Serde Serialization Framework](#serde-serialization-framework)
12. [Binary Serialization (bincode)](#binary-serialization-bincode)
13. [Redis Async Operations](#redis-async-operations)
14. [Async Subprocess Management](#async-subprocess-management)
15. [Rayon Data Parallelism](#rayon-data-parallelism)
16. [Markdown & HTML Processing](#markdown--html-processing)
17. [Image Processing](#image-processing)
18. [Concurrency Patterns](#concurrency-patterns)
19. [File I/O & Path Handling](#file-io--path-handling)
20. [Environment Variables & Configuration](#environment-variables--configuration)
21. [Module Organization](#module-organization)
22. [Type System Patterns](#type-system-patterns)
23. [String Handling](#string-handling)
24. [Iterator & Functional Patterns](#iterator--functional-patterns)
25. [Feature Flags & Conditional Compilation](#feature-flags--conditional-compilation)
26. [Testing Patterns](#testing-patterns)
27. [Crate Reference Table](#crate-reference-table)

---

## Project Overview

| Project | Path | Description |
|---------|------|-------------|
| csearch-rscraper | `csearch-rscraper/` | Async data pipeline: Congress.gov scraper with Tokio, SQLx, Redis |
| CLI Hash Cache | `PHASE1/1.1-CLI-HASH-CACHE/` | File hashing, change detection, bincode/JSON serialization |
| Markdown Sanitizer | `PHASE1/1.2-MARKDOWN-SANITIZER/` | Markdown to HTML rendering with XSS sanitization |
| Image Thumbnail Generator | `PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/` | Batch image processing with sequential, rayon, and tokio strategies |

---

## Tokio Async Runtime

### Entry Point with `#[tokio::main]`

The `#[tokio::main]` macro creates a multi-threaded Tokio runtime and wraps your
async main function. Without it, you can't use `.await` in main.

```rust
// csearch-rscraper/src/main.rs:62-86

#[tokio::main]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
    init_tracing();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!(error = %err, "scraper run failed");
            ExitCode::FAILURE
        }
    }
}
```

The macro expands to roughly:
```rust
fn main() -> ExitCode {
    tokio::runtime::Runtime::new().unwrap().block_on(async { ... })
}
```

### Async Functions and `.await`

Every async operation returns a `Future` that must be `.await`ed. The `.await`
keyword suspends the current task (not the thread) until the operation completes.

```rust
// csearch-rscraper/src/main.rs:125-152

async fn run() -> anyhow::Result<()> {
    let cfg = Config::load()?;
    let pool = db::open_pool(&cfg).await?;     // suspends until connected
    
    if cfg.run_votes {
        votes::update_votes(&cfg).await?;       // suspends until sync completes
        votes::process_votes(&pool, &cfg, &mut vote_hashes, &mut stats).await?;
    }

    Ok(())
}
```

### JoinSet for Task Collection

`JoinSet` is a collection of spawned async tasks. You drain results one at a time
with `join_next()` — like `asyncio.TaskGroup` in Python.

```rust
// csearch-rscraper/src/votes.rs:180-252

let write_sem = Arc::new(Semaphore::new(DB_WRITE_CONCURRENCY));
let mut write_tasks = JoinSet::new();

for changed_vote in collected.changed_votes {
    let pool = pool.clone();
    let write_sem = write_sem.clone();

    // `async move` takes OWNERSHIP of captured variables
    write_tasks.spawn(async move {
        let _permit = write_sem.acquire_owned().await?;
        insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
        Ok::<_, anyhow::Error>(changed_vote)
    });
}

// Drain results — double-Result handles task panics vs business errors
while let Some(result) = write_tasks.join_next().await {
    match result {
        Ok(Ok(changed_vote)) => { /* success */ }
        Ok(Err(err)) => { /* DB write failed */ }
        Err(err) => { /* task panicked */ }
    }
}
```

### Semaphore for Concurrency Limiting

`Semaphore` limits how many tasks run concurrently. The permit is released
automatically when dropped (RAII pattern).

```rust
// csearch-rscraper/src/votes.rs:179-207

let write_sem = Arc::new(Semaphore::new(DB_WRITE_CONCURRENCY));

write_tasks.spawn(async move {
    // Blocks if 4 tasks already hold permits
    let _permit = write_sem.acquire_owned().await?;
    insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
    // _permit dropped here → releases the semaphore slot
    Ok::<_, anyhow::Error>(changed_vote)
});
```

---

## spawn_blocking for CPU Work

Image decoding, JSON parsing, and file hashing are CPU-bound. Running them
directly in a tokio task would starve the async executor. `spawn_blocking` moves
the work to a dedicated thread pool.

```rust
// csearch-rscraper/src/votes.rs:360-385

tasks.spawn(async move {
    let _permit = parse_sem.acquire_owned().await?;

    // parse_vote_job is synchronous (CPU-heavy JSON parsing + file I/O).
    // spawn_blocking moves it to a separate OS thread pool.
    tokio::task::spawn_blocking(move || {
        parse_vote_job(job, &known_hashes)
    }).await?
});
```

**When to use spawn_blocking:**
- File I/O (hashing, reading large files)
- JSON/XML parsing of large documents
- Image encoding/decoding
- Any CPU-intensive work that doesn't yield to the executor

The Tokio batch strategy in the image thumbnail generator uses the same pattern:

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs (tokio batch section)

// Each image is decoded+resized on the blocking pool,
// while the async runtime manages concurrency via Semaphore + JoinSet
```

---

## JSON Parsing (serde_json)

### Typed Deserialization

Define a struct that mirrors the JSON shape. `serde_json::from_str` validates
structure at parse time — like Pydantic models or Zod schemas.

```rust
// csearch-rscraper/src/votes.rs:469-474

let data = fs::read_to_string(path)?;
let vote_json: VoteJson = serde_json::from_str(&data)?;
```

The struct definition with serde attributes:

```rust
// csearch-rscraper/src/models.rs:610-638

#[derive(Debug, Deserialize, Default, Clone)]
pub struct VoteJson {
    #[serde(default)]
    pub bill: VoteBillJson,
    #[serde(default)]
    pub number: i32,
    #[serde(rename = "bill_type", default)]
    pub bill_type: String,
    #[serde(rename = "date", default)]
    pub votedate: String,
    #[serde(rename = "type", default)]       // "type" is a reserved keyword
    pub votetype: String,
    #[serde(rename = "vote_id", default)]
    pub vote_id: String,
    // Dynamic keys → HashMap. Values are mixed types → Vec<Value>
    #[serde(default)]
    pub votes: std::collections::HashMap<String, Vec<Value>>,
}
```

### Untyped JSON with `serde_json::Value`

When JSON contains mixed types (objects, strings, nulls in the same array),
use `serde_json::Value` and filter manually:

```rust
// csearch-rscraper/src/votes.rs:548-565

fn parse_vote_members(items: Vec<serde_json::Value>) -> Result<Vec<VoteMemberJson>> {
    let mut members = Vec::with_capacity(items.len());
    for item in items {
        match item {
            serde_json::Value::Null | serde_json::Value::String(_) => continue,
            value => members.push(serde_json::from_value(value)?),
        }
    }
    Ok(members)
}
```

### Custom Deserializer for Null Handling

When a JSON field can be `null` or missing and you want a default value:

```rust
// csearch-rscraper/src/models.rs:59-66

fn null_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let value = Option::<T>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

// Usage:
#[derive(Deserialize)]
pub struct BillJson {
    #[serde(default, deserialize_with = "null_or_default")]
    pub number: String,
}
```

### JSON Serialization

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:535

serde_json::to_vec_pretty(cache).context("failed to serialize cache to JSON")?
```

---

## XML Parsing (quick-xml)

`quick-xml` uses the same serde framework as JSON. Define structs that mirror
the XML shape, then call `from_str`.

### XML Struct Definitions

```rust
// csearch-rscraper/src/models.rs:264-296

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ItemXml {
    #[serde(rename = "actionDate", default)]    // maps to <actionDate> tag
    pub acted_at: String,
    #[serde(rename = "text", default)]
    pub text: String,
    #[serde(rename = "type", default)]
    pub item_type: String,
    #[serde(rename = "sourceSystem", default)]  // nested element
    pub source_system: SourceSystemXml,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ActionsXml {
    #[serde(rename = "item", default)]          // repeated <item> → Vec
    pub actions: Vec<ItemXml>,
}
```

### XML Root Element Mapping

```rust
// csearch-rscraper/src/models.rs:482-495

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename = "billStatus")]                 // root XML element name
pub struct BillXmlRootNew {
    #[serde(rename = "bill", default)]
    pub bill: BillXmlNew,
}
```

### Parsing XML

```rust
// csearch-rscraper/src/bills.rs (parsing function)

use quick_xml::de::from_str;

let file_contents = fs::read_to_string(&path)?;
let parsed: BillXmlRootNew = from_str(&file_contents)?;
```

### Key Serde Attributes for XML

| Attribute | Purpose | Example |
|-----------|---------|---------|
| `#[serde(rename = "xmlTag")]` | Map Rust field to XML tag name | `rename = "billType"` |
| `#[serde(default)]` | Use `Default::default()` if tag missing | Empty string, 0, empty vec |
| `#[serde(rename = "item")]` on a `Vec<T>` | Repeated child elements | `<item>` children → vector |
| `#[serde(deserialize_with = "fn")]` | Custom parse logic | `null_or_default` |

---

## SQLx Database Operations

### Connection Pooling

```rust
// csearch-rscraper/src/db.rs:93-106

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

`PgPool` is cloneable and thread-safe — you can share it across async tasks with
`pool.clone()`. Internally it's reference-counted, so cloning is cheap.

### Parameterized Queries (SQL Injection Prevention)

```rust
// csearch-rscraper/src/db.rs:138-190

pub async fn insert_vote(
    tx: &mut Transaction<'_, Postgres>,
    vote: &InsertVoteParams,
) -> Result<()> {
    sqlx::query(
        r#"
INSERT INTO votes (voteid, bill_type, bill_number, congress, votenumber)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (voteid) DO UPDATE SET
    bill_type = excluded.bill_type,
    bill_number = excluded.bill_number
        "#,
    )
    .bind(&vote.voteid)          // $1 — & borrows the String (avoids cloning)
    .bind(&vote.bill_type)       // $2 — Option<String> becomes NULL if None
    .bind(vote.bill_number)      // $3 — small types (i32) are copied, not borrowed
    .bind(vote.congress)         // $4
    .bind(vote.votenumber)       // $5
    .execute(&mut **tx)          // double deref: &mut Transaction → inner connection
    .await?;
    Ok(())
}
```

**Key SQLx patterns:**
- `$N` placeholders for PostgreSQL
- `.bind()` chains for each parameter
- `&mut **tx` — double dereference to unwrap Transaction's inner connection
- `ON CONFLICT ... DO UPDATE SET` for upsert operations
- `ON CONFLICT ... DO NOTHING` to skip duplicates silently

### Transactions with RAII

```rust
// csearch-rscraper/src/votes.rs:601-632

async fn insert_parsed_vote(pool: &PgPool, parsed_vote: &ParsedVote) -> Result<()> {
    let mut tx = pool.begin().await?;               // start transaction

    db::insert_vote(&mut tx, &parsed_vote.vote).await?;
    db::delete_vote_members(&mut tx, &parsed_vote.vote.voteid).await?;

    for member in &parsed_vote.members {
        db::insert_vote_member(&mut tx, member).await?;
    }

    tx.commit().await?;     // explicit commit
    // If we don't call .commit() (e.g., early return via ?),
    // the transaction auto-rolls back when `tx` is dropped.
    Ok(())
}
```

### RETURNING for Auto-Generated IDs

```rust
// csearch-rscraper/src/db.rs:336-358

pub async fn insert_bill_action(
    tx: &mut Transaction<'_, Postgres>,
    action: &InsertBillActionParams,
) -> Result<i64> {
    let id = sqlx::query_scalar(
        r#"
INSERT INTO bill_actions (billtype, billnumber, congress, acted_at, action_text)
VALUES ($1, $2, $3, $4, $5)
RETURNING id
        "#,
    )
    .bind(&action.billtype)
    .bind(action.billnumber)
    .bind(action.congress)
    .bind(action.acted_at)
    .bind(&action.action_text)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}
```

### Raw SQL Execution

```rust
// csearch-rscraper/src/db.rs:109-115

async fn ensure_schema_compatibility(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(ENSURE_SCHEMA_COMPATIBILITY_SQL)
        .execute(pool)
        .await
        .context("ensure schema compatibility")?;
    Ok(())
}
```

---

## File Hashing (SHA-256)

### Chunked File Reading for Memory Efficiency

Reading in fixed 8KB chunks avoids loading the entire file into memory.

```rust
// csearch-rscraper/src/hashes.rs:150-175

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    // Fixed-size stack-allocated buffer — NOT heap-allocated like a Vec
    let mut buffer = [0_u8; 8192];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 { break; }
        // Slice of actual bytes read (not the full 8KB buffer)
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))  // lowercase hex
}
```

Same pattern with BufReader in the CLI Hash Cache project:

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:332-353

fn hash_file(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open file '{}'", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_SIZE];

    loop {
        let bytes_read = reader.read(&mut buffer)
            .with_context(|| format!("failed while reading '{}'", path.display()))?;
        if bytes_read == 0 { break; }
        hasher.update(&buffer[..bytes_read]);
    }

    let digest = hasher.finalize();
    Ok(format!("{digest:x}"))
}
```

### Hash-Based Change Detection

```rust
// csearch-rscraper/src/hashes.rs:38-139

#[derive(Debug, Default)]
pub struct FileHashStore {
    path: PathBuf,
    hashes: HashMap<String, String>,  // file_path → sha256_hash
}

impl FileHashStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> { /* ... */ }

    pub fn needs_processing(&self, path: &Path) -> Result<(String, bool)> {
        let hash = sha256_file(path)?;
        let key = path.to_string_lossy();
        let changed = self.hashes.get(key.as_ref()) != Some(&hash);
        Ok((hash, changed))
    }

    pub fn mark_processed(&mut self, path: &Path, hash: String) {
        self.hashes.insert(path.to_string_lossy().into_owned(), hash);
    }

    pub fn save(&self) -> Result<()> {
        let bytes = bincode::serialize(&PersistedHashes(self.hashes.clone()))?;
        fs::write(&self.path, bytes)?;
        Ok(())
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.hashes.clone()  // clone for sharing across async tasks via Arc
    }
}
```

---

## Logging (tracing)

### Setup with JSON Output

```rust
// csearch-rscraper/src/main.rs:93-112

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| {
            let level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
            EnvFilter::try_new(level)
        })
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .json()                         // JSON-formatted output for log aggregation
        .with_env_filter(filter)        // controlled by RUST_LOG or LOG_LEVEL
        .with_current_span(false)
        .with_span_list(false)
        .init();
}
```

### Structured Log Events

Key-value fields become structured JSON fields:

```rust
// csearch-rscraper/src/main.rs:144-148

info!(
    run_votes = cfg.run_votes,
    run_bills = cfg.run_bills,
    "scraper run starting"
);

// Output:
// {"level":"INFO","run_votes":true,"run_bills":true,"message":"scraper run starting"}
```

### Log Levels

```rust
info!(congress, candidates = jobs.len(), "processing vote congress");
warn!(congress, error = %err, "vote sync skipped");    // %err = Display format
error!(error = %err, "scraper run failed");
```

---

## Error Handling (anyhow)

### The `?` Operator

Propagates errors up the call stack without try/catch:

```rust
let cfg = Config::load()?;              // returns Err early if this fails
let pool = db::open_pool(&cfg).await?;  // same
```

### `bail!` for Early Returns

```rust
// csearch-rscraper/src/config.rs:99-101

if !congress_dir.exists() {
    bail!("CONGRESSDIR does not exist: {}", congress_dir.display());
}
```

### `.context()` and `.with_context()` for Error Messages

```rust
// csearch-rscraper/src/db.rs:94-101

let pool = PgPoolOptions::new()
    .max_connections(DB_WRITE_CONCURRENCY)
    .connect(&cfg.postgres_dsn())
    .await
    .context("connect to postgres")?;     // adds message to the error chain
```

`.with_context()` is lazy — the closure only runs if there's an error:

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:218-231

let metadata = fs::metadata(directory).with_context(|| {
    format!("failed to read metadata for directory '{}'", directory.display())
})?;
```

### `.map_err()` for Error Transformation

```rust
// csearch-rscraper/src/config.rs:93-95

let congress_dir = env::var("CONGRESSDIR")
    .map(PathBuf::from)
    .map_err(|_| anyhow::anyhow!("missing CONGRESSDIR"))?;
```

### Result Type Aliases

```rust
use anyhow::Result;  // = Result<T, anyhow::Error>

pub fn load() -> Result<Self> { /* ... */ }
async fn run() -> anyhow::Result<()> { /* ... */ }
```

---

## CLI Argument Parsing (clap)

### Derive Macro for CLI

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:30-65

#[derive(Debug, Parser)]
#[command(
    name = "cli-hash-cache",
    version,
    about = "Hash files in a directory, persist the results, and report changes."
)]
pub struct Args {
    /// The directory we want to scan recursively.     ← becomes help text
    pub directory: PathBuf,                            // positional argument

    /// Where to store the cache on disk.
    #[arg(short, long)]                                // --cache-file / -c
    pub cache_file: Option<PathBuf>,

    /// Save the cache in JSON instead of bincode.
    #[arg(long)]                                       // --json
    pub json: bool,

    /// Suppress the list of unchanged files.
    #[arg(short, long)]                                // --quiet / -q
    pub quiet: bool,
}
```

### ValueEnum for Restricted Choices

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:58-63

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
}

// In Args:
#[arg(long)]
pub format: Option<Format>,                // --format jpeg|png|webp

#[arg(long, default_value = "sequential")]
pub parallel: Parallel,                    // --parallel sequential|rayon|tokio
```

### Default Values

```rust
#[arg(short = 'w', long, default_value_t = DEFAULT_MAX_WIDTH)]
pub max_width: u32,

#[arg(long, default_value_t = DEFAULT_JPEG_QUALITY)]
pub quality: u8,

#[arg(long, default_value = "sequential")]  // string → ValueEnum conversion
pub parallel: Parallel,
```

### CLI Entry Point Pattern

All projects use the same pattern — separate parsing from logic:

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:171-181

pub fn main_exit_code() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(outcome) => outcome.exit_code(),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}
```

---

## Serde Serialization Framework

### Derive Macros

```rust
#[derive(Serialize, Deserialize)]   // auto-generate JSON/XML/bincode support
#[derive(Debug)]                     // {?:} formatting (like __repr__)
#[derive(Clone)]                     // deep copy with .clone()
#[derive(Default)]                   // Type::default() → zeroed fields
#[derive(PartialEq, Eq)]           // == comparison
```

### Common Serde Attributes

| Attribute | Purpose | Example |
|-----------|---------|---------|
| `#[serde(rename = "name")]` | Map to different JSON/XML key | `rename = "bill_type"` |
| `#[serde(default)]` | Use Default if field missing | Empty string, 0, empty vec |
| `#[serde(deserialize_with = "fn")]` | Custom deserialization | `null_or_default` |
| `#[serde(rename = "rootTag")]` on struct | Root XML element name | `rename = "billStatus"` |
| `#[serde(rename = "item")]` on Vec | Repeated XML children | `<item>` → vector |

### Using `r#type` for Reserved Keywords

```rust
// csearch-rscraper/src/models.rs:553-555

#[serde(default, deserialize_with = "null_or_default")]
pub r#type: String,    // "type" is a reserved keyword in Rust
```

---

## Binary Serialization (bincode)

Compact and fast — alternative to JSON for cache files.

### bincode v1 (csearch-rscraper)

```rust
// csearch-rscraper/src/hashes.rs:78-79

let PersistedHashes(hashes) = bincode::deserialize(&bytes)?;
let bytes = bincode::serialize(&PersistedHashes(self.hashes.clone()))?;
```

### bincode v2 (CLI Hash Cache)

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:367-377

// Deserialize
let config = bincode::config::standard();
let (cache, _bytes_read): (StoredCache, usize) =
    bincode::serde::decode_from_slice(&bytes, config)?;

// Serialize
let serialized = bincode::serde::encode_to_vec(cache, config)?;
```

---

## Redis Async Operations

### Connection and PING

```rust
// csearch-rscraper/src/redis_cache.rs:34-46

pub async fn clear_api_cache(cfg: &Config) -> Result<usize> {
    let client = redis::Client::open(cfg.redis_url.clone())?;
    let mut connection = client.get_multiplexed_async_connection().await?;

    let _: String = redis::cmd("PING").query_async(&mut connection).await?;
```

### Cursor-Based SCAN Loop

Non-blocking iteration through Redis keys (unlike `KEYS *` which freezes Redis):

```rust
// csearch-rscraper/src/redis_cache.rs:59-92

let mut cursor = 0_u64;
let mut deleted = 0_usize;

loop {
    let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
        .cursor_arg(cursor)
        .arg("MATCH")
        .arg(format!("{REDIS_CACHE_KEY_PREFIX}*"))
        .arg("COUNT")
        .arg(100)
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
    if cursor == 0 { break; }   // cursor 0 = full scan complete
}
```

---

## Async Subprocess Management

### Spawning a Child Process

```rust
// csearch-rscraper/src/python.rs:36-60

pub async fn run_congress_task(cfg: &Config, args: &[&str]) -> Result<()> {
    let mut command = Command::new("python3");
    command.arg(&run_py);
    command.args(args);
    command.current_dir(&congress_dir);
    command.env("PYTHONPATH", python_path);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command.spawn()
        .with_context(|| format!("spawn {}", run_py.display()))?;
```

### Concurrent stdout/stderr Streaming

Two concurrent tasks prevent pipe buffer deadlock:

```rust
// csearch-rscraper/src/python.rs:67-99

let stdout = child.stdout.take().context("missing python stdout")?;
let stderr = child.stderr.take().context("missing python stderr")?;

let stdout_task = tokio::spawn(stream_output("stdout", stdout));
let stderr_task = tokio::spawn(stream_output("stderr", stderr));

let status = child.wait().await?;

// Double ? — first unwraps JoinHandle, second unwraps stream_output's Result
stdout_task.await??;
stderr_task.await??;

if !status.success() {
    bail!("python task failed with status {status}");
}
```

### Async Line-by-Line Reading

```rust
// csearch-rscraper/src/python.rs:152-170

async fn stream_output(
    source: &'static str,                               // must be 'static for tokio::spawn
    reader: impl tokio::io::AsyncRead + Unpin,
) -> Result<()> {
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        info!(stream = source, output = line, "python");
    }
    Ok(())
}
```

---

## Rayon Data Parallelism

### Generic Parallel Batch Helper

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:723-732

#[cfg(feature = "rayon")]
pub fn par_batch<T, U, E, F>(items: Vec<T>, f: F) -> Vec<Result<U, E>>
where
    T: Send,                                  // items can cross thread boundaries
    U: Send,                                  // results can cross thread boundaries
    E: Send,                                  // errors can cross thread boundaries
    F: Fn(T) -> Result<U, E> + Sync + Send,  // closure is thread-safe
{
    items.into_par_iter().map(f).collect()    // one word different from sequential!
}
```

### Using rayon for Image Processing

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:740-755

#[cfg(feature = "rayon")]
fn run_batch_rayon(files: Vec<PathBuf>, config: ThumbnailConfig, ...) -> Result<()> {
    let results = par_batch(files, |input| -> Result<ThumbnailResult> {
        let format = detect_format(&input, None, format_override)?;
        let output = resolve_output(&input, None, format);
        generate_thumbnail(&input, &output, format, config)
    });

    for result in results {
        match result {
            Ok(thumb) => { processed += 1; print_result(&thumb); }
            Err(err) => { errors += 1; eprintln!("warning: {err:#}"); }
        }
    }
}
```

**Sequential → Parallel is a one-word change:**
```rust
// Sequential:
items.into_iter().map(|x| work(x)).collect();

// Parallel (add `use rayon::prelude::*`):
items.into_par_iter().map(|x| work(x)).collect();
```

---

## Markdown & HTML Processing

### Markdown Rendering with pulldown-cmark

```rust
// PHASE1/1.2-MARKDOWN-SANITIZER/src/lib.rs:107-112

pub fn render_markdown(&self, markdown: &str) -> String {
    let parser = MarkdownParser::new_ext(markdown, self.markdown_options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}
```

### HTML Sanitization with ammonia

Strips dangerous content (XSS) while preserving safe markup:

```rust
// PHASE1/1.2-MARKDOWN-SANITIZER/src/lib.rs:116-118

pub fn sanitize_html(&self, html: &str) -> String {
    self.sanitizer.clean(html).to_string()
}
```

### Pipeline Pattern (Configurable Builder)

```rust
// PHASE1/1.2-MARKDOWN-SANITIZER/src/lib.rs:56-130

pub struct MarkdownPipeline {
    markdown_options: Options,
    sanitizer: ammonia::Builder<'static>,
}

impl MarkdownPipeline {
    pub fn new() -> Self { Self::default() }

    // Builder-style API: returns Self for chaining
    pub fn with_markdown_options(mut self, opts: Options) -> Self {
        self.markdown_options = opts;
        self
    }

    pub fn render_and_sanitize(&self, markdown: &str) -> RenderedDocument {
        let raw_html = self.render_markdown(markdown);
        let sanitized_html = self.sanitize_html(&raw_html);
        RenderedDocument { raw_html, sanitized_html }
    }
}
```

### Markdown Options (Bitflag OR)

```rust
// PHASE1/1.2-MARKDOWN-SANITIZER/src/lib.rs:137-142

pub fn default_markdown_options() -> Options {
    Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
}
```

---

## Image Processing

### Load → Resize → Save Pattern

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:325-372

pub fn generate_thumbnail(
    input: &Path, output: &Path, format: Format, config: ThumbnailConfig,
) -> Result<ThumbnailResult> {
    // 1. Load
    let img = image::open(input)
        .with_context(|| format!("failed to open image '{}'", input.display()))?;

    let original = Dimensions::from_image(&img);
    let target = original.scale_to_width(config.max_width);

    // 2. Resize (skip if already small enough)
    let resized = if target == original {
        img
    } else {
        img.thumbnail(target.width, target.height)
    };

    // 3. Save
    save_image(&resized, output, format, config.jpeg_quality)?;
    Ok(ThumbnailResult { /* ... */ })
}
```

### Custom JPEG Quality Control

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:383-399

fn save_image(img: &DynamicImage, output: &Path, format: Format, jpeg_quality: u8) -> Result<()> {
    match format {
        Format::Jpeg => {
            let file = File::create(output)?;
            let encoder = JpegEncoder::new_with_quality(BufWriter::new(file), jpeg_quality);
            img.write_with_encoder(encoder)?;
        }
        Format::Png | Format::Webp => {
            img.save_with_format(output, format.to_image_format())?;
        }
    }
    Ok(())
}
```

### Aspect Ratio Preservation

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:260-268

pub fn scale_to_width(self, max_width: u32) -> Self {
    if self.width <= max_width {
        return self;                           // no resize needed
    }
    let scale = max_width as f64 / self.width as f64;
    let new_height = (self.height as f64 * scale).round() as u32;
    Self { width: max_width, height: new_height.max(1) }
}
```

---

## Concurrency Patterns

### Arc for Shared Ownership Across Tasks

```rust
// csearch-rscraper/src/votes.rs:346-357

// Wrap immutable data in Arc for sharing across tasks
let known_hashes = Arc::new(hashes.snapshot());
let parse_sem = Arc::new(Semaphore::new(WORKER_LIMIT));

for job in jobs {
    let known_hashes = known_hashes.clone();   // cheap: bumps reference count
    let parse_sem = parse_sem.clone();

    tasks.spawn(async move {
        // Each task gets its own Arc clone — data is shared, not copied
    });
}
```

### `async move` Closures

The `move` keyword transfers ownership of captured variables into the closure.
Required because spawned tasks may outlive the current scope.

```rust
write_tasks.spawn(async move {
    let _permit = write_sem.acquire_owned().await?;
    insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
    Ok::<_, anyhow::Error>(changed_vote)
});
```

### Explicit Type Annotations in Async Blocks

Rust can't always infer the error type inside async blocks:

```rust
Ok::<_, anyhow::Error>(changed_vote)
// Says: "Ok carries ChangedVote, Err carries anyhow::Error"
```

---

## File I/O & Path Handling

### PathBuf vs &Path

```
PathBuf = owned path (like String)     — stored in structs, returned from functions
&Path   = borrowed path (like &str)    — passed as function parameters
```

```rust
// csearch-rscraper/src/config.rs:39,152-160

pub struct Config {
    pub congress_dir: PathBuf,          // owned
}

pub fn congress_runtime_dir(&self) -> PathBuf {
    let runtime_dir = self.congress_dir.join("congress");  // .join() concatenates
    if runtime_dir.join("run.py").exists() {
        runtime_dir
    } else {
        PathBuf::from("/opt/csearch/congress")
    }
}
```

### Directory Traversal with walkdir

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:262-301

fn discover_files(scan_root: &Path, cache_path: &Path) -> Result<Vec<DiscoveredFile>> {
    let mut discovered = Vec::new();

    for entry in WalkDir::new(scan_root) {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path == cache_path { continue; }       // skip cache file
                if !entry.file_type().is_file() { continue; }

                let relative_path = path.strip_prefix(scan_root)?;
                discovered.push(DiscoveredFile {
                    relative_path: relative_path.to_path_buf(),
                    absolute_path: path.to_path_buf(),
                });
            }
            Err(error) => {
                eprintln!("warning: failed to read a directory entry: {error}");
            }
        }
    }

    discovered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(discovered)
}
```

### Atomic File Writes

Write to a temp file, then rename into place to prevent partial writes:

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:518-578

fn save_cache(cache_path: &Path, format: CacheFormat, cache: &StoredCache) -> Result<()> {
    // 1. Create parent directories
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // 2. Serialize content
    let serialized = match format { /* ... */ };

    // 3. Write to temporary file
    let temp_path = temporary_cache_path(cache_path);
    let mut temp_file = File::create(&temp_path)?;
    temp_file.write_all(&serialized)?;
    temp_file.flush()?;
    drop(temp_file);  // explicitly close before rename

    // 4. Atomic rename into place
    if cache_path.exists() { fs::remove_file(cache_path)?; }
    fs::rename(&temp_path, cache_path)?;
    Ok(())
}
```

### stdin/stdout I/O

```rust
// PHASE1/1.2-MARKDOWN-SANITIZER/src/lib.rs:196-227

fn read_input(path: Option<&PathBuf>) -> Result<String> {
    match path {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read input file '{}'", path.display())),
        None => {
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer)
                .context("failed to read markdown from stdin")?;
            Ok(buffer)
        }
    }
}

fn write_output(path: Option<&PathBuf>, html: &str) -> Result<()> {
    match path {
        Some(path) => fs::write(path, html)
            .with_context(|| format!("failed to write output file '{}'", path.display())),
        None => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(html.as_bytes())
                .context("failed to write sanitized HTML to stdout")?;
            Ok(())
        }
    }
}
```

---

## Environment Variables & Configuration

### Loading .env Files

```rust
// csearch-rscraper/src/main.rs:66

let _ = dotenvy::dotenv();   // load .env; ignore if missing
```

### Reading and Parsing Environment Variables

```rust
// csearch-rscraper/src/config.rs:85-137

// Required variable with custom error
let congress_dir = env::var("CONGRESSDIR")
    .map(PathBuf::from)
    .map_err(|_| anyhow::anyhow!("missing CONGRESSDIR"))?;

// Optional with default
let redis_url = env::var("REDIS_URL")
    .unwrap_or_else(|_| "redis://localhost:6379".to_string());

// Parse with default on failure
let db_port: u16 = env::var("DB_PORT")
    .ok()                                      // Result → Option
    .and_then(|value| value.parse().ok())     // Option chain
    .unwrap_or(5432);                          // default

// Boolean parsing
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

---

## Module Organization

### Module Declarations

Rust requires explicit `mod` declarations — no auto-discovery like Python:

```rust
// csearch-rscraper/src/main.rs:14-23

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

### Visibility (pub vs private)

```rust
pub struct FileHashStore {     // public struct
    path: PathBuf,             // private field
    hashes: HashMap<String, String>,  // private field
}

impl FileHashStore {
    pub fn load(...) -> Result<Self> { ... }   // public method
    pub fn save(&self) -> Result<()> { ... }   // public method
}
```

### Import Patterns

```rust
use std::env;                   // stdlib
use std::path::PathBuf;

use anyhow::{Result, bail};    // external crate
use chrono::Datelike;

use crate::config::Config;     // absolute from crate root
use crate::hashes::FileHashStore;

use super::*;                   // parent module (in tests)
```

---

## Type System Patterns

### Option<T> for Nullable Values

```rust
// csearch-rscraper/src/models.rs:146-160

pub struct InsertVoteParams {
    pub voteid: String,                // required
    pub bill_type: Option<String>,     // nullable → SQL NULL when None
    pub bill_number: Option<i32>,      // nullable
    pub votedate: Option<NaiveDate>,   // nullable
}
```

### Enum as Tagged Union (Algebraic Data Type)

```rust
// csearch-rscraper/src/votes.rs:99-103

enum VoteParseOutcome {
    Changed(ChangedVote),    // variant with data
    Skipped,                 // variant without data
    Missing,
}

// Pattern matching
match outcome {
    VoteParseOutcome::Changed(vote) => { /* process */ }
    VoteParseOutcome::Skipped => { stats.votes_skipped += 1; }
    VoteParseOutcome::Missing => { stats.votes_missing += 1; }
}
```

### Default Trait

```rust
// csearch-rscraper/src/stats.rs:19-27

#[derive(Debug, Default)]
pub struct RunStats {
    pub bills_processed: u64,   // Default for u64 = 0
    pub bills_skipped: u64,
    pub votes_processed: u64,
}

let mut stats = RunStats::default();  // all zeros
```

### Newtype Pattern (Wrapper Struct)

```rust
// csearch-rscraper/src/hashes.rs:50-51

#[derive(Debug, Serialize, Deserialize)]
struct PersistedHashes(HashMap<String, String>);  // tuple struct wrapping a HashMap
```

### Copy vs Clone

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:298-302

#[derive(Debug, Clone, Copy)]   // Copy = bitwise copy, no .clone() needed
pub struct ThumbnailConfig {
    pub max_width: u32,
    pub jpeg_quality: u8,
}
// Copy types are automatically Send + Sync — perfect for passing to closures
```

### `impl Into<T>` for Flexible Parameters

```rust
// csearch-rscraper/src/hashes.rs:63-65

pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
    let path = path.into();  // accepts &str, String, Path, PathBuf, etc.
    // ...
}

// csearch-rscraper/src/util.rs:89-92
pub fn option_string(value: impl Into<String>) -> Option<String> {
    let value = value.into();
    if value.is_empty() { None } else { Some(value) }
}
```

### `impl AsRef<str>` for Read-Only Access

```rust
// PHASE1/1.2-MARKDOWN-SANITIZER/src/lib.rs:149-151

pub fn render_markdown(markdown: impl AsRef<str>) -> String {
    MarkdownPipeline::default().render_markdown(markdown.as_ref())
}
```

---

## String Handling

### &str vs String

```
&str    = borrowed, immutable view (like a pointer to chars)
String  = owned, growable string (like Python/JS strings)
```

```rust
fn env_enabled(name: &str, ...) -> bool { ... }   // borrows: read-only
pub fn postgres_dsn(&self) -> String { ... }       // returns owned String
```

### Common Conversions

```rust
"hello".to_string()                  // &str → String
string.as_str()                      // String → &str (for match)
path.to_string_lossy()               // Path → Cow<str> (replaces non-UTF8)
path.to_string_lossy().into_owned()  // Cow<str> → String
```

### String Formatting

```rust
format!("postgres://{}:{}@{}:{}/{}", user, pass, host, port, db)
format!("{:x}", hasher.finalize())   // lowercase hex
format!("{trimmed:?}")               // Debug formatting (adds quotes)
format!("{stem}_thumb.{}", ext)      // inline variable + positional
```

### Raw String Literals

```rust
r#"
INSERT INTO votes (voteid, bill_type)
VALUES ($1, $2)
ON CONFLICT (voteid) DO UPDATE SET bill_type = excluded.bill_type
"#
```

No escaping needed for quotes or backslashes inside `r#"..."#`.

---

## Iterator & Functional Patterns

### Method Chains

```rust
// csearch-rscraper/src/config.rs:129-132

db_port: env::var("DB_PORT")
    .ok()                                    // Result → Option
    .and_then(|value| value.parse().ok())   // chain Option operations
    .unwrap_or(5432),                        // default value
```

### Sorting

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:299

discovered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
```

### Collecting Into Containers

```rust
let paths: Vec<PathBuf> = discovered
    .into_iter()
    .map(|file| file.relative_path)
    .collect();
```

### `.then_some()` for Conditional Values

```rust
// csearch-rscraper/src/votes.rs:488-489

let congress = (vote_json.bill.congress != 0).then_some(vote_json.bill.congress);
// true → Some(value), false → None
```

### `Vec::with_capacity` for Pre-allocation

```rust
let mut members = Vec::with_capacity(items.len());
// Pre-allocates memory; avoids repeated reallocations as the vector grows
```

### BTreeMap for Sorted Keys

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:77-81

#[derive(Serialize, Deserialize)]
struct StoredCache {
    version: u32,
    algorithm: String,
    entries: BTreeMap<PathBuf, String>,  // sorted keys → stable output
}
```

---

## Feature Flags & Conditional Compilation

### Cargo.toml

```toml
# PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/Cargo.toml

[features]
rayon = ["dep:rayon"]
tokio = ["dep:tokio"]

[dependencies]
rayon = { version = "1", optional = true }
tokio = { version = "1", features = ["rt-multi-thread", "sync", "macros"], optional = true }
```

### Conditional Imports

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:9-17

#[cfg(feature = "rayon")]
use rayon::prelude::*;

#[cfg(feature = "tokio")]
use std::sync::Arc;
#[cfg(feature = "tokio")]
use tokio::sync::Semaphore;
```

### Runtime Feature Detection with Helpful Errors

```rust
// PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs:587-594

Parallel::Rayon => {
    #[cfg(feature = "rayon")]
    return run_batch_rayon(files, config, format_override, quiet);
    #[cfg(not(feature = "rayon"))]
    bail!(
        "--parallel rayon requires the rayon feature; \
         rebuild with: cargo build --features rayon"
    );
}
```

### Building with Features

```bash
cargo build --features rayon
cargo build --features tokio
cargo build --features rayon,tokio
```

---

## Testing Patterns

### Unit Tests with `#[cfg(test)]`

```rust
// csearch-rscraper/src/config.rs:206-263

#[cfg(test)]       // only compiled during `cargo test`
mod tests {
    use super::*;  // import everything from parent module
    use tempfile::TempDir;

    #[test]
    fn load_config_from_env() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir = TempDir::new().unwrap();

        unsafe {
            env::set_var("CONGRESSDIR", temp_dir.path());
            env::set_var("POSTGRESURI", "localhost");
        }

        let cfg = Config::load().unwrap();
        assert_eq!(cfg.congress_dir, temp_dir.path());
        assert!(cfg.run_bills);
    }
}
```

### Mutex for Serializing Environment Variable Tests

```rust
// csearch-rscraper/src/config.rs:218-221

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
```

### Round-Trip Testing (Save → Load → Compare)

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:664-677

#[test]
fn save_and_load_cache_round_trip_in_json() {
    let temp_dir = tempdir().expect("failed to create tempdir");
    let cache_path = temp_dir.path().join(".hash_cache.json");

    let original = StoredCache::new(BTreeMap::from([
        (PathBuf::from("a.txt"), "111".to_string()),
        (PathBuf::from("nested/b.txt"), "222".to_string()),
    ]));

    save_cache(&cache_path, CacheFormat::Json, &original).expect("save");
    let loaded = load_cache(&cache_path, CacheFormat::Json).expect("load");

    assert_eq!(loaded, original);
}
```

### Known-Value Testing

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:607-618

#[test]
fn hash_file_matches_known_sha256_digest() {
    let temp_dir = tempdir().expect("failed to create tempdir");
    let file_path = temp_dir.path().join("sample.txt");
    fs::write(&file_path, "abc").expect("write");

    let digest = hash_file(&file_path).expect("hash_file");

    assert_eq!(
        digest,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
```

### XSS Sanitization Tests

```rust
// PHASE1/1.2-MARKDOWN-SANITIZER/src/lib.rs:247-252

#[test]
fn strips_script_tags_from_embedded_html() {
    let rendered = render_and_sanitize("before<script>alert('xss')</script>after");

    assert!(rendered.raw_html.contains("<script>alert('xss')</script>"));
    assert_eq!(rendered.sanitized_html, "<p>beforeafter</p>\n");
}
```

### JSON Test Fixtures

```rust
// csearch-rscraper/src/votes.rs:644-684

#[test]
fn parse_vote_skips_legacy_string_markers() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("data.json");
    let payload = r#"{
      "vote_id": "s47-110.2008",
      "votes": {
        "Yea": ["VP", {"display_name": "...", "id": "S999", "party": "D", "state": "IL"}],
        "Nay": []
      }
    }"#;

    fs::write(&path, payload).unwrap();
    let parsed = parse_vote(&path).unwrap();

    assert_eq!(parsed.members.len(), 1);          // "VP" string was filtered out
    assert_eq!(parsed.members[0].bioguide_id, "S999");
}
```

### Integration Test (Full CLI Workflow)

```rust
// PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs:698-713

#[test]
fn run_reports_changes_on_first_scan() {
    let temp_dir = tempdir().expect("failed to create tempdir");
    write_file(temp_dir.path(), "file.txt", "hello");

    let args = Args {
        directory: temp_dir.path().to_path_buf(),
        cache_file: None,
        json: true,
        quiet: true,
    };

    let outcome = run(args).expect("run should succeed");
    assert!(outcome.changes_detected);
    assert!(temp_dir.path().join(".hash_cache.json").exists());
}
```

---

## Crate Reference Table

| Crate | Purpose | Version | Used In |
|-------|---------|---------|---------|
| `anyhow` | Error handling with context | 1.0 | All projects |
| `serde` + `serde_json` | JSON serialization framework | 1.0 | csearch-rscraper, 1.1 |
| `bincode` | Binary serialization | 1.0 (scraper), 2.0 (1.1) | csearch-rscraper, 1.1 |
| `clap` | CLI argument parsing | 4.5 | All projects |
| `tokio` | Async runtime | 1.0 | csearch-rscraper, 1.3 (optional) |
| `sqlx` | Async PostgreSQL database | 0.8 | csearch-rscraper |
| `quick-xml` | XML parsing via serde | 0.37 | csearch-rscraper |
| `redis` | Async Redis client | 0.27 | csearch-rscraper |
| `sha2` | SHA-256 hashing | 0.10 | csearch-rscraper, 1.1 |
| `tracing` | Structured logging | 0.1 | csearch-rscraper |
| `tracing-subscriber` | Log output formatting | 0.3 | csearch-rscraper |
| `chrono` | Date/time handling | 0.4 | csearch-rscraper |
| `pulldown-cmark` | Markdown → HTML rendering | 0.13 | 1.2 |
| `ammonia` | HTML sanitization (XSS prevention) | 4.0 | 1.2 |
| `image` | Image decoding/encoding/resizing | 0.25 | 1.3 |
| `rayon` | Data-parallel work-stealing pool | 1.0 | 1.3 (optional) |
| `walkdir` | Recursive directory traversal | 2.5 | 1.1, 1.3 |
| `tempfile` | Temporary files/directories for tests | 3.20 | All (dev) |
| `dotenvy` | .env file loading | 0.15 | csearch-rscraper |

---

## Key Rust Idioms Summary

| Idiom | What It Replaces | Example |
|-------|-----------------|---------|
| `Option<T>` | null/None | Compile-time null safety |
| `Result<T, E>` + `?` | try/catch | Explicit error propagation |
| RAII (Drop) | try/finally | Auto-cleanup for transactions, locks, permits |
| Ownership + Borrowing | Garbage collection | Memory safety without GC |
| `match` | switch/case | Exhaustive pattern matching on enums |
| `#[derive(...)]` | Boilerplate code | Auto-generate Debug, Clone, Serialize, etc. |
| `impl Trait` | Duck typing | Compile-time checked generic parameters |
| `async`/`.await` | Callbacks | Non-blocking I/O with Tokio |
| Feature flags | Runtime config | Conditional compilation of optional deps |
| `Arc<T>` | Shared pointers | Thread-safe reference counting |
| `&str` vs `String` | One string type | Borrowed vs owned string data |
| `PathBuf` vs `&Path` | One path type | Owned vs borrowed path data |
