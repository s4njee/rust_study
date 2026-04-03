# Overview And Core Ideas

This doc covers the mental model that makes the rest of the repo easier to
understand.

## Ownership, Borrowing, And Why Rust Cares

In plain English: Rust always wants to know who is responsible for a piece of
data. That sounds strict, but it prevents a whole class of bugs where one part
of a program changes or frees data while another part is still using it.

There are three levels of access:

- **owned value:** the current code is responsible for it — it can read, write,
  and ultimately drop it
- **borrowed value (`&T`):** the current code may read it, but does not own it
  and cannot drop it
- **mutable borrow (`&mut T`):** the current code may read *and* write it, but
  only one mutable borrow can exist at a time

Shared ownership is possible, but you ask for it explicitly with types like
`Arc<T>` (see below).

This repo uses that distinction constantly:

```rust
// PathBuf owns a path — good for storing in structs
pub struct Config {
    pub congress_dir: PathBuf,
}

// &Path borrows a path — good for function parameters that only need to read
fn canonicalize_directory(directory: &Path) -> Result<PathBuf> {
    // ...
}
```

The same ownership split applies to strings:

```rust
// String owns text — use when you need to store or return text
pub fn postgres_dsn(&self) -> String {
    format!(
        "postgres://{}:{}@{}:{}/{}",
        self.db_user, self.db_password, self.postgres_uri, self.db_port, self.db_name
    )
}

// &str borrows text — use when a function only needs to inspect it
pub fn env_enabled(name: &str, default_value: bool) -> bool {
    // ...
}
```

**Why this matters in practice:** In Python or JavaScript, the garbage collector
figures out when data is no longer needed. In Rust, the compiler tracks
ownership at compile time, so there is zero runtime cost and no garbage
collector pauses. The trade-off is that you must satisfy the borrow checker
before your code compiles.

### Mutable Borrows In Action

The scraper pipeline shows mutable borrowing at its clearest:

```rust
// &mut passes a mutable reference — the function can modify these values
// but does not take ownership of them
votes::process_votes(&pool, &cfg, &mut vote_hashes, &mut stats).await?;
```

Rust enforces that `vote_hashes` and `stats` cannot be read or written anywhere
else while `process_votes` holds the mutable borrow. That eliminates data races
at compile time.

## `Option<T>` Replaces "Maybe Null"

In plain English: Rust makes you admit when a value might be missing, and the
compiler forces you to handle both cases before the code compiles.

```rust
pub struct InsertVoteParams {
    pub bill_type: Option<String>,
    pub bill_number: Option<i32>,
}
```

Instead of "hope this is not null," the type itself says "this may or may not
exist." That makes missing data something you handle on purpose.

Common ways to work with `Option`:

```rust
// Pattern matching — handle both cases explicitly
match vote.bill_type {
    Some(bt) => println!("Bill type: {bt}"),
    None => println!("No bill type"),
}

// .unwrap_or() — provide a default if the value is missing
let port = env::var("DB_PORT")
    .ok()                              // Result → Option
    .and_then(|val| val.parse().ok())   // try to parse, get Option<u16>
    .unwrap_or(5432);                   // use 5432 if any step failed

// .is_some_and() — conditional check without unwrapping
// used in csearch-rscraper/src/votes.rs for conditional values
let should_include = item.bill_type.is_some_and(|bt| bt == "hr");
```

Examples in the repo:

- nullable SQL fields in `csearch-rscraper/src/models.rs`
- conditional values like `.then_some(...)` in `csearch-rscraper/src/votes.rs`
- optional CLI arguments in `PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs`

## `Result<T, E>` Replaces Hidden Failure

In plain English: a Rust function usually tells you up front whether it can
fail. That keeps errors from hiding until much later.

```rust
pub fn load() -> anyhow::Result<Self> { /* ... */ }
```

The return type itself is a contract: "this function might fail, and you must
deal with that." Compare this to Python where any function can raise any
exception with no indication in the signature.

You will see three common patterns in this repo:

```rust
// ? means "if this failed, stop here and return the error to the caller"
let cfg = Config::load()?;

// bail! means "stop right now with a clear error message"
if !congress_dir.exists() {
    bail!("CONGRESSDIR does not exist: {}", congress_dir.display());
}

// .context(...) adds a friendlier explanation before the error bubbles up
let pool = PgPoolOptions::new()
    .max_connections(DB_WRITE_CONCURRENCY)
    .connect(&cfg.postgres_dsn())
    .await
    .context("connect to postgres")?;
```

The `?` operator is the backbone of Rust error handling. It is shorthand for:

```rust
// These two are equivalent:
let cfg = Config::load()?;

let cfg = match Config::load() {
    Ok(value) => value,
    Err(err) => return Err(err.into()),
};
```

This means errors propagate explicitly at every call site — you can see exactly
where a function might bail out just by scanning for `?`.

## RAII Means Cleanup Happens Automatically

In plain English: Rust ties cleanup to scope. When a value goes out of scope,
its cleanup code (the `Drop` trait) runs automatically. This is called RAII —
"Resource Acquisition Is Initialization."

That is why these patterns feel safe in this repo:

**Transactions roll back if you forget to commit:**

```rust
let mut tx = pool.begin().await?;
db::insert_vote(&mut tx, &parsed_vote.vote).await?;
db::delete_vote_members(&mut tx, &parsed_vote.vote.voteid).await?;
// If this function returns early (via ?) before commit, tx is dropped
// and the transaction automatically rolls back
tx.commit().await?;
```

**Semaphore permits are released when the variable is dropped:**

```rust
write_tasks.spawn(async move {
    let _permit = write_sem.acquire_owned().await?;
    // _permit is held for the duration of this block
    insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
    Ok::<_, anyhow::Error>(changed_vote)
    // _permit is dropped here → the semaphore slot opens up for another task
});
```

**Files get closed when they leave scope:**

```rust
fn hash_file(path: &Path) -> Result<String> {
    let file = File::open(path)?; // file handle acquired
    let mut reader = BufReader::new(file);
    // ... read and hash the file ...
    Ok(format!("{digest:x}"))
    // reader (and the file inside it) are dropped here → file handle closed
}
```

**Mutex guards unlock automatically:**

```rust
let _guard = env_lock().lock().unwrap();
// the lock is held for the rest of this scope
// when _guard is dropped, the lock is automatically released
```

You do not need a lot of manual "finally" style cleanup logic. If cleanup is
tied to a value, Rust handles it for you.

## `Copy` vs `Clone`

In plain English:

- `Copy` is for tiny, cheap values that can be duplicated automatically
  (bit-for-bit copy, like copying an integer)
- `Clone` is for values where duplication is a real operation that allocates
  memory or does meaningful work

```rust
#[derive(Debug, Clone, Copy)]
pub struct ThumbnailConfig {
    pub max_width: u32,
    pub jpeg_quality: u8,
}
```

That works because the struct only contains small numeric fields. The compiler
can copy it with a simple memcpy.

In contrast, `String` is `Clone` but not `Copy` because cloning a string means
allocating new heap memory and copying bytes into it. Rust forces you to write
`.clone()` explicitly so you know you are paying that cost:

```rust
// This allocates a new string
let name_copy = name.clone();

// This would NOT compile — String is not Copy:
// let name_copy = name;  // this MOVES name, it doesn't copy it
```

**Rule of thumb:** If a struct only contains `Copy` fields (integers, bools,
floats, other `Copy` structs), you can derive `Copy`. If it contains `String`,
`Vec`, `PathBuf`, or any heap-allocated type, you should derive `Clone` instead.

## Newtype Pattern

In plain English: sometimes you wrap a value in a small struct so it has a more
specific meaning. This prevents mixing up two values that have the same
underlying type but represent different things.

```rust
struct PersistedHashes(HashMap<String, String>);
```

This is useful when a raw `HashMap` is technically correct but too generic.
The wrapper says, "this map is specifically the saved hash cache."

Another example from the repo is `Dimensions`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}
```

Without this struct, you would pass `(u32, u32)` tuples and constantly wonder
"is this width-height or height-width?" The named fields eliminate that
ambiguity.

## `impl Into<T>` And `impl AsRef<T>`

In plain English:

- `Into<T>` is good when you want to accept several input types and turn them
  into an owned value
- `AsRef<T>` is good when you only need read-only access

```rust
pub fn load(path: impl Into<PathBuf>) -> Result<Self> { /* ... */ }
pub fn render_markdown(markdown: impl AsRef<str>) -> String { /* ... */ }
```

This makes APIs easier to call without adding lots of overloads:

```rust
// All of these work because of `impl Into<PathBuf>`:
FileHashStore::load("path/to/file");         // &str → PathBuf
FileHashStore::load(String::from("path"));   // String → PathBuf
FileHashStore::load(PathBuf::from("path"));  // PathBuf → PathBuf (no conversion)

// All of these work because of `impl AsRef<str>`:
render_markdown("# Hello");                   // &str
render_markdown(String::from("# Hello"));    // String (via AsRef<str>)
```

## `Arc<T>` For Shared Ownership Across Tasks

In plain English: `Arc<T>` is how several tasks can point at the same data
safely. "Arc" stands for "Atomically Reference Counted."

```rust
let known_hashes = Arc::new(hashes.snapshot());
let parse_sem = Arc::new(Semaphore::new(WORKER_LIMIT));
```

Cloning an `Arc` does **not** copy the underlying data. It just increases the
reference count. When the last `Arc` is dropped, the inner data is freed.

This matters in concurrent code because each spawned task needs its own handle
to the shared data:

```rust
let pool = Arc::new(pool);
let sem = Arc::new(Semaphore::new(4));

for file in files {
    let pool = Arc::clone(&pool);   // cheap reference count bump
    let sem = Arc::clone(&sem);

    tokio::spawn(async move {
        let _permit = sem.acquire().await?;
        process_file(&pool, &file).await
    });
}
```

## Rust Idioms You Will See Everywhere

| Idiom | Plain-English meaning | Example in this repo |
|---|---|---|
| `match` | "Handle every possible case" | Vote parse outcome dispatch |
| `#[derive(...)]` | "Generate the boring code for me" | Every struct definition |
| `Option<T>` | "This value might be missing" | Nullable SQL fields |
| `Result<T, E>` | "This operation might fail" | Every I/O function |
| `Arc<T>` | "Several tasks need shared read access" | Hash store, semaphores |
| `async`/`.await` | "Pause this task without blocking the whole thread" | DB queries, Redis calls |
| `?` | "Propagate this error to the caller" | Almost every function |
| `mut` | "I will modify this value" | Stats counters, hash stores |

## Good Next Step

Read [`02-async-and-concurrency.md`](02-async-and-concurrency.md) next if the
async code in `csearch-rscraper` feels unfamiliar.
