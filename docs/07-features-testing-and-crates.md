# Features, Testing, And Crates

This doc covers optional compilation, testing patterns, and the crate cheat
sheet for this repo.

## Feature Flags

In plain English: feature flags let one crate support optional capabilities
without forcing every build to include every dependency. They are compile-time
switches that include or exclude code and dependencies.

Example from the thumbnail generator's `Cargo.toml`:

```toml
[features]
rayon = ["dep:rayon"]
tokio = ["dep:tokio"]

[dependencies]
rayon = { version = "1", optional = true }
tokio = { version = "1", features = ["rt-multi-thread", "sync"], optional = true }
```

That means:

- the base crate stays **simpler** — no rayon or tokio by default
- parallel backends can be turned on **when needed**: `cargo build --features rayon`
- learners can see how optional dependencies work in practice
- the default binary is **smaller** because unused code is not compiled

**How features propagate:** When you enable a feature, Cargo also enables any
features it depends on. The `dep:rayon` syntax means "enable the `rayon`
dependency."

## Conditional Compilation

In plain English: `#[cfg(...)]` is Rust's way of saying "only compile this code
when a certain condition is true." This is **compile-time** branching, not
runtime branching — the code is literally absent from the binary when the
condition is false.

```rust
// Only compiled when the "rayon" feature is enabled
#[cfg(feature = "rayon")]
use rayon::prelude::*;

// Only compiled when the "tokio" feature is enabled
#[cfg(feature = "tokio")]
use tokio::sync::Semaphore;
```

This applies to entire functions too:

```rust
#[cfg(feature = "rayon")]
pub fn par_batch<T, U, E, F>(items: Vec<T>, f: F) -> Vec<Result<U, E>>
where
    T: Send,
    U: Send,
    E: Send,
    F: Fn(T) -> Result<U, E> + Sync + Send,
{
    items.into_par_iter().map(f).collect()
}
```

When `rayon` is not enabled, this entire function does not exist in the compiled
binary. Any code that tries to call it will also need a `#[cfg(feature =
"rayon")]` guard.

**Common `#[cfg]` conditions:**

| Condition | What it checks |
|---|---|
| `#[cfg(feature = "rayon")]` | Is the `rayon` feature enabled? |
| `#[cfg(test)]` | Are we compiling for `cargo test`? |
| `#[cfg(target_os = "windows")]` | Are we compiling for Windows? |
| `#[cfg(not(feature = "rayon"))]` | Is the `rayon` feature NOT enabled? |

## Helpful Runtime Errors Around Missing Features

In plain English: even when a feature is disabled at compile time, the CLI can
still explain what the user should do. This turns confusion into a direct next
step.

The thumbnail generator uses complementary `#[cfg]` pairs:

```rust
fn run_batch(/* ... */, parallel: Parallel) -> Result<()> {
    match parallel {
        Parallel::Sequential => run_batch_sequential(/* ... */),

        Parallel::Rayon => {
            // When rayon IS enabled: call the rayon implementation
            #[cfg(feature = "rayon")]
            return run_batch_rayon(files, config, format_override, quiet);

            // When rayon is NOT enabled: tell the user how to enable it
            #[cfg(not(feature = "rayon"))]
            bail!(
                "--parallel rayon requires the rayon feature; \
                 rebuild with: cargo build --features rayon"
            );
        }

        Parallel::Tokio => {
            #[cfg(feature = "tokio")]
            return run_batch_tokio(files, config, format_override, concurrency, quiet);

            #[cfg(not(feature = "tokio"))]
            bail!(
                "--parallel tokio requires the tokio feature; \
                 rebuild with: cargo build --features tokio"
            );
        }
    }
}
```

Both `#[cfg]` arms cannot be compiled at the same time, so there is no
dead-code warning. The user always gets either the feature or a clear
explanation of what to do.

This is a good pattern because:

1. `--help` always shows all three `--parallel` options (even when some are
   disabled)
2. The error message includes the exact `cargo build` command to fix it
3. The user does not need to guess why `--parallel rayon` silently does nothing

## Testing Patterns In The Repo

### Unit Tests

In plain English: unit tests sit close to the code they verify. They live in the
same file, in a `tests` module that is only compiled during `cargo test`.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_file_matches_known_sha256_digest() {
        let temp_dir = tempdir().expect("failed to create tempdir");
        let file_path = temp_dir.path().join("sample.txt");
        fs::write(&file_path, "abc").expect("failed to write sample file");

        let digest = hash_file(&file_path).expect("hash_file should succeed");

        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
```

Key details:

- `#[cfg(test)]` means this module is **stripped from production builds**
- `use super::*` imports everything from the parent module — including private
  functions, which is why unit tests can test internal helpers
- `#[test]` marks a function as a test case — `cargo test` discovers and runs it

### Environment Variable Tests

In plain English: tests that modify environment variables can interfere with one
another because env vars are **process-global** state. The repo serializes them
with a `Mutex`.

```rust
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn load_config_from_env() {
    // Acquire the lock — no other env-modifying test can run concurrently
    let _guard = env_lock().lock().unwrap();
    let temp_dir = TempDir::new().unwrap();

    // Setting env vars is unsafe because it is not thread-safe
    unsafe {
        env::set_var("CONGRESSDIR", temp_dir.path());
        env::set_var("POSTGRESURI", "localhost");
        env::set_var("RUN_VOTES", "false");
    }

    let cfg = Config::load().unwrap();
    assert_eq!(cfg.congress_dir, temp_dir.path());
    assert!(!cfg.run_votes);
}
```

Why this matters:

- `cargo test` runs tests in **parallel** by default
- Two tests modifying the same env var simultaneously would cause flaky failures
- The `Mutex` ensures only one env-modifying test runs at a time
- `OnceLock` makes the `Mutex` a global singleton, initialized lazily

The `unsafe` block is required because `env::set_var` is not thread-safe — Rust
forces you to acknowledge this explicitly. In Python you would just write
`os.environ["KEY"] = value` without any special syntax.

### Round-Trip Tests

In plain English: write something, read it back, and confirm it is unchanged.
This is the most important test for any persistence layer.

```rust
#[test]
fn save_and_load_cache_round_trip_in_json() {
    let temp_dir = tempdir().expect("failed to create tempdir");
    let cache_path = temp_dir.path().join(".hash_cache.json");

    let original = StoredCache::new(BTreeMap::from([
        (PathBuf::from("a.txt"), "111".to_string()),
        (PathBuf::from("nested/b.txt"), "222".to_string()),
    ]));

    save_cache(&cache_path, CacheFormat::Json, &original)
        .expect("save_cache should succeed");

    let loaded = load_cache(&cache_path, CacheFormat::Json)
        .expect("load_cache should succeed");

    assert_eq!(loaded, original);
}
```

This test proves **three** things at once:

1. Serialization does not lose or corrupt data
2. Deserialization correctly reconstructs the original structure
3. The `PartialEq` implementation on `StoredCache` works correctly

The same test exists for bincode, ensuring both formats round-trip correctly.

### Known-Value Tests

In plain English: sometimes the simplest correct test is comparing against a
well-known answer. These are also called "golden tests" — you know the expected
output and test that the code produces it exactly.

```rust
#[test]
fn hash_file_matches_known_sha256_digest() {
    let temp_dir = tempdir().expect("failed to create tempdir");
    let file_path = temp_dir.path().join("sample.txt");
    fs::write(&file_path, "abc").expect("failed to write sample file");

    let digest = hash_file(&file_path).expect("hash_file should succeed");

    // The SHA-256 hash of "abc" is well-known and published
    assert_eq!(
        digest,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
```

These tests are great for proving correctness of deterministic algorithms.
If the hash of "abc" ever changes, something is fundamentally broken.

### Security-Focused Tests

In plain English: the markdown sanitizer tests prove that unsafe content is
removed while safe content still works. Security behavior should be
**intentionally tested**, not assumed.

```rust
#[test]
fn strips_script_tags_from_embedded_html() {
    let rendered = render_and_sanitize("before<script>alert('xss')</script>after");

    // The raw HTML should contain the script (markdown trusts it)
    assert!(rendered.raw_html.contains("<script>alert('xss')</script>"));

    // The sanitized HTML should have the script completely removed
    assert_eq!(rendered.sanitized_html, "<p>beforeafter</p>\n");
}

#[test]
fn removes_javascript_urls_from_links() {
    let rendered = render_and_sanitize("[click me](javascript:alert('xss'))");

    // The raw HTML contains the dangerous href
    assert!(rendered.raw_html.contains("href=\"javascript:alert"));

    // The sanitized HTML strips the href but keeps the link text
    assert_eq!(
        rendered.sanitized_html,
        "<p><a rel=\"noopener noreferrer\">click me</a></p>\n"
    );
}
```

These tests verify the contract between the two pipeline stages:

- `raw_html` contains the dangerous content (correct — the markdown parser
  just renders what it sees)
- `sanitized_html` has the dangerous content removed (correct — the sanitizer
  does its job)

Testing both stages independently proves that security does not depend on any
particular markdown parser behavior.

### Integration Tests

In plain English: integration tests exercise a realistic workflow instead of one
small helper. They test the code **as a user would use it**.

From the CLI hash cache:

```rust
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

    // First run should always detect changes (everything is new)
    assert!(outcome.changes_detected);
    // The cache file should have been created
    assert!(temp_dir.path().join(".hash_cache.json").exists());
}
```

From the markdown sanitizer:

```rust
#[test]
fn run_reads_from_file_and_writes_sanitized_output() {
    let input_file = NamedTempFile::new().expect("temp input file");
    let output_file = NamedTempFile::new().expect("temp output file");

    fs::write(
        input_file.path(),
        "[safe?](javascript:alert('xss')) and **world**",
    ).expect("write markdown");

    run(Args {
        input: Some(input_file.path().to_path_buf()),
        output: Some(output_file.path().to_path_buf()),
    }).expect("run sanitizer");

    let saved = fs::read_to_string(output_file.path()).expect("read output");
    assert!(saved.contains("<strong>world</strong>"));
    assert!(!saved.contains("javascript:"));
}
```

This kind of test is especially valuable in CLI projects because:

- it exercises the full pipeline from input to output
- it uses the same `Args` struct that `clap` produces
- it verifies file I/O actually works (not just in-memory logic)
- it catches integration bugs that unit tests miss

### Test Helper Patterns

The repo uses a few reusable helpers across test modules:

**Creating test files:**

```rust
fn write_file(root: &Path, relative_path: &str, contents: &str) {
    let absolute_path = root.join(relative_path);
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).expect("create test parent dir");
    }
    fs::write(&absolute_path, contents).expect("write test file");
}
```

**Using `tempfile` for cleanup:**

```rust
let temp_dir = tempdir().expect("failed to create tempdir");
// temp_dir automatically deletes its contents when dropped
```

`tempfile::tempdir()` returns a `TempDir` that automatically cleans up when
it goes out of scope. This means tests never leave test files behind, even if
they panic.

## Crate Reference Table

| Crate | Purpose | Used in |
|---|---|---|
| `anyhow` | Error handling with context chains | All projects |
| `serde` + `serde_json` | JSON serialization and deserialization | `csearch-rscraper`, Phase 1.1 |
| `bincode` | Compact binary serialization | `csearch-rscraper`, Phase 1.1 |
| `clap` | CLI argument parsing with derive macros | Phase 1 crates |
| `tokio` | Async runtime and task management | `csearch-rscraper`, optional in Phase 1.3 |
| `sqlx` | Async Postgres with compile-time SQL checks | `csearch-rscraper` |
| `quick-xml` | XML parsing through serde | `csearch-rscraper` |
| `redis` | Async Redis client | `csearch-rscraper` |
| `sha2` | SHA-256 hashing | `csearch-rscraper`, Phase 1.1 |
| `tracing` | Structured application logging | `csearch-rscraper` |
| `tracing-subscriber` | Logging output configuration (JSON, filters) | `csearch-rscraper` |
| `pulldown-cmark` | Markdown to HTML rendering | Phase 1.2 |
| `ammonia` | HTML sanitization (XSS prevention) | Phase 1.2 |
| `image` | Image loading, resizing, format conversion | Phase 1.3 |
| `rayon` | Data-parallel CPU processing | optional in Phase 1.3 |
| `walkdir` | Recursive directory traversal | Phase 1.1, Phase 1.3 |
| `tempfile` | Temporary files and directories for tests | multiple crates |
| `dotenvy` | `.env` file loading for local development | `csearch-rscraper` |
| `chrono` | Date/time handling (congress year calculation) | `csearch-rscraper` |

## Final Reminder

Feature flags and tests are not side topics in Rust. They are part of how Rust
projects stay explicit, predictable, and safe as they grow. Feature flags keep
your dependency tree lean. Tests document how your code is supposed to behave.
Together, they form the safety net that makes refactoring and extending Rust
code feel confident.
