# CLI, Files, And Media

This doc covers the patterns that show up in the smaller Phase 1 projects.

## `clap` For Command-Line Interfaces

In plain English: `clap` turns a Rust struct into a real command-line interface.
You describe the arguments once, and it handles parsing, help text, and
validation.

```rust
#[derive(Debug, Parser)]
#[command(
    name = "cli-hash-cache",
    version,
    about = "Hash files in a directory, persist the results, and report changes."
)]
pub struct Args {
    pub directory: PathBuf,

    #[arg(short, long)]
    pub cache_file: Option<PathBuf>,

    #[arg(long)]
    pub json: bool,

    #[arg(short, long)]
    pub quiet: bool,
}
```

See `PHASE1/1.1-CLI-HASH-CACHE/src/lib.rs`.

## Value Enums For Safe Choices

In plain English: if an argument should only accept a few named values, use an
enum instead of free-form text.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
}
```

That means the CLI itself will reject unknown formats before your logic runs.

## Path Handling

In plain English:

- `PathBuf` is the owned version of a path
- `&Path` is the borrowed version

Use `PathBuf` when storing or returning a path. Use `&Path` when a function only
needs to inspect one.

## Directory Traversal

In plain English: `walkdir` is the "visit every file under this directory"
tool.

```rust
for entry in WalkDir::new(scan_root) {
    match entry {
        Ok(entry) => {
            let path = entry.path();
            if path == cache_path { continue; }
            if !entry.file_type().is_file() { continue; }
        }
        Err(error) => {
            eprintln!("warning: failed to read a directory entry: {error}");
        }
    }
}
```

That is a realistic pattern: keep going when a single directory entry is bad,
but warn the user.

## Atomic File Writes

In plain English: do not write important files directly if a crash could leave
them half-written. Write a temp file first, then rename it into place.

That pattern appears in the hash cache:

1. serialize the cache
2. write to a temp path
3. flush and close the temp file
4. rename it into place

This is a boring pattern, which is exactly why it is good.

## Reading From Files Or stdin

In plain English: the markdown sanitizer accepts input from a path or from
standard input, which is the normal Unix way to make tools composable.

```rust
match path {
    Some(path) => fs::read_to_string(path),
    None => {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        Ok(buffer)
    }
}
```

## Markdown Rendering And Sanitization

In plain English: the markdown project is a two-stage pipeline.

1. turn markdown into HTML
2. remove unsafe HTML

That is important because user-facing markdown often needs both formatting and
safety.

Rendering:

```rust
let parser = MarkdownParser::new_ext(markdown, self.markdown_options);
html::push_html(&mut html_output, parser);
```

Sanitizing:

```rust
self.sanitizer.clean(html).to_string()
```

## Builder-Style Pipeline Design

In plain English: the pipeline struct stores its settings, and methods can
return an updated `Self` so configuration reads smoothly.

```rust
pub fn with_markdown_options(mut self, opts: Options) -> Self {
    self.markdown_options = opts;
    self
}
```

That is a nice pattern when you want a configurable tool without a giant
constructor.

## Image Processing Pipeline

In plain English: the thumbnail generator follows a simple, teachable workflow.

1. open the image
2. inspect its size
3. compute the target size
4. resize if needed
5. save in the requested format

```rust
let img = image::open(input)?;
let original = Dimensions::from_image(&img);
let target = original.scale_to_width(config.max_width);
let resized = if target == original { img } else { img.thumbnail(target.width, target.height) };
save_image(&resized, output, format, config.jpeg_quality)?;
```

This is a good example of Rust code being explicit without being complicated.

## Aspect Ratio Preservation

In plain English: when the thumbnail width changes, the height must change by
the same proportion or the image looks squashed.

```rust
let scale = max_width as f64 / self.width as f64;
let new_height = (self.height as f64 * scale).round() as u32;
```

## Practical Lesson

These Phase 1 projects are good Rust practice because they force you to handle
real edges:

- missing files
- awkward user input
- partial writes
- different input/output formats
- CPU-heavy work like hashing and image resizing
