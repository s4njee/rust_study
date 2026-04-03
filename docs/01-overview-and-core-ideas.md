# Overview And Core Ideas

This doc covers the mental model that makes the rest of the repo easier to
understand.

## Ownership, Borrowing, And Why Rust Cares

In plain English: Rust always wants to know who is responsible for a piece of
data. That sounds strict, but it prevents a whole class of bugs where one part
of a program changes or frees data while another part is still using it.

- owned value: the current code is responsible for it
- borrowed value: the current code may use it, but does not own it
- shared ownership: possible, but you ask for it explicitly with types like `Arc<T>`

This repo uses that distinction constantly:

- `PathBuf` owns a path, so it is good for storing in structs
- `&Path` borrows a path, so it is good for function parameters
- `String` owns text, while `&str` is a borrowed view into text

## `Option<T>` Replaces "Maybe Null"

In plain English: Rust makes you admit when a value might be missing.

```rust
pub struct InsertVoteParams {
    pub bill_type: Option<String>,
    pub bill_number: Option<i32>,
}
```

Instead of "hope this is not null," the type itself says "this may or may not
exist." That makes missing data something you handle on purpose.

Examples in the repo:

- nullable SQL fields in `csearch-rscraper/src/models.rs`
- conditional values like `.then_some(...)` in `csearch-rscraper/src/votes.rs`

## `Result<T, E>` Replaces Hidden Failure

In plain English: a Rust function usually tells you up front whether it can
fail. That keeps errors from hiding until much later.

```rust
pub fn load() -> anyhow::Result<Self> { /* ... */ }
```

You will see three common patterns in this repo:

- `?` means "if this failed, stop here and return the error"
- `bail!` means "stop right now with a clear error"
- `.context(...)` adds a friendlier explanation before the error bubbles up

## RAII Means Cleanup Happens Automatically

In plain English: Rust ties cleanup to scope. When a value goes out of scope,
its cleanup code runs automatically.

That is why these patterns feel safe:

- transactions roll back if you forget to commit
- semaphore permits get released when the variable is dropped
- files get closed when they leave scope

You do not need a lot of manual "finally" style cleanup logic.

## `Copy` vs `Clone`

In plain English:

- `Copy` is for tiny, cheap values that can be duplicated automatically
- `Clone` is for values where duplication is a real operation

Example:

```rust
#[derive(Debug, Clone, Copy)]
pub struct ThumbnailConfig {
    pub max_width: u32,
    pub jpeg_quality: u8,
}
```

That works because the struct only contains small numeric fields.

## Newtype Pattern

In plain English: sometimes you wrap a value in a small struct so it has a more
specific meaning.

```rust
struct PersistedHashes(HashMap<String, String>);
```

This is useful when a raw `HashMap` is technically correct but too generic.
The wrapper says, "this map is specifically the saved hash cache."

## `impl Into<T>` And `impl AsRef<T>`

In plain English:

- `Into<T>` is good when you want to accept several input types and turn them
  into an owned value
- `AsRef<T>` is good when you only need read-only access

Examples:

```rust
pub fn load(path: impl Into<PathBuf>) -> Result<Self> { /* ... */ }
pub fn render_markdown(markdown: impl AsRef<str>) -> String { /* ... */ }
```

This makes APIs easier to call without adding lots of overloads.

## Rust Idioms You Will See Everywhere

| Idiom | Plain-English meaning |
|---|---|
| `match` | "Handle every possible case" |
| `#[derive(...)]` | "Generate the boring code for me" |
| `Option<T>` | "This value might be missing" |
| `Result<T, E>` | "This operation might fail" |
| `Arc<T>` | "Several tasks need shared read access" |
| `async`/`.await` | "Pause this task without blocking the whole thread" |

## Good Next Step

Read [`02-async-and-concurrency.md`](02-async-and-concurrency.md) next if the
async code in `csearch-rscraper` feels unfamiliar.
