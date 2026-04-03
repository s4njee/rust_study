# Features, Testing, And Crates

This doc covers optional compilation, testing patterns, and the crate cheat
sheet for this repo.

## Feature Flags

In plain English: feature flags let one crate support optional capabilities
without forcing every build to include every dependency.

Example from the thumbnail generator:

```toml
[features]
rayon = ["dep:rayon"]
tokio = ["dep:tokio"]
```

That means:

- the base crate stays simpler
- parallel backends can be turned on when needed
- learners can see how optional dependencies work in practice

## Conditional Compilation

In plain English: `#[cfg(...)]` is Rust's way of saying "only compile this code
when a certain condition is true."

```rust
#[cfg(feature = "rayon")]
use rayon::prelude::*;

#[cfg(feature = "tokio")]
use tokio::sync::Semaphore;
```

This is compile-time branching, not runtime branching.

## Helpful Runtime Errors Around Missing Features

In plain English: even when a feature is disabled at compile time, the CLI can
still explain what the user should do.

```rust
#[cfg(not(feature = "rayon"))]
bail!(
    "--parallel rayon requires the rayon feature; \
     rebuild with: cargo build --features rayon"
);
```

That is a good pattern because it turns confusion into a direct next step.

## Testing Patterns In The Repo

## Unit Tests

In plain English: unit tests sit close to the code they verify.

```rust
#[cfg(test)]
mod tests {
    use super::*;
}
```

This is good for small helpers, parsers, config loading, and edge cases.

## Environment Variable Tests

In plain English: tests that modify environment variables can interfere with one
another, so the repo serializes them with a `Mutex`.

```rust
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
```

That prevents flaky tests caused by shared global state.

## Round-Trip Tests

In plain English: write something, read it back, and confirm it is unchanged.

This is used for cache persistence because the point is not just "can I save?"
but "can I save and recover the same meaning?"

## Known-Value Tests

In plain English: sometimes the simplest correct test is comparing against a
well-known answer.

Example:

- hashing `"abc"` and checking the expected SHA-256 digest

These tests are great for proving correctness of deterministic algorithms.

## Security-Focused Tests

In plain English: the markdown sanitizer tests prove that unsafe content is
removed while safe content still works.

That matters because security behavior should be intentionally tested, not
assumed.

## Integration Tests

In plain English: integration tests exercise a realistic workflow instead of one
small helper.

Example from the CLI hash cache:

- create files
- run the CLI logic
- confirm changes are reported
- confirm the cache file gets written

That kind of test is especially valuable in CLI projects because user-facing
behavior matters as much as internal helpers.

## Crate Reference Table

| Crate | Purpose | Used in |
|---|---|---|
| `anyhow` | Error handling with context | All projects |
| `serde` + `serde_json` | JSON serialization and deserialization | `csearch-rscraper`, Phase 1.1 |
| `bincode` | Compact binary serialization | `csearch-rscraper`, Phase 1.1 |
| `clap` | CLI argument parsing | Phase 1 crates |
| `tokio` | Async runtime | `csearch-rscraper`, optional in Phase 1.3 |
| `sqlx` | Async Postgres access | `csearch-rscraper` |
| `quick-xml` | XML parsing through serde | `csearch-rscraper` |
| `redis` | Async Redis client | `csearch-rscraper` |
| `sha2` | SHA-256 hashing | `csearch-rscraper`, Phase 1.1 |
| `tracing` | Structured application logging | `csearch-rscraper` |
| `tracing-subscriber` | Logging output configuration | `csearch-rscraper` |
| `pulldown-cmark` | Markdown to HTML | Phase 1.2 |
| `ammonia` | HTML sanitization | Phase 1.2 |
| `image` | Image loading, resizing, saving | Phase 1.3 |
| `rayon` | CPU parallelism | optional in Phase 1.3 |
| `walkdir` | Recursive directory traversal | Phase 1.1, Phase 1.3 |
| `tempfile` | Temporary files for tests | multiple crates |
| `dotenvy` | `.env` loading | `csearch-rscraper` |

## Final Reminder

Feature flags and tests are not side topics in Rust. They are part of how Rust
projects stay explicit, predictable, and safe as they grow.
