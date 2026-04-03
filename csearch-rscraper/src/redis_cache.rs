// ============================================================================
// redis_cache.rs — Redis API cache invalidation
// ============================================================================
//
// After writing new data to PostgreSQL, we need to invalidate cached API
// responses in Redis so the frontend serves fresh data. This module finds
// all Redis keys with the "csearch:" prefix and deletes them.
//
// We use Redis SCAN (not KEYS) because SCAN is non-blocking — it iterates
// through keys in batches using a cursor, so it doesn't freeze the Redis
// server the way `KEYS *` would on large datasets.
// ============================================================================

use anyhow::Result;

use crate::config::Config;

/// Prefix for all API cache keys in Redis.
/// `const` is a compile-time constant — like `UPPER_CASE = "value"` in Python
/// or `const` in JS, but truly immutable and inlined by the compiler.
///
/// `&str` (with a `'static` lifetime implied) means this string lives for
/// the entire program duration — it's baked into the binary, not heap-allocated.
const REDIS_CACHE_KEY_PREFIX: &str = "csearch:";

/// Deletes all Redis keys matching "csearch:*" and returns the count deleted.
///
/// This function is `async` because Redis I/O is non-blocking — we `.await`
/// each Redis command, allowing Tokio to do other work while waiting for
/// Redis to respond (like `await redis.call(...)` in Node).
///
/// `usize` is an unsigned pointer-sized integer — used for counts and sizes.
/// On 64-bit systems it's 64 bits. Think of it as Python's `int` but unsigned.
pub async fn clear_api_cache(cfg: &Config) -> Result<usize> {
    // Create a Redis client from the connection URL.
    let client = redis::Client::open(cfg.redis_url.clone())?;

    // `get_multiplexed_async_connection()` creates a single TCP connection
    // that can handle multiple concurrent Redis commands (multiplexed).
    // This is more efficient than one-connection-per-command.
    let mut connection = client.get_multiplexed_async_connection().await?;

    // Ping Redis to verify the connection is alive.
    // The `let _: String` discards the "PONG" response — we just want to
    // know it didn't error.
    let _: String = redis::cmd("PING").query_async(&mut connection).await?;

    // ========================================================================
    // Cursor-based SCAN loop
    // ========================================================================
    // Redis SCAN works like pagination:
    //   1. Start with cursor = 0
    //   2. Each SCAN call returns (next_cursor, keys_batch)
    //   3. When next_cursor == 0, we've iterated through all keys
    //
    // This is the same pattern you'd use in Python with `redis.scan_iter()`,
    // but here we manage the cursor manually.
    // ========================================================================
    let mut cursor = 0_u64;
    let mut deleted = 0_usize;

    loop {
        // SCAN returns a tuple: (next_cursor, list_of_matching_keys).
        // The type annotation `(u64, Vec<String>)` tells Rust what to
        // deserialize the Redis response into. `Vec<String>` is a growable
        // array of strings — like Python's `list[str]` or JS's `string[]`.
        let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .cursor_arg(cursor)
            .arg("MATCH")
            .arg(format!("{REDIS_CACHE_KEY_PREFIX}*"))
            .arg("COUNT")
            .arg(100) // Hint: process ~100 keys per iteration.
            .query_async(&mut connection)
            .await?;

        // Delete the batch of matched keys (if any).
        if !keys.is_empty() {
            let removed: i64 = redis::cmd("DEL")
                .arg(&keys)
                .query_async(&mut connection)
                .await?;
            // `as usize` casts i64 to usize. Rust doesn't do implicit
            // numeric conversions — you must be explicit about type casts.
            deleted += removed as usize;
        }

        cursor = next_cursor;
        // cursor == 0 means Redis has completed the full scan.
        if cursor == 0 {
            break;
        }
    }

    Ok(deleted)
}
