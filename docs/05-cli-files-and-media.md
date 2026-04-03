# CLI, Files, And Media

This doc covers the patterns that show up in the smaller Phase 1 projects.

## `clap` For Command-Line Interfaces

In plain English: `clap` turns a Rust struct into a real command-line interface.
You describe the arguments once as struct fields with attributes, and clap
handles parsing, help text, validation, and error messages.

```rust
#[derive(Debug, Parser)]
#[command(
    name = "cli-hash-cache",
    version,
    about = "Hash files in a directory, persist the results, and report changes."
)]
pub struct Args {
    /// The directory to scan recursively. (positional — no flag needed)
    pub directory: PathBuf,

    /// Where to store the cache on disk. (optional, has a default)
    #[arg(short, long)]
    pub cache_file: Option<PathBuf>,

    /// Save the cache in JSON instead of bincode.
    #[arg(long)]
    pub json: bool,

    /// Suppress the list of unchanged files.
    #[arg(short, long)]
    pub quiet: bool,
}
```

See `PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs`.

The key attributes on each field control the CLI behavior:

| Attribute | Effect |
|---|---|
| (no attribute) | Positional argument — required, no `--` flag |
| `#[arg(short, long)]` | Flag with both `-c` and `--cache-file` forms |
| `#[arg(long)]` | Long flag only: `--json` |
| `Option<T>` type | Makes the argument optional |
| `bool` type | Creates a boolean flag (no value needed) |
| `#[arg(default_value_t = 400)]` | Default value when the flag is omitted |

**Why put `Args` in the library, not `main.rs`?** This pattern separates parsing
from logic. Tests can construct `Args` directly without spawning a subprocess,
and the same library can be reused in other contexts (like a web handler or WASM
module):

```rust
// In tests, construct Args directly:
let args = Args {
    directory: temp_dir.path().to_path_buf(),
    cache_file: None,
    json: true,
    quiet: true,
};
let outcome = run(args).expect("run should succeed");
```

## Value Enums For Safe Choices

In plain English: if an argument should only accept a few named values, use an
enum instead of free-form text. The CLI itself will reject unknown values before
your logic runs.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
}
```

This generates CLI usage like `--format jpeg|png|webp`. If a user types
`--format tiff`, clap produces a helpful error message listing the valid options.

The enum also provides a natural place for per-variant helper methods:

```rust
impl Format {
    fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
        }
    }

    fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }
}
```

`from_path` returns `Option` rather than `Result` because an unrecognized
extension is not always an error — sometimes the caller wants to try a
different detection strategy.

## Path Handling

In plain English: Rust has two path types, mirroring the `String`/`&str` split:

- `PathBuf` is the **owned** version — use when storing or returning a path
- `&Path` is the **borrowed** version — use when a function only needs to read

```rust
// PathBuf: owned, can be stored in a struct or returned from a function
pub struct DiscoveredFile {
    relative_path: PathBuf,
    absolute_path: PathBuf,
}

// &Path: borrowed, efficient for function parameters
fn canonicalize_directory(directory: &Path) -> Result<PathBuf> {
    fs::canonicalize(directory)
        .with_context(|| format!("failed to canonicalize '{}'", directory.display()))
}
```

Converting between the two:

```rust
// &Path → PathBuf (allocates a new PathBuf)
let owned = path.to_path_buf();

// PathBuf → &Path (borrows — free, no allocation)
let borrowed: &Path = &owned;
// or implicitly through Deref:
some_function(&owned);  // auto-borrows PathBuf as &Path
```

Common path operations used in the repo:

```rust
// Join segments (like os.path.join in Python)
let vote_hash_path = cfg.congress_dir.join("data").join("voteHashes.rscraper.bin");

// Get the file extension
let ext = path.extension();  // returns Option<&OsStr>

// Get the file stem (name without extension)
let stem = path.file_stem();  // returns Option<&OsStr>

// Display a path (for error messages and logging)
format!("failed to read '{}'", path.display());

// Strip a prefix to get a relative path
let relative = absolute_path.strip_prefix(scan_root)?;
```

## Directory Traversal

In plain English: `walkdir` is the "visit every file under this directory"
tool. It handles recursion, symlinks, and permission errors so you don't have
to.

```rust
for entry in WalkDir::new(scan_root) {
    match entry {
        Ok(entry) => {
            let path = entry.path();

            // Skip the cache file so we don't hash our own output
            if path == cache_path { continue; }

            // Skip directories — we only hash files
            if !entry.file_type().is_file() { continue; }

            let relative_path = path.strip_prefix(scan_root)?;
            discovered.push(DiscoveredFile {
                relative_path: relative_path.to_path_buf(),
                absolute_path: path.to_path_buf(),
            });
        }
        Err(error) => {
            // Keep going when a single directory entry is bad
            eprintln!("warning: failed to read a directory entry: {error}");
        }
    }
}

// Sort for deterministic output
discovered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
```

That is a realistic pattern: keep going when a single directory entry is bad,
but warn the user. Sorting ensures the output is deterministic regardless of
filesystem ordering.

For the thumbnail generator, `WalkDir` is limited to one level deep:

```rust
for entry in WalkDir::new(dir).min_depth(1).max_depth(1) {
    // only immediate children, not subdirectories
}
```

## Atomic File Writes

In plain English: do not write important files directly if a crash could leave
them half-written. Write a temp file first, then rename it into place. The
rename operation is atomic on most filesystems.

The hash cache uses this pattern:

```rust
fn save_cache(cache_path: &Path, format: CacheFormat, cache: &StoredCache) -> Result<()> {
    // 1. Ensure the parent directory exists
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // 2. Serialize the data in memory
    let serialized = match format {
        CacheFormat::Bincode => {
            let config = bincode::config::standard();
            bincode::serde::encode_to_vec(cache, config)?
        }
        CacheFormat::Json => {
            serde_json::to_vec_pretty(cache)?
        }
    };

    // 3. Write to a temporary file in the SAME directory
    let temp_path = cache_path.with_file_name(
        format!("{}.tmp", cache_path.file_name().unwrap().to_string_lossy())
    );

    let mut temp_file = File::create(&temp_path)?;
    temp_file.write_all(&serialized)?;
    temp_file.flush()?;     // ensure bytes are flushed to disk

    // 4. Close the temp file explicitly (by dropping it)
    drop(temp_file);

    // 5. Rename into place (atomic on most filesystems)
    fs::rename(&temp_path, cache_path)?;

    Ok(())
}
```

**Why write in the same directory?** The `rename` syscall is only guaranteed to
be atomic when source and destination are on the same filesystem. If they are on
different filesystems, the OS falls back to a copy-then-delete, which is not
atomic.

This is a boring pattern, which is exactly why it is good. You never risk
losing your cache to a crash or power failure.

## Reading From Files Or stdin

In plain English: the markdown sanitizer accepts input from a path or from
standard input, which is the normal Unix way to make tools composable. This
lets users pipe data into the tool: `cat README.md | markdown-sanitizer`.

```rust
fn read_input(path: Option<&PathBuf>) -> Result<String> {
    match path {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read input file '{}'", path.display())),
        None => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .context("failed to read markdown from stdin")?;
            Ok(buffer)
        }
    }
}
```

The same pattern on the output side:

```rust
fn write_output(path: Option<&PathBuf>, html: &str) -> Result<()> {
    match path {
        Some(path) => fs::write(path, html)
            .with_context(|| format!("failed to write output file '{}'", path.display())),
        None => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(html.as_bytes())
                .context("failed to write sanitized HTML to stdout")?;
            Ok(())
        }
    }
}
```

Notice `io::stdout().lock()` — locking stdout ensures that no other thread
can write to it while we are writing. Without the lock, concurrent writes
could interleave.

## Markdown Rendering And Sanitization

In plain English: the markdown project is a two-stage pipeline. This separation
is important because markdown rendering and HTML sanitization serve different
purposes and should be independently configurable.

**Stage 1 — Render markdown to HTML:**

```rust
let parser = MarkdownParser::new_ext(markdown, self.markdown_options);
let mut html_output = String::new();
html::push_html(&mut html_output, parser);
```

`new_ext` enables extra markdown features controlled by option flags:

```rust
pub fn default_markdown_options() -> Options {
    Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
}
```

**Stage 2 — Sanitize the HTML to remove dangerous content:**

```rust
self.sanitizer.clean(html).to_string()
```

`ammonia` removes or neutralizes dangerous HTML like `<script>` tags, `javascript:`
URLs, and event handler attributes (`onclick`, etc.). This is critical when
rendering user-supplied markdown, which might contain malicious content.

**Why two stages?** The markdown renderer (`pulldown-cmark`) does not know or
care about security — it faithfully converts markdown to HTML, including any
embedded HTML. The sanitizer (`ammonia`) does not know or care about markdown —
it only looks at the HTML output. Keeping them separate means:

- You can swap in a different markdown parser without touching the security layer
- You can adjust the sanitization policy without touching the rendering
- Tests can verify each stage independently

## Builder-Style Pipeline Design

In plain English: the pipeline struct stores its settings, and methods return
an updated `Self` so configuration reads smoothly as a chain.

```rust
pub struct MarkdownPipeline {
    markdown_options: Options,
    sanitizer: ammonia::Builder<'static>,
}

impl MarkdownPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_markdown_options(mut self, opts: Options) -> Self {
        self.markdown_options = opts;
        self
    }

    pub fn with_sanitizer(mut self, sanitizer: ammonia::Builder<'static>) -> Self {
        self.sanitizer = sanitizer;
        self
    }
}
```

Usage reads naturally as a chain:

```rust
let pipeline = MarkdownPipeline::new()
    .with_markdown_options(custom_options)
    .with_sanitizer(strict_sanitizer);

let result = pipeline.render_and_sanitize("# Hello **world**");
```

That is a nice pattern when you want a configurable tool without a giant
constructor with 10 parameters. Each `.with_*()` call is optional, and the
defaults are sensible.

## Image Processing Pipeline

In plain English: the thumbnail generator follows a simple, teachable workflow
with clearly separated steps.

```rust
pub fn generate_thumbnail(
    input: &Path,
    output: &Path,
    format: Format,
    config: ThumbnailConfig,
) -> Result<ThumbnailResult> {
    // 1. Load — open and fully decode the source image into memory
    let img = image::open(input)
        .with_context(|| format!("failed to open image '{}'", input.display()))?;

    // 2. Measure — get the original dimensions
    let original = Dimensions::from_image(&img);

    // 3. Compute — calculate target size preserving aspect ratio
    let target = original.scale_to_width(config.max_width);

    // 4. Resize — only if the image is actually larger than the target
    let resized = if target == original {
        img  // already small enough, skip resizing
    } else {
        img.thumbnail(target.width, target.height)
    };

    // 5. Save — encode in the requested format and write to disk
    save_image(&resized, output, format, config.jpeg_quality)?;

    Ok(ThumbnailResult {
        input_path: input.to_path_buf(),
        output_path: output.to_path_buf(),
        original,
        thumbnail: Dimensions::from_image(&resized),
        format,
    })
}
```

This is a good example of Rust code being explicit without being complicated.
Each step is visible, named, and independently testable.

**Returning a result struct instead of printing** keeps side effects separate
from computation. Tests can verify dimensions and paths without touching stdout:

```rust
let result = generate_thumbnail(input, output, Format::Jpeg, config)?;
assert_eq!(result.thumbnail.width, 400);
assert_eq!(result.original.width, 1200);
```

## Aspect Ratio Preservation

In plain English: when the thumbnail width changes, the height must change by
the same proportion or the image looks squashed. This is basic proportional
scaling.

```rust
pub fn scale_to_width(self, max_width: u32) -> Self {
    if self.width <= max_width {
        return self;  // already small enough, no change
    }

    let scale = max_width as f64 / self.width as f64;
    let new_height = (self.height as f64 * scale).round() as u32;

    Self {
        width: max_width,
        height: new_height.max(1),  // never go below 1 pixel
    }
}
```

The `as f64` casts are necessary because integer division would truncate:
`333 / 1000 = 0` in integer math, but `333.0 / 1000.0 = 0.333` in float math.
Using `.round()` instead of truncation avoids off-by-one errors in the height.

The `.max(1)` guard ensures we never produce a zero-height image, which would
be invalid.

## Format-Specific Encoding

In plain English: JPEG needs special handling because you need to control the
quality setting. PNG and WebP use the `image` crate's built-in defaults.

```rust
fn save_image(img: &DynamicImage, output: &Path, format: Format, jpeg_quality: u8) -> Result<()> {
    match format {
        Format::Jpeg => {
            let file = File::create(output)?;
            let encoder = JpegEncoder::new_with_quality(BufWriter::new(file), jpeg_quality);
            img.write_with_encoder(encoder)?;
        }
        Format::Png | Format::Webp => {
            img.save_with_format(output, format.to_image_format())?;
        }
    }
    Ok(())
}
```

`BufWriter` wraps the file to batch many small `write()` calls into fewer,
larger system calls — a meaningful performance win when the encoder produces
output in small chunks.

## Practical Lesson

These Phase 1 projects are good Rust practice because they force you to handle
real edges:

- missing files
- awkward user input
- partial writes (atomic rename pattern)
- different input/output formats
- CPU-heavy work like hashing and image resizing
- composable tools (stdin/stdout piping)

Each project also follows the same structural pattern: `clap` at the edge,
plain Rust values in the core, `anyhow` for errors, and `ExitCode` for the
process boundary. Learning this pattern once makes every subsequent CLI project
feel familiar.
