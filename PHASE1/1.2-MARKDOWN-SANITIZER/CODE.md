# 1.2 — Markdown Sanitizer: Library & CLI Reference

A Rust library (with a thin CLI wrapper) that converts Markdown to HTML using `pulldown-cmark`, then strips dangerous content with `ammonia`. Exposes both a simple free-function API and a configurable `MarkdownPipeline` struct.

---

## Reused from 1.1

These crates and standard library modules were already documented in [1.1 CODE.md](../1.1-CLI-HASH-CACHE/CODE.md) and are used here in the same way:

| Item | 1.2 Usage |
|------|-----------|
| `std::fs` | `read_to_string` / `write` for file I/O |
| `std::path::PathBuf` / `Path` | CLI argument types for `--input` / `--output` |
| `std::io::Read` | Reading from stdin when no file is provided |
| `clap` (derive) | `Args` struct with optional positional + flag arguments |
| `anyhow` | `Context` / `with_context()` wrapping on all fallible I/O |

---

## New Standard Library Usage

### `std::io::Write` / `stdout().lock()`

📖 [Write](https://doc.rust-lang.org/std/io/trait.Write.html) · [stdout](https://doc.rust-lang.org/std/io/fn.stdout.html)

1.1 only read files; 1.2 adds a stdout output path. `stdout().lock()` acquires the lock once instead of per-write, which matters when writing large HTML documents.

```rust
use std::io::{self, Write};

let mut stdout = io::stdout().lock();
stdout.write_all(html.as_bytes())?;
```

### `std::process::ExitCode`

📖 [Reference](https://doc.rust-lang.org/std/process/struct.ExitCode.html)

Used to return proper exit codes from `main()`. This is the idiomatic Rust approach instead of calling `std::process::exit()` directly, because it lets destructors run cleanly.

```rust
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(1)
        }
    }
}
```

### `impl AsRef<str>` Pattern

📖 [AsRef](https://doc.rust-lang.org/std/convert/trait.AsRef.html)

The public API uses `impl AsRef<str>` to accept both `&str` and `String` without forcing the caller to convert. This is a common Rust idiom for string-accepting functions.

```rust
// impl AsRef<str> lets callers pass &str, String, Cow<str>, etc.
pub fn render_markdown(markdown: impl AsRef<str>) -> String {
    let input = markdown.as_ref();
    // ...
}

// pulldown-cmark pushes rendered HTML into a pre-allocated String
let mut html_output = String::new();
pulldown_cmark::html::push_html(&mut html_output, parser);
```

---

## New External Crates

### `pulldown-cmark` — Markdown → HTML Rendering

📖 [docs.rs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/) · [GitHub](https://github.com/raphlinus/pulldown-cmark) · [Parser docs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/struct.Parser.html) · [Options docs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/struct.Options.html)

**What it does:** A pull-parser for CommonMark markdown. Instead of building an AST in memory, it yields a stream of `Event` values (heading start, text, code block, etc.) that can be consumed by `html::push_html` to produce HTML. This streaming design keeps memory usage proportional to the current tag depth, not the document size.

**Cargo.toml:**
```toml
pulldown-cmark = "0.13"
```

**Usage:**
```rust
use pulldown_cmark::{Options, Parser, html};

// Configure which markdown extensions to enable
let options = Options::ENABLE_STRIKETHROUGH
    | Options::ENABLE_TABLES
    | Options::ENABLE_TASKLISTS
    | Options::ENABLE_FOOTNOTES;

// Create a parser that yields events from the markdown source
let parser = Parser::new_ext(markdown, options);

// Render the event stream into an HTML string
let mut html_output = String::new();
html::push_html(&mut html_output, parser);

println!("{html_output}");
```

**Available `Options` flags:**

| Flag | What it enables |
|------|-----------------|
| `ENABLE_TABLES` | GitHub-flavored pipe tables |
| `ENABLE_FOOTNOTES` | `[^1]` footnote references and definitions |
| `ENABLE_STRIKETHROUGH` | `~~deleted~~` text |
| `ENABLE_TASKLISTS` | `- [x]` / `- [ ]` checkbox lists |
| `ENABLE_HEADING_ATTRIBUTES` | `# Heading { #id .class }` |
| `ENABLE_SMART_PUNCTUATION` | Curly quotes, em dashes, ellipses |

---

### `ammonia` — HTML Sanitization

📖 [docs.rs](https://docs.rs/ammonia/latest/ammonia/) · [GitHub](https://github.com/rust-ammonia/ammonia) · [Builder docs](https://docs.rs/ammonia/latest/ammonia/struct.Builder.html)

**What it does:** Strips or rewrites HTML tags, attributes, and URL schemes according to a configurable policy. Prevents XSS by removing `<script>`, `javascript:` URLs, `onclick` attributes, and similar dangerous content while preserving safe formatting tags like `<strong>`, `<em>`, `<a>`, `<code>`, etc.

**Cargo.toml:**
```toml
ammonia = "4.0"
```

**Usage (default policy):**
```rust
// The simplest call — uses ammonia's built-in safe-HTML defaults
let clean_html = ammonia::clean("<p>Hello</p><script>alert('xss')</script>");
assert_eq!(clean_html, "<p>Hello</p>");
```

**Usage (custom `Builder` policy):**
```rust
use ammonia::Builder;
use std::collections::HashSet;

let sanitizer = Builder::default()
    .tags(HashSet::from(["p", "strong", "em", "a", "iframe"]))
    .url_schemes(HashSet::from(["https"]))  // only allow https: URLs
    .link_rel(Some("noopener noreferrer"))   // add rel attrs to links
    .clone();

let clean = sanitizer.clean("<iframe src='https://example.com'></iframe>").to_string();
```

**Default allowed tags include:**
`a`, `abbr`, `b`, `blockquote`, `br`, `code`, `dd`, `del`, `dl`, `dt`, `em`, `h1`–`h6`, `hr`, `i`, `img`, `kbd`, `li`, `ol`, `p`, `pre`, `q`, `s`, `strong`, `sub`, `sup`, `table`, `tbody`, `td`, `th`, `thead`, `tr`, `ul`

**What gets stripped by default:**
`<script>`, `<style>`, `<iframe>`, `<object>`, `<embed>`, `<form>`, `<input>`, `<textarea>`, `<button>`, event handler attributes (`onclick`, `onerror`, etc.), `javascript:` and `data:` URL schemes

---

### `tempfile` — Temporary Files for Testing (dev-dependency)

📖 [docs.rs](https://docs.rs/tempfile/latest/tempfile/) · [GitHub](https://github.com/Stebalien/tempfile)

**What it does:** Creates temporary files and directories that are automatically cleaned up when the value is dropped. Used in `#[cfg(test)]` modules to test file I/O without polluting the real filesystem.

**Cargo.toml:**
```toml
[dev-dependencies]
tempfile = "3.20"
```

**Usage:**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::fs;

    #[test]
    fn run_reads_from_file_and_writes_sanitized_output() {
        // NamedTempFile auto-deletes when dropped
        let input_file = NamedTempFile::new().expect("temp input file");
        let output_file = NamedTempFile::new().expect("temp output file");

        fs::write(
            input_file.path(),
            "[safe?](javascript:alert('xss')) and **world**",
        )
        .expect("write markdown");

        run(Args {
            input: Some(input_file.path().to_path_buf()),
            output: Some(output_file.path().to_path_buf()),
        })
        .expect("run sanitizer");

        let saved = fs::read_to_string(output_file.path()).expect("read output");
        assert!(saved.contains("<strong>world</strong>"));
        assert!(!saved.contains("javascript:"));
    }
}
```

---

## Cargo.toml Summary

```toml
[package]
name = "markdown-sanitizer"
version = "0.1.0"
edition = "2024"

[dependencies]
ammonia = "4.0"
anyhow = "1.0"
clap = { version = "4.5", features = ["derive"] }
pulldown-cmark = "0.13"

[dev-dependencies]
tempfile = "3.20"
```

---

## Architecture: lib + bin Pattern

This project uses the **lib + bin crate** pattern, where the binary is a minimal wrapper around the library:

```
src/
├── lib.rs      296 lines — all logic, types, and tests
└── main.rs       3 lines — delegates to lib::main_exit_code()
```

**`main.rs`:**
```rust
fn main() -> std::process::ExitCode {
    markdown_sanitizer::main_exit_code()
}
```

**Why this pattern matters:**
- The library can be imported by other Rust crates (`use markdown_sanitizer::render_and_sanitize`)
- Tests run against the library directly — no subprocess spawning
- Future WASM compilation targets `lib.rs` without touching `main.rs`
- CLI argument parsing is at the edge; core logic only takes plain Rust values

---

## Expected Output Example

```
$ echo '# Hello World

This is **important** and [click me](javascript:alert("xss")).

<script>document.cookie</script>' | markdown-sanitizer

<h1>Hello World</h1>
<p>This is <strong>important</strong> and <a rel="noopener noreferrer">click me</a>.</p>
<p></p>
```

**What happened:**
- `# Hello World` → `<h1>Hello World</h1>` (markdown rendering)
- `**important**` → `<strong>important</strong>` (bold preserved)
- `javascript:alert(...)` → href stripped, link text kept (XSS prevented)
- `<script>` → completely removed (XSS prevented)

```
$ markdown-sanitizer article.md -o clean.html
$ cat clean.html
# sanitized HTML written to file
```
