# 1.2 — Markdown Sanitizer: Implementation Decisions

Every choice in the implementation is a _default_, not a requirement. This document catalogs each decision, explains the trade-offs, and lists alternatives worth considering.

---

## Table of Contents

1. [Markdown Parser](#1-markdown-parser)
2. [HTML Sanitizer](#2-html-sanitizer)
3. [Crate Architecture: lib + bin](#3-crate-architecture-lib--bin)
4. [API Design: Struct Pipeline vs Free Functions](#4-api-design-struct-pipeline-vs-free-functions)
5. [Markdown Extensions](#5-markdown-extensions)
6. [Sanitizer Policy Customization](#6-sanitizer-policy-customization)
7. [I/O Strategy: stdin/stdout vs File-Only](#7-io-strategy-stdinstdout-vs-file-only)
8. [Testing Strategy](#8-testing-strategy)
9. [WASM Compilation (Stretch)](#9-wasm-compilation-stretch)

---

## 1. Markdown Parser

**Current choice:** `pulldown-cmark` for Markdown → HTML rendering.

`pulldown-cmark` is a **pull-parser** — it yields a stream of `Event` values that are consumed by `html::push_html`. It never builds a full AST in memory, so memory usage stays proportional to nesting depth, not document size.

### Alternatives

| Parser | Crate | Approach | Speed | CommonMark Conformance | Notes |
|--------|-------|----------|-------|:----------------------:|-------|
| **pulldown-cmark** | `pulldown-cmark` | Pull-parser / event stream | Fast | ✓ | De facto standard in the Rust ecosystem. Used by rustdoc. |
| **comrak** | `comrak` | AST (full tree in memory) | Moderate | ✓ | Port of GitHub's `cmark-gfm`. Builds a mutable AST you can transform before rendering. Supports GFM extensions natively. |
| **markdown-rs** | `markdown` | Event-based | Fast | ✓ | Newer, safety-focused. Closer to the `micromark` JS parser. |
| **markdown-it** | `markdown-it` | Plugin-based (port of JS library) | Moderate | Partial | Highly extensible with custom syntax plugins. |

### When you'd switch

**AST manipulation** is the main reason to choose `comrak` over `pulldown-cmark`. If you need to transform the document structure before rendering (e.g., rewrite links, inject heading anchors, extract a table of contents), an AST makes that natural:

```rust
// Cargo.toml: comrak = "0.31"
use comrak::{markdown_to_html, Options};

let html = markdown_to_html("# Hello **world**", &Options::default());
```

```rust
// With full AST access for transformations:
use comrak::{parse_document, format_html, Arena, Options};
use comrak::nodes::NodeValue;

let arena = Arena::new();
let root = parse_document(&arena, "# Title\nBody text", &Options::default());

// Walk the AST and modify nodes
for node in root.descendants() {
    if let NodeValue::Text(ref mut text) = node.data.borrow_mut().value {
        *text = text.replace("Title", "Modified Title");
    }
}

let mut output = Vec::new();
format_html(root, &Options::default(), &mut output).unwrap();
```

**pulldown-cmark** is better when you just need markdown → HTML without transformations. It's faster, lower memory, and has a simpler API.

### Decision Guidance

- **Render markdown to HTML?** → `pulldown-cmark` (simpler, faster).
- **Transform the document before rendering?** → `comrak` (mutable AST).
- **Custom syntax extensions?** → `markdown-it` (plugin system).
- **Maximum compatibility with GitHub rendering?** → `comrak` (direct port of `cmark-gfm`).

📖 [pulldown-cmark](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/) · [comrak](https://docs.rs/comrak/latest/comrak/) · [markdown-rs](https://docs.rs/markdown/latest/markdown/)

---

## 2. HTML Sanitizer

**Current choice:** `ammonia` for stripping dangerous HTML.

`ammonia` applies a tag/attribute allowlist policy to HTML. Anything not on the list is stripped. This is the **correct** approach — allowlisting is safer than denylisting because new attack vectors can't bypass an allowlist.

### Alternatives

| Sanitizer | Crate | Approach | Notes |
|-----------|-------|----------|-------|
| **ammonia** | `ammonia` | Allowlist (safe by default) | Most popular Rust HTML sanitizer. Built on `html5ever` (same parser as Firefox's Servo). |
| **sanitize-html (manual)** | (none) | Regex stripping | **Never do this.** Regex cannot reliably parse HTML. Every regex-based sanitizer has known bypasses. |
| **html5ever + custom walk** | `html5ever` | Parse → walk tree → drop nodes | Full control but significantly more code. You'd be reimplementing ammonia. |

### Why there's no real alternative

HTML sanitization is a security-critical operation. `ammonia` is the only mature, maintained Rust crate that does it properly. It uses `html5ever` (the parser from Mozilla's Servo engine) to parse HTML the same way a browser would, then applies the policy to the parsed tree. This handles all the edge cases that break regex-based approaches:

```html
<!-- These ALL bypass naive regex sanitizers but ammonia handles them correctly: -->
<img src=x onerror=alert(1)>
<a href="java&#115;cript:alert(1)">click</a>
<div style="background:url(javascript:alert(1))">
<<script>script>alert(1)<</script>/script>
```

### Decision Guidance

- **Sanitize HTML in Rust?** → `ammonia`. There's no practical alternative.
- **Need a custom tree walk?** → Use `ammonia::Builder` to configure the policy first. Only drop down to `html5ever` if ammonia's policy model can't express what you need.

📖 [ammonia](https://docs.rs/ammonia/latest/ammonia/) · [html5ever](https://docs.rs/html5ever/latest/html5ever/)

---

## 3. Crate Architecture: lib + bin

**Current choice:** All logic lives in `lib.rs`. The binary (`main.rs`) is a 3-line wrapper.

```
src/
├── lib.rs      296 lines — Args, MarkdownPipeline, I/O, tests
└── main.rs       3 lines — calls lib::main_exit_code()
```

### Alternatives

| Layout | Pros | Cons |
|--------|------|------|
| **lib + bin** (current) | Library is reusable, testable without subprocess. WASM-ready. | Must mark items `pub` that would otherwise be private. |
| **bin only** | Simpler. Everything is private by default. | Can't `use markdown_sanitizer::render_and_sanitize` from another crate. Can't compile to WASM easily. |
| **Workspace** (`sanitizer-lib` + `sanitizer-cli`) | Clean separation. Each crate has its own `Cargo.toml` and semver. | Heavier structure for a small project. Two crates to maintain. |
| **lib + `examples/` dir** | Library crate with example binaries | Examples aren't installed by `cargo install`. Not a real CLI. |

### When you'd switch

The **workspace** layout is better when the library and CLI have different dependency sets. For example, if the CLI adds `clap` but the library shouldn't depend on it:

```toml
# Cargo.toml (workspace root)
[workspace]
members = ["sanitizer-lib", "sanitizer-cli"]

# sanitizer-lib/Cargo.toml — no clap dependency
[dependencies]
pulldown-cmark = "0.13"
ammonia = "4.0"

# sanitizer-cli/Cargo.toml — depends on the lib + clap
[dependencies]
sanitizer-lib = { path = "../sanitizer-lib" }
clap = { version = "4.5", features = ["derive"] }
```

In the current 1.2 implementation, `clap` is acceptable in the library because `Args` is a public type that other binaries might want to reuse. But if the library were published to crates.io, you'd want to move `clap` behind a feature flag or into a separate binary crate.

### Decision Guidance

- **Small project, WASM stretch goal?** → lib + bin (current).
- **Publishing the library to crates.io?** → Workspace or feature-gate the CLI deps.
- **Just a script?** → bin only.

---

## 4. API Design: Struct Pipeline vs Free Functions

**Current choice:** Both. `MarkdownPipeline` struct for configurable callers, plus free functions (`render_and_sanitize`, `render_markdown`, `sanitize_html`) for simple usage.

### The trade-off

**Free functions** are the simplest API — one call, done:

```rust
let doc = markdown_sanitizer::render_and_sanitize("# Hello");
```

**Struct pipeline** lets callers configure behavior without changing the function signatures:

```rust
let pipeline = MarkdownPipeline::new()
    .with_markdown_options(Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH)
    .with_sanitizer(custom_builder);

// Reuse the same configuration for many documents
let doc1 = pipeline.render_and_sanitize(article_1);
let doc2 = pipeline.render_and_sanitize(article_2);
```

### Alternatives

| Pattern | Pros | Cons |
|---------|------|------|
| **Free functions only** | Simplest API. No state to manage. | Can't configure per-call. Every option becomes a function parameter. |
| **Struct with builder** (current) | Configurable, reusable. Builder pattern is idiomatic Rust. | More types to learn. Over-engineered for a one-shot script. |
| **Free functions with `Options` parameter** | Middle ground. One config struct passed to functions. | Doesn't bundle renderer + sanitizer config together. |
| **Trait-based** (`impl Renderer`, `impl Sanitizer`) | Maximum extensibility. Callers can swap implementations. | Significant complexity for a 2-step pipeline. |

### Decision Guidance

- **Library crate that others import?** → Struct pipeline (current). Callers can configure once and reuse.
- **One-off script?** → Free functions only.
- **Rule of thumb:** If you have ≤2 config options, free function params are fine. At 3+, extract a config struct.

---

## 5. Markdown Extensions

**Current choice:** Enable strikethrough, tables, task lists, and footnotes by default.

```rust
Options::ENABLE_STRIKETHROUGH
    | Options::ENABLE_TABLES
    | Options::ENABLE_TASKLISTS
    | Options::ENABLE_FOOTNOTES
```

### Available extensions and their trade-offs

| Extension | Behavior | Risk | Default? |
|-----------|----------|------|:--------:|
| `ENABLE_STRIKETHROUGH` | `~~text~~` → `<del>text</del>` | None | ✓ |
| `ENABLE_TABLES` | Pipe tables → `<table>` | None | ✓ |
| `ENABLE_TASKLISTS` | `- [x]` → `<input type="checkbox">` | ammonia strips `<input>` by default, so checkboxes render as text | ✓ |
| `ENABLE_FOOTNOTES` | `[^1]` references | None | ✓ |
| `ENABLE_HEADING_ATTRIBUTES` | `# Title {#id .class}` | Could inject `id`/`class` that ammonia might strip | ✗ |
| `ENABLE_SMART_PUNCTUATION` | `"quotes"` → `"curly"`, `--` → `—` | May surprise users who want literal punctuation | ✗ |

### The task list gotcha

Enabling `ENABLE_TASKLISTS` in `pulldown-cmark` generates `<input type="checkbox">` elements. But ammonia's default policy strips `<input>` tags (because form elements can be used for phishing). The result: task list checkboxes produce text like `[ ]` or `[x]` instead of visual checkboxes.

**Fix options:**
1. Add `input` to ammonia's allowed tags (opens a small surface area).
2. Accept the text-only rendering (current behavior).
3. Post-process the HTML to replace `<input>` with a CSS-styled `<span>`.

### Decision Guidance

- **Blog/CMS content?** → The current defaults are good.
- **Documentation?** → Consider adding `ENABLE_HEADING_ATTRIBUTES` for anchor links.
- **Plain text input from untrusted users?** → Use `Options::empty()` for maximum strictness.

---

## 6. Sanitizer Policy Customization

**Current choice:** Use ammonia's built-in default policy.

The default policy allows common formatting tags and strips everything dangerous. This is the right starting point for most use cases.

### When to customize

| Scenario | Policy Change | Example |
|----------|--------------|---------|
| **Embed YouTube/Vimeo** | Allow `<iframe>` with `src` from specific domains | `builder.add_tags(["iframe"]).add_tag_attributes("iframe", ["src", "width", "height"])` |
| **Custom styling** | Allow `class` attributes on certain tags | `builder.add_tag_attributes("div", ["class"])` |
| **Stricter than default** | Remove `<img>` to prevent image abuse | `builder.rm_tags(["img"])` |
| **Heading anchors** | Allow `id` on headings | `builder.add_tag_attributes("h1", ["id"])` (and h2–h6) |

### Code comparison

**Strict policy (no images, no links):**
```rust
use ammonia::Builder;
use std::collections::HashSet;

let strict = Builder::default()
    .rm_tags(HashSet::from(["img", "a"]))
    .clone();

let clean = strict.clean("<p><img src=x> and <a href='evil'>link</a></p>");
// → "<p> and link</p>"
```

**Permissive policy (allow classes and iframes from trusted domains):**
```rust
use ammonia::Builder;

let mut builder = Builder::default();
builder
    .add_tags(["iframe", "div", "span"].iter())
    .add_generic_attributes(["class"].iter())
    .add_tag_attributes("iframe", ["src", "width", "height"].iter())
    .url_schemes(HashSet::from(["https"]));
```

### Decision Guidance

- **User-generated content?** → Default policy (current). Add nothing unless you have a specific need.
- **CMS with embeds?** → Allow `<iframe>` from a domain allowlist.
- **Comments section?** → Consider removing `<img>` to prevent image spam.
- **Principle:** Start strict, loosen as needed. Never start permissive and try to tighten.

📖 [ammonia::Builder](https://docs.rs/ammonia/latest/ammonia/struct.Builder.html)

---

## 7. I/O Strategy: stdin/stdout vs File-Only

**Current choice:** Support both file paths and stdin/stdout, determined by whether the arguments are present.

```
markdown-sanitizer                         # stdin → stdout
markdown-sanitizer article.md              # file → stdout
markdown-sanitizer article.md -o clean.html # file → file
```

### Alternatives

| Strategy | Pros | Cons |
|----------|------|------|
| **File + stdin/stdout** (current) | Unix-composable (`cat file | sanitizer | tee out.html`). Works in pipelines. | Must handle both paths in read/write code. |
| **File-only** | Simpler code. No stdin edge cases. | Can't pipe. Breaks Unix conventions. |
| **stdin/stdout only** | Simplest possible I/O. | Requires shell redirection for file-to-file. Loses the file path for error messages. |
| **In-place (`-i` flag)** | Convenient for batch processing. | Destructive — overwrites the original. Dangerous without backups. |

### The Unix filter convention

The current choice follows the **Unix filter pattern**: programs that read from stdin and write to stdout when no arguments are given. This makes the tool composable:

```sh
# Chain with other tools
cat draft.md | markdown-sanitizer | wc -l

# Process many files
for f in posts/*.md; do
    markdown-sanitizer "$f" -o "build/$(basename "$f" .md).html"
done

# Use with find + xargs
find docs/ -name '*.md' -exec markdown-sanitizer {} -o {}.html \;
```

### Decision Guidance

- **CLI tool?** → Always support stdin/stdout. It's the expected behavior.
- **Batch processing?** → Add a `--glob` or `--dir` mode that processes many files in one invocation.
- **Overwrite original?** → Only behind an explicit `--in-place` flag with a `--backup` option.

---

## 8. Testing Strategy

**Current choice:** `#[cfg(test)]` inline module with `tempfile` for I/O tests.

### What's tested in the current suite

| Test | What it proves |
|------|----------------|
| `renders_markdown_headings_and_emphasis` | `pulldown-cmark` produces expected HTML for basic formatting |
| `strips_script_tags_from_embedded_html` | Sanitizer removes `<script>` (XSS prevention) |
| `removes_javascript_urls_from_links` | Sanitizer strips `javascript:` href (XSS prevention) |
| `run_reads_from_file_and_writes_sanitized_output` | Full round-trip: file → pipeline → file |
| `pipeline_can_be_reused_with_custom_markdown_options` | `with_markdown_options()` builder works correctly |

### Alternative testing approaches

| Approach | Crate | Pros | Cons |
|----------|-------|------|------|
| **Inline `#[cfg(test)]`** (current) | (std) | Tests live next to the code. No separate files. | Tests and source in one big file. |
| **`tests/` directory** (integration tests) | (std) | Tests see the crate as an external consumer. Catches visibility bugs. | Can't test private functions. |
| **Snapshot testing** | `insta` | Assert against saved output files. Easy to update when output changes intentionally. | Another dev-dependency. Must review snapshot diffs carefully. |
| **Fuzz testing** | `cargo-fuzz` | Finds crashes and panics in edge cases. Excellent for parsers. | Runs indefinitely. Requires corpus. Setup overhead. |
| **Property testing** | `proptest` | Generates random inputs and checks invariants (e.g., "output never contains `<script>`"). | Slower. Requires defining properties. |

### Fuzz testing is especially valuable here

Markdown → HTML → sanitize is a pipeline of two parsers. Parsers are notoriously prone to edge-case panics. A fuzzer that throws random byte sequences at `render_and_sanitize` can find crashes that handwritten tests miss:

```rust
// fuzz/fuzz_targets/render.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        // Should never panic, regardless of input
        let _ = markdown_sanitizer::render_and_sanitize(input);
    }
});
```

```sh
cargo install cargo-fuzz
cargo fuzz run render -- -max_total_time=60
```

### Decision Guidance

- **Getting started?** → Inline `#[cfg(test)]` (current) is fine.
- **Library published to crates.io?** → Add integration tests in `tests/` and snapshot tests with `insta`.
- **Security-sensitive?** → Fuzz test the pipeline. Parsers + sanitizers are exactly the kind of code that benefits most from fuzzing.

📖 [insta](https://docs.rs/insta/latest/insta/) · [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) · [proptest](https://docs.rs/proptest/latest/proptest/)

---

## 9. WASM Compilation (Stretch)

**Current choice:** Not implemented yet. The lib + bin architecture is WASM-ready.

The stretch goal from the study guide is to compile the library to WASM with `wasm-pack` so it can be called from JavaScript.

### How it would work

```rust
// In lib.rs, add a wasm-bindgen export:
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn sanitize_markdown(input: &str) -> String {
    render_and_sanitize(input).sanitized_html
}
```

```sh
# Build the WASM package
wasm-pack build --target web
```

```javascript
// In JavaScript:
import init, { sanitize_markdown } from './pkg/markdown_sanitizer.js';

await init();
const html = sanitize_markdown('# Hello **world**');
```

### Alternatives

| Target | Tool | Notes |
|--------|------|-------|
| **wasm-pack** | `wasm-pack` | Generates npm-ready packages. Handles `wasm-bindgen` glue. |
| **wasm32-unknown-unknown** | `cargo build --target wasm32-...` | Raw WASM without JS bindings. You write the glue. |
| **wasm-bindgen only** | `wasm-bindgen` | Lower-level than wasm-pack. More control over the output. |
| **wasi** | `cargo build --target wasm32-wasi` | Runs in WASI runtimes (Wasmtime, Wasmer) instead of browsers. Has filesystem access. |

### Compatibility considerations

Both `pulldown-cmark` and `ammonia` compile to WASM — they're pure Rust with no system dependencies. The `clap` dependency is only in the CLI path and won't be included in the WASM build if the `#[wasm_bindgen]` functions don't reference `Args`.

The main caveat: `ammonia` pulls in `html5ever`, which is a large crate. The WASM binary will be ~500 KB–1 MB gzipped.

### Decision Guidance

- **Need it in a web app?** → `wasm-pack` with `--target web`.
- **Need it in a Node.js pipeline?** → `wasm-pack` with `--target nodejs`.
- **Need it in a server-side WASM runtime?** → `cargo build --target wasm32-wasi`.
- **Don't need WASM?** → Skip this. The lib + bin architecture means you can add it later without refactoring.

📖 [wasm-pack](https://rustwasm.github.io/docs/wasm-pack/) · [wasm-bindgen](https://rustwasm.github.io/docs/wasm-bindgen/)
