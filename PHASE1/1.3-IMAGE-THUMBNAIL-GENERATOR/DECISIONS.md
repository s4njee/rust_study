# 1.3 — Image Thumbnail Generator: Implementation Decisions

Every choice in the implementation is a _default_, not a requirement. This document catalogs each decision, explains the trade-offs, and lists alternatives worth considering. Section 6 is the heart of this document — a deep decision guide for choosing between rayon and tokio.

---

## Table of Contents

1. [Image Processing Library](#1-image-processing-library)
2. [Resize Algorithm](#2-resize-algorithm)
3. [Format Detection Strategy](#3-format-detection-strategy)
4. [JPEG Quality Control](#4-jpeg-quality-control)
5. [Output Path Convention](#5-output-path-convention)
6. [Rayon vs Tokio — When to Use Which](#6-rayon-vs-tokio--when-to-use-which)
7. [Bounding Concurrency](#7-bounding-concurrency)
8. [Feature Flags vs Always-On Dependencies](#8-feature-flags-vs-always-on-dependencies)
9. [Generic Helpers vs Inline Parallel Code](#9-generic-helpers-vs-inline-parallel-code)

---

## 1. Image Processing Library

**Current choice:** The `image` crate (v0.25) for decoding, resizing, and encoding.

`image` is the de facto standard image library in the Rust ecosystem. It's pure Rust, supports 15+ formats, and bundles resizing filters. The trade-off is binary size: the default features pull in decoders for many formats you may not need.

### Alternatives

| Library | Crate | Approach | Speed | Formats | Notes |
|---------|-------|----------|-------|---------|-------|
| **image** | `image` | Pure Rust, all-in-one | Moderate | 15+ | Batteries included. One crate for decode + resize + encode. |
| **fast_image_resize** | `fast_image_resize` | SIMD-optimized resize only | 2–10× faster resize | N/A (resize only) | Pairs with `image` for decode/encode. Worth it for batch pipelines. |
| **libvips** (bindings) | `libvips` | C bindings to libvips | Very fast | 20+ | Requires system library. Not pure Rust. Excellent for servers. |
| **imagemagick** (bindings) | `magick_rust` | C bindings to ImageMagick | Fast | 100+ | Heavy dependency. Overkill for thumbnails. |
| **zune-image** | `zune-image` | Pure Rust, performance-focused | Fast | JPEG, PNG, others | Newer alternative to `image`. Faster JPEG decode. |
| **turbojpeg** | `turbojpeg` | C bindings to libjpeg-turbo | Very fast (JPEG) | JPEG only | 3–5× faster JPEG decode/encode. Requires system library. |

### Code: `fast_image_resize` (pair with `image`)

```rust
// Cargo.toml: fast_image_resize = "5", image = "0.25"
use fast_image_resize::{self as fir, images::Image};

let src_image = image::open("photo.jpg")?;
let (w, h) = src_image.dimensions();

// Create source and destination pixel buffers
let src = fir::images::ImageRef::new(w, h, src_image.as_bytes(), fir::PixelType::U8x3)?;
let mut dst = Image::new(400, 300, fir::PixelType::U8x3);

// Resize with a SIMD-accelerated Lanczos3 filter
let mut resizer = fir::Resizer::new();
resizer.resize(&src, &mut dst, None)?;
```

### Decision Guidance

- **General purpose thumbnails?** → `image` (current). One dependency, covers everything.
- **Batch pipeline where resize is the bottleneck?** → `image` for decode/encode + `fast_image_resize` for the resize step.
- **Server handling thousands of images/sec?** → `libvips` bindings. The C library is battle-tested at scale.
- **Only JPEG?** → `turbojpeg` for maximum JPEG performance.
- **Binary size matters (WASM, embedded)?** → `image` with `default-features = false` and only the codecs you need.

📖 [image](https://docs.rs/image/latest/image/) · [fast_image_resize](https://docs.rs/fast_image_resize/latest/fast_image_resize/) · [zune-image](https://docs.rs/zune-image/latest/zune_image/)

---

## 2. Resize Algorithm

**Current choice:** `DynamicImage::thumbnail()`, which uses a Nearest Neighbour pass followed by Lanczos3.

### Available approaches

| Method | API | Quality | Speed | Notes |
|--------|-----|---------|-------|-------|
| **thumbnail** (current) | `img.thumbnail(w, h)` | Good | Fast | Two-pass: Nearest Neighbour downsample + Lanczos3 finish. Preserves aspect ratio automatically. |
| **resize + Lanczos3** | `img.resize(w, h, FilterType::Lanczos3)` | Best | Slow | Single-pass full Lanczos3. Sharper but ~3× slower. |
| **resize + CatmullRom** | `img.resize(w, h, FilterType::CatmullRom)` | Very good | Moderate | Bicubic interpolation. Good compromise. |
| **resize + Triangle** | `img.resize(w, h, FilterType::Triangle)` | Acceptable | Fast | Bilinear interpolation. Slightly soft. |
| **resize + Nearest** | `img.resize(w, h, FilterType::Nearest)` | Poor | Fastest | Blocky. Only useful for pixel art or icons. |
| **resize_exact** | `img.resize_exact(w, h, filter)` | Varies | Varies | Does NOT preserve aspect ratio — stretches to exact dimensions. |

### How `thumbnail` differs from `resize`

`thumbnail(max_width, max_height)` is not just `resize` with a preset filter. It:

1. Computes the target size to fit within `max_width × max_height` while preserving aspect ratio.
2. If the downscale factor is large (e.g., 4000→400), it first does a fast Nearest Neighbour downsample to an intermediate size, _then_ applies Lanczos3 for the final pass.

This two-pass approach is measurably faster for large downscales (>2×) while producing nearly identical output to a full Lanczos3 pass.

### Decision Guidance

- **Web thumbnails, batch processing?** → `thumbnail` (current). Speed matters, quality is good enough.
- **Hero images, print output?** → `resize` with `Lanczos3`.
- **Pixel art or icons?** → `resize` with `Nearest` to preserve sharp edges.
- **Processing thousands of images?** → Consider `fast_image_resize` crate (see section 1).

📖 [DynamicImage::thumbnail](https://docs.rs/image/latest/image/enum.DynamicImage.html#method.thumbnail) · [FilterType](https://docs.rs/image/latest/image/imageops/enum.FilterType.html)

---

## 3. Format Detection Strategy

**Current choice:** Extension-based detection using `Path::extension()`.

```rust
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

### Alternatives

| Strategy | Approach | Reliability | Speed | Notes |
|----------|----------|-------------|-------|-------|
| **Extension** (current) | Match on file extension | Low — relies on correct naming | Instant | Simplest. Fails on misnamed files. |
| **Magic bytes** | Read first few bytes of the file | High | Requires file I/O | JPEG starts with `0xFF 0xD8 0xFF`, PNG with `0x89 0x50 0x4E 0x47`. |
| **image::ImageFormat::from_path** | Delegates to the `image` crate's extension map | Low (same as extension) | Instant | Covers more formats than our enum. |
| **image::io::Reader::open().with_guessed_format()** | Opens file + uses magic bytes | Highest | File open | Best accuracy. The `image` crate combines extension + magic bytes. |
| **infer** crate | Dedicated MIME/magic byte detection | High | Requires file I/O | Supports 100+ file types, not just images. |

### Code: Magic byte detection with `image`

```rust
use image::io::Reader as ImageReader;

let format = ImageReader::open("photo.jpg")?
    .with_guessed_format()?
    .format();
// format: Some(ImageFormat::Jpeg)
```

### Code: `infer` crate

```rust
// Cargo.toml: infer = "0.16"
let info = infer::get_from_path("photo.jpg")?.expect("unknown format");
println!("MIME: {}", info.mime_type());  // "image/jpeg"
```

### Decision Guidance

- **User provides files with correct extensions?** → Extension-based (current). Simple and fast.
- **Processing user-uploaded files?** → Magic bytes (`image::io::Reader` or `infer`). Extensions can be wrong or spoofed.
- **Mixed input from web scraping?** → Magic bytes are essential. Scraped files often have wrong or missing extensions.

📖 [image::io::Reader](https://docs.rs/image/latest/image/io/struct.Reader.html) · [infer](https://docs.rs/infer/latest/infer/)

---

## 4. JPEG Quality Control

**Current choice:** Use `JpegEncoder::new_with_quality` directly, bypassing `img.save()` which uses a fixed default quality.

```rust
let encoder = JpegEncoder::new_with_quality(BufWriter::new(file), 85);
img.write_with_encoder(encoder)?;
```

### Why not `img.save("output.jpg")`?

`DynamicImage::save()` infers the format from the extension and uses each codec's default settings. For JPEG, the `image` crate's internal default quality is 75 — adequate but noticeably worse than 85 for photographs. Since quality control is a core feature of a thumbnail tool, we use the encoder directly.

### Alternatives for quality control

| Approach | Pros | Cons |
|----------|------|------|
| **Encoder directly** (current) | Full control over every codec parameter | More code. Must create `File` + `BufWriter` manually. |
| **save() + accept default** | One-liner. No encoder boilerplate. | Locked to internal default quality. |
| **DynamicImage::write_to** | Writes to any `impl Write` with format specified | Same quality limitation as `save`. |
| **Format-specific crate** (e.g., `mozjpeg`) | Higher compression efficiency at same quality | Another dependency. `mozjpeg` is C, not pure Rust. |

### Quality numbers in practice

| Quality | Typical file size (3024×4032 photo) | Visual difference from 100 |
|---------|-------------------------------------|---------------------------|
| 100 | ~12 MB | None |
| 95 | ~4 MB | Imperceptible |
| 85 | ~2 MB | Imperceptible to casual viewing |
| 75 | ~1.2 MB | Slight softness in fine detail |
| 50 | ~600 KB | Visible JPEG artifacts |
| 25 | ~300 KB | Heavy artifacts, blocky |

For thumbnails (400px wide), the absolute sizes are much smaller (~20–100 KB at quality 85), and quality differences above 70 are nearly invisible at thumbnail resolution.

### Decision Guidance

- **Thumbnail tool?** → 85 is the right default. Users can tune with `--quality`.
- **Archival storage?** → 95 or lossless PNG.
- **Bandwidth-constrained (mobile)?** → 70–75 with WebP as an alternative.
- **Maximum JPEG compression efficiency?** → Use `mozjpeg` crate (produces 10–20% smaller files at the same quality).

📖 [JpegEncoder](https://docs.rs/image/latest/image/codecs/jpeg/struct.JpegEncoder.html) · [mozjpeg](https://docs.rs/mozjpeg/latest/mozjpeg/)

---

## 5. Output Path Convention

**Current choice:** Append `_thumb` to the input stem in the same directory: `photo.jpg` → `photo_thumb.jpg`.

### Alternatives

| Convention | Example | Pros | Cons |
|-----------|---------|------|------|
| **`_thumb` suffix** (current) | `photo_thumb.jpg` | Clear, in-place. Easy to filter with `*_thumb.*`. | Pollutes source directory. Must skip `_thumb` files in batch mode. |
| **Subdirectory** | `thumbs/photo.jpg` | Source directory stays clean. Batch mode is simpler (no skip logic). | Must create the directory. Harder to pair thumb with source. |
| **Prefix** | `thumb_photo.jpg` | Easy to sort thumbnails together. | Breaks alphabetical ordering by original filename. |
| **Separate output dir flag** | `--output-dir ./thumbs/` | User chooses location. | More complex CLI. Must handle name collisions. |
| **Size in name** | `photo_400w.jpg` | Self-documenting width. | Longer names. Must parse to avoid re-processing. |

### Decision Guidance

- **Dev tool, quick thumbnails?** → `_thumb` suffix (current). Simple, local.
- **Build pipeline producing assets?** → Subdirectory (`--output-dir thumbs/`).
- **CDN/web assets with multiple sizes?** → Size-in-name (`photo_400w.jpg`, `photo_200w.jpg`).
- **Photo gallery?** → Subdirectory per size (`small/`, `medium/`, `large/`).

---

## 6. Rayon vs Tokio — When to Use Which

**Current choice:** Both are available behind feature flags. The user chooses at runtime with `--parallel sequential|rayon|tokio`.

This is the most important decision section in this document. Rayon and tokio solve different problems, and picking the wrong one leads to either unnecessary complexity (tokio for pure CPU work) or poor performance (rayon for I/O-bound work). This section provides a framework for making the right choice across projects.

### The one-sentence rule

> If every task is CPU-bound and independent, use **rayon**. If any task involves waiting (network, disk, database, sleep), use **tokio**.

### Understanding the difference at the thread level

**Rayon** creates a fixed pool of OS threads (one per CPU core) and distributes work items across them. Each thread runs one item to completion before taking the next. There are no suspension points — the thread is busy the entire time.

```
Rayon (4 cores, 8 images):

  Core 0: [decode img1][resize][encode] → [decode img5][resize][encode]
  Core 1: [decode img2][resize][encode] → [decode img6][resize][encode]
  Core 2: [decode img3][resize][encode] → [decode img7][resize][encode]
  Core 3: [decode img4][resize][encode] → [decode img8][resize][encode]
             ↑                                ↑
        work-steal if                    100% CPU utilization,
        one finishes early               no idle time
```

**Tokio** creates a small pool of executor threads that run thousands of _cooperative_ tasks. Each task runs until it hits an `.await` point, then yields so another task can run. This is efficient when tasks spend most of their time waiting.

```
Tokio (2 executor threads, many tasks):

  Executor 0: [task_a: send HTTP] .await → [task_c: parse response] .await → ...
  Executor 1: [task_b: send HTTP] .await → [task_d: read file]      .await → ...
                                    ↑
                              task suspends while waiting for I/O;
                              executor runs a different task

  Blocking pool (dynamic threads):
  Thread 0: [decode img1][resize][encode]    ← CPU work lives here
  Thread 1: [decode img2][resize][encode]
```

### Decision matrix

Ask these questions in order:

#### Question 1: Is the work CPU-bound or I/O-bound?

| Workload | Bound | Example |
|----------|-------|---------|
| Image resize/encode | CPU | This project |
| SHA-256 hashing | CPU | 1.1 stretch goal |
| JSON/XML parsing | CPU | Phase 2.1 scraper |
| HTTP requests | I/O | Downloading images |
| Database queries | I/O | Phase 2.2 API |
| File reads on SSD | Both | Read-heavy, but CPU still dominates for image decode |
| File reads on HDD | I/O | Seek time dominates |

**If CPU-bound → rayon.** The work never waits, so cooperative scheduling (tokio) has no advantage. Rayon's work-stealing is optimized for exactly this case.

**If I/O-bound → tokio.** The work spends most of its time waiting. Tokio can run thousands of I/O tasks on a handful of threads because each task yields while waiting.

#### Question 2: Is your application already async?

If you're inside an Axum handler, a reqwest call, or any `async fn`, you're already in a tokio runtime. Adding rayon inside async code creates problems:

```rust
// ❌ BAD: rayon inside an async handler
async fn handle_upload(image: Bytes) -> Response {
    // par_iter() runs on rayon's thread pool, but this blocks
    // the tokio executor thread while waiting for rayon to finish.
    let results = items.par_iter().map(|x| resize(x)).collect();
    // ↑ This BLOCKS the async executor. Other HTTP requests stall.
}

// ✅ GOOD: spawn_blocking inside an async handler
async fn handle_upload(image: Bytes) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        resize(&image)  // CPU work on the blocking pool
    }).await?;
}

// ✅ ALSO GOOD: rayon inside spawn_blocking
async fn handle_batch(images: Vec<Bytes>) -> Response {
    let results = tokio::task::spawn_blocking(move || {
        images.par_iter().map(|img| resize(img)).collect::<Vec<_>>()
    }).await?;
}
```

**If already async → tokio** (use `spawn_blocking` for CPU work). You're already paying for the runtime.

**If not async → rayon** for CPU work. Don't introduce a runtime you don't need.

#### Question 3: Do tasks need to communicate?

| Pattern | Rayon | Tokio |
|---------|-------|-------|
| Independent items → collect results | `par_iter().map().collect()` | `JoinSet` + `join_next` |
| Producer → consumer pipeline | `par_bridge()` + channels | `mpsc::channel` + `tokio::spawn` |
| Shared mutable state | `Mutex` (fine-grained) or thread-local accumulation | `tokio::sync::Mutex` or channels |
| Back-pressure (slow consumer) | No built-in mechanism | `Semaphore`, bounded channels |

**Independent work → either works.** Rayon is simpler.

**Pipeline with back-pressure → tokio.** The `Semaphore` and bounded channels give you natural flow control.

#### Question 4: How many items?

| Item count | Recommendation |
|-----------|----------------|
| 1–10 | Sequential. Parallelism overhead exceeds benefit. |
| 10–100 | Rayon. Low overhead, linear speedup on multi-core. |
| 100–10,000 | Rayon or tokio with bounded concurrency. |
| 10,000+ | Tokio with `Semaphore`. Must control memory. Rayon is OK if each item is small. |

### Real-world scenarios

#### Scenario A: Thumbnail generator (this project)

```
Input:   local files on disk
Work:    decode → resize → encode (CPU-bound)
Output:  local files on disk

→ RAYON. Pure CPU work, independent items, no async needed.
```

#### Scenario B: Web scraper that downloads and processes images

```
Input:   HTTP URLs
Work:    download (I/O) → decode → resize → encode (CPU) → upload (I/O)
Output:  S3 bucket

→ TOKIO. The pipeline has I/O stages that benefit from async.
  Use spawn_blocking for the CPU resize step.
```

#### Scenario C: Axum API endpoint that resizes uploaded images

```
Context: already inside a tokio async runtime
Input:   HTTP upload body
Work:    decode → resize → encode (CPU-bound)
Output:  HTTP response

→ TOKIO (spawn_blocking). You're already async. Don't fight the runtime.
```

#### Scenario D: CI pipeline hashing 50,000 files for change detection

```
Input:   local files on SSD
Work:    read → SHA-256 (CPU-bound)
Output:  hash map

→ RAYON. Same reasoning as scenario A. On HDD, limit threads to 2-4
  to avoid seek contention.
```

#### Scenario E: Data pipeline that fetches from 3 APIs, joins results, writes to DB

```
Input:   REST APIs (I/O-bound)
Work:    HTTP requests → join → transform → SQL insert
Output:  Postgres

→ TOKIO. Almost all waiting. The transform step is fast enough to run
  inline on the executor.
```

#### Scenario F: Video transcoding pipeline

```
Input:   local video files
Work:    decode frames → process → encode (extremely CPU-bound)
Output:  local files

→ RAYON. Each frame is independent CPU work. Or use rayon inside
  spawn_blocking if you're in an async context.
```

### The hybrid pattern: rayon inside tokio

Sometimes you need both. A common production pattern is tokio for the outer orchestration (HTTP, DB) and rayon for inner CPU work:

```rust
async fn process_batch(urls: Vec<String>) -> Result<Vec<Thumbnail>> {
    // 1. Download all images concurrently (I/O-bound → tokio)
    let images: Vec<DynamicImage> = futures::future::join_all(
        urls.iter().map(|url| download_image(url))
    ).await.into_iter().collect::<Result<_>>()?;

    // 2. Resize all images in parallel (CPU-bound → rayon inside spawn_blocking)
    let thumbnails = tokio::task::spawn_blocking(move || {
        images.into_par_iter().map(|img| {
            img.thumbnail(400, 400)
        }).collect::<Vec<_>>()
    }).await?;

    Ok(thumbnails)
}
```

This gives you the best of both worlds: tokio's async I/O for downloads, rayon's work-stealing for CPU work, with `spawn_blocking` as the bridge.

### Anti-patterns to avoid

**1. Rayon inside a hot async loop**
```rust
// ❌ Each par_iter blocks the executor
for batch in batches {
    let results = batch.par_iter().map(|x| work(x)).collect();
    save(results).await;
}

// ✅ Move rayon to blocking pool
for batch in batches {
    let results = tokio::task::spawn_blocking(move || {
        batch.par_iter().map(|x| work(x)).collect()
    }).await?;
    save(results).await;
}
```

**2. Tokio spawn (not spawn_blocking) for CPU work**
```rust
// ❌ CPU work starves the executor — other tasks can't run
tokio::spawn(async { heavy_computation() });

// ✅ CPU work on the blocking pool
tokio::task::spawn_blocking(|| heavy_computation());
```

**3. Creating a new runtime inside an existing runtime**
```rust
// ❌ Panics: "Cannot start a runtime from within a runtime"
async fn handler() {
    tokio::runtime::Runtime::new().unwrap().block_on(async { ... });
}

// ✅ Just .await — you're already in a runtime
async fn handler() {
    do_work().await;
}
```

**4. Unbounded spawning without memory limits**
```rust
// ❌ Spawns 100,000 tasks, each decoding a 10 MB image = 1 TB RAM
for file in files {
    set.spawn(tokio::task::spawn_blocking(move || decode(&file)));
}

// ✅ Semaphore limits concurrent tasks
let sem = Arc::new(Semaphore::new(8));
for file in files {
    let permit = Arc::clone(&sem).acquire_owned().await?;
    set.spawn(tokio::task::spawn_blocking(move || {
        let result = decode(&file);
        drop(permit);
        result
    }));
}
```

### Summary decision flowchart

```
Is the work CPU-bound?
├── YES → Are you already inside an async runtime?
│         ├── YES → tokio::task::spawn_blocking (or rayon inside it)
│         └── NO  → rayon (simplest, best performance)
│
└── NO (I/O-bound) → tokio
                      └── Does any sub-task have CPU-heavy steps?
                          ├── YES → tokio for I/O + spawn_blocking for CPU
                          └── NO  → tokio::spawn for everything
```

📖 [rayon](https://docs.rs/rayon/latest/rayon/) · [tokio](https://docs.rs/tokio/latest/tokio/) · [tokio::task::spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)

---

## 7. Bounding Concurrency

**Current choice:** `tokio::sync::Semaphore` with `max_concurrent` defaulting to the number of CPU cores.

When processing a batch of images, each decoded image consumes ~10–50 MB of memory (width × height × 4 bytes for RGBA). Processing 1,000 images concurrently without a bound would require 10–50 GB of RAM.

### Alternatives

| Mechanism | Crate | How it bounds | Pros | Cons |
|-----------|-------|---------------|------|------|
| **Semaphore** (current) | `tokio::sync` | Permit-based. Tasks acquire before starting, release when done. | Fine-grained. Back-pressure via `.await`. | Requires `Arc` for sharing. |
| **Bounded channel** | `tokio::sync::mpsc` | Channel capacity. Sender blocks when full. | Natural for producer-consumer. | Requires channel architecture. |
| **Buffer unordered** | `futures::stream::buffer_unordered` | Limits how many futures poll concurrently. | One-liner on a stream. | Less flexible than `JoinSet` + `Semaphore`. |
| **Rayon thread pool** | `rayon` | Fixed thread count. Work items queue implicitly. | Automatic. No manual bounding. | Only for rayon, not for async. |
| **Manual chunking** | (none) | Split input into chunks, process one chunk at a time. | No primitives needed. | Uneven chunk sizes → wasted cores. |

### Code: `buffer_unordered` (alternative)

```rust
use futures::stream::{self, StreamExt};

let results: Vec<Result<Thumbnail>> = stream::iter(files)
    .map(|path| async move {
        tokio::task::spawn_blocking(move || generate_thumbnail(&path)).await?
    })
    .buffer_unordered(8)   // ← at most 8 concurrent futures
    .collect()
    .await;
```

`buffer_unordered(n)` polls at most `n` futures concurrently. It's more concise than `JoinSet` + `Semaphore` but gives less control (no dynamic permit adjustment, no abort-on-error).

### Code: Bounded channel (alternative)

```rust
use tokio::sync::mpsc;

let (tx, mut rx) = mpsc::channel::<PathBuf>(8); // buffer of 8

// Producer: sends file paths (blocks when channel is full)
tokio::spawn(async move {
    for path in files {
        tx.send(path).await.unwrap();
    }
});

// Consumer(s): receive and process
while let Some(path) = rx.recv().await {
    tokio::task::spawn_blocking(move || generate_thumbnail(&path));
}
```

### Decision Guidance

- **JoinSet with spawn-all pattern?** → `Semaphore` (current). Pairs naturally with `JoinSet`.
- **Stream-based pipeline?** → `buffer_unordered`. More concise.
- **Producer-consumer with different rates?** → Bounded `mpsc` channel.
- **Rayon?** → No bounding needed. The fixed thread pool provides implicit bounding.
- **How many concurrent tasks?** → Start with CPU core count. Increase if I/O-bound. Decrease if memory-constrained.

📖 [tokio::sync::Semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html) · [buffer_unordered](https://docs.rs/futures/latest/futures/stream/trait.StreamExt.html#method.buffer_unordered)

---

## 8. Feature Flags vs Always-On Dependencies

**Current choice:** `rayon` and `tokio` are behind optional Cargo features. The base binary has neither.

```toml
[features]
rayon = ["dep:rayon"]
tokio = ["dep:tokio"]
```

### Alternatives

| Approach | Pros | Cons |
|----------|------|------|
| **Optional features** (current) | Minimal binary when features aren't needed. Explicit opt-in. | Users must know to pass `--features`. Conditional compilation (`#[cfg]`) adds complexity. |
| **Always-on** | Simpler code — no `#[cfg]` attributes. Always available. | Larger binary. Longer compile time. Users pay for what they don't use. |
| **Separate binaries** | `image-thumbnail` (sequential), `image-thumbnail-par` (rayon). Zero conditional compilation. | Multiple binaries to maintain. Code duplication or workspace required. |
| **Runtime detection** | Check at startup if rayon/tokio are available (not practical in Rust). | Rust is compiled, not interpreted. This pattern doesn't apply. |

### Impact on compile time and binary size

| Configuration | Compile time (release, from clean) | Binary size (stripped) |
|---------------|-----------------------------------|----------------------|
| No features | ~18 s | ~4 MB |
| `--features rayon` | ~20 s (+2 s) | ~4.1 MB (+100 KB) |
| `--features tokio` | ~25 s (+7 s) | ~5.5 MB (+1.5 MB) |
| Both | ~27 s (+9 s) | ~5.6 MB (+1.6 MB) |

Tokio is noticeably heavier because it includes the multi-threaded runtime, synchronization primitives, and timer infrastructure. For a thumbnail tool, this overhead is acceptable. For a constrained environment (WASM, embedded), it might not be.

### Decision Guidance

- **Library crate?** → Feature flags. Don't force heavy dependencies on all consumers.
- **Application binary?** → Either. Feature flags add complexity but reduce bloat.
- **Study project?** → Feature flags (current) to learn the pattern. It's a Cargo skill worth having.
- **Rule of thumb:** If a dependency adds >500 KB or >5 s compile time and is only used by a subset of features, put it behind a flag.

📖 [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html) · [Optional dependencies](https://doc.rust-lang.org/cargo/reference/features.html#optional-dependencies)

---

## 9. Generic Helpers vs Inline Parallel Code

**Current choice:** Generic, reusable helper functions (`par_batch`, `spawn_blocking_batch_bounded`) that are called by the image-specific batch functions.

```rust
// Generic — works for any workload
pub fn par_batch<T, U, E, F>(items: Vec<T>, f: F) -> Vec<Result<U, E>> { ... }

// Image-specific — calls the generic helper
fn run_batch_rayon(files: Vec<PathBuf>, ...) -> Result<()> {
    let results = par_batch(files, |input| { ... });
    ...
}
```

### Alternatives

| Approach | Pros | Cons |
|----------|------|------|
| **Generic helpers** (current) | Reusable. Demonstrates trait bounds as a teaching tool. Separates concurrency from business logic. | More types to understand. Generic bounds can be confusing. |
| **Inline** | Simpler. All logic visible in one function. No indirection. | Not reusable. Concurrency and business logic are tangled. |
| **Trait-based** (`impl BatchProcessor`) | Maximum extensibility. Could swap strategies at runtime. | Over-engineered for 3 strategies with no runtime dispatch. |

### When generics are worth the complexity

The generic helpers serve two purposes in this study project:

1. **Teaching:** They make the trait bounds explicit and documented. You can see exactly what `Send + Sync + Clone + 'static` means and why each bound is required.

2. **Reuse:** The same `par_batch` function works for hashing files (1.1), sanitizing markdown documents (1.2), or any future batch workload.

In production code, you'd likely inline the parallel logic unless you're building a library that exposes batch processing as an API.

### Decision Guidance

- **Study project?** → Generic helpers (current). The exercise of writing generic bounds is the point.
- **Application code?** → Inline unless the pattern appears 3+ times.
- **Library for others?** → Generic helpers with clear documentation.
- **Rule of thumb:** Don't extract a generic until you've used the pattern at least twice. Premature abstraction is worse than mild duplication.
