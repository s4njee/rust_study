# Rust Patterns Docs

This folder breaks the old `RUST.md` into smaller study notes.

The goal is not just to list syntax. It is to explain what each Rust pattern is
trying to help you do in normal human terms:

- keep track of who owns data
- avoid surprise crashes from missing values or ignored errors
- handle I/O and concurrency without blocking everything else
- build small programs that are predictable and easy to test

## Suggested Reading Order

1. [`01-overview-and-core-ideas.md`](01-overview-and-core-ideas.md)
2. [`02-async-and-concurrency.md`](02-async-and-concurrency.md)
3. [`03-data-formats-and-persistence.md`](03-data-formats-and-persistence.md)
4. [`04-services-and-configuration.md`](04-services-and-configuration.md)
5. [`05-cli-files-and-media.md`](05-cli-files-and-media.md)
6. [`06-types-strings-and-iterators.md`](06-types-strings-and-iterators.md)
7. [`07-features-testing-and-crates.md`](07-features-testing-and-crates.md)

## Projects Referenced

| Project | Path | What it teaches |
|---|---|---|
| csearch-rscraper | `csearch-rscraper/` | Async runtime, SQLx, Redis, parsing, structured logging |
| CLI Hash Cache | `PHASE1/1.1-CLI-HASH-CACHE/` | Hashing, file traversal, serialization, CLI shape |
| Markdown Sanitizer | `PHASE1/1.2-MARKDOWN-SANITIZER/` | Markdown rendering, HTML sanitization, pipeline design |
| Image Thumbnail Generator | `PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/` | Image processing, feature flags, parallel batch work |

## How To Use These Notes

- Read the plain-English summary first.
- Then read the short Rust pattern notes.
- Then open the referenced project file and study the real code in context.

These docs are intentionally learning-oriented, not exhaustive API references.
