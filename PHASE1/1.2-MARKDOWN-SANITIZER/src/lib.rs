use anyhow::{Context, Result};
use clap::Parser;
use pulldown_cmark::{Options, Parser as MarkdownParser, html};
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// `Args` is intentionally tiny because the interesting logic lives in the
/// library API, not in the binary wrapper.
///
/// This is a useful pattern for study projects and production crates alike:
/// keep `clap` at the edge of the application, then pass plain Rust values into
/// reusable functions. That makes the code easier to test, easier to embed in
/// another binary, and easier to port later into a web handler or WASM module.
#[derive(Debug, Parser)]
#[command(
    name = "markdown-sanitizer",
    version,
    about = "Render Markdown to HTML and sanitize unsafe markup."
)]
pub struct Args {
    /// Read markdown from a file. If omitted, read from stdin.
    pub input: Option<PathBuf>,

    /// Write the sanitized HTML to a file. If omitted, write to stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

/// A small bundle of intermediate and final results that is handy for tests
/// and for callers that want access to both render stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDocument {
    /// The HTML produced directly by the markdown parser before sanitization.
    ///
    /// Keeping this around is useful when:
    /// - tests want to verify the renderer and sanitizer separately,
    /// - a caller wants to diff "unsafe" vs. "safe" output,
    /// - a debugging tool wants to show each pipeline stage.
    pub raw_html: String,

    /// The final HTML after the sanitizer removes dangerous content.
    pub sanitized_html: String,
}

/// `MarkdownPipeline` packages together the configuration for the full
/// markdown -> HTML -> sanitized HTML flow.
///
/// Putting options into a struct instead of hard-coding them inside free
/// functions is a practical reuse pattern:
/// - applications can share one configured pipeline,
/// - tests can tweak individual behaviors,
/// - future extensions do not require breaking all existing function calls.
#[derive(Debug)]
pub struct MarkdownPipeline {
    /// Rendering flags for `pulldown-cmark`.
    ///
    /// We store them on the pipeline so callers can opt into a custom markdown
    /// dialect without rewriting the rendering logic itself.
    markdown_options: Options,

    /// Sanitizer policy object from `ammonia`.
    ///
    /// `ammonia::Builder` lets callers tune which tags, attributes, and URL
    /// schemes should be preserved. Holding the builder in the pipeline makes
    /// the sanitization step configurable while keeping the simple API.
    sanitizer: ammonia::Builder<'static>,
}

impl Default for MarkdownPipeline {
    fn default() -> Self {
        Self {
            markdown_options: default_markdown_options(),
            sanitizer: ammonia::Builder::default(),
        }
    }
}

impl MarkdownPipeline {
    /// Create a pipeline with sensible defaults for a blog- or CMS-style
    /// markdown workflow.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the markdown option set.
    ///
    /// Returning `Self` supports a builder-style API:
    /// `MarkdownPipeline::new().with_markdown_options(custom_options)`.
    pub fn with_markdown_options(mut self, markdown_options: Options) -> Self {
        self.markdown_options = markdown_options;
        self
    }

    /// Replace the sanitizer configuration.
    ///
    /// This is where a caller can define a stricter or looser HTML policy while
    /// still reusing the rest of the pipeline.
    pub fn with_sanitizer(mut self, sanitizer: ammonia::Builder<'static>) -> Self {
        self.sanitizer = sanitizer;
        self
    }

    /// Convert markdown into HTML using the pipeline's configured markdown
    /// extensions.
    pub fn render_markdown(&self, markdown: &str) -> String {
        let parser = MarkdownParser::new_ext(markdown, self.markdown_options);

        let mut html_output = String::new();
        html::push_html(&mut html_output, parser);
        html_output
    }

    /// Sanitize HTML using the configured `ammonia` policy.
    pub fn sanitize_html(&self, html: &str) -> String {
        self.sanitizer.clean(html).to_string()
    }

    /// Run the full pipeline and return both the raw and sanitized HTML.
    pub fn render_and_sanitize(&self, markdown: &str) -> RenderedDocument {
        let raw_html = self.render_markdown(markdown);
        let sanitized_html = self.sanitize_html(&raw_html);

        RenderedDocument {
            raw_html,
            sanitized_html,
        }
    }
}

/// Return the default markdown flags used by this crate.
///
/// Keeping this in a dedicated helper makes the default behavior explicit and
/// gives callers a convenient starting point when they want to extend or reduce
/// the feature set.
pub fn default_markdown_options() -> Options {
    Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
}

/// Convert markdown into HTML with the crate's default pipeline.
///
/// This free function is the "easy mode" API: great for small programs and
/// examples. Under the hood it delegates to `MarkdownPipeline`, which is the
/// more reusable abstraction for larger applications.
pub fn render_markdown(markdown: impl AsRef<str>) -> String {
    MarkdownPipeline::default().render_markdown(markdown.as_ref())
}

/// Sanitize HTML with the crate's default sanitizer policy.
pub fn sanitize_html(html: impl AsRef<str>) -> String {
    MarkdownPipeline::default().sanitize_html(html.as_ref())
}

/// Run the full pipeline with the crate's default configuration.
pub fn render_and_sanitize(markdown: impl AsRef<str>) -> RenderedDocument {
    MarkdownPipeline::default().render_and_sanitize(markdown.as_ref())
}

/// Parse CLI arguments, execute the tool, and map failures to exit codes.
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

/// Execute the CLI workflow.
///
/// `run` accepts plain parsed arguments and returns `anyhow::Result<()>`, which
/// is another pattern worth copying into future projects:
/// - argument parsing stays near the binary entrypoint,
/// - side effects are isolated here,
/// - the function can be tested directly without spawning a subprocess.
pub fn run(args: Args) -> Result<()> {
    let input = read_input(args.input.as_ref())?;
    let pipeline = MarkdownPipeline::default();
    let rendered = pipeline.render_and_sanitize(&input);
    write_output(args.output.as_ref(), &rendered.sanitized_html)?;
    Ok(())
}

/// Read markdown either from a file path or from standard input.
///
/// A small I/O helper like this is often more reusable than placing the logic
/// inline in `run`, because the branching behavior is now named, documented,
/// and independently testable.
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

/// Write sanitized HTML either to a file or to standard output.
///
/// Notice that we accept `&str` here instead of `RenderedDocument`. Keeping the
/// function focused on one job makes it easier to reuse in other contexts where
/// the caller already has an HTML string from somewhere else.
fn write_output(path: Option<&PathBuf>, html: &str) -> Result<()> {
    match path {
        Some(path) => fs::write(path, html)
            .with_context(|| format!("failed to write output file '{}'", path.display())),
        None => {
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(html.as_bytes())
                .context("failed to write sanitized HTML to stdout")?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn renders_markdown_headings_and_emphasis() {
        let rendered = render_and_sanitize("# Hello\n\nThis is **important**.");

        assert!(rendered.raw_html.contains("<h1>Hello</h1>"));
        assert!(
            rendered
                .sanitized_html
                .contains("<strong>important</strong>")
        );
    }

    #[test]
    fn strips_script_tags_from_embedded_html() {
        let rendered = render_and_sanitize("before<script>alert('xss')</script>after");

        assert!(rendered.raw_html.contains("<script>alert('xss')</script>"));
        assert_eq!(rendered.sanitized_html, "<p>beforeafter</p>\n");
    }

    #[test]
    fn removes_javascript_urls_from_links() {
        let rendered = render_and_sanitize("[click me](javascript:alert('xss'))");

        assert!(rendered.raw_html.contains("href=\"javascript:alert"));
        assert_eq!(
            rendered.sanitized_html,
            "<p><a rel=\"noopener noreferrer\">click me</a></p>\n"
        );
    }

    #[test]
    fn run_reads_from_file_and_writes_sanitized_output() {
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
        assert!(saved.contains("<a rel=\"noopener noreferrer\">safe?</a>"));
        assert!(!saved.contains("javascript:"));
    }

    #[test]
    fn pipeline_can_be_reused_with_custom_markdown_options() {
        let pipeline = MarkdownPipeline::new().with_markdown_options(Options::empty());
        let rendered = pipeline.render_markdown("~not strikethrough~");

        assert!(rendered.contains("<p>~not strikethrough~</p>"));
    }
}
