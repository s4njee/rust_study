# 1.1 — CLI Hash Cache: Implementation Decisions

Every choice in the [PLAN.md](./PLAN.md) is a _default_, not a requirement. This document catalogs each decision, explains the trade-offs, and lists alternatives worth considering.

---

## Table of Contents

1. [Hash Algorithm](#1-hash-algorithm)
2. [Serialization & Cache Format](#2-serialization--cache-format)
3. [Error Handling Strategy](#3-error-handling-strategy)
4. [Parallel Hashing & Rust Concurrency](#4-parallel-hashing--rust-concurrency)
5. [Directory Walking](#5-directory-walking)
6. [CLI Argument Parsing](#6-cli-argument-parsing)
7. [Buffer Size for File Reading](#7-buffer-size-for-file-reading)
8. [Cache Key Design](#8-cache-key-design)
9. [Atomic Writes](#9-atomic-writes)

---

## 1. Hash Algorithm

**Current choice:** SHA-256 via the `sha2` crate.

**Why SHA-256:** It matches the existing Go scraper's behavior, produces a fixed 32-byte digest, and is cryptographically strong enough to guarantee collision resistance for file change detection.

### Alternatives

| Algorithm | Crate | Digest Size | Speed vs SHA-256 | Notes |
|-----------|-------|-------------|-------------------|-------|
| **BLAKE3** | `blake3` | 32 bytes | ~3–5× faster | Designed for hashing speed. Uses SIMD and multithreading internally. Best option if performance matters more than compatibility with the Go scraper. |
| **SHA-512** | `sha2` | 64 bytes | ~1.3× faster on 64-bit | Wider digest, faster on modern 64-bit CPUs due to native 64-bit operations. Same crate, just swap the type. |
| **xxHash (XXH3)** | `xxhash-rust` | 8 or 16 bytes | ~10× faster | Non-cryptographic. Perfect for change detection where collision attacks aren't a concern. Extremely fast. |
| **MD5** | `md5` | 16 bytes | ~1.5× faster | Cryptographically broken but still fine for change detection. Not recommended for new code. |
| **CRC32** | `crc32fast` | 4 bytes | ~20× faster | Checksum, not a hash. Higher collision risk. Only suitable for very casual change detection. |

### Code Comparison

**SHA-256 (current):**
```rust
use sha2::{Sha256, Digest};

let mut hasher = Sha256::new();
hasher.update(&buffer[..bytes_read]);
let result = hasher.finalize();
let hex = format!("{:x}", result);
```

**BLAKE3 (alternative):**
```rust
use blake3;

let mut hasher = blake3::Hasher::new();
hasher.update(&buffer[..bytes_read]);
let hash = hasher.finalize();
let hex = hash.to_hex().to_string();
```

**XXH3 (alternative):**
```rust
use xxhash_rust::xxh3::xxh3_64;

// One-shot, no streaming needed for small files
let hash = xxh3_64(&file_bytes);
let hex = format!("{:016x}", hash);
```

### Decision Guidance

- **Porting the Go scraper?** → Stick with SHA-256 for consistency.
- **Pure performance?** → BLAKE3 or XXH3.
- **Don't care about cryptographic strength?** → XXH3 is the fastest by a wide margin.

📖 [RustCrypto hashes](https://github.com/RustCrypto/hashes) · [BLAKE3](https://docs.rs/blake3/latest/blake3/) · [xxhash-rust](https://docs.rs/xxhash-rust/latest/xxhash_rust/)

---

## 2. Serialization & Cache Format

**Current choice:** `bincode` for binary serialization, `serde_json` as an optional debug format.

### Alternatives

| Format | Crate | Human Readable | File Size | Speed | Schema Evolution |
|--------|-------|:--------------:|-----------|-------|-----------------|
| **bincode** | `bincode` | ✗ | Smallest | Fastest | Poor — breaking on struct changes |
| **JSON** | `serde_json` | ✓ | Largest | Moderate | Moderate — additive changes ok |
| **MessagePack** | `rmp-serde` | ✗ | Small | Fast | Moderate |
| **CBOR** | `ciborium` | ✗ | Small | Fast | Good — self-describing |
| **RON** | `ron` | ✓ | Large | Moderate | Moderate — Rust-flavored syntax |
| **TOML** | `toml` | ✓ | Large | Moderate | Moderate |
| **postcard** | `postcard` | ✗ | Smallest | Fastest | Poor — no-std friendly |

### Trade-off Analysis

**bincode** is the right default for a local cache file: it's fast, compact, and the cache is ephemeral (can always be deleted and rebuilt). The downside is that **any change to the `HashCache` struct fields will break deserialization of old cache files** — you silently get garbage or a deserialization error.

**JSON** is the best debugging format because you can inspect the cache with `cat` or `jq`. It's ~3–5× larger on disk and slower to parse for large caches.

**MessagePack** is a sweet spot: binary and compact like bincode, but with better schema tolerance since it can encode as named maps.

### Code Comparison

**MessagePack (alternative):**
```rust
// Cargo.toml: rmp-serde = "1"
use rmp_serde;

fn save_cache(cache: &HashCache, path: &Path) -> anyhow::Result<()> {
    let encoded = rmp_serde::to_vec(cache)?;
    std::fs::write(path, encoded)?;
    Ok(())
}

fn load_cache(path: &Path) -> anyhow::Result<HashCache> {
    let data = std::fs::read(path)?;
    let cache: HashCache = rmp_serde::from_slice(&data)?;
    Ok(cache)
}
```

**RON — Rusty Object Notation (alternative):**
```rust
// Cargo.toml: ron = "0.8"
use ron;

fn save_cache(cache: &HashCache, path: &Path) -> anyhow::Result<()> {
    let pretty = ron::ser::PrettyConfig::default();
    let s = ron::ser::to_string_pretty(cache, pretty)?;
    std::fs::write(path, s)?;
    Ok(())
}
```

### Decision Guidance

- **Local-only cache, rebuild is cheap?** → bincode (current choice is fine).
- **Need to inspect the cache?** → JSON or RON.
- **Want compact + schema tolerant?** → MessagePack.
- **Version the cache:** Consider adding a version byte header regardless of format, e.g. `[0x01] ++ bincode_bytes`, so you can detect and migrate old formats.

📖 [bincode](https://docs.rs/bincode/latest/bincode/) · [rmp-serde](https://docs.rs/rmp-serde/latest/rmp_serde/) · [ciborium](https://docs.rs/ciborium/latest/ciborium/) · [ron](https://docs.rs/ron/latest/ron/)

---

## 3. Error Handling Strategy

**Current choice:** `anyhow` for all error handling.

`anyhow` is a convenience crate for _applications_ (binaries). It provides a single, type-erased `anyhow::Error` that can wrap any `std::error::Error` and lets you annotate errors with `.context()` messages.

### Alternatives

| Approach | Crate | Best For | Trade-off |
|----------|-------|----------|-----------|
| **anyhow** | `anyhow` | Applications / binaries | Easy but hides error types — callers can't match on specific errors |
| **thiserror** | `thiserror` | Libraries | Define your own typed error enum with derive macros. More boilerplate, but callers can pattern-match |
| **Manual enums** | (std only) | Learning / small projects | Full control, no dependencies. Teaches `From`, `Display`, `Error` traits |
| **eyre + color-eyre** | `eyre`, `color-eyre` | CLIs with rich output | Like anyhow but with colorized, `RUST_BACKTRACE`-style reports. Very pretty terminal output |
| **miette** | `miette` | Developer tools / compilers | Fancy diagnostic output with source code spans, labels, and help text |

### Code Comparison

**anyhow (current):**
```rust
use anyhow::{Context, Result};

fn hash_file(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    // ...
}
```

**thiserror (alternative) — define a typed enum:**
```rust
use thiserror::Error;

#[derive(Error, Debug)]
enum HashError {
    #[error("Failed to open file {path}: {source}")]
    OpenFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Failed to read file {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn hash_file(path: &Path) -> Result<String, HashError> {
    let file = File::open(path).map_err(|e| HashError::OpenFailed {
        path: path.to_owned(),
        source: e,
    })?;
    // ...
}
```

**Manual impl (no crate):**
```rust
use std::fmt;

#[derive(Debug)]
enum HashError {
    Io(std::io::Error),
    Cache(String),
}

impl fmt::Display for HashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashError::Io(e) => write!(f, "IO error: {}", e),
            HashError::Cache(msg) => write!(f, "Cache error: {}", msg),
        }
    }
}

impl std::error::Error for HashError {}

// This lets the `?` operator convert std::io::Error into HashError
impl From<std::io::Error> for HashError {
    fn from(err: std::io::Error) -> Self {
        HashError::Io(err)
    }
}
```

**color-eyre (alternative) — pretty terminal errors:**
```rust
// Cargo.toml: color-eyre = "0.6"
use color_eyre::eyre::{Context, Result};

fn main() -> Result<()> {
    color_eyre::install()?; // set up colorized panic/error handler
    let hash = hash_file(Path::new("test.txt"))
        .wrap_err("Failed during hashing phase")?;
    Ok(())
}
```

### Decision Guidance

- **Binary / CLI tool?** → `anyhow` or `color-eyre`. You're the only consumer of the error types.
- **Library crate that others import?** → `thiserror` so callers can match on your errors.
- **Learning exercise?** → Try manual enums first to understand the `Error`, `Display`, and `From` traits, then switch to `thiserror`.
- **Both?** → Use `thiserror` for your library errors, `anyhow` in `main()` to wrap them.

📖 [anyhow](https://docs.rs/anyhow/latest/anyhow/) · [thiserror](https://docs.rs/thiserror/latest/thiserror/) · [color-eyre](https://docs.rs/color-eyre/latest/color_eyre/) · [miette](https://docs.rs/miette/latest/miette/)

---

## 4. Parallel Hashing & Rust Concurrency

**Current choice:** Sequential hashing, with `rayon` as a stretch goal.

This is the deepest decision space in the project. Rust's concurrency model is fundamentally different from languages like Go or Python, so this section covers the underlying concepts before comparing approaches.

### How Concurrency Works in Rust

#### Ownership & Send/Sync — The Foundation

Rust prevents data races at **compile time** through two marker traits:

- **`Send`** — A type is `Send` if it can be safely **moved** to another thread. Almost everything in Rust is `Send` (exceptions: `Rc<T>`, raw pointers).
- **`Sync`** — A type is `Sync` if it can be safely **shared** (via `&T` reference) across threads. A type is `Sync` if `&T` is `Send`.

These traits are automatically implemented by the compiler. You never write `impl Send for MyStruct` — the compiler checks if all fields are `Send` and auto-derives it. If you try to send a non-`Send` type across a thread boundary, you get a **compile error**, not a runtime race condition.

```rust
use std::rc::Rc;
use std::sync::Arc;

let rc = Rc::new(42);
// std::thread::spawn(move || println!("{}", rc));
// ^ ERROR: `Rc<i32>` is not `Send`
// Rc uses non-atomic reference counting, so it's unsafe to share across threads.

let arc = Arc::new(42);
std::thread::spawn(move || println!("{}", arc));
// ^ OK: `Arc<i32>` IS `Send` because it uses atomic reference counting.
```

#### Borrowing Rules Prevent Data Races

Rust's borrow checker enforces at compile time:
- **Multiple readers OR one writer** — never both simultaneously.
- **No dangling references** — data cannot be freed while references exist.

This means you literally **cannot write** the classic data race pattern (two threads writing to the same `Vec` without a lock) — the compiler rejects it. In Go or C++, this compiles fine and blows up at runtime.

```rust
let mut data = vec![1, 2, 3];

// This will NOT compile:
// std::thread::spawn(|| data.push(4));  // tries to mutably borrow `data`
// println!("{:?}", data);               // tries to immutably borrow `data`
// ^ ERROR: cannot borrow `data` as immutable because it is also borrowed as mutable

// To share mutable data across threads, you MUST use a synchronization primitive:
use std::sync::{Arc, Mutex};

let data = Arc::new(Mutex::new(vec![1, 2, 3]));
let data_clone = Arc::clone(&data);

std::thread::spawn(move || {
    let mut locked = data_clone.lock().unwrap();
    locked.push(4);
});
```

#### Concurrency vs. Parallelism

These are often confused but are distinct concepts:

- **Concurrency** — Multiple tasks make progress by interleaving execution. A single CPU core can run concurrent tasks by switching between them. This is what `async/await` and `tokio` provide.
- **Parallelism** — Multiple tasks execute **simultaneously** on different CPU cores. This is what OS threads and `rayon` provide.

For **file hashing**, the bottleneck is either:
1. **CPU** (computing the hash) → Parallelism helps (use more cores).
2. **Disk I/O** (reading the file) → Concurrency helps (read multiple files while waiting for I/O), but too many parallel reads on a spinning HDD can actually hurt due to seek contention. SSDs handle parallel reads well.

### Approach 1: Rayon — Data Parallelism (Recommended)

📖 [docs.rs](https://docs.rs/rayon/latest/rayon/) · [GitHub](https://github.com/rayon-rs/rayon)

Rayon implements **work-stealing parallelism**. It maintains a thread pool (default: one thread per CPU core) and distributes iterator work items across those threads. If one thread finishes early, it "steals" work from another thread's queue.

**How it works internally:**
1. You call `.par_iter()` on a slice/vec.
2. Rayon recursively splits the data in half (like merge sort).
3. Each half is assigned to a thread pool worker.
4. Workers process items and steal from each other when idle.
5. Results are collected in parallel using the `FromParallelIterator` trait.

**Why it's good for file hashing:**
- Each file is an independent unit of work — no shared mutable state needed.
- The work is embarrassingly parallel — no inter-task dependencies.
- Rayon's `.par_iter()` is a one-line change from `.iter()`.

```rust
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;

fn hash_all_files(files: &[PathBuf]) -> HashMap<PathBuf, String> {
    files
        .par_iter()                              // parallel iterator
        .filter_map(|path| {
            match hash_file(path) {              // each hash runs on a pool thread
                Ok(hash) => Some((path.clone(), hash)),
                Err(e) => {
                    eprintln!("Skipping {}: {}", path.display(), e);
                    None
                }
            }
        })
        .collect()                               // collected in parallel
}
```

**Controlling the thread pool:**
```rust
use rayon::ThreadPoolBuilder;

// Limit to 4 threads (useful to avoid overwhelming disk I/O)
let pool = ThreadPoolBuilder::new()
    .num_threads(4)
    .build()
    .unwrap();

pool.install(|| {
    // All par_iter() calls inside here use this 4-thread pool
    let results = hash_all_files(&files);
});
```

**Rayon also provides `par_bridge()`** for adapting non-parallel iterators (like `walkdir`):
```rust
use rayon::iter::ParallelBridge;

let results: HashMap<PathBuf, String> = WalkDir::new(dir)
    .into_iter()
    .filter_map(|e| e.ok())
    .filter(|e| e.file_type().is_file())
    .par_bridge()                // bridge sequential iterator into rayon
    .filter_map(|entry| {
        let path = entry.into_path();
        hash_file(&path).ok().map(|h| (path, h))
    })
    .collect();
```

### Approach 2: std::thread — Manual OS Threads

📖 [std::thread](https://doc.rust-lang.org/std/thread/index.html)

The standard library provides direct OS thread spawning. This gives full control but requires manual work distribution and result collection.

```rust
use std::thread;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::path::PathBuf;

fn hash_all_threaded(files: Vec<PathBuf>, num_threads: usize) -> HashMap<PathBuf, String> {
    // Split work into chunks, one per thread
    let chunks: Vec<Vec<PathBuf>> = files
        .chunks(files.len().div_ceil(num_threads))
        .map(|c| c.to_vec())
        .collect();

    let results = Arc::new(Mutex::new(HashMap::new()));

    let handles: Vec<_> = chunks
        .into_iter()
        .map(|chunk| {
            let results = Arc::clone(&results);
            thread::spawn(move || {
                let mut local: HashMap<PathBuf, String> = HashMap::new();
                for path in chunk {
                    if let Ok(hash) = hash_file(&path) {
                        local.insert(path, hash);
                    }
                }
                // Merge local results into shared map
                results.lock().unwrap().extend(local);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    Arc::try_unwrap(results).unwrap().into_inner().unwrap()
}
```

**Key concepts demonstrated:**
- `Arc<T>` — Atomically Reference Counted pointer. Like `Rc<T>` but thread-safe. Each thread gets a clone of the `Arc`, and the inner data lives until the last `Arc` is dropped.
- `Mutex<T>` — Mutual exclusion lock. Only one thread can access the inner `T` at a time. `.lock()` returns a `MutexGuard` that auto-unlocks when dropped.
- `thread::spawn(move || ...)` — The `move` keyword transfers ownership of captured variables into the closure, which is required because the thread may outlive the calling scope.

**Downsides vs. Rayon:**
- Manual chunk splitting (uneven file sizes → load imbalance).
- No work stealing — if one thread gets all the large files, others sit idle.
- More boilerplate for `Arc`/`Mutex`.
- Thread spawn/join overhead for small workloads.

### Approach 3: Crossbeam — Scoped Threads

📖 [crossbeam docs.rs](https://docs.rs/crossbeam/latest/crossbeam/)

Crossbeam provides **scoped threads** — threads that can borrow from the parent stack frame without `Arc`. The scope guarantees all threads are joined before the scope exits, so the borrow checker can prove the references are valid.

```rust
use crossbeam::thread;
use std::collections::HashMap;
use std::path::PathBuf;

fn hash_all_scoped(files: &[PathBuf]) -> HashMap<PathBuf, String> {
    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let chunks: Vec<&[PathBuf]> = files.chunks(files.len().div_ceil(num_threads)).collect();
    let mut all_results = HashMap::new();

    thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                s.spawn(move |_| {
                    let mut local = HashMap::new();
                    for path in chunk {
                        if let Ok(hash) = hash_file(path) {
                            local.insert(path.clone(), hash);
                        }
                    }
                    local
                })
            })
            .collect();

        for handle in handles {
            all_results.extend(handle.join().unwrap());
        }
    })
    .unwrap();

    all_results
}
```

> **Note:** As of Rust 1.63, `std::thread::scope` was stabilized in the standard library, so crossbeam is no longer strictly necessary for scoped threads. The std version works the same way:
> ```rust
> std::thread::scope(|s| {
>     s.spawn(|| { /* can borrow from parent */ });
> });
> ```

**Why crossbeam still matters:** It provides additional concurrency primitives like lock-free queues (`crossbeam::channel`), epoch-based memory reclamation, and `crossbeam::deque` (the work-stealing deque that Rayon is built on top of).

### Approach 4: Tokio — Async I/O

📖 [tokio.rs](https://tokio.rs/) · [docs.rs](https://docs.rs/tokio/latest/tokio/)

Tokio is an **async runtime** — it uses cooperative multitasking (not OS threads) to run many tasks on a small thread pool. Tasks yield at `.await` points, allowing other tasks to make progress.

Async is ideal when the bottleneck is **I/O waiting** (network, slow disk), not CPU. For CPU-bound hashing, tokio is overkill — but it's worth understanding because the study guide uses it extensively in Phase 2.

```rust
use tokio::fs;
use tokio::task;
use std::collections::HashMap;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let files: Vec<PathBuf> = discover_files(Path::new("./src"));

    let mut handles = Vec::new();
    for path in files {
        // spawn_blocking moves CPU-bound work OFF the async executor
        let handle = task::spawn_blocking(move || {
            hash_file(&path).map(|h| (path, h))
        });
        handles.push(handle);
    }

    let mut results = HashMap::new();
    for handle in handles {
        if let Ok(Ok((path, hash))) = handle.await {
            results.insert(path, hash);
        }
    }

    Ok(())
}
```

**Important:** `spawn_blocking` is critical here. If you run CPU-heavy work inside a regular `tokio::spawn` task, you **starve** the async executor — other tasks can't make progress because the CPU-heavy task never yields. `spawn_blocking` runs the work on a separate thread pool dedicated to blocking operations.

**When this matters:** If the CLI hash cache ever evolves to fetch files over the network (e.g., comparing local hashes against a remote API), tokio becomes the right tool.

### Comparison Summary

| Approach | Crate | Complexity | Work Stealing | Best For |
|----------|-------|:----------:|:-------------:|----------|
| **Sequential** | (none) | Trivial | N/A | Small directories, simplicity |
| **Rayon** | `rayon` | Low (1-line change) | ✓ | CPU-bound parallel work on data collections |
| **std::thread** | (std) | Medium | ✗ | Full control, learning exercise |
| **Scoped threads** | `crossbeam` or std | Medium | ✗ | Borrow from parent scope without Arc |
| **Tokio** | `tokio` | High | ✓ (async) | I/O-bound or mixed I/O + CPU workloads |

### Performance Expectations

For a directory of ~10,000 mixed files (code, images, binaries) on an SSD:

| Approach | Relative Speed | Notes |
|----------|:--------------:|-------|
| Sequential | 1× | Baseline |
| Rayon (default threads) | 4–8× | Scales with CPU cores |
| Rayon (4 threads, HDD) | 1–2× | Disk seek is the bottleneck, not CPU |
| std::thread (naive chunks) | 3–6× | No work stealing → load imbalance |
| Tokio spawn_blocking | 4–8× | Same CPU parallelism, more overhead |

The **biggest variable** is disk type. On an NVMe SSD, parallelism scales well. On a spinning HDD, parallel reads can cause head thrashing and actually slow things down. Rayon's `ThreadPoolBuilder::num_threads(2)` can help here.

### Recommendation

**Start sequential, then add `rayon` as a stretch goal.** It's a one-line change (`.iter()` → `.par_iter()`) and teaches the most important lesson: Rust's ownership system means you get parallelism _for free_ when your data is independent — no locks, no races, no bugs.

---

## 5. Directory Walking

**Current choice:** `walkdir` crate.

### Alternatives

| Approach | Crate | Recursive | Parallel | Notes |
|----------|-------|:---------:|:--------:|-------|
| **walkdir** | `walkdir` | ✓ | ✗ | De facto standard. Handles symlinks, errors, depth limits. |
| **std::fs::read_dir** | (std) | Manual | ✗ | Only lists one directory level. You write your own recursion. Good for learning. |
| **ignore** | `ignore` | ✓ | ✓ | By the same author as walkdir. Respects `.gitignore` rules and can walk in parallel. Used by `ripgrep`. |
| **jwalk** | `jwalk` | ✓ | ✓ | Parallel directory walker. Faster than walkdir on large trees with many subdirectories. |

### Code: `std::fs::read_dir` (manual recursion)

```rust
use std::path::{Path, PathBuf};

fn walk_dir(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, files)?;     // recurse
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}
```

### Code: `ignore` (gitignore-aware)

```rust
// Cargo.toml: ignore = "0.4"
use ignore::WalkBuilder;

let files: Vec<PathBuf> = WalkBuilder::new(dir)
    .hidden(false)          // include hidden files
    .git_ignore(true)       // respect .gitignore
    .build()
    .filter_map(|e| e.ok())
    .filter(|e| e.file_type().map_or(false, |ft| ft.is_file()))
    .map(|e| e.into_path())
    .collect();
```

### Decision Guidance

- **General use?** → `walkdir` (simple, battle-tested).
- **Want to skip `.git/`, `node_modules/`, etc.?** → `ignore`.
- **Millions of files, directory traversal itself is slow?** → `jwalk`.
- **Learning exercise?** → `std::fs::read_dir` with manual recursion.

📖 [walkdir](https://docs.rs/walkdir/latest/walkdir/) · [ignore](https://docs.rs/ignore/latest/ignore/) · [jwalk](https://docs.rs/jwalk/latest/jwalk/)

---

## 6. CLI Argument Parsing

**Current choice:** `clap` with derive macros.

### Alternatives

| Approach | Crate | Style | Binary Size Impact | Notes |
|----------|-------|-------|--------------------|-------|
| **clap (derive)** | `clap` | Declarative struct | +300–600 KB | Most popular. Rich features: subcommands, completions, env vars |
| **clap (builder)** | `clap` | Imperative builder API | +300–600 KB | Same crate, runtime-constructed instead of derive |
| **std::env::args** | (std) | Manual parsing | None | Full control, no dependency. Tedious for anything beyond trivial args |
| **pico-args** | `pico-args` | Minimal API | +10 KB | Tiny, zero-dependency. Good for simple CLIs |
| **lexopt** | `lexopt` | Manual match | +10 KB | Low-level, zero-copy. No usage/help generation |
| **bpaf** | `bpaf` | Combinator + derive | +100 KB | Haskell-style, strong composition |

### Code: `std::env::args` (no dependency)

```rust
let args: Vec<String> = std::env::args().collect();

let directory = args.get(1).expect("Usage: hashcache <DIRECTORY>");
let json_mode = args.iter().any(|a| a == "--json");
```

### Decision Guidance

- **Full-featured CLI?** → `clap` derive.
- **Minimal binary, simple args?** → `pico-args` or `std::env::args`.
- **Learning?** → Start with `std::env::args` to understand the problem, then switch to `clap`.

📖 [clap](https://docs.rs/clap/latest/clap/) · [pico-args](https://docs.rs/pico-args/latest/pico_args/) · [lexopt](https://docs.rs/lexopt/latest/lexopt/)

---

## 7. Buffer Size for File Reading

**Current choice:** 8 KB (8192 bytes).

This is the size of the buffer used when reading files in chunks for hashing.

| Buffer Size | Pros | Cons |
|-------------|------|------|
| **4 KB** | Matches OS page size on most systems | More syscalls per file |
| **8 KB** | Good default, matches `BufReader` default | — |
| **64 KB** | Fewer syscalls, better for large files | More stack memory per hash operation |
| **1 MB** | Minimal syscall overhead | Wastes memory on small files; problematic with parallel hashing (N threads × 1 MB) |

For parallel hashing, keep in mind that each thread gets its own buffer. With rayon (default = 1 thread per core) on a 10-core machine with a 64 KB buffer, that's only 640 KB total — negligible.

### Decision Guidance

- **Default?** → 8 KB is fine.
- **Large files (videos, disk images)?** → Bump to 64 KB.
- **Want to be thorough?** → Use `criterion` to benchmark different sizes on your actual workload.

---

## 8. Cache Key Design

**Current choice:** `PathBuf` (the file's full path relative to the scanned directory).

### Alternatives

| Key | Pros | Cons |
|-----|------|------|
| **Relative `PathBuf`** | Human-readable, stable across runs | Breaks if the directory is moved. Platform-dependent path separators (`/` vs `\`) |
| **Canonicalized (absolute) path** | Resolves symlinks, unambiguous | Changes if the directory is moved to a different location |
| **String (UTF-8 normalized)** | Cross-platform safe serialization | Lossy on non-UTF-8 filenames (rare but possible on Unix) |

### Consideration: Relative vs. Absolute

The safest approach is to store paths **relative to the scanned directory** and canonicalize at scan time:

```rust
let base = std::fs::canonicalize(&args.directory)?;

for file in discover_files(&base) {
    let relative = file.strip_prefix(&base)?;
    cache.insert(relative.to_path_buf(), hash);
}
```

This way the cache survives the directory being moved (because the keys are relative like `src/main.rs`, not `/Users/you/old/path/src/main.rs`).

---

## 9. Atomic Writes

**Current choice:** Write to a `.tmp` file, then `std::fs::rename`.

This prevents a half-written cache file if the process is killed mid-write.

### Alternatives

| Approach | Crate | Safety | Notes |
|----------|-------|--------|-------|
| **Direct write** | (std) | ✗ | Simple but risks corruption on crash |
| **Temp + rename** | (std) | ✓ | Current plan. `rename` is atomic on most filesystems |
| **tempfile + persist** | `tempfile` | ✓ | Creates a secure temp file in the same directory, then atomically renames |
| **fslock** | `fslock` | ✓ | File-based lock to prevent concurrent cache writes |

### Code: `tempfile` (alternative)

```rust
// Cargo.toml: tempfile = "3"
use tempfile::NamedTempFile;

fn save_atomic(cache: &HashCache, final_path: &Path) -> anyhow::Result<()> {
    let dir = final_path.parent().unwrap_or(Path::new("."));
    let mut tmp = NamedTempFile::new_in(dir)?;  // same dir = same filesystem = rename works
    let encoded = bincode::serialize(cache)?;
    std::io::Write::write_all(&mut tmp, &encoded)?;
    tmp.persist(final_path)?;                   // atomic rename
    Ok(())
}
```

📖 [tempfile](https://docs.rs/tempfile/latest/tempfile/)
