// ============================================================================
// hashes.rs — File change detection via SHA-256 hashing
// ============================================================================
//
// This module tracks which files have been processed by storing their SHA-256
// hashes in a binary file. Before processing a file, we compute its hash
// and compare it to the stored hash — if they match, the file hasn't changed
// and we skip it. This avoids re-processing thousands of unchanged files
// on every run.
//
// Think of it as a simple key-value cache: { file_path: sha256_hash }.
// The cache is serialized to disk using `bincode` (a compact binary format).
// ============================================================================

use std::collections::HashMap;
use std::fs;
// `std::io::Read` is a trait (interface) for reading bytes. We need it in
// scope to call `.read()` on files. In Rust, you must import traits to use
// their methods — they don't work automatically like in Python/JS.
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ============================================================================
// FileHashStore — The main struct for hash-based change detection
// ============================================================================
//
// `#[derive(Debug, Default)]`:
//   - `Debug`: can be printed with `{:?}`
//   - `Default`: `FileHashStore::default()` creates an empty instance
//
// Fields without `pub` are private — only code in this module can access them.
// External code must use the public methods below to interact with the store.
// ============================================================================
#[derive(Debug, Default)]
pub struct FileHashStore {
    /// Where to persist the hash store on disk.
    path: PathBuf,
    /// In-memory map of file_path -> sha256_hash.
    /// `HashMap` is Rust's dict/Map — like Python's `dict` or JS's `Map`.
    hashes: HashMap<String, String>,
}

/// Wrapper struct for serialization. The `Serialize`/`Deserialize` derives
/// let `bincode` (or `serde_json`) automatically convert this to/from bytes.
/// It's like Python's `pickle` but safe and cross-platform.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedHashes(HashMap<String, String>);

// ============================================================================
// Implementation of FileHashStore methods
// ============================================================================
impl FileHashStore {
    /// Loads a hash store from disk, or creates an empty one if the file
    /// doesn't exist yet.
    ///
    /// `impl Into<PathBuf>` accepts anything convertible to a PathBuf
    /// (strings, &str, Path references, etc.) — like duck typing but
    /// checked at compile time.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        // `.into()` converts the argument to PathBuf (whatever type it was).
        let path = path.into();
        if !path.exists() {
            return Ok(Self {
                path,
                hashes: HashMap::new(),
            });
        }

        // Read the entire file into a Vec<u8> (byte vector).
        // `Vec<u8>` is Rust's growable byte array — like Python's `bytes`
        // or Node's `Buffer`.
        let bytes = fs::read(&path)?;
        // Deserialize the binary data back into a HashMap.
        // `bincode::deserialize` is like `pickle.loads()` in Python.
        let PersistedHashes(hashes) = bincode::deserialize(&bytes)?;
        Ok(Self { path, hashes })
    }

    /// Checks if a file needs processing by comparing its current hash
    /// against the stored hash.
    ///
    /// Returns a tuple `(hash, changed)`:
    ///   - `hash`: the SHA-256 of the file (we return it so the caller can
    ///     store it later without recomputing)
    ///   - `changed`: true if the file is new or has been modified
    ///
    /// `&self` = immutable borrow of the store (read-only access).
    pub fn needs_processing(&self, path: &Path) -> Result<(String, bool)> {
        let hash = sha256_file(path)?;
        // `.to_string_lossy()` converts a Path to a string, replacing any
        // invalid Unicode with '�'. Paths on some OSes can contain non-UTF8
        // bytes, so Rust forces you to handle this explicitly.
        let key = path.to_string_lossy();
        // Compare the computed hash to the stored hash (if any).
        // `self.hashes.get(...)` returns `Option<&String>` — Some if found, None if not.
        let changed = self.hashes.get(key.as_ref()) != Some(&hash);
        Ok((hash, changed))
    }

    /// Records that a file has been successfully processed.
    ///
    /// `&mut self` = mutable borrow — this method can modify the store.
    /// In Python, all methods can modify `self`. In Rust, you must declare
    /// the intent with `&mut`.
    pub fn mark_processed(&mut self, path: &Path, hash: String) {
        // `.into_owned()` converts the Cow<str> from `to_string_lossy()`
        // into an owned String, suitable for storing in the HashMap.
        self.hashes
            .insert(path.to_string_lossy().into_owned(), hash);
    }

    /// Persists the hash store to disk in bincode format.
    pub fn save(&self) -> Result<()> {
        // Create parent directories if they don't exist.
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // `.clone()` creates a deep copy of the HashMap. Needed because
        // `PersistedHashes` takes ownership. In Python/JS, this would be
        // implicit — objects are reference-counted. In Rust, you must be
        // explicit about copying data.
        let bytes = bincode::serialize(&PersistedHashes(self.hashes.clone()))?;
        fs::write(&self.path, bytes)?;
        Ok(())
    }

    /// Creates a snapshot (clone) of the internal hash map.
    ///
    /// Used to share hashes across async tasks via `Arc<HashMap<...>>`.
    /// We clone instead of sharing a reference because async tasks may
    /// outlive the original FileHashStore reference.
    pub fn snapshot(&self) -> HashMap<String, String> {
        self.hashes.clone()
    }
}

/// Computes the SHA-256 hash of a file, reading in 8KB chunks.
///
/// Reading in chunks avoids loading the entire file into memory at once,
/// which matters for large files. This is like doing:
///   ```python
///   with open(path, 'rb') as f:
///       while chunk := f.read(8192):
///           hasher.update(chunk)
///   ```
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    // `[0_u8; 8192]` creates a fixed-size array of 8192 zero bytes on the
    // stack. This is NOT heap-allocated like a Vec — it's more like a C
    // array. The `_u8` suffix specifies the element type (unsigned byte).
    let mut buffer = [0_u8; 8192];

    loop {
        // `.read(&mut buffer)` fills the buffer with bytes from the file
        // and returns how many bytes were read. When it returns 0, we've
        // reached the end of the file.
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        // `&buffer[..read]` takes a slice of the first `read` bytes.
        // Slices are like Python's `buffer[:read]` — a view into an array
        // without copying data.
        hasher.update(&buffer[..read]);
    }

    // `format!("{:x}", ...)` formats the hash as lowercase hexadecimal.
    // `.finalize()` consumes the hasher and returns the digest.
    Ok(format!("{:x}", hasher.finalize()))
}

// ============================================================================
// Tests
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn saves_and_detects_changes() {
        let dir = tempdir().unwrap();
        let source_path = dir.path().join("example.txt");
        let cache_path = dir.path().join("cache.bin");

        // Write initial content and check that it's detected as new.
        fs::write(&source_path, "first").unwrap();
        let mut store = FileHashStore::load(&cache_path).unwrap();
        let (hash, changed) = store.needs_processing(&source_path).unwrap();
        assert!(changed);

        // Mark as processed and save to disk.
        store.mark_processed(&source_path, hash);
        store.save().unwrap();

        // Reload from disk — the file should NOT be detected as changed.
        let reloaded = FileHashStore::load(&cache_path).unwrap();
        let (_, changed_again) = reloaded.needs_processing(&source_path).unwrap();
        assert!(!changed_again);

        // Modify the file — now it SHOULD be detected as changed.
        fs::write(&source_path, "second").unwrap();
        let (_, changed_after_edit) = reloaded.needs_processing(&source_path).unwrap();
        assert!(changed_after_edit);
    }
}
