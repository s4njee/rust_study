# 1.3 — Image Thumbnail Generator: Library & CLI Reference

A Rust CLI tool (with library core) that takes an input image, detects its format, generates a resized thumbnail preserving aspect ratio, and writes it to disk. Supports JPEG, PNG, and WebP. Includes three interchangeable batch-processing strategies — sequential, rayon (data parallelism), and tokio (async concurrency) — to teach the fundamental patterns of concurrent Rust.

---

## Reused from 1.1 / 1.2

These items were already documented in [1.1 CODE.md](../1.1-CLI-HASH-CACHE/CODE.md) and [1.2 CODE.md](../1.2-MARKDOWN-SANITIZER/CODE.md):

| Item | 1.3 Usage |
|------|-----------|
| `std::fs` | `File::create`, `create_dir_all` for output directories |
| `std::path::PathBuf` / `Path` | All file path manipulation — `file_stem`, `extension`, `parent`, `join` |
| `std::io::BufWriter` | Buffered writes to the JPEG encoder (see [1.1 `BufReader`](../1.1-CLI-HASH-CACHE/CODE.md)) |
| `std::process::ExitCode` | Structured exit codes from `main()` (see [1.2](../1.2-MARKDOWN-SANITIZER/CODE.md)) |
| `clap` (derive) | `Args` struct with positional, flag, and enum-valued arguments |
| `anyhow` | `Context` / `with_context()` / `bail!` on every fallible operation |
| `walkdir` | Collecting image files in batch mode (`min_depth` / `max_depth`) |
| `tempfile` | `tempdir()` for test fixtures |

---

## New Standard Library Usage

### `as` casts and `f64` arithmetic for aspect ratios

📖 [Numeric casts](https://doc.rust-lang.org/reference/expressions/operator-expr.html#type-cast-expressions) · [f64](https://doc.rust-lang.org/std/primitive.f64.html)

1.1 and 1.2 worked exclusively with integers and strings. 1.3 introduces floating-point arithmetic for proportional scaling. Integer division truncates — `333 * 400 / 1000` gives `133` but `800 * 200 / 1200` gives `133` rather than the correct `133.3 → 133`. Using `f64` and rounding gives precise results:

```rust
let scale = max_width as f64 / self.width as f64;
let new_height = (self.height as f64 * scale).round() as u32;
```

The `as` keyword does **unchecked** numeric conversion. `u32 as f64` is always lossless (every `u32` fits in `f64`). The reverse (`f64 as u32`) truncates toward zero and saturates at `u32::MAX` — we use `.round()` first to get nearest-integer behaviour.

### `std::thread::available_parallelism`

📖 [Reference](https://doc.rust-lang.org/std/thread/fn.available_parallelism.html)

Returns the number of logical CPU cores the OS has made available to this process. Used as the default value for `--max-concurrent` in tokio mode:

```rust
let concurrency = std::thread::available_parallelism()
    .ok()           // Result<NonZero<usize>> → Option<NonZero<usize>>
    .map(|n| n.get()) // NonZero<usize> → usize
    .unwrap_or(4);    // fallback if the OS can't report it
```

This is stable since Rust 1.59. It respects cgroup limits (Docker, Kubernetes) and CPU affinity masks — unlike simply reading `/proc/cpuinfo`.

### `#[derive(Default)]` with `#[default]` on enum variants

📖 [Default](https://doc.rust-lang.org/std/default/trait.Default.html)

Rust 1.62 stabilised `#[default]` on enum variants, letting `derive(Default)` work for enums — not just structs:

```rust
#[derive(Default)]
pub enum Parallel {
    #[default]          // ← marks this variant as the default
    Sequential,
    Rayon,
    Tokio,
}

let p = Parallel::default(); // → Parallel::Sequential
```

Before this feature, you had to write `impl Default for Parallel` manually.

### Conditional compilation with `#[cfg(feature = "…")]`

📖 [Conditional compilation](https://doc.rust-lang.org/reference/conditional-compilation.html) · [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html)

`#[cfg]` attributes remove code at compile time based on feature flags, target platform, or other conditions. In this project, rayon and tokio support are optional:

```rust
// This function only exists when the `rayon` feature is enabled.
#[cfg(feature = "rayon")]
fn run_batch_rayon(files: Vec<PathBuf>, ...) -> Result<()> { ... }
```

When writing a match arm that needs two mutually exclusive implementations, use paired `#[cfg]` / `#[cfg(not(…))]` on consecutive statements:

```rust
Parallel::Rayon => {
    // Only ONE of these two lines will be compiled:
    #[cfg(feature = "rayon")]
    return run_batch_rayon(files, config, format_override, quiet);

    #[cfg(not(feature = "rayon"))]
    bail!("rebuild with: cargo build --features rayon");
}
```

In `Cargo.toml`, the `dep:` prefix (Rust 1.60+) lets you name a feature the same as its dependency without auto-enabling it everywhere:

```toml
[features]
rayon = ["dep:rayon"]      # feature "rayon" activates crate "rayon"
tokio = ["dep:tokio"]

[dependencies]
rayon = { version = "1", optional = true }
tokio = { version = "1", features = ["rt-multi-thread", "sync", "macros"], optional = true }
```

Build commands:
```bash
cargo build                        # no optional deps
cargo build --features rayon       # + rayon
cargo build --features tokio       # + tokio
cargo build --features rayon,tokio # + both
cargo test  --features rayon,tokio # run ALL tests
```

### `clap::ValueEnum` for enum-valued CLI flags

📖 [ValueEnum](https://docs.rs/clap/latest/clap/trait.ValueEnum.html)

1.1 and 1.2 used `bool` flags and `PathBuf` arguments. 1.3 introduces enum-valued flags — clap parses a string like `"rayon"` directly into an enum variant:

```rust
use clap::ValueEnum;

#[derive(ValueEnum, Clone, Copy)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
}

#[derive(Parser)]
struct Args {
    /// Override the output format.
    #[arg(long)]
    format: Option<Format>,
    // usage: image-thumbnail photo.jpg --format webp
}
```

Clap auto-generates the list of valid values for `--help` and rejects unknown values with a clear error. Variant names are lowercased by default (`Jpeg` → `jpeg`, `Webp` → `webp`).

### `OsStr` methods: `file_stem`, `extension`, `is_some_and`

📖 [Path::file_stem](https://doc.rust-lang.org/std/path/struct.Path.html#method.file_stem) · [Path::extension](https://doc.rust-lang.org/std/path/struct.Path.html#method.extension) · [Option::is_some_and](https://doc.rust-lang.org/std/option/enum.Option.html#method.is_some_and)

Path decomposition methods return `Option<&OsStr>`, which must be converted to `&str` for string matching. The `is_some_and` method (Rust 1.70) combines the `is_some` check and a predicate into one call:

```rust
// Detect "_thumb" suffix to skip re-thumbnailing previous output
fn is_thumb_file(path: &Path) -> bool {
    path.file_stem()                     // Option<&OsStr>
        .and_then(|s| s.to_str())        // Option<&str> (None if not UTF-8)
        .is_some_and(|s| s.ends_with("_thumb"))
}

// Extension-based format detection with chained Option methods
fn from_path(path: &Path) -> Option<Format> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some(Format::Jpeg),
        "png" => Some(Format::Png),
        "webp" => Some(Format::Webp),
        _ => None,
    }
}
```

---

## New External Crates

### `image` — Image Decoding, Resizing, and Encoding

📖 [docs.rs](https://docs.rs/image/latest/image/) · [GitHub](https://github.com/image-rs/image) · [DynamicImage](https://docs.rs/image/latest/image/enum.DynamicImage.html) · [GenericImageView](https://docs.rs/image/latest/image/trait.GenericImageView.html)

**What it does:** A pure-Rust image processing library supporting 15+ formats. It decodes images into in-memory pixel buffers, provides resizing/cropping/filtering operations, and encodes the result back to disk.

**Cargo.toml:**
```toml
image = "0.25"
```

**Key types:**

| Type | Role |
|------|------|
| `DynamicImage` | An enum wrapping decoded pixel data (`ImageRgb8`, `ImageRgba8`, `ImageLuma8`, …). All resize/encode operations work on this type. |
| `GenericImageView` | Trait providing `.dimensions() → (u32, u32)` and `.get_pixel()`. Must be imported to call `.dimensions()` on a `DynamicImage`. |
| `ImageFormat` | Enum of supported formats (`Jpeg`, `Png`, `WebP`, `Gif`, …). Used with `save_with_format`. |
| `JpegEncoder` | Format-specific encoder that exposes quality control. |

**Basic usage — open, resize, save:**
```rust
use image::{DynamicImage, GenericImageView};

// 1. Open and decode (format auto-detected from extension)
let img: DynamicImage = image::open("photo.jpg")?;

// 2. Read dimensions (requires GenericImageView in scope)
let (width, height) = img.dimensions();

// 3. Resize — thumbnail() preserves aspect ratio
let thumb: DynamicImage = img.thumbnail(400, 400);
// thumbnail(max_w, max_h) scales DOWN so neither dimension exceeds the max.
// The actual output may be 400×300 for a 4:3 source.

// 4. Save (format inferred from extension)
thumb.save("photo_thumb.jpg")?;
```

**Controlling JPEG quality:**

`save()` and `save_with_format()` use a fixed internal quality. To control JPEG quality, use the encoder directly:

```rust
use image::codecs::jpeg::JpegEncoder;
use std::fs::File;
use std::io::BufWriter;

let file = File::create("thumb.jpg")?;
let encoder = JpegEncoder::new_with_quality(BufWriter::new(file), 85);
img.write_with_encoder(encoder)?;
```

The `write_with_encoder` method accepts any type implementing `ImageEncoder`, so the same pattern works for customising PNG compression, WebP quality, etc.

**`thumbnail` vs `resize`:**

| Method | Algorithm | Speed | Quality | Use when |
|--------|-----------|-------|---------|----------|
| `thumbnail(w, h)` | Nearest + Lanczos3 | Fast | Good | Web thumbnails, batch jobs |
| `resize(w, h, filter)` | Chosen filter only | Varies | Tuneable | Quality-critical single images |

Available `FilterType` values: `Nearest` (fastest, blocky), `Triangle` (bilinear), `CatmullRom` (bicubic), `Gaussian`, `Lanczos3` (sharpest, slowest).

**Format transcoding:**

Since `DynamicImage` is format-agnostic (it holds decoded pixels, not encoded bytes), you can open a JPEG and save it as PNG — the format only matters at the I/O boundary:

```rust
let img = image::open("input.jpg")?;
img.save_with_format("output.png", image::ImageFormat::Png)?;
```

---

## Rayon — Data Parallelism (Deep Dive)

📖 [docs.rs](https://docs.rs/rayon/latest/rayon/) · [GitHub](https://github.com/rayon-rs/rayon) · [ParallelIterator](https://docs.rs/rayon/latest/rayon/iter/trait.ParallelIterator.html) · [FAQ](https://github.com/rayon-rs/rayon/blob/main/FAQ.md)

**Cargo.toml (optional):**
```toml
rayon = { version = "1", optional = true }
```

### What rayon does

Rayon provides **data-parallel** versions of standard iterator adapters. The core idea: replace `.iter()` with `.par_iter()` and rayon distributes the work across CPU cores using a global work-stealing thread pool. You do not create threads, manage lifetimes, or send messages — rayon handles all of that.

### The mental model

```
Sequential:
  ┌─────────────────────────────────────────────────────┐
  │  Thread 0: [item0] [item1] [item2] [item3] [item4]  │
  └─────────────────────────────────────────────────────┘

Rayon (4 cores):
  ┌──────────────────┐
  │ Thread 0: [item0] [item4]  ← steals item4 when done early
  │ Thread 1: [item1]
  │ Thread 2: [item2]
  │ Thread 3: [item3]
  └──────────────────┘
```

Rayon divides the input recursively (like merge-sort). When a thread finishes its share, it **steals** work from the tail of another thread's deque. This gives automatic load-balancing without any tuning.

### `par_iter()` vs `into_par_iter()`

```rust
use rayon::prelude::*;

let items = vec![1, 2, 3, 4, 5];

// par_iter() — borrows each element as &T
// The collection survives and can be used later.
let sums: Vec<i32> = items.par_iter().map(|&x| x + 1).collect();

// into_par_iter() — moves each element as T
// The collection is consumed; elements are owned by the closure.
let sums: Vec<i32> = items.into_par_iter().map(|x| x + 1).collect();
```

**When to use which:**
- `par_iter()` when you need the collection afterward, or when `T` is `Copy`
- `into_par_iter()` when `T` is expensive to clone (like `PathBuf`, `String`, `Vec<u8>`) and you don't need the original collection

### The `Send + Sync` contract

Rayon's `map` requires: `F: Fn(T) -> R + Sync + Send`.

Why **both**?
- `Send` — the closure can be moved to a worker thread
- `Sync` — a `&F` reference can be shared between threads (multiple workers call `f` concurrently via shared reference)

Anything a closure **captures** must also be `Send + Sync`:

```rust
// ✅ Works — ThumbnailConfig: Copy, which implies Send + Sync
let config = ThumbnailConfig { max_width: 400, jpeg_quality: 85 };
let results: Vec<_> = files.into_par_iter().map(|path| {
    generate_thumbnail(&path, &output, format, config)  // config is Copy
}).collect();

// ❌ Would NOT compile — Rc<T> is NOT Send
use std::rc::Rc;
let shared = Rc::new(some_data);
files.into_par_iter().map(|path| {
    // ERROR: Rc<T> cannot be sent between threads safely
    do_something(&shared, &path)
}).collect();

// ✅ Fix — use Arc<T> instead (atomically reference-counted → Send + Sync)
use std::sync::Arc;
let shared = Arc::new(some_data);
files.into_par_iter().map(|path| {
    do_something(&shared, &path)  // Arc<T>: Send + Sync when T: Send + Sync
}).collect();
```

### The generic `par_batch` pattern

This project extracts the rayon pattern into a reusable helper that works for any workload:

```rust
#[cfg(feature = "rayon")]
pub fn par_batch<T, U, E, F>(items: Vec<T>, f: F) -> Vec<Result<U, E>>
where
    T: Send,             // items cross thread boundaries
    U: Send,             // results cross thread boundaries
    E: Send,             // errors cross thread boundaries
    F: Fn(T) -> Result<U, E> + Sync + Send,  // closure is thread-safe
{
    items.into_par_iter().map(f).collect()
}
```

**To use this in your own code,** replace `par_batch` with the one-liner if you prefer:

```rust
// These are equivalent:
let results = par_batch(items, |x| work(x));
let results: Vec<_> = items.into_par_iter().map(|x| work(x)).collect();
```

### Order guarantees

Rayon's `collect()` preserves input order. `results[i]` corresponds to `items[i]`, even though item 5 might finish before item 2. Rayon achieves this by allocating the output vector upfront and writing each result into its position.

If you don't need order, `for_each` is slightly cheaper:

```rust
files.into_par_iter().for_each(|path| {
    if let Err(e) = process(&path) {
        eprintln!("warning: {e}");
    }
});
```

### How the image-specific code uses rayon

```rust
#[cfg(feature = "rayon")]
fn run_batch_rayon(files: Vec<PathBuf>, config: ThumbnailConfig, ...) -> Result<()> {
    // config is Copy → captured by value in every thread without cloning.
    // format_override is Option<Format>, also Copy.
    let results = par_batch(files, |input| -> Result<ThumbnailResult> {
        let format = detect_format(&input, None, format_override)?;
        let output = resolve_output(&input, None, format);
        generate_thumbnail(&input, &output, format, config)
    });

    // Report results AFTER all threads finish to avoid interleaved output.
    for result in results {
        match result {
            Ok(thumb) => print_result(&thumb),
            Err(err) => eprintln!("warning: {err:#}"),
        }
    }
    Ok(())
}
```

Notice the closure body is **identical** to the sequential loop body. The only change is the surrounding iterator machinery.

### Thread pool tuning

Rayon creates one OS thread per logical CPU core by default. To override:

```rust
rayon::ThreadPoolBuilder::new()
    .num_threads(2)
    .build_global()     // sets the global pool (call once, early in main)
    .expect("failed to build rayon thread pool");
```

For most workloads the default is optimal — rayon benchmarks show diminishing returns beyond the physical core count.

---

## Tokio — Async Concurrency (Deep Dive)

📖 [docs.rs](https://docs.rs/tokio/latest/tokio/) · [Tutorial](https://tokio.rs/tokio/tutorial) · [spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) · [JoinSet](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html) · [Semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html)

**Cargo.toml (optional):**
```toml
tokio = { version = "1", features = ["rt-multi-thread", "sync", "macros"], optional = true }
```

**Features explained:**
| Feature | What it provides |
|---------|-----------------|
| `rt-multi-thread` | `Runtime::new()` — the multi-threaded async executor |
| `sync` | `Semaphore`, `Mutex`, channels |
| `macros` | `#[tokio::test]` for async tests |

### Why tokio for image processing?

Short answer: it's not the best fit — rayon is better for pure CPU work. But tokio is included here because:

1. It teaches async Rust concepts you'll need in Phase 2 (Axum, sqlx, reqwest)
2. It shows `spawn_blocking`, which is how you integrate CPU-bound work into an async application
3. It demonstrates `Semaphore`-bounded concurrency, a critical production pattern

In a real application, you'd use tokio when your batch pipeline includes network I/O (downloading images from URLs) alongside CPU work (resizing). Tokio lets you overlap downloads with resizing; rayon cannot.

### Concept 1: `async fn` and `.await`

An `async fn` does **not** run immediately. It returns a `Future` — a value representing "work that hasn't happened yet." The work only executes when you `.await` the future inside an async context:

```rust
// This function does NOT resize anything when called.
// It returns a Future that, when awaited, will do the resize.
async fn resize_later(path: PathBuf) -> Result<()> {
    let img = image::open(&path)?;   // runs only when awaited
    let thumb = img.thumbnail(400, 400);
    thumb.save("thumb.jpg")?;
    Ok(())
}

// To actually run it:
resize_later(path).await?;  // NOW the code executes
```

`.await` is a **suspension point**: the executor can pause this task and run another one while waiting. This is only useful for I/O — if the code between awaits is CPU-bound (like `image::open` + `thumbnail`), the executor thread is blocked and cannot run other tasks.

### Concept 2: `spawn_blocking` — the CPU-bound escape hatch

`tokio::task::spawn_blocking` moves a closure to a **separate thread pool** dedicated to blocking work. The async executor threads stay free for I/O tasks:

```
Normal tokio::spawn:
  ┌──────────────────────────────────┐
  │ Executor Thread 0: [async task]  │ ← CPU-bound work BLOCKS this thread
  │ Executor Thread 1: [async task]  │   other tasks starve
  └──────────────────────────────────┘

spawn_blocking:
  ┌──────────────────────────────────┐
  │ Executor Thread 0: [async I/O]  │ ← stays free for light async work
  │ Executor Thread 1: [async I/O]  │
  ├──────────────────────────────────┤
  │ Blocking Thread 0: [CPU work]   │ ← heavy work runs here instead
  │ Blocking Thread 1: [CPU work]   │
  └──────────────────────────────────┘
```

```rust
use tokio::task;

// Moves the closure to the blocking thread pool.
// Returns a JoinHandle<T> — a Future that resolves when the work completes.
let handle = task::spawn_blocking(move || {
    // This runs on a blocking OS thread, NOT an executor thread.
    generate_thumbnail(&input, &output, format, config)
});

// .await suspends the caller until the blocking work finishes.
// The Result layers:
//   outer: Err(JoinError) if the blocking task panicked
//   inner: whatever generate_thumbnail returned
let result: Result<Result<ThumbnailResult>, JoinError> = handle.await;
```

**Rule of thumb:**
| Work type | Spawn with |
|-----------|-----------|
| Async I/O (network, file with tokio::fs) | `tokio::spawn` |
| CPU-bound (image resize, hashing, compression) | `tokio::task::spawn_blocking` |
| Quick synchronous code (< 10μs) | Neither — just do it inline |

### Concept 3: `JoinSet` — tracking multiple tasks

`JoinSet<T>` is a collection of spawned tasks. You add tasks with `.spawn()` and retrieve results with `.join_next().await`:

```rust
use tokio::task::JoinSet;

let mut set: JoinSet<String> = JoinSet::new();

// Spawn several tasks
for i in 0..5 {
    set.spawn(async move {
        format!("result {i}")
    });
}

// Collect results as they complete (order is NOT guaranteed)
while let Some(result) = set.join_next().await {
    match result {
        Ok(value) => println!("got: {value}"),
        Err(join_err) => eprintln!("task panicked: {join_err}"),
    }
}
// When join_next() returns None, all tasks have completed.
```

**`JoinError` vs your error:** `join_next()` returns `Result<T, JoinError>`. The `JoinError` means the task itself panicked or was cancelled — it is NOT your application error. If `T` is itself `Result<U, E>`, you have a double-wrapped result:

```rust
// JoinSet<Result<ThumbnailResult, anyhow::Error>>
//
// join_next().await returns:
//   Ok(Ok(thumb))   — task finished, thumbnail succeeded
//   Ok(Err(e))      — task finished, thumbnail failed (your error)
//   Err(join_err)   — task panicked (tokio infrastructure error)
```

### Concept 4: Unbounded spawning — the simple (dangerous) version

```rust
pub async fn spawn_blocking_batch<T, U, F>(items: Vec<T>, f: F) -> Vec<anyhow::Result<U>>
where
    T: Send + 'static,
    U: Send + 'static,
    F: Fn(T) -> anyhow::Result<U> + Send + Clone + 'static,
{
    let mut set: JoinSet<anyhow::Result<U>> = JoinSet::new();

    for item in items {
        let f = f.clone();
        set.spawn(async move {
            tokio::task::spawn_blocking(move || f(item))
                .await
                .unwrap_or_else(|e| anyhow::bail!("task panicked: {e}"))
        });
    }

    let mut results = Vec::with_capacity(set.len());
    while let Some(join_result) = set.join_next().await {
        results.push(join_result.unwrap_or_else(|e| anyhow::bail!("join error: {e}")));
    }
    results
}
```

**Why the trait bounds?**

| Bound | Reason |
|-------|--------|
| `T: Send + 'static` | Items are moved to another thread. `'static` means no borrowed references — the thread might outlive the caller's stack frame. |
| `U: Send + 'static` | Results are sent back across the thread boundary. |
| `F: Fn(T) + Send` | The closure is moved to the blocking thread. |
| `F: Clone` | Each task gets its own copy of `f`. Unlike rayon (which shares `&F`), tokio tasks are `'static` and cannot hold references. |
| `F: 'static` | Same as `T` — the closure might outlive the spawning scope. |

**The danger:** if `items` has 10,000 entries, this spawns 10,000 blocking tasks immediately. Each decoded image is ~10 MB in memory, so you'd need ~100 GB of RAM. This is where the bounded version comes in.

### Concept 5: `Semaphore` — bounding concurrency

A `Semaphore` is a concurrency primitive that holds a fixed number of **permits**. A task must acquire a permit before starting; when the permit is dropped, the slot becomes available:

```
Semaphore(3):
  Permits: [■] [■] [■]  ← 3 slots available

Task A acquires:  [A] [■] [■]
Task B acquires:  [A] [B] [■]
Task C acquires:  [A] [B] [C]
Task D tries:     [A] [B] [C]  ← D suspends (awaits), no permit available

Task A finishes, drops permit:
                  [■] [B] [C]  ← D wakes up, acquires the freed permit
                  [D] [B] [C]
```

**Why `Arc<Semaphore>`?**

The semaphore must be shared between the spawning loop and every spawned task. Rust's ownership rules prevent this with a plain reference (the tasks are `'static` — they can't borrow from the local scope). `Arc` (Atomic Reference Count) provides shared ownership:

```rust
use std::sync::Arc;
use tokio::sync::Semaphore;

// Arc::new wraps the Semaphore in a reference-counted pointer.
let semaphore = Arc::new(Semaphore::new(4));  // allow 4 concurrent tasks

for item in items {
    // Arc::clone is cheap — it increments an atomic counter, not deep-copying.
    let sem = Arc::clone(&semaphore);

    set.spawn(async move {
        // acquire_owned() returns an OwnedSemaphorePermit.
        // If all permits are taken, this .await SUSPENDS — it does NOT
        // spin or block. The executor runs other tasks while we wait.
        let permit = sem.acquire_owned().await.expect("semaphore closed");

        // The permit is moved into the blocking closure.
        // When the closure returns, the permit is dropped → slot released.
        tokio::task::spawn_blocking(move || {
            let result = f(item);
            drop(permit);  // explicit for clarity; happens on return anyway
            result
        }).await.unwrap_or_else(|e| anyhow::bail!("panic: {e}"))
    });
}
```

**`acquire_owned()` vs `acquire()`:**

| Method | Returns | Can cross `.await`? | Can move into closures? |
|--------|---------|---------------------|------------------------|
| `acquire()` | `SemaphorePermit<'_>` (borrows `&self`) | No — lifetime tied to `&sem` | No |
| `acquire_owned()` | `OwnedSemaphorePermit` (owns an `Arc`) | Yes | Yes |

Since our permit must survive across the `spawn_blocking` boundary (which requires `'static`), we must use `acquire_owned`.

### Concept 6: The full bounded pattern

```rust
pub async fn spawn_blocking_batch_bounded<T, U, F>(
    items: Vec<T>,
    f: F,
    max_concurrent: usize,
) -> Vec<anyhow::Result<U>>
where
    T: Send + 'static,
    U: Send + 'static,
    F: Fn(T) -> anyhow::Result<U> + Send + Clone + 'static,
{
    let semaphore = Arc::new(Semaphore::new(max_concurrent));
    let mut set: JoinSet<anyhow::Result<U>> = JoinSet::new();

    for item in items {
        let f = f.clone();
        let sem = Arc::clone(&semaphore);

        set.spawn(async move {
            // ← BACK-PRESSURE: if all permits are held, this suspends
            let permit = sem.acquire_owned().await.expect("semaphore closed");

            tokio::task::spawn_blocking(move || {
                let result = f(item);
                drop(permit);       // ← release slot for next task
                result
            })
            .await
            .unwrap_or_else(|e| anyhow::bail!("panic: {e}"))
        });
    }

    let mut results = Vec::with_capacity(set.len());
    while let Some(r) = set.join_next().await {
        results.push(r.unwrap_or_else(|e| anyhow::bail!("join: {e}")));
    }
    results
}
```

### Concept 7: Bridging sync ↔ async with `block_on`

The `run` function is synchronous. To call async code, create a `Runtime` and use `block_on`:

```rust
fn run_batch_tokio(files: Vec<PathBuf>, ...) -> Result<()> {
    // Runtime::new() creates a multi-threaded executor.
    // Equivalent to #[tokio::main] on main().
    tokio::runtime::Runtime::new()
        .context("failed to start tokio runtime")?
        .block_on(run_batch_tokio_inner(files, ...))
    //  ^^^^^^^^ drives the async fn to completion, blocking the caller
}
```

In a fully async application (like an Axum web server), you'd use `#[tokio::main]` on `main()` and `.await` directly:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    run_batch_tokio_inner(files, ...).await
}
```

### Testing async code with `#[tokio::test]`

The `#[tokio::test]` attribute creates a test-scoped runtime automatically:

```rust
#[cfg(feature = "tokio")]
#[tokio::test]
async fn batch_processes_all_items() {
    let items = vec![1u32, 2, 3, 4, 5];
    let results = spawn_blocking_batch(items, |n| Ok(n * 2)).await;

    // Results may arrive in any order — sort before comparing.
    let mut values: Vec<u32> = results.into_iter().map(|r| r.unwrap()).collect();
    values.sort_unstable();
    assert_eq!(values, vec![2, 4, 6, 8, 10]);
}
```

This is equivalent to wrapping the body in `Runtime::new().block_on(async { ... })`.

---

## Rayon vs Tokio — Decision Guide

| Dimension | Rayon | Tokio |
|-----------|-------|-------|
| **Model** | Data parallelism (work-stealing) | Async concurrency (cooperative scheduling) |
| **Best for** | CPU-bound batch work | I/O-bound or mixed I/O + CPU work |
| **Thread pool** | 1 thread per core (fixed) | Executor threads + blocking pool (dynamic) |
| **API style** | Iterator adapters (`.par_iter()`) | `async`/`.await`, `spawn`, `JoinSet` |
| **Closure bounds** | `Fn + Sync + Send` (shared ref) | `Fn + Send + Clone + 'static` (owned copy) |
| **Order** | `.collect()` preserves order | Results arrive in completion order |
| **Concurrency control** | Automatic (work-stealing) | Manual (`Semaphore`) |
| **Learning curve** | Low (swap one word) | Higher (async lifetime rules, `'static`) |
| **When to use in this project** | Pure image resizing | Downloading images from URLs + resizing |
| **Dependency weight** | Light (~60 KB) | Heavy (~2 MB with rt-multi-thread) |

For this project (local files, CPU-bound resize), rayon is the better choice. Tokio is included to teach the async patterns you'll need in Phase 2 projects (Axum server, HTTP clients, database queries).

---

## Cargo.toml Summary

```toml
[package]
name = "image-thumbnail"
version = "0.1.0"
edition = "2024"

[features]
rayon = ["dep:rayon"]
tokio = ["dep:tokio"]

[dependencies]
anyhow  = "1.0"
clap    = { version = "4.5", features = ["derive"] }
image   = "0.25"
walkdir = "2.5"
rayon   = { version = "1", optional = true }
tokio   = { version = "1", features = ["rt-multi-thread", "sync", "macros"], optional = true }

[dev-dependencies]
tempfile = "3.20"
```

---

## Architecture

```
src/
├── lib.rs    ~600 lines — core logic, 3 batch strategies, 28 tests
└── main.rs     3 lines — delegates to lib::main_exit_code()
```

The parallel strategies share the same pipeline:
```
collect_batch_files()  ─→  [Vec<PathBuf>]
                                │
                ┌───────────────┼───────────────┐
                ▼               ▼               ▼
          Sequential        Rayon           Tokio
           for loop       par_batch     spawn_blocking
               │               │         + Semaphore
               ▼               ▼               ▼
        generate_thumbnail (identical closure body)
               │               │               ▼
               └───────┬───────┘         join_next()
                       ▼
                 print_result
```

---

## Expected Output Example

```
$ image-thumbnail photo.jpg
photo.jpg → photo_thumb.jpg  (JPEG 4032 × 3024 → 400 × 300)

$ image-thumbnail photo.jpg -w 200 --format png -o small.png
photo.jpg → small.png  (PNG 4032 × 3024 → 200 × 150)

$ image-thumbnail --batch ./photos/ --parallel rayon --quiet
# (no output — quiet mode)

$ image-thumbnail --batch ./photos/ --parallel tokio --max-concurrent 4
photos/a.jpg → photos/a_thumb.jpg  (JPEG 4032 × 3024 → 400 × 300)
photos/b.png → photos/b_thumb.png  (PNG 1920 × 1080 → 400 × 225)
photos/c.webp → photos/c_thumb.webp  (WebP 3000 × 2000 → 400 × 267)

Batch complete (tokio) — processed: 3, errors: 0
```
