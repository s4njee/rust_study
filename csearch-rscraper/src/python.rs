// ============================================================================
// python.rs — Running Python subprocesses to sync congress data
// ============================================================================
//
// The congress data sync tool is written in Python. This module spawns it
// as a child process, captures its stdout/stderr, and streams the output
// into our structured logs.
//
// This is similar to Python's `subprocess.Popen()` or Node's
// `child_process.spawn()`, but using Tokio's async process API so we
// don't block the async runtime while waiting for the process to finish.
// ============================================================================

use std::env;
use std::path::Path;

use anyhow::{Context, Result, bail};
// Tokio provides async versions of I/O primitives. `AsyncBufReadExt` adds
// the `.lines()` method to async readers (like `readline()` in Python),
// and `BufReader` wraps a reader with an internal buffer for efficiency.
use tokio::io::{AsyncBufReadExt, BufReader};
// `tokio::process::Command` is the async version of `std::process::Command`.
// Like Python's `asyncio.create_subprocess_exec()` — it spawns a process
// without blocking the event loop.
use tokio::process::Command;
use tracing::info;

use crate::config::Config;

/// Spawns a Python subprocess to run a congress data sync task.
///
/// `args: &[&str]` is a slice of string references — like `List[str]` in
/// Python or `string[]` in JS, but borrowed (not owned). Slices are Rust's
/// way of passing arrays without transferring ownership. For example,
/// `&["votes", "--congress=118"]` creates a temporary slice of two strings.
pub async fn run_congress_task(cfg: &Config, args: &[&str]) -> Result<()> {
    let congress_dir = cfg.congress_runtime_dir();
    let run_py = congress_dir.join("run.py");

    // Build the command — equivalent to:
    //   subprocess.Popen(
    //       ["python3", "run.py", *args],
    //       cwd=congress_dir,
    //       env={**os.environ, "PYTHONPATH": ...},
    //       stdout=subprocess.PIPE,
    //       stderr=subprocess.PIPE,
    //   )
    let mut command = Command::new("python3");
    command.arg(&run_py);
    command.args(args);
    command.current_dir(&congress_dir);
    command.env("PYTHONPATH", python_path_for_congress_dir(&congress_dir));
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    // `.spawn()` starts the process. `.with_context(...)` adds a descriptive
    // message to the error if spawning fails (e.g., python3 not found).
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {}", run_py.display()))?;

    // `.take()` extracts the stdout/stderr handles from the child process.
    // It returns `Option<ChildStdout>` — `take()` gives us ownership and
    // replaces the field with `None`. This is a common Rust pattern for
    // transferring ownership out of a struct.
    // `.context(...)` converts `None` into an error with a message.
    let stdout = child.stdout.take().context("missing python stdout")?;
    let stderr = child.stderr.take().context("missing python stderr")?;

    // ========================================================================
    // Concurrent output streaming with tokio::spawn
    // ========================================================================
    //
    // `tokio::spawn()` creates a new async task that runs concurrently —
    // like launching a goroutine in Go, or `asyncio.create_task()` in Python,
    // or `Promise.all()` in JS.
    //
    // We spawn TWO tasks: one for stdout, one for stderr. This way, both
    // streams are read simultaneously. If we read them sequentially, one
    // stream's buffer could fill up and block the child process (deadlock).
    //
    // Each `tokio::spawn` returns a `JoinHandle` — a future that resolves
    // when the task completes. We `.await` them later to collect results.
    // ========================================================================
    let stdout_task = tokio::spawn(stream_output("stdout", stdout));
    let stderr_task = tokio::spawn(stream_output("stderr", stderr));

    // Wait for the process to exit. This suspends our task (not the thread!)
    // until the child process terminates.
    let status = child.wait().await?;

    // Wait for both streaming tasks to finish.
    // The double `?` is because:
    //   - First `?`: unwraps the JoinHandle result (handles task panics)
    //   - Second `?`: unwraps the Result from stream_output (handles I/O errors)
    // This is like `await task` in Python, where you might get either a
    // CancelledError (task-level) or an exception from the coroutine itself.
    stdout_task.await??;
    stderr_task.await??;

    // `bail!` returns an error if the process exited with a non-zero code.
    if !status.success() {
        bail!("python task failed with status {status}");
    }

    Ok(())
}

/// Builds the PYTHONPATH environment variable for the congress library.
///
/// `&Path` is a borrowed reference to a path (read-only).
/// Returns an owned `String` because the caller needs to pass it to
/// the process builder, which takes ownership.
fn python_path_for_congress_dir(congress_dir: &Path) -> String {
    // `vec![...]` creates a new Vec (growable array) with initial elements.
    // Like `[value]` in Python.
    let mut parts = vec![
        congress_dir
            .parent()
            .unwrap_or(congress_dir)
            .to_string_lossy()
            .into_owned(),
    ];

    // Append existing PYTHONPATH if set (preserves user's Python path).
    if let Ok(existing) = env::var("PYTHONPATH") {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }

    // Join path components with ":" on Unix or ";" on Windows.
    // `cfg!(windows)` is a compile-time check — the compiler picks one
    // branch and completely removes the other from the binary.
    parts.join(if cfg!(windows) { ";" } else { ":" })
}

/// Reads lines from an async reader and logs each line.
///
/// `source: &'static str` — the `'static` lifetime means this string must
/// live forever (i.e., it must be a string literal like "stdout" or "stderr").
/// This is required because `tokio::spawn` may run the task on any thread
/// at any time, so it can't hold references to short-lived data.
///
/// `impl tokio::io::AsyncRead + Unpin` is a generic parameter meaning
/// "anything that supports async reading and can be moved in memory".
///   - `AsyncRead`: the trait for async byte streams (like Node's Readable)
///   - `Unpin`: a marker trait saying "this value can be safely moved in
///     memory". Most types are Unpin by default. It's a Rust-specific concept
///     related to how async/await works internally with pinned memory.
async fn stream_output(
    source: &'static str,
    reader: impl tokio::io::AsyncRead + Unpin,
) -> Result<()> {
    // Wrap the raw reader in a buffered reader, then get an async line iterator.
    // This is like `for line in io.TextIOWrapper(stream)` in Python.
    let mut lines = BufReader::new(reader).lines();

    // `.next_line().await?` reads one line at a time asynchronously.
    // Returns `Option<String>` — `Some(line)` for each line, `None` at EOF.
    // `while let Some(line) = ...` is pattern matching in a loop — it
    // keeps looping as long as the pattern matches (i.e., until EOF).
    while let Some(line) = lines.next_line().await? {
        // Log each line with structured metadata (which stream it came from).
        info!(stream = source, output = line, "python");
    }

    Ok(())
}
