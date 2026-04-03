// ============================================================================
// stats.rs — Simple counters for tracking scraper run statistics
// ============================================================================
//
// This is a plain data struct with no behavior beyond a single helper method.
// In Python, you'd likely use a `@dataclass`. In JS, just a plain object.
//
// `#[derive(Debug, Default)]`:
//   - `Debug`: auto-generates a way to print the struct for debugging
//     (like Python's `__repr__`).
//   - `Default`: auto-generates a constructor that zeros out all numeric
//     fields. So `RunStats::default()` gives you all zeros — like calling
//     `RunStats(0, 0, 0, 0, 0, 0)`.
//
// `u64` = unsigned 64-bit integer (0 to ~18 quintillion). Unsigned because
// counts can never be negative, and 64-bit gives plenty of headroom.
// ============================================================================

#[derive(Debug, Default)]
pub struct RunStats {
    pub bills_processed: u64,
    pub bills_skipped: u64,
    pub bills_failed: u64,
    pub votes_processed: u64,
    pub votes_skipped: u64,
    pub votes_failed: u64,
}

impl RunStats {
    /// Returns true if any bills or votes were actually written to the database.
    /// Used to decide whether to clear the Redis cache.
    pub fn has_writes(&self) -> bool {
        self.bills_processed > 0 || self.votes_processed > 0
    }
}
