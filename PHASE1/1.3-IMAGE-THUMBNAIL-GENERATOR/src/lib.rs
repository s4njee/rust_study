// ---------------------------------------------------------------------------
// Conditional imports
//
// Items gated behind a `#[cfg(feature = "…")]` attribute are compiled only
// when that feature is enabled. Putting the imports here keeps them visible
// at the top of the file while making the dependency explicit.
// ---------------------------------------------------------------------------

#[cfg(feature = "rayon")]
use rayon::prelude::*; // IntoParallelIterator, ParallelIterator, par_iter, …

#[cfg(feature = "tokio")]
use std::sync::Arc;
#[cfg(feature = "tokio")]
use tokio::sync::Semaphore;
#[cfg(feature = "tokio")]
use tokio::task::JoinSet;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The default maximum thumbnail width when `--max-width` is not specified.
///
/// 400 px matches the thumbnail size used in the s8njee-web Django photo
/// gallery, which is the original Python project this tool ports.
const DEFAULT_MAX_WIDTH: u32 = 400;

/// The default JPEG quality used when writing JPEG thumbnails.
///
/// 85 is the standard "high quality but not lossless" trade-off. It produces
/// files roughly 40–60 % smaller than quality 100 with negligible visible
/// difference for typical photographs.
const DEFAULT_JPEG_QUALITY: u8 = 85;

// ---------------------------------------------------------------------------
// Format enum
// ---------------------------------------------------------------------------

/// The image formats this tool can read and write.
///
/// This is a separate enum from `image::ImageFormat` for two reasons:
///
/// 1. We only want to expose the supported subset, so the `--format` flag
///    produces a clean, short list of options in `--help`.
/// 2. Owning the enum lets us attach project-specific helper methods (like
///    `extension` and `display_name`) without fighting orphan rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
}

impl Format {
    /// Try to guess the format from a file path's extension.
    ///
    /// Returning `Option` instead of `Result` keeps the caller in control of
    /// error handling: sometimes an unknown extension is a fatal error, and
    /// sometimes the caller wants to fall back to a different detection strategy.
    fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }

    /// Return a lowercase file extension string for this format.
    ///
    /// Used when constructing default output paths from input paths.
    fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    /// A human-readable name for progress reporting.
    fn display_name(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
        }
    }

    /// Convert to the `image` crate's own `ImageFormat` enum.
    fn to_image_format(self) -> image::ImageFormat {
        match self {
            Self::Jpeg => image::ImageFormat::Jpeg,
            Self::Png => image::ImageFormat::Png,
            Self::Webp => image::ImageFormat::WebP,
        }
    }
}

// ---------------------------------------------------------------------------
// Parallel strategy enum
// ---------------------------------------------------------------------------

/// The concurrency strategy used for batch processing.
///
/// This enum lives in the CLI args and is used to dispatch to the right
/// batch implementation at runtime. All three variants are always compiled
/// into the binary so `--help` always shows the full option list; the
/// runtime check for a missing feature produces a helpful error message
/// instead of a silent no-op.
///
/// # When to choose each strategy
///
/// | Strategy   | Best for                                   | Avoid when               |
/// |------------|--------------------------------------------|--------------------------|
/// | Sequential | Debugging, low-core machines, tiny batches | Large batches on many cores |
/// | Rayon      | CPU-bound uniform workloads (image resize)  | Heavy async I/O           |
/// | Tokio      | Mixed CPU + async I/O (e.g. download→resize)| Pure CPU work (prefer rayon) |
///
/// For batch image resizing, rayon is the better fit because every task is
/// CPU-bound and roughly the same cost. Tokio shines when some tasks block
/// on network or disk I/O while others run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum Parallel {
    /// Process images one at a time on the calling thread (default).
    #[default]
    Sequential,

    /// Use rayon's work-stealing thread pool.
    ///
    /// Requires the `rayon` feature: `cargo build --features rayon`.
    Rayon,

    /// Use tokio's async runtime with `spawn_blocking` for each image.
    ///
    /// Requires the `tokio` feature: `cargo build --features tokio`.
    Tokio,
}

// ---------------------------------------------------------------------------
// Args
// ---------------------------------------------------------------------------

/// `clap` turns this struct into a full command-line interface.
///
/// Keeping `Args` in the library (not just `main.rs`) means `run` can be
/// called directly from tests with typed arguments — the same pattern used
/// in 1.1 and 1.2: `clap` lives at the edge; the rest of the code works
/// with plain Rust values.
#[derive(Debug, Parser)]
#[command(
    name = "image-thumbnail",
    version,
    about = "Resize an image to a thumbnail, preserving aspect ratio. Supports JPEG, PNG, and WebP."
)]
pub struct Args {
    /// Input image file, or a directory when `--batch` is used.
    ///
    /// Format is auto-detected from the file extension. Supported inputs:
    /// .jpg / .jpeg, .png, .webp (and any other format the `image` crate
    /// can decode, though only those three can be written as output).
    pub input: PathBuf,

    /// Output path for the thumbnail. Ignored in `--batch` mode.
    ///
    /// If omitted, the thumbnail is written next to the input with a
    /// `_thumb` suffix: `photo.jpg` → `photo_thumb.jpg`.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Maximum width of the thumbnail in pixels.
    ///
    /// Height is scaled proportionally. If the input is already narrower,
    /// it is saved without resizing to avoid unnecessary re-encoding.
    #[arg(short = 'w', long, default_value_t = DEFAULT_MAX_WIDTH)]
    pub max_width: u32,

    /// JPEG quality from 1 (smallest file) to 100 (best quality).
    ///
    /// Only applied when the output format is JPEG. PNG and WebP use their
    /// own quality/compression models.
    #[arg(long, default_value_t = DEFAULT_JPEG_QUALITY)]
    pub quality: u8,

    /// Override the output format.
    ///
    /// When omitted, the format is inferred from the output extension; if no
    /// output path was given, the format is copied from the input.
    #[arg(long)]
    pub format: Option<Format>,

    /// Process all images in the input directory instead of a single file.
    ///
    /// Thumbnails are written alongside each source image using the default
    /// `_thumb` naming convention.
    #[arg(long)]
    pub batch: bool,

    /// Concurrency strategy for batch mode.
    ///
    /// Has no effect unless `--batch` is also passed.
    #[arg(long, default_value = "sequential")]
    pub parallel: Parallel,

    /// Maximum number of images processed concurrently (tokio mode only).
    ///
    /// Defaults to the number of logical CPU cores. This bounds memory
    /// pressure: without a limit, all images would be decoded into memory
    /// simultaneously before any are written back to disk.
    #[arg(long)]
    pub max_concurrent: Option<usize>,

    /// Suppress per-file progress lines.
    #[arg(short, long)]
    pub quiet: bool,
}

// ---------------------------------------------------------------------------
// Dimensions
// ---------------------------------------------------------------------------

/// The pixel dimensions of an image.
///
/// We define our own small struct rather than passing `(u32, u32)` tuples
/// everywhere. Named fields eliminate the "is this width-height or
/// height-width?" ambiguity that tuple pairs invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

impl Dimensions {
    /// Read the dimensions directly from a decoded `DynamicImage`.
    fn from_image(img: &DynamicImage) -> Self {
        let (width, height) = img.dimensions();
        Self { width, height }
    }

    /// Compute new dimensions after scaling width down to `max_width` while
    /// preserving the original aspect ratio.
    ///
    /// If the image is already at or below `max_width`, the original
    /// dimensions are returned unchanged — no unnecessary re-encoding.
    ///
    /// We use floating-point arithmetic for the scale factor to avoid integer
    /// truncation errors. For example, a 1000 × 333 image scaled to width 400
    /// should produce height 133, not 132.
    pub fn scale_to_width(self, max_width: u32) -> Self {
        if self.width <= max_width {
            return self;
        }
        let scale = max_width as f64 / self.width as f64;
        let new_height = (self.height as f64 * scale).round() as u32;
        Self { width: max_width, height: new_height.max(1) }
    }
}

// ---------------------------------------------------------------------------
// Result and config types
// ---------------------------------------------------------------------------

/// The result produced after processing one image.
///
/// Returning a typed result struct instead of printing inside the generator
/// function keeps side effects separate from computation. Tests can verify
/// dimensions and paths without touching stdout at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailResult {
    pub input_path: PathBuf,
    pub output_path: PathBuf,
    pub original: Dimensions,
    pub thumbnail: Dimensions,
    pub format: Format,
}

/// Configuration that applies to every thumbnail in a run.
///
/// Collecting settings into a struct rather than passing them as individual
/// function arguments is a practical pattern when more than two or three
/// related values need to travel together.
///
/// Note that `ThumbnailConfig` is `Copy`. This matters for the parallel
/// implementations: a `Copy` type can be moved into every closure without
/// cloning, and it automatically satisfies the `Send + Sync` bounds required
/// by both rayon and tokio closures.
#[derive(Debug, Clone, Copy)]
pub struct ThumbnailConfig {
    pub max_width: u32,
    pub jpeg_quality: u8,
}

impl Default for ThumbnailConfig {
    fn default() -> Self {
        Self { max_width: DEFAULT_MAX_WIDTH, jpeg_quality: DEFAULT_JPEG_QUALITY }
    }
}

// ---------------------------------------------------------------------------
// Core: generate_thumbnail
// ---------------------------------------------------------------------------

/// Generate one thumbnail from `input` and write it to `output`.
///
/// This is the core library function. Keeping it free-standing (not a method
/// on a struct) makes it trivial to call from tests, other binaries, a web
/// handler, or — crucially for this project — a closure passed to a parallel
/// iterator or async task.
///
/// The three main steps are intentionally separated:
/// 1. **Load** — open and fully decode the source image into memory.
/// 2. **Resize** — produce a smaller `DynamicImage` if needed.
/// 3. **Save** — encode the resized image and write it to disk.
pub fn generate_thumbnail(
    input: &Path,
    output: &Path,
    format: Format,
    config: ThumbnailConfig,
) -> Result<ThumbnailResult> {
    // --- 1. load ------------------------------------------------------------
    //
    // `image::open` detects the format from the file extension and decodes
    // the image into a `DynamicImage`. `DynamicImage` is an enum that wraps
    // the decoded pixel buffer regardless of colour depth (Rgb8, Rgba8, …).
    let img = image::open(input)
        .with_context(|| format!("failed to open image '{}'", input.display()))?;

    let original = Dimensions::from_image(&img);
    let target = original.scale_to_width(config.max_width);

    // --- 2. resize ----------------------------------------------------------
    //
    // `DynamicImage::thumbnail` uses Nearest Neighbour for a fast initial
    // downsample, then Lanczos3 for the final pass — a good balance of speed
    // and quality. `resize` with `FilterType::Lanczos3` is slightly higher
    // quality but slower; for web thumbnails `thumbnail` is the right default.
    //
    // We skip resizing entirely when the image is already small enough to
    // avoid a decode-encode round trip that could introduce JPEG artefacts.
    let resized = if target == original { img } else { img.thumbnail(target.width, target.height) };
    let thumbnail_dims = Dimensions::from_image(&resized);

    // --- 3. save ------------------------------------------------------------
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create output directory '{}'", parent.display())
            })?;
        }
    }

    save_image(&resized, output, format, config.jpeg_quality)?;

    Ok(ThumbnailResult {
        input_path: input.to_path_buf(),
        output_path: output.to_path_buf(),
        original,
        thumbnail: thumbnail_dims,
        format,
    })
}

/// Encode and write a `DynamicImage` to disk in the requested format.
///
/// We dispatch on `Format` rather than relying on `image`'s path-based
/// auto-detection because JPEG quality control requires using `JpegEncoder`
/// directly. The standard `img.save(path)` API writes JPEG at a fixed
/// internal default quality with no way to override it.
///
/// `BufWriter` amortises the many small `write` calls that an encoder makes
/// into fewer, larger system calls — a meaningful win on larger images.
fn save_image(img: &DynamicImage, output: &Path, format: Format, jpeg_quality: u8) -> Result<()> {
    match format {
        Format::Jpeg => {
            let file = File::create(output)
                .with_context(|| format!("failed to create '{}'", output.display()))?;
            let encoder = JpegEncoder::new_with_quality(BufWriter::new(file), jpeg_quality);
            img.write_with_encoder(encoder)
                .with_context(|| format!("failed to encode JPEG '{}'", output.display()))?;
        }
        Format::Png | Format::Webp => {
            img.save_with_format(output, format.to_image_format()).with_context(|| {
                format!("failed to save {} '{}'", format.display_name(), output.display())
            })?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Determine the output file path.
///
/// Resolution order (highest → lowest priority):
/// 1. An explicit `--output` path.
/// 2. A derived path: the input stem with `_thumb` appended, in the same
///    directory, using the detected format's extension.
///
/// Example derivation:
/// ```text
/// input:   /photos/holiday.jpg  format: PNG
/// derived: /photos/holiday_thumb.png
/// ```
pub fn resolve_output(input: &Path, explicit: Option<&Path>, format: Format) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let file_name = format!("{stem}_thumb.{}", format.extension());
    match input.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

/// Determine which `Format` to use for the output image.
///
/// Resolution order — highest to lowest priority:
/// 1. `--format <FMT>` (explicit CLI override)
/// 2. The extension of `output` (if an output path was given)
/// 3. The extension of `input`
///
/// This function never touches the filesystem — it only inspects path strings.
pub fn detect_format(
    input: &Path,
    output: Option<&Path>,
    override_format: Option<Format>,
) -> Result<Format> {
    if let Some(fmt) = override_format {
        return Ok(fmt);
    }
    if let Some(out) = output {
        if let Some(fmt) = Format::from_path(out) {
            return Ok(fmt);
        }
    }
    Format::from_path(input).with_context(|| {
        format!(
            "could not detect image format from '{}'; \
             pass --format jpeg|png|webp to specify one",
            input.display()
        )
    })
}

/// Return `true` if `path` looks like a thumbnail produced by a previous run.
///
/// We detect thumbnails by their `_thumb` suffix so that batch mode does not
/// re-thumbnail its own output on subsequent runs.
fn is_thumb_file(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.ends_with("_thumb"))
}

/// Print a one-line summary of a completed thumbnail operation.
fn print_result(result: &ThumbnailResult) {
    println!(
        "{} → {}  ({} {} × {} → {} × {})",
        result.input_path.display(),
        result.output_path.display(),
        result.format.display_name(),
        result.original.width,
        result.original.height,
        result.thumbnail.width,
        result.thumbnail.height,
    );
}

// ---------------------------------------------------------------------------
// CLI entry points
// ---------------------------------------------------------------------------

/// Parse arguments, run the tool, and map failures to exit codes.
pub fn main_exit_code() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// Execute the CLI workflow: dispatch to single-file or batch mode.
pub fn run(args: Args) -> Result<()> {
    let config = ThumbnailConfig { max_width: args.max_width, jpeg_quality: args.quality };

    if args.batch {
        let files = collect_batch_files(&args.input)?;
        run_batch(files, config, args.format, args.parallel, args.max_concurrent, args.quiet)
    } else {
        let format = detect_format(&args.input, args.output.as_deref(), args.format)?;
        let output = resolve_output(&args.input, args.output.as_deref(), format);
        let result = generate_thumbnail(&args.input, &output, format, config)?;
        if !args.quiet {
            print_result(&result);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Batch: file collection (shared across all strategies)
// ---------------------------------------------------------------------------

/// Walk `dir` one level deep and collect every supported image file, skipping
/// any file whose stem ends with `_thumb` (output from a previous run).
///
/// Extracting file collection into its own function means every parallel
/// strategy starts from the same `Vec<PathBuf>`, which keeps the strategies
/// themselves focused on the concurrency mechanics.
fn collect_batch_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        bail!("'{}' is not a directory (batch mode requires a directory)", dir.display());
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(dir).min_depth(1).max_depth(1) {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                eprintln!("warning: {err}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if is_thumb_file(&path) {
            continue;
        }
        // Only include files we can identify as a supported image format.
        if Format::from_path(&path).is_some() {
            files.push(path);
        }
    }

    Ok(files)
}

// ---------------------------------------------------------------------------
// Batch: strategy dispatcher
// ---------------------------------------------------------------------------

/// Dispatch to the correct batch implementation based on `parallel`.
///
/// The `#[cfg]` pairs below are the idiom for "compile this only when the
/// feature is present; otherwise emit a helpful runtime error":
///
/// ```text
/// #[cfg(feature = "rayon")]          → compiled in, executes and returns
/// #[cfg(not(feature = "rayon"))]    → compiled in instead, bails with message
/// ```
///
/// Both arms cannot be compiled at the same time, so there is no dead-code
/// warning. The runtime error message points the user to the right fix.
fn run_batch(
    files: Vec<PathBuf>,
    config: ThumbnailConfig,
    format_override: Option<Format>,
    parallel: Parallel,
    max_concurrent: Option<usize>,
    quiet: bool,
) -> Result<()> {
    match parallel {
        Parallel::Sequential => run_batch_sequential(files, config, format_override, quiet),

        Parallel::Rayon => {
            #[cfg(feature = "rayon")]
            return run_batch_rayon(files, config, format_override, quiet);
            #[cfg(not(feature = "rayon"))]
            bail!(
                "--parallel rayon requires the rayon feature; \
                 rebuild with: cargo build --features rayon"
            );
        }

        Parallel::Tokio => {
            // Default concurrency = number of logical CPU cores.
            // `available_parallelism` is stable since Rust 1.59 and returns
            // the number of threads the OS has made available to this process.
            let concurrency = max_concurrent
                .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
                .unwrap_or(4);

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

// ---------------------------------------------------------------------------
// Batch strategy 1: Sequential
// ---------------------------------------------------------------------------

/// Process files one at a time on the calling thread.
///
/// This is intentionally simple. Read it before the parallel versions to
/// understand what the parallel code is doing to this baseline.
fn run_batch_sequential(
    files: Vec<PathBuf>,
    config: ThumbnailConfig,
    format_override: Option<Format>,
    quiet: bool,
) -> Result<()> {
    let (mut processed, mut errors) = (0u32, 0u32);

    for input in &files {
        let format = match detect_format(input, None, format_override) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let output = resolve_output(input, None, format);
        match generate_thumbnail(input, &output, format, config) {
            Ok(result) => {
                processed += 1;
                if !quiet {
                    print_result(&result);
                }
            }
            Err(err) => {
                errors += 1;
                eprintln!("warning: {}: {err:#}", input.display());
            }
        }
    }

    if !quiet {
        println!("\nBatch complete (sequential) — processed: {processed}, errors: {errors}");
    }
    Ok(())
}

// ============================================================
// STUDY SECTION — RAYON DATA PARALLELISM
// ============================================================
//
// Rayon is Rust's data-parallelism library. It provides parallel versions of
// standard iterator adapters (`par_iter`, `par_bridge`, `into_par_iter`, …)
// that automatically distribute work across a shared work-stealing thread pool
// backed by one OS thread per logical CPU core.
//
// KEY CONCEPTS
//
// 1. `par_iter()` vs `into_par_iter()`
//    - `par_iter()` borrows each element (&T) — the collection is not consumed.
//    - `into_par_iter()` moves each element (T) — the collection is consumed.
//    Use `into_par_iter` when you want to move owned values into closures,
//    which avoids cloning.
//
// 2. `Sync` and `Send` bounds on the closure
//    - Multiple threads call `f` concurrently, so `f` must be `Sync`
//      (a `&F` reference can be shared between threads safely).
//    - Rayon moves `f` to the thread pool, so `f` must also be `Send`.
//    - Any data the closure *captures* must also be `Send + Sync`.
//    - `Copy` types (like `ThumbnailConfig`) are automatically `Send + Sync`.
//
// 3. Work-stealing scheduler
//    Rayon's thread pool splits work recursively. If one thread finishes its
//    share early, it "steals" tasks from the tail of another thread's queue.
//    This self-balancing behaviour means you get good CPU utilisation even
//    when tasks vary in duration.
//
// 4. When to use rayon
//    - CPU-bound batch workloads (image resize, hashing, compression).
//    - Tasks are roughly uniform in cost (rayon can't rebalance mid-task).
//    - You do NOT need async I/O (use tokio for that).
//
// GENERIC HELPER — par_batch
//
// `par_batch` captures the core rayon pattern in a form that can be reused
// for any CPU-bound workload, not just image processing. The only difference
// between it and a sequential `items.iter().map(f).collect()` is the single
// word `par` in `.into_par_iter()`.
//
// Trait bounds explained:
//   T: Send          — items can be moved across thread boundaries
//   U: Send          — results can be moved across thread boundaries
//   E: Send          — errors can be moved across thread boundaries
//   F: Fn(T) → …    — closure takes ownership of each item (into_par_iter)
//   F: Sync + Send   — closure is safe to call from multiple threads at once
// ============================================================

/// Apply `f` to every item in `items` using rayon's work-stealing thread pool,
/// collecting the results in the same order as the input.
///
/// This is the canonical rayon batch pattern. Adapting it to your own code:
///
/// ```ignore
/// // Before (sequential):
/// let results: Vec<_> = items.into_iter().map(|x| work(x)).collect();
///
/// // After (parallel — add `use rayon::prelude::*` and swap one word):
/// let results: Vec<_> = items.into_par_iter().map(|x| work(x)).collect();
/// ```
///
/// The result order is preserved — rayon guarantees that `.collect()` on a
/// parallel iterator produces the same ordering as the sequential version.
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

/// Process a batch of image files using rayon's work-stealing thread pool.
///
/// This calls `par_batch` with an image-processing closure, showing how the
/// generic helper is used in practice. The closure is identical to the body
/// of the sequential `for` loop — the only structural change is that it runs
/// on multiple threads concurrently.
#[cfg(feature = "rayon")]
fn run_batch_rayon(
    files: Vec<PathBuf>,
    config: ThumbnailConfig,
    format_override: Option<Format>,
    quiet: bool,
) -> Result<()> {
    // The closure captures `config` and `format_override` by copy.
    // `ThumbnailConfig: Copy` means the compiler inserts a bitwise copy for
    // each thread automatically — no explicit `.clone()` required.
    let results = par_batch(files, |input| -> Result<ThumbnailResult> {
        let format = detect_format(&input, None, format_override)?;
        let output = resolve_output(&input, None, format);
        generate_thumbnail(&input, &output, format, config)
    });

    // Collect results after all threads finish.
    //
    // We gather results first, then report, to prevent interleaved terminal
    // output from multiple threads printing at the same time. In production
    // code you would use a `Mutex<Vec<_>>` or a channel to stream results as
    // they complete; for a study project, batch collection is simpler.
    let (mut processed, mut errors) = (0u32, 0u32);
    for result in results {
        match result {
            Ok(thumb) => {
                processed += 1;
                if !quiet {
                    print_result(&thumb);
                }
            }
            Err(err) => {
                errors += 1;
                eprintln!("warning: {err:#}");
            }
        }
    }

    if !quiet {
        println!("\nBatch complete (rayon) — processed: {processed}, errors: {errors}");
    }
    Ok(())
}

// ============================================================
// STUDY SECTION — TOKIO CONCURRENT BATCH
// ============================================================
//
// Tokio is Rust's most widely used async runtime. Unlike rayon, tokio is
// designed for I/O-bound concurrency: it multiplexes many async tasks over a
// small fixed set of OS threads using cooperative scheduling.
//
// KEY CONCEPTS
//
// 1. CPU-bound vs I/O-bound work
//    Image decoding and encoding is CPU-bound — it never yields to the
//    executor. Running it directly inside a tokio task (via `tokio::spawn`)
//    would starve other tasks of CPU time. The solution is `spawn_blocking`,
//    which moves the blocking work to a separate OS thread pool that tokio
//    manages independently of its async executor threads.
//
//    Rule of thumb:
//      - async/await code with .await → tokio::spawn
//      - blocking / CPU-bound code   → tokio::task::spawn_blocking
//
// 2. JoinSet
//    `JoinSet<T>` tracks a collection of tokio tasks and lets you await them
//    as they complete. `join_next().await` returns `Some(Result<T, JoinError>)`
//    for the next finished task, or `None` when all tasks have finished.
//    The outer `Result` is a `JoinError` (the task panicked or was cancelled);
//    the inner type is whatever the task returned.
//
// 3. Semaphore — bounding concurrency
//    Without a bound, the loop below would spawn all N tasks immediately,
//    loading all N images into memory at once. A `Semaphore` with `k` permits
//    allows at most `k` images to be in-flight simultaneously.
//
//    The pattern:
//      let sem = Arc::new(Semaphore::new(k));
//      for item in items {
//          let permit = Arc::clone(&sem).acquire_owned().await?;
//          set.spawn(async move {
//              let result = spawn_blocking(move || { work(item); drop(permit) }).await?;
//              result
//          });
//      }
//
//    - `Arc` lets each spawned task share ownership of the semaphore.
//    - `acquire_owned()` returns an `OwnedSemaphorePermit` which is `Send`,
//      so it can be moved into async blocks across `.await` points.
//    - Dropping `permit` inside the blocking closure releases the slot as
//      soon as the CPU work finishes, not when the outer async task is polled.
//
// 4. Bridging sync → async (block_on)
//    `run` is a synchronous function. To call async code from it we create a
//    `tokio::runtime::Runtime` and call `block_on`, which runs the async
//    function to completion on the calling thread. In a fully async binary
//    (using `#[tokio::main]`) you would use `.await` directly instead.
//
// GENERIC HELPERS
//
// Two helpers are provided:
//   - `spawn_blocking_batch`         — unbounded (spawns all tasks at once)
//   - `spawn_blocking_batch_bounded` — bounded by a `Semaphore`
//
// For real workloads always prefer the bounded version. The unbounded version
// is included to make the progression from simple → safe explicit.
// ============================================================

/// Apply `f` to every item using tokio's blocking thread pool, collecting
/// results as tasks complete.
///
/// **Unbounded version.** All tasks are spawned immediately. Suitable only
/// for small batches or tasks with small memory footprints. For large batches,
/// prefer `spawn_blocking_batch_bounded` to cap memory usage.
///
/// # How to adapt this to your own code
///
/// Replace the `generate_thumbnail` closure with any `FnOnce` that does
/// CPU-bound or blocking I/O work. Make sure captured variables are `'static`
/// (no borrowed references that might be dropped while the task runs).
///
/// ```ignore
/// let results = spawn_blocking_batch(files, |path| {
///     heavy_cpu_work(&path)       // your blocking function here
/// }).await;
/// ```
#[cfg(feature = "tokio")]
pub async fn spawn_blocking_batch<T, U, F>(items: Vec<T>, f: F) -> Vec<anyhow::Result<U>>
where
    T: Send + 'static,
    U: Send + 'static,
    // `Clone` so we can hand one copy of `f` to each spawned task.
    // `Sync` is not required here because each task gets its own clone.
    F: Fn(T) -> anyhow::Result<U> + Send + Clone + 'static,
{
    let mut set: JoinSet<anyhow::Result<U>> = JoinSet::new();

    for item in items {
        let f = f.clone();
        set.spawn(async move {
            // `spawn_blocking` moves the closure to tokio's blocking thread
            // pool. The pool grows dynamically (default cap: 512 threads).
            // The `move` keyword transfers ownership of `item` and `f` into
            // the closure so neither outlives the task.
            //
            // We `.await` the JoinHandle here so the outer JoinSet sees
            // `anyhow::Result<U>` directly. Without the `.await`, the JoinSet
            // would contain `Result<anyhow::Result<U>, JoinError>` — a doubly
            // wrapped result that is harder to work with.
            tokio::task::spawn_blocking(move || f(item))
                .await
                .unwrap_or_else(|e| anyhow::bail!("task panicked: {e}"))
        });
    }

    // Collect results as tasks complete (order is not guaranteed).
    // `join_next().await` suspends until the next task finishes, then returns:
    //   Ok(inner_result)  — task completed; inner_result is what f() returned
    //   Err(join_error)   — task panicked or was cancelled
    let mut results = Vec::with_capacity(set.len());
    while let Some(join_result) = set.join_next().await {
        results.push(join_result.unwrap_or_else(|e| anyhow::bail!("task panicked: {e}")));
    }
    results
}

/// Apply `f` to every item using tokio's blocking thread pool, with at most
/// `max_concurrent` tasks running at the same time.
///
/// **Bounded version (prefer this in production).** The `Semaphore` prevents
/// unbounded memory growth by ensuring no more than `max_concurrent` images
/// are decoded into memory simultaneously.
///
/// # Semaphore mechanics
///
/// A `Semaphore` holds a fixed number of "permits". Each task must acquire a
/// permit before starting its blocking work. When the permit is dropped (at
/// the end of the blocking closure), the slot becomes available for the next
/// task. If all permits are held, `acquire_owned().await` suspends the calling
/// async task until one is released — this is cooperative back-pressure.
///
/// # Why `Arc<Semaphore>`?
///
/// `Arc` (Atomic Reference Count) gives each spawned task shared ownership of
/// the semaphore without a lifetime dependency on the caller's stack frame.
/// `Arc::clone` increments the reference count (cheap); when the last `Arc`
/// is dropped, the semaphore is deallocated. Every tokio task receives its own
/// `Arc<Semaphore>` clone so they can all reach the shared semaphore.
#[cfg(feature = "tokio")]
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
    // `Arc` is needed because the semaphore must outlive the spawning loop AND
    // every spawned task. Moving it into each task would require transferring
    // ownership, but we want to share it, hence reference counting.
    let semaphore = Arc::new(Semaphore::new(max_concurrent));
    let mut set: JoinSet<anyhow::Result<U>> = JoinSet::new();

    for item in items {
        let f = f.clone();
        // Clone the Arc to give this task a reference to the shared semaphore.
        // `Arc::clone` is O(1) — it only increments an atomic counter.
        let sem = Arc::clone(&semaphore);

        set.spawn(async move {
            // --- acquire permit (back-pressure point) -----------------------
            //
            // `acquire_owned()` returns an `OwnedSemaphorePermit` that is
            // `Send`, meaning it can cross `.await` points and be moved into
            // closures. A regular `acquire()` returns a `SemaphorePermit`
            // with a lifetime tied to `&sem`, which is NOT `Send`.
            //
            // If all permits are taken, this `.await` suspends the current
            // async task without blocking the executor thread — other tasks
            // continue to make progress while we wait.
            let permit = sem
                .acquire_owned()
                .await
                .expect("semaphore closed while tasks were still running");

            // --- run CPU-bound work on a blocking thread --------------------
            //
            // `permit` is moved into the closure. When the closure returns
            // (and therefore `permit` is dropped), the semaphore slot is
            // released — allowing the next waiting task to proceed.
            //
            // We drop `permit` explicitly at the end of the closure so that
            // it is clear exactly when the slot becomes available. In
            // production you might want to drop it slightly earlier, before
            // any logging or cleanup, to maximise throughput.
            tokio::task::spawn_blocking(move || {
                let result = f(item);
                drop(permit); // release the semaphore slot
                result
            })
            .await
            // Flatten JoinError (panic/cancellation) into anyhow::Error.
            .unwrap_or_else(|e| anyhow::bail!("task panicked: {e}"))
        });
    }

    let mut results = Vec::with_capacity(set.len());
    while let Some(join_result) = set.join_next().await {
        results.push(join_result.unwrap_or_else(|e| anyhow::bail!("outer join error: {e}")));
    }
    results
}

/// Async inner function: process image files with bounded tokio concurrency.
///
/// This calls `spawn_blocking_batch_bounded` with the image-processing closure.
/// The separation between this async fn and `run_batch_tokio` (the sync wrapper)
/// is intentional: async fns cannot be called directly from sync code, so we
/// need the `block_on` bridge in the wrapper.
#[cfg(feature = "tokio")]
async fn run_batch_tokio_inner(
    files: Vec<PathBuf>,
    config: ThumbnailConfig,
    format_override: Option<Format>,
    max_concurrent: usize,
    quiet: bool,
) -> Result<()> {
    let results = spawn_blocking_batch_bounded(
        files,
        move |input: PathBuf| -> anyhow::Result<ThumbnailResult> {
            // This closure body is identical to the sequential loop body.
            // The concurrency model changes around the closure, not inside it.
            let format = detect_format(&input, None, format_override)?;
            let output = resolve_output(&input, None, format);
            generate_thumbnail(&input, &output, format, config)
        },
        max_concurrent,
    )
    .await;

    let (mut processed, mut errors) = (0u32, 0u32);
    for result in results {
        match result {
            Ok(thumb) => {
                processed += 1;
                if !quiet {
                    print_result(&thumb);
                }
            }
            Err(err) => {
                errors += 1;
                eprintln!("warning: {err:#}");
            }
        }
    }

    if !quiet {
        println!("\nBatch complete (tokio) — processed: {processed}, errors: {errors}");
    }
    Ok(())
}

/// Synchronous wrapper: creates a tokio runtime and drives the async batch.
///
/// `Runtime::new()` builds a multi-threaded executor (equivalent to
/// `#[tokio::main]`). `block_on` then runs the given future to completion
/// on the calling thread, returning when it finishes.
///
/// This is the standard bridge from synchronous code into an async function.
/// In a fully async binary (annotated with `#[tokio::main]`), you would
/// call `run_batch_tokio_inner(...).await` directly and omit this wrapper.
#[cfg(feature = "tokio")]
fn run_batch_tokio(
    files: Vec<PathBuf>,
    config: ThumbnailConfig,
    format_override: Option<Format>,
    max_concurrent: usize,
    quiet: bool,
) -> Result<()> {
    tokio::runtime::Runtime::new()
        .context("failed to start tokio runtime")?
        .block_on(run_batch_tokio_inner(files, config, format_override, max_concurrent, quiet))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // -----------------------------------------------------------------------
    // Test helper
    // -----------------------------------------------------------------------

    /// Create a small solid-colour image in a temp directory.
    ///
    /// PNG is lossless, so `image::open` will always decode the exact same
    /// pixel values back, making it the most reliable format for test fixtures.
    fn make_test_image(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
        let path = dir.join(name);
        DynamicImage::new_rgb8(width, height).save(&path).expect("write test image");
        path
    }

    // -----------------------------------------------------------------------
    // Dimensions
    // -----------------------------------------------------------------------

    #[test]
    fn scale_to_width_shrinks_wide_image_proportionally() {
        let dims = Dimensions { width: 800, height: 600 };
        let scaled = dims.scale_to_width(400);
        assert_eq!(scaled.width, 400);
        assert_eq!(scaled.height, 300);
    }

    #[test]
    fn scale_to_width_leaves_narrow_image_unchanged() {
        let dims = Dimensions { width: 200, height: 150 };
        let scaled = dims.scale_to_width(400);
        assert_eq!(scaled, dims);
    }

    #[test]
    fn scale_to_width_handles_exact_match() {
        let dims = Dimensions { width: 400, height: 300 };
        assert_eq!(dims.scale_to_width(400), dims);
    }

    #[test]
    fn scale_to_width_rounds_height_to_nearest_pixel() {
        // 1000 × 333 at scale 0.4 → height = 133.2 → rounds to 133
        let dims = Dimensions { width: 1000, height: 333 };
        let scaled = dims.scale_to_width(400);
        assert_eq!(scaled.width, 400);
        assert_eq!(scaled.height, 133);
    }

    // -----------------------------------------------------------------------
    // Format detection
    // -----------------------------------------------------------------------

    #[test]
    fn detect_format_from_jpeg_extension() {
        assert_eq!(detect_format(Path::new("photo.jpg"), None, None).unwrap(), Format::Jpeg);
    }

    #[test]
    fn detect_format_from_jpeg_long_extension() {
        assert_eq!(detect_format(Path::new("photo.jpeg"), None, None).unwrap(), Format::Jpeg);
    }

    #[test]
    fn detect_format_from_png_extension() {
        assert_eq!(detect_format(Path::new("photo.png"), None, None).unwrap(), Format::Png);
    }

    #[test]
    fn detect_format_from_webp_extension() {
        assert_eq!(detect_format(Path::new("photo.webp"), None, None).unwrap(), Format::Webp);
    }

    #[test]
    fn detect_format_prefers_explicit_override() {
        let fmt = detect_format(Path::new("photo.png"), None, Some(Format::Jpeg)).unwrap();
        assert_eq!(fmt, Format::Jpeg);
    }

    #[test]
    fn detect_format_uses_output_extension_over_input() {
        let fmt =
            detect_format(Path::new("photo.jpg"), Some(Path::new("out.png")), None).unwrap();
        assert_eq!(fmt, Format::Png);
    }

    #[test]
    fn detect_format_fails_for_unknown_extension() {
        // .bmp is not in our three-format subset even though `image` can read it.
        assert!(detect_format(Path::new("photo.bmp"), None, None).is_err());
    }

    // -----------------------------------------------------------------------
    // Output path resolution
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_output_inserts_thumb_suffix_for_jpeg() {
        let out = resolve_output(Path::new("/photos/holiday.jpg"), None, Format::Jpeg);
        assert_eq!(out, PathBuf::from("/photos/holiday_thumb.jpg"));
    }

    #[test]
    fn resolve_output_changes_extension_when_format_differs() {
        let out = resolve_output(Path::new("/photos/holiday.jpg"), None, Format::Png);
        assert_eq!(out, PathBuf::from("/photos/holiday_thumb.png"));
    }

    #[test]
    fn resolve_output_respects_explicit_path() {
        let out = resolve_output(
            Path::new("/photos/holiday.jpg"),
            Some(Path::new("/out/small.jpg")),
            Format::Jpeg,
        );
        assert_eq!(out, PathBuf::from("/out/small.jpg"));
    }

    // -----------------------------------------------------------------------
    // generate_thumbnail
    // -----------------------------------------------------------------------

    #[test]
    fn generate_thumbnail_resizes_jpeg_correctly() {
        let dir = tempdir().expect("tempdir");
        let input = make_test_image(dir.path(), "test.jpg", 800, 600);
        let output = dir.path().join("test_thumb.jpg");

        let result = generate_thumbnail(&input, &output, Format::Jpeg, ThumbnailConfig::default())
            .expect("generate_thumbnail");

        assert_eq!(result.original, Dimensions { width: 800, height: 600 });
        assert_eq!(result.thumbnail, Dimensions { width: 400, height: 300 });
        assert!(output.exists());
    }

    #[test]
    fn generate_thumbnail_skips_resize_for_small_images() {
        let dir = tempdir().expect("tempdir");
        let input = make_test_image(dir.path(), "small.jpg", 100, 75);
        let output = dir.path().join("small_thumb.jpg");

        let result = generate_thumbnail(&input, &output, Format::Jpeg, ThumbnailConfig::default())
            .expect("generate_thumbnail");

        assert_eq!(result.thumbnail, Dimensions { width: 100, height: 75 });
    }

    #[test]
    fn generate_thumbnail_writes_png_output() {
        let dir = tempdir().expect("tempdir");
        let input = make_test_image(dir.path(), "source.png", 800, 600);
        let output = dir.path().join("thumb.png");

        generate_thumbnail(&input, &output, Format::Png, ThumbnailConfig::default())
            .expect("generate_thumbnail");

        let saved = image::open(&output).expect("open saved png");
        assert_eq!(saved.width(), 400);
        assert_eq!(saved.height(), 300);
    }

    #[test]
    fn generate_thumbnail_can_transcode_jpeg_to_png() {
        let dir = tempdir().expect("tempdir");
        let input = make_test_image(dir.path(), "source.jpg", 800, 600);
        let output = dir.path().join("thumb.png");

        generate_thumbnail(&input, &output, Format::Png, ThumbnailConfig::default())
            .expect("transcode");

        let saved = image::open(&output).expect("open saved png");
        assert_eq!(saved.width(), 400);
    }

    #[test]
    fn generate_thumbnail_respects_custom_max_width() {
        let dir = tempdir().expect("tempdir");
        let input = make_test_image(dir.path(), "wide.jpg", 1200, 800);
        let output = dir.path().join("wide_thumb.jpg");

        let result = generate_thumbnail(
            &input,
            &output,
            Format::Jpeg,
            ThumbnailConfig { max_width: 200, jpeg_quality: DEFAULT_JPEG_QUALITY },
        )
        .expect("generate_thumbnail");

        assert_eq!(result.thumbnail.width, 200);
        assert_eq!(result.thumbnail.height, 133); // 800 * (200/1200) = 133.3 → 133
    }

    // -----------------------------------------------------------------------
    // run — single file
    // -----------------------------------------------------------------------

    #[test]
    fn run_single_file_creates_thumbnail_with_default_output() {
        let dir = tempdir().expect("tempdir");
        let input = make_test_image(dir.path(), "photo.jpg", 800, 600);

        run(Args {
            input: input.clone(),
            output: None,
            max_width: DEFAULT_MAX_WIDTH,
            quality: DEFAULT_JPEG_QUALITY,
            format: None,
            batch: false,
            parallel: Parallel::Sequential,
            max_concurrent: None,
            quiet: true,
        })
        .expect("run");

        let thumb = dir.path().join("photo_thumb.jpg");
        assert!(thumb.exists());
        assert_eq!(image::open(&thumb).expect("open thumb").width(), 400);
    }

    // -----------------------------------------------------------------------
    // run — sequential batch
    // -----------------------------------------------------------------------

    #[test]
    fn run_batch_sequential_processes_all_images() {
        let dir = tempdir().expect("tempdir");
        make_test_image(dir.path(), "a.jpg", 800, 600);
        make_test_image(dir.path(), "b.png", 800, 600);

        run(Args {
            input: dir.path().to_path_buf(),
            output: None,
            max_width: DEFAULT_MAX_WIDTH,
            quality: DEFAULT_JPEG_QUALITY,
            format: None,
            batch: true,
            parallel: Parallel::Sequential,
            max_concurrent: None,
            quiet: true,
        })
        .expect("run batch");

        assert!(dir.path().join("a_thumb.jpg").exists());
        assert!(dir.path().join("b_thumb.png").exists());
    }

    #[test]
    fn run_batch_skips_existing_thumbnails() {
        let dir = tempdir().expect("tempdir");
        make_test_image(dir.path(), "photo.jpg", 800, 600);
        make_test_image(dir.path(), "photo_thumb.jpg", 400, 300);

        run(Args {
            input: dir.path().to_path_buf(),
            output: None,
            max_width: DEFAULT_MAX_WIDTH,
            quality: DEFAULT_JPEG_QUALITY,
            format: None,
            batch: true,
            parallel: Parallel::Sequential,
            max_concurrent: None,
            quiet: true,
        })
        .expect("run batch");

        // A second thumbnail of the thumbnail must NOT have been created.
        assert!(!dir.path().join("photo_thumb_thumb.jpg").exists());
    }

    // -----------------------------------------------------------------------
    // par_batch generic helper (rayon)
    // -----------------------------------------------------------------------

    /// Verify that `par_batch` produces the same results as a sequential map.
    ///
    /// We test the generic helper directly so it is covered independently of
    /// the image-processing code. The closure doubles each number and wraps
    /// it in `Ok` — no file I/O needed.
    #[cfg(feature = "rayon")]
    #[test]
    fn par_batch_applies_function_to_all_items() {
        let items = vec![1u32, 2, 3, 4, 5];
        let results: Vec<Result<u32, String>> =
            par_batch(items, |n| Ok::<u32, String>(n * 2));

        let values: Vec<u32> = results.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(values, vec![2, 4, 6, 8, 10]);
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn par_batch_propagates_errors() {
        let items = vec![1u32, 2, 3];
        // Fail on even numbers.
        let results: Vec<Result<u32, String>> =
            par_batch(items, |n| if n % 2 == 0 { Err("even".into()) } else { Ok(n) });

        assert!(results[0].is_ok());  // 1
        assert!(results[1].is_err()); // 2
        assert!(results[2].is_ok());  // 3
    }

    // -----------------------------------------------------------------------
    // run — rayon batch
    // -----------------------------------------------------------------------

    #[cfg(feature = "rayon")]
    #[test]
    fn run_batch_rayon_produces_same_output_as_sequential() {
        let dir = tempdir().expect("tempdir");
        make_test_image(dir.path(), "x.jpg", 800, 600);
        make_test_image(dir.path(), "y.png", 800, 600);

        run(Args {
            input: dir.path().to_path_buf(),
            output: None,
            max_width: DEFAULT_MAX_WIDTH,
            quality: DEFAULT_JPEG_QUALITY,
            format: None,
            batch: true,
            parallel: Parallel::Rayon,
            max_concurrent: None,
            quiet: true,
        })
        .expect("run rayon batch");

        let x_thumb = dir.path().join("x_thumb.jpg");
        let y_thumb = dir.path().join("y_thumb.png");
        assert!(x_thumb.exists());
        assert!(y_thumb.exists());
        assert_eq!(image::open(&x_thumb).expect("open x").width(), 400);
        assert_eq!(image::open(&y_thumb).expect("open y").width(), 400);
    }

    // -----------------------------------------------------------------------
    // spawn_blocking_batch generic helpers (tokio)
    //
    // `#[tokio::test]` expands to a synchronous test function that creates a
    // tokio runtime and drives the async test body to completion. It requires
    // the `tokio/macros` feature, which we enable in Cargo.toml when the
    // `tokio` feature is active.
    // -----------------------------------------------------------------------

    /// Unbounded version: all tasks start immediately.
    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn spawn_blocking_batch_applies_function_to_all_items() {
        let items = vec![1u32, 2, 3, 4, 5];
        let results =
            spawn_blocking_batch(items, |n| Ok::<u32, anyhow::Error>(n * 2)).await;

        // Results may arrive out of order; sort before asserting.
        let mut values: Vec<u32> = results.into_iter().map(|r| r.unwrap()).collect();
        values.sort_unstable();
        assert_eq!(values, vec![2, 4, 6, 8, 10]);
    }

    /// Bounded version: at most `max_concurrent` tasks run at once.
    ///
    /// We verify correctness (same output as sequential) rather than measuring
    /// actual concurrency, which would require timing or coordination
    /// primitives that add complexity beyond a study project.
    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn spawn_blocking_batch_bounded_applies_function_to_all_items() {
        let items = vec![10u32, 20, 30, 40, 50];
        // max_concurrent = 2: at most 2 tasks hold a semaphore permit at once.
        let results =
            spawn_blocking_batch_bounded(items, |n| Ok::<u32, anyhow::Error>(n + 1), 2).await;

        let mut values: Vec<u32> = results.into_iter().map(|r| r.unwrap()).collect();
        values.sort_unstable();
        assert_eq!(values, vec![11, 21, 31, 41, 51]);
    }

    // -----------------------------------------------------------------------
    // run — tokio batch
    // -----------------------------------------------------------------------

    #[cfg(feature = "tokio")]
    #[test]
    fn run_batch_tokio_produces_same_output_as_sequential() {
        let dir = tempdir().expect("tempdir");
        make_test_image(dir.path(), "p.jpg", 800, 600);
        make_test_image(dir.path(), "q.png", 800, 600);

        run(Args {
            input: dir.path().to_path_buf(),
            output: None,
            max_width: DEFAULT_MAX_WIDTH,
            quality: DEFAULT_JPEG_QUALITY,
            format: None,
            batch: true,
            parallel: Parallel::Tokio,
            max_concurrent: Some(2),
            quiet: true,
        })
        .expect("run tokio batch");

        let p_thumb = dir.path().join("p_thumb.jpg");
        let q_thumb = dir.path().join("q_thumb.png");
        assert!(p_thumb.exists());
        assert!(q_thumb.exists());
        assert_eq!(image::open(&p_thumb).expect("open p").width(), 400);
        assert_eq!(image::open(&q_thumb).expect("open q").width(), 400);
    }
}
