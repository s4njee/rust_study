// ============================================================================
// util.rs — Small utility functions for parsing and string handling
// ============================================================================
//
// These are pure helper functions with no state — the Rust equivalent of
// a Python `utils.py` file or a JS `utils.js` module.
// ============================================================================

use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::{DateTime, NaiveDate};

/// Attempts to parse a date string in multiple formats.
///
/// Returns `Ok(Some(date))` if parsing succeeds, `Ok(None)` if the input
/// is empty, or `Err(...)` if the string is non-empty but unparseable.
///
/// `Option<NaiveDate>` is Rust's way of saying "a date or nothing". `Option`
/// is like Python's `Optional` type hint, but enforced at compile time.
/// You MUST check for `None` before using the value — no null pointer
/// exceptions possible. It has two variants:
///   - `Some(value)` — contains a value
///   - `None` — no value (like Python's `None` or JS's `null`)
///
/// `NaiveDate` means a date without timezone info (just year-month-day).
pub fn parse_date_value(value: &str) -> Result<Option<NaiveDate>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    // Try YYYY-MM-DD format first (most common in our data).
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Ok(Some(date));
    }

    // Try RFC 3339 format (e.g., "2024-01-15T10:30:00-05:00").
    // `.date_naive()` strips the time and timezone, keeping just the date.
    if let Ok(date_time) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(Some(date_time.date_naive()));
    }

    // Try "YYYY-MM-DD HH:MM:SS" format.
    if let Ok(date_time) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
        return Ok(Some(date_time.date()));
    }

    // None of the formats matched — return an error.
    // `anyhow!(...)` creates an error with a formatted message.
    // `{trimmed:?}` uses Debug formatting (adds quotes around strings).
    Err(anyhow!("invalid date {trimmed:?}"))
}

/// Like `parse_date_value`, but swallows errors and returns `None` instead.
///
/// `.ok()` converts `Result<T, E>` to `Option<T>` (discards the error).
/// `.flatten()` collapses `Option<Option<T>>` into `Option<T>`.
///
/// Useful when a missing/bad date isn't fatal — we just want None.
pub fn must_parse_date_value(value: &str) -> Option<NaiveDate> {
    parse_date_value(value).ok().flatten()
}

/// Parses a string as a 32-bit integer.
///
/// `.parse()` is Rust's generic string-to-type parser (like `int()` in
/// Python or `parseInt()` in JS, but type-inferred from context).
/// The `?` propagates the parse error if the string isn't a valid number.
pub fn parse_i32_value(value: &str) -> Result<i32> {
    Ok(value.parse()?)
}

/// Checks if a file exists at the given path.
///
/// `&Path` is a borrowed reference to a path — similar to `&str` for strings.
/// `PathBuf` owns its data; `&Path` borrows it. Like the difference between
/// `str` (owned) and a string reference in other contexts.
pub fn file_exists(path: &Path) -> bool {
    path.exists()
}

/// Converts a string to `Option<String>`: returns `None` if empty.
///
/// `impl Into<String>` is a generic parameter that accepts anything
/// convertible to a String (&str, String, Cow<str>, etc.). This is like
/// Python's duck typing but checked at compile time — the compiler generates
/// a specialized version for each type you call it with.
pub fn option_string(value: impl Into<String>) -> Option<String> {
    let value = value.into();
    if value.is_empty() { None } else { Some(value) }
}
