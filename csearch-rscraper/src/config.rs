// ============================================================================
// config.rs — Application configuration loaded from environment variables
// ============================================================================
//
// This module handles loading and validating configuration, similar to how
// you might use `pydantic.BaseSettings` in Python or a config module that
// reads from `process.env` in Node.
// ============================================================================

use std::env;
use std::path::PathBuf;
// `Mutex` and `OnceLock` are used only in tests (see bottom of file).
// `Mutex` is like Python's `threading.Lock` — it protects shared data
// from concurrent access. `OnceLock` is a thread-safe "initialize once" cell.
use std::sync::{Mutex, OnceLock};

use anyhow::{Result, bail};
use chrono::Datelike;

// ============================================================================
// Config struct
// ============================================================================
//
// `#[derive(Debug, Clone)]` are Rust's "derive macros" — they auto-generate
// trait implementations. Think of traits as interfaces in TypeScript or
// abstract classes/protocols in Python.
//
//   - `Debug`: Allows printing with `{:?}` format (like Python's `__repr__`)
//   - `Clone`: Allows creating a deep copy (like `copy.deepcopy()`)
//
// `pub` means the struct and its fields are accessible from other modules.
// Without `pub`, they'd be private (Rust defaults to private, unlike Python).
// ============================================================================
#[derive(Debug, Clone)]
pub struct Config {
    /// Filesystem path to the congress data directory.
    /// `PathBuf` is Rust's owned path type — like `pathlib.Path` in Python.
    /// It handles OS-specific path separators (/ vs \) automatically.
    pub congress_dir: PathBuf,

    /// PostgreSQL host/URI (just the host portion, not the full DSN).
    pub postgres_uri: String,

    /// Full Redis connection URL (e.g., "redis://localhost:6379").
    pub redis_url: String,

    /// Database credentials and connection details.
    pub db_user: String,
    pub db_password: String,
    pub db_name: String,

    /// `u16` = unsigned 16-bit integer (0–65535). Rust has explicit integer
    /// sizes unlike Python (arbitrary precision) or JS (all numbers are f64).
    /// Port numbers fit perfectly in u16.
    pub db_port: u16,

    /// Feature flags: whether to run the vote/bill pipelines.
    pub run_votes: bool,
    pub run_bills: bool,

    /// Log level string (e.g., "info", "debug", "warn").
    pub log_level: String,
}

// ============================================================================
// `impl Config` — Methods on the Config struct
// ============================================================================
//
// In Rust, methods are defined in `impl` blocks, not inside the struct itself.
// This is different from Python/JS where methods go inside the class body.
// You can have multiple `impl` blocks for the same struct.
// ============================================================================
impl Config {
    /// Loads configuration from environment variables.
    ///
    /// `Result<Self>` means this function returns either `Ok(Config)` or an
    /// error. `Self` is an alias for the type being implemented (`Config`).
    ///
    /// This is roughly equivalent to:
    ///   ```python
    ///   @classmethod
    ///   def load(cls) -> "Config":
    ///       ...
    ///   ```
    pub fn load() -> Result<Self> {
        // Load .env file if it exists.
        let _ = dotenvy::dotenv();

        // `env::var("NAME")` returns `Result<String, VarError>`.
        // `.map(PathBuf::from)` converts the string to a path if present.
        // `.map_err(...)` replaces the error with a more descriptive one.
        // The final `?` propagates the error if the var is missing.
        let congress_dir = env::var("CONGRESSDIR")
            .map(PathBuf::from)
            .map_err(|_| anyhow::anyhow!("missing CONGRESSDIR"))?;

        // `bail!` is anyhow's macro for `return Err(anyhow!(...))` — it's
        // an early return with an error, like `raise ValueError(...)` in Python.
        if !congress_dir.exists() {
            bail!("CONGRESSDIR does not exist: {}", congress_dir.display());
        }

        let postgres_uri =
            env::var("POSTGRESURI").map_err(|_| anyhow::anyhow!("missing POSTGRESURI"))?;

        // Build the Config struct. `Ok(Self { ... })` wraps it in a Result.
        //
        // `.unwrap_or_else(|_| "default".to_string())` provides a default
        // if the env var is missing. This is like `os.getenv("KEY", "default")`
        // in Python or `process.env.KEY || "default"` in JS.
        //
        // `.to_string()` converts a string literal (`&str`, a borrowed
        // reference) into an owned `String`. In Rust, there are two string
        // types:
        //   - `&str`: a borrowed, immutable view (like a pointer to chars)
        //   - `String`: an owned, growable string (like Python/JS strings)
        Ok(Self {
            congress_dir,
            postgres_uri,
            redis_url: env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
            db_user: env::var("DB_USER").unwrap_or_else(|_| "postgres".to_string()),
            db_password: env::var("DB_PASSWORD").unwrap_or_else(|_| "postgres".to_string()),
            db_name: env::var("DB_NAME").unwrap_or_else(|_| "csearch".to_string()),
            // `.ok()` converts `Result` to `Option` (discarding the error).
            // `.and_then(...)` chains an operation on the inner value if present.
            // `.unwrap_or(5432)` provides the default if parsing fails or var is missing.
            // This whole chain is like: `int(os.getenv("DB_PORT", "5432"))` in Python.
            db_port: env::var("DB_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(5432),
            run_votes: env_enabled("RUN_VOTES", true),
            run_bills: env_enabled("RUN_BILLS", true),
            log_level: env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
        })
    }

    /// Builds the full PostgreSQL connection string (DSN).
    /// `&self` means this method borrows the Config immutably — it can read
    /// fields but not modify them. Like a regular method in Python (but
    /// Python's `self` is always mutable).
    pub fn postgres_dsn(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            self.db_user, self.db_password, self.postgres_uri, self.db_port, self.db_name
        )
    }

    /// Returns the path to the Python congress library.
    /// Checks for a local copy first, then falls back to a system-wide install.
    pub fn congress_runtime_dir(&self) -> PathBuf {
        let runtime_dir = self.congress_dir.join("congress");
        if runtime_dir.join("run.py").exists() {
            runtime_dir
        } else {
            // `PathBuf::from(...)` creates a path from a string literal.
            PathBuf::from("/opt/csearch/congress")
        }
    }
}

/// Calculates the current congress number from the current year.
///
/// Congress numbers started in 1789 with the 1st Congress. Each Congress
/// lasts 2 years. So: (current_year - 1789) / 2 + 1.
///
/// `i32` is a signed 32-bit integer — Rust's equivalent of a normal int,
/// but with explicit size. Rust requires you to choose: i8, i16, i32, i64,
/// i128, or isize (pointer-sized). No implicit conversions between them.
pub fn current_congress() -> i32 {
    (chrono::Utc::now().year() - 1789) / 2 + 1
}

/// Parses a boolean environment variable with a default value.
///
/// `&str` is a borrowed string slice — a reference to string data owned
/// elsewhere. Function parameters that only need to *read* a string
/// typically take `&str` instead of `String` (avoids unnecessary copying).
///
/// The `let Ok(value) = ... else { return default_value }` syntax is a
/// "let-else" pattern — it's like an early return if the pattern doesn't
/// match. Similar to: `if value is None: return default` in Python.
pub fn env_enabled(name: &str, default_value: bool) -> bool {
    let Ok(value) = env::var(name) else {
        return default_value;
    };

    // `.trim()` removes whitespace, `.to_ascii_lowercase()` lowercases,
    // `.as_str()` converts `String` back to `&str` for pattern matching.
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default_value,
    }
}

// ============================================================================
// Tests
// ============================================================================
//
// `#[cfg(test)]` means this module only compiles when running `cargo test`.
// It's completely stripped from the production binary. This is similar to
// `if __name__ == "__main__"` guards in Python, but enforced at compile time.
// ============================================================================
#[cfg(test)]
mod tests {
    // `use super::*` imports everything from the parent module (config.rs).
    // Like `from . import *` in Python.
    use super::*;
    use tempfile::TempDir;

    /// Returns a global mutex to serialize env-var-modifying tests.
    ///
    /// Environment variables are process-global state, so tests that modify
    /// them must not run in parallel. `OnceLock` ensures the mutex is
    /// initialized exactly once (like a module-level singleton in Python).
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn load_config_from_env() {
        // `.lock().unwrap()` acquires the mutex. `_guard` keeps the lock
        // held until it goes out of scope (end of the function). This is
        // Rust's RAII pattern — resources are automatically cleaned up
        // when variables are dropped (go out of scope). No try/finally needed.
        let _guard = env_lock().lock().unwrap();
        let temp_dir = TempDir::new().unwrap();

        // `unsafe` block is required because modifying env vars is not
        // thread-safe. Rust forces you to acknowledge this explicitly.
        // In Python/JS, you'd just do `os.environ["KEY"] = value` without
        // any special syntax — the unsafety is hidden from you.
        unsafe {
            env::set_var("CONGRESSDIR", temp_dir.path());
            env::set_var("POSTGRESURI", "localhost");
            env::set_var("RUN_VOTES", "false");
            env::set_var("RUN_BILLS", "true");
            env::set_var("LOG_LEVEL", "debug");
        }

        let cfg = Config::load().unwrap();
        assert_eq!(cfg.congress_dir, temp_dir.path());
        assert_eq!(cfg.postgres_uri, "localhost");
        assert!(!cfg.run_votes);
        assert!(cfg.run_bills);
        assert_eq!(cfg.log_level, "debug");
    }

    #[test]
    fn missing_congress_dir_errors() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            env::remove_var("CONGRESSDIR");
            env::set_var("POSTGRESURI", "localhost");
        }

        let err = Config::load().unwrap_err();
        assert!(err.to_string().contains("missing CONGRESSDIR"));
    }
}
