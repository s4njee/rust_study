# 1.1 — CLI Hash Cache: Implementation Plan

A Rust CLI tool that walks a directory, computes a SHA-256 hash for every file, persists the hash map to disk, and on subsequent runs reports which files have changed, been added, or removed.

---

## High-Level Steps

### Step 1 — Project Scaffolding

- Run `cargo init` to create a new binary crate
- Set up `Cargo.toml` with all dependencies (see [Crate Reference](#external-crates) below)
- Create an initial `main.rs` with a skeleton `fn main() -> anyhow::Result<()>`

### Step 2 — CLI Argument Parsing

- Define a `clap` struct to accept:
  - **`<DIRECTORY>`** — positional arg, the directory to scan
  - **`--cache-file`** / **`-c`** — optional path for the persisted hash map (default: `.hash_cache.bin`)
  - **`--json`** — flag to output the cache as JSON instead of bincode (useful for debugging)
- Parse args at the top of `main()`

### Step 3 — Directory Walking & File Discovery

- Use `walkdir` to recursively traverse the target directory
- Filter entries to files only (skip directories, symlinks)
- Collect file paths into a `Vec<PathBuf>`
- Handle permission errors gracefully (log a warning and skip)

### Step 4 — SHA-256 Hashing

- For each file path, open the file with `std::fs::File`
- Read the file in buffered chunks (8 KB) using `std::io::BufReader`
- Feed each chunk into a `sha2::Sha256` hasher
- Finalize and convert the digest to a hex string
- Store results in a `HashMap<PathBuf, String>` (path → hex hash)

### Step 5 — Load Previous Cache & Diff

- On startup, attempt to load the previous hash map from the cache file
  - If the file doesn't exist, treat everything as "new"
  - Deserialize with `bincode` (or `serde_json` if `--json` flag was used at save time)
- Compare old vs. new hash maps:
  - **Added** — key exists in new but not old
  - **Removed** — key exists in old but not new
  - **Changed** — key exists in both but hash differs
  - **Unchanged** — key exists in both with same hash
- Print a summary report to stdout

### Step 6 — Persist Updated Cache

- Serialize the new `HashMap<PathBuf, String>` to disk using `bincode` (or JSON)
- Write atomically: write to a `.tmp` file first, then `std::fs::rename` to the final path

### Step 7 — Error Handling & Polish

- Use `anyhow::Context` to add context to every fallible operation
- Return meaningful exit codes (0 = no changes, 1 = changes detected, 2 = error)
- Add `--quiet` flag to suppress unchanged file listings

### Step 8 (Stretch) — Parallel Hashing with Rayon

- Replace the sequential hash loop with `rayon::par_iter()`
- Collect results into a concurrent-safe structure
- Benchmark sequential vs. parallel on a large directory

---

## Standard Library Usage

These modules are part of `std` and require **no external dependencies**.

### `std::fs`

📖 [Reference](https://doc.rust-lang.org/std/fs/index.html)

File system operations — reading files, checking metadata, renaming.

```rust
use std::fs::File;
use std::io::{BufReader, Read};

let file = File::open(&path)?;
let mut reader = BufReader::new(file);
let mut buffer = [0u8; 8192];

loop {
    let bytes_read = reader.read(&mut buffer)?;
    if bytes_read == 0 {
        break;
    }
    // feed buffer[..bytes_read] to hasher
}
```

### `std::collections::HashMap`

📖 [Reference](https://doc.rust-lang.org/std/collections/struct.HashMap.html)

The core data structure mapping file paths to their SHA-256 hex digests.

```rust
use std::collections::HashMap;
use std::path::PathBuf;

let mut cache: HashMap<PathBuf, String> = HashMap::new();
cache.insert(PathBuf::from("src/main.rs"), "a1b2c3...".to_string());

// Check if a key exists
if let Some(old_hash) = cache.get(&path) {
    // compare
}
```

### `std::path::PathBuf` / `std::path::Path`

📖 [Path](https://doc.rust-lang.org/std/path/struct.Path.html) · [PathBuf](https://doc.rust-lang.org/std/path/struct.PathBuf.html)

Used for all file path manipulation — joining, comparing, displaying.

```rust
use std::path::{Path, PathBuf};

let base = Path::new("/home/user/project");
let cache_path = base.join(".hash_cache.bin");
```

### `std::io::BufReader` / `std::io::Read`

📖 [BufReader](https://doc.rust-lang.org/std/io/struct.BufReader.html) · [Read](https://doc.rust-lang.org/std/io/trait.Read.html)

Buffered reading for efficient file hashing (avoids loading entire files into memory).

```rust
use std::io::{BufReader, Read};

let file = std::fs::File::open("large_file.bin")?;
let mut reader = BufReader::new(file);
let mut buf = [0u8; 8192];

while reader.read(&mut buf)? > 0 {
    // process chunk
}
```

---

## External Crates

### `clap` — CLI Argument Parsing

📖 [docs.rs](https://docs.rs/clap/latest/clap/) · [GitHub](https://github.com/clap-rs/clap) · [Derive Tutorial](https://docs.rs/clap/latest/clap/_derive/_tutorial/index.html)

**What it does:** Declaratively defines CLI arguments, flags, and subcommands using a derive macro. Generates `--help` and `--version` automatically.

**Cargo.toml:**
```toml
clap = { version = "4", features = ["derive"] }
```

**Usage:**
```rust
use clap::Parser;

/// A CLI tool that hashes files in a directory and detects changes.
#[derive(Parser, Debug)]
#[command(name = "hashcache", version, about)]
struct Args {
    /// Directory to scan
    directory: std::path::PathBuf,

    /// Path to the cache file
    #[arg(short, long, default_value = ".hash_cache.bin")]
    cache_file: std::path::PathBuf,

    /// Output cache as JSON instead of bincode
    #[arg(long)]
    json: bool,

    /// Suppress listing of unchanged files
    #[arg(short, long)]
    quiet: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    println!("Scanning: {:?}", args.directory);
    Ok(())
}
```

**How clap derive attributes map to CLI behavior:**

A **bare field** with no `#[arg(...)]` attribute becomes a **positional argument** — the user passes it by position, not by name. If the field type is not `Option<T>`, it's required:

```rust
/// Directory to scan
directory: std::path::PathBuf,
//  ↳ usage: hashcache ./src
//                      ^^^^^  positional, required
```

Adding `#[arg(long)]` turns the field into a **named flag**. Clap derives the flag name from the field name, converting underscores to hyphens (`cache_file` → `--cache-file`):

```rust
#[arg(long)]
json: bool,
//  ↳ usage: hashcache ./src --json
//    A bool field is a toggle — present means true, absent means false.
```

Adding `short` alongside `long` creates a **single-letter shorthand**. Clap uses the first letter of the field name by default (`cache_file` → `-c`, `quiet` → `-q`):

```rust
#[arg(short, long)]
quiet: bool,
//  ↳ usage: hashcache ./src -q
//           hashcache ./src --quiet
//           both are equivalent
```

`default_value` makes a named argument **optional** — if the user doesn't pass it, clap uses the default. The field type determines what value the flag expects (a `PathBuf` expects a path string, a `bool` expects nothing):

```rust
#[arg(short, long, default_value = ".hash_cache.bin")]
cache_file: std::path::PathBuf,
//  ↳ usage: hashcache ./src                         (uses .hash_cache.bin)
//           hashcache ./src -c my_hashes.bin         (overrides with short flag)
//           hashcache ./src --cache-file custom.bin  (overrides with long flag)
```

The `///` doc comments above each field become the **help text** shown by `hashcache --help`.

---

### `sha2` — SHA-256 Hashing

📖 [docs.rs](https://docs.rs/sha2/latest/sha2/) · [GitHub](https://github.com/RustCrypto/hashes)

**What it does:** Provides pure-Rust implementations of SHA-2 family hash functions. Uses the `Digest` trait from the `digest` crate for a consistent API.

**Cargo.toml:**
```toml
sha2 = "0.10"
```

**Usage:**
```rust
use sha2::{Sha256, Digest};
use std::io::Read;

fn hash_file(path: &std::path::Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}
```

---

### `walkdir` — Recursive Directory Traversal

📖 [docs.rs](https://docs.rs/walkdir/latest/walkdir/) · [GitHub](https://github.com/BurntSushi/walkdir)

**What it does:** Recursively walks a directory tree, yielding each entry. Handles symlinks, permission errors, and max-depth configuration.

**Cargo.toml:**
```toml
walkdir = "2"
```

**Usage:**
```rust
use walkdir::WalkDir;
use std::path::PathBuf;

fn discover_files(dir: &std::path::Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| {
            match entry {
                Ok(e) if e.file_type().is_file() => Some(e.into_path()),
                Ok(_) => None, // skip directories
                Err(err) => {
                    eprintln!("Warning: {}", err);
                    None
                }
            }
        })
        .collect()
}
```

---

### `serde` + `bincode` — Serialization & Persistence

📖 [serde docs.rs](https://docs.rs/serde/latest/serde/) · [serde.rs Guide](https://serde.rs/) · [bincode docs.rs](https://docs.rs/bincode/latest/bincode/) · [serde_json docs.rs](https://docs.rs/serde_json/latest/serde_json/)

**What it does:** `serde` is the standard serialization framework. `bincode` provides a compact, fast binary encoding — ideal for a cache file that doesn't need to be human-readable.

**Cargo.toml:**
```toml
serde = { version = "1", features = ["derive"] }
bincode = "1"
serde_json = "1"   # optional, for --json mode
```

**Usage:**
```rust
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct HashCache {
    entries: HashMap<PathBuf, String>,
}

// Save to disk (bincode)
fn save_cache(cache: &HashCache, path: &std::path::Path) -> anyhow::Result<()> {
    let encoded = bincode::serialize(cache)?;
    std::fs::write(path, encoded)?;
    Ok(())
}

// Load from disk (bincode)
fn load_cache(path: &std::path::Path) -> anyhow::Result<HashCache> {
    let data = std::fs::read(path)?;
    let cache: HashCache = bincode::deserialize(&data)?;
    Ok(cache)
}

// Save as JSON (for debugging)
fn save_cache_json(cache: &HashCache, path: &std::path::Path) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(cache)?;
    std::fs::write(path, json)?;
    Ok(())
}
```

---

### `anyhow` — Error Handling

📖 [docs.rs](https://docs.rs/anyhow/latest/anyhow/) · [GitHub](https://github.com/dtolnay/anyhow) · [Context trait](https://docs.rs/anyhow/latest/anyhow/trait.Context.html)

**What it does:** Provides a flexible error type (`anyhow::Error`) that can wrap any error implementing `std::error::Error`. The key advantage over raw `std::io::Error` or custom enums is that you get error chaining and context annotations for free, without defining error types for every function.

**Cargo.toml:**
```toml
anyhow = "1"
```

#### The `?` operator with `anyhow::Result`

`anyhow::Result<T>` is just an alias for `Result<T, anyhow::Error>`. The `?` operator automatically converts any `std::error::Error` into `anyhow::Error`:

```rust
use anyhow::Result;

// ? converts std::io::Error → anyhow::Error automatically
fn read_cache(path: &std::path::Path) -> Result<Vec<u8>> {
    let data = std::fs::read(path)?;  // io::Error becomes anyhow::Error
    Ok(data)
}
```

Without `anyhow`, you'd need to either define your own error enum or use `Box<dyn Error>`. Anyhow handles this boilerplate.

#### Adding context with `.context()` and `.with_context()`

Raw errors like "No such file or directory" don't tell you *which* file failed. `.context()` wraps the error with a human-readable message:

```rust
use anyhow::{Context, Result};

fn hash_file(path: &std::path::Path) -> Result<String> {
    // .context() adds a static string
    let data = std::fs::read(path)
        .context("failed to read file for hashing")?;

    // .with_context() takes a closure — use when you need to format a message
    // (avoids the format!() cost on the success path)
    let data = std::fs::read(path)
        .with_context(|| format!("failed to read '{}'", path.display()))?;

    Ok("abc123".to_string())
}
```

The difference: `.context("static string")` always allocates the message. `.with_context(|| ...)` only runs the closure when the error actually happens, which is slightly more efficient when the message includes `format!()`.

#### How error chains display

When an error propagates through multiple `.context()` calls, anyhow builds a **chain**. The display format depends on whether you use `{}` or `{:#}`:

```rust
fn load_and_verify(dir: &std::path::Path) -> Result<()> {
    let cache_path = dir.join(".hash_cache.bin");
    let data = std::fs::read(&cache_path)
        .with_context(|| format!("failed to read cache from '{}'", cache_path.display()))?;
    let _cache: Vec<u8> = bincode::deserialize(&data)
        .context("cache file is corrupted or was written by an incompatible version")?;
    Ok(())
}

fn main() {
    if let Err(err) = load_and_verify(std::path::Path::new("./project")) {
        // Display format — just the outermost message:
        eprintln!("Error: {err}");
        // → Error: cache file is corrupted or was written by an incompatible version

        // Alternate display — the full chain, separated by ": "
        eprintln!("Error: {err:#}");
        // → Error: cache file is corrupted or was written by an incompatible version: \
        //   failed to read cache from './project/.hash_cache.bin': \
        //   No such file or directory (os error 2)

        // Debug format — the full chain plus a backtrace (if RUST_BACKTRACE=1)
        eprintln!("Error: {err:?}");
    }
}
```

The `{:#}` (alternate display) format is the most useful for CLI tools — it shows the full chain in one line. The `{:?}` (debug) format is better for development because it includes backtraces.

#### Creating errors directly with `bail!` and `anyhow!`

When you need to *create* an error rather than *wrap* one, use `bail!` (which returns early) or `anyhow!` (which creates the error value):

```rust
use anyhow::{bail, anyhow, Result};

fn validate_directory(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        // bail! is shorthand for: return Err(anyhow!(...))
        bail!("directory '{}' does not exist", path.display());
    }

    if !path.is_dir() {
        // anyhow! creates the error without returning — useful in match arms
        return Err(anyhow!("'{}' is a file, not a directory", path.display()));
    }

    Ok(())
}
```

#### Using anyhow in `main()`

When `main()` returns `anyhow::Result<()>`, Rust prints the error with `Debug` formatting on failure. This gives you the full chain + backtrace:

```rust
fn main() -> anyhow::Result<()> {
    let hash = hash_file(std::path::Path::new("test.txt"))?;
    println!("Hash: {hash}");
    Ok(())
}

// If test.txt doesn't exist, the program prints:
//   Error: Failed to open file: test.txt
//
//   Caused by:
//       No such file or directory (os error 2)
```

If you want more control over the output format, use `ExitCode` instead (see 1.2) and format the error yourself with `{:#}`.

---

### `rayon` — Parallel Iteration (Stretch Goal)

📖 [docs.rs](https://docs.rs/rayon/latest/rayon/) · [GitHub](https://github.com/rayon-rs/rayon)

**What it does:** Drop-in data parallelism for iterators. Converts `.iter()` to `.par_iter()` and automatically distributes work across CPU cores using a work-stealing thread pool.

**Cargo.toml:**
```toml
rayon = "1"
```

**Usage:**
```rust
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;

fn hash_files_parallel(files: &[PathBuf]) -> HashMap<PathBuf, String> {
    files
        .par_iter()
        .filter_map(|path| {
            match hash_file(path) {
                Ok(hash) => Some((path.clone(), hash)),
                Err(err) => {
                    eprintln!("Error hashing {}: {}", path.display(), err);
                    None
                }
            }
        })
        .collect()
}
```

---

## Cargo.toml Summary

```toml
[package]
name = "hashcache"
version = "0.1.0"
edition = "2021"

[dependencies]
clap = { version = "4", features = ["derive"] }
sha2 = "0.10"
walkdir = "2"
serde = { version = "1", features = ["derive"] }
bincode = "1"
serde_json = "1"
anyhow = "1"

# Stretch goal
# rayon = "1"
```

---

## Expected Output Example

```
$ hashcache ./src

Scanning: ./src
Cache loaded from: .hash_cache.bin (12 entries)

  [+] ADDED     src/new_module.rs
  [~] CHANGED   src/main.rs
  [-] REMOVED   src/old_file.rs
  [ ] UNCHANGED src/lib.rs
  [ ] UNCHANGED src/utils.rs
  ... (8 more unchanged)

Summary: 1 added, 1 changed, 1 removed, 10 unchanged
Cache saved to: .hash_cache.bin (12 entries)
```
