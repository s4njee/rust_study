use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use walkdir::WalkDir;

/// The buffer size we use while reading a file during hashing.
///
/// We intentionally read files in chunks instead of loading the entire file into memory.
/// That makes the tool work well for both tiny text files and much larger binary files.
const HASH_BUFFER_SIZE: usize = 8 * 1024;

/// We keep a tiny version number in the serialized cache format.
///
/// Even though this project is small, versioning is a good habit to build early.
/// If we ever change the cache structure later, we can detect older files and decide
/// whether to migrate them or show a helpful error.
const CACHE_FORMAT_VERSION: u32 = 1;

/// `clap` turns this struct into a full command-line interface:
/// - it parses user input,
/// - validates types,
/// - generates `--help`,
/// - and gives us a strongly typed value to work with.
#[derive(Debug, Parser)]
#[command(
    name = "cli-hash-cache",
    version,
    about = "Hash files in a directory, persist the results, and report changes between runs."
)]
pub struct Args {
    /// The directory we want to scan recursively.
    ///
    /// This is positional because it is the most important required input.
    pub directory: PathBuf,

    /// Where to store the cache on disk.
    ///
    /// We keep this optional so we can choose a sensible default based on the
    /// selected output format. If omitted:
    /// - bincode mode uses `<DIRECTORY>/.hash_cache.bin`
    /// - JSON mode uses `<DIRECTORY>/.hash_cache.json`
    #[arg(short, long)]
    pub cache_file: Option<PathBuf>,

    /// Save the cache in JSON instead of bincode.
    ///
    /// JSON is easier to inspect manually during learning and debugging.
    /// Bincode is more compact and faster to serialize/deserialize.
    #[arg(long)]
    pub json: bool,

    /// Suppress the list of unchanged files.
    ///
    /// This becomes useful once the directory is large. By default, we show
    /// unchanged files too, because this repository is meant for study and
    /// visibility is more important than terseness.
    #[arg(short, long)]
    pub quiet: bool,
}

/// The persisted representation we write to disk.
///
/// A `BTreeMap` keeps keys sorted, which makes:
/// - debug output stable,
/// - JSON files easier to read,
/// - test results deterministic.
///
/// We store paths relative to the scanned root instead of absolute paths.
/// That choice makes the cache more portable and easier to inspect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StoredCache {
    version: u32,
    algorithm: String,
    entries: BTreeMap<PathBuf, String>,
}

impl StoredCache {
    /// Create a new cache object from freshly computed entries.
    fn new(entries: BTreeMap<PathBuf, String>) -> Self {
        Self {
            version: CACHE_FORMAT_VERSION,
            algorithm: "sha256".to_string(),
            entries,
        }
    }
}

/// A single file we discovered while walking the directory tree.
///
/// We keep both paths:
/// - `absolute_path` is used for actually opening the file on disk
/// - `relative_path` is used as the cache key and for user-facing reporting
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredFile {
    relative_path: PathBuf,
    absolute_path: PathBuf,
}

/// The different serialization formats our cache can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheFormat {
    Bincode,
    Json,
}

impl CacheFormat {
    /// Pick the format from the `--json` flag.
    fn from_args(args: &Args) -> Self {
        if args.json { Self::Json } else { Self::Bincode }
    }

    /// Return the default cache filename for this format.
    fn default_filename(self) -> &'static str {
        match self {
            Self::Bincode => ".hash_cache.bin",
            Self::Json => ".hash_cache.json",
        }
    }
}

/// This stores the comparison between the previous run and the current run.
///
/// Splitting the diff into categories makes the report easier to understand and
/// gives us a natural place to add tests.
#[derive(Debug, Default, PartialEq, Eq)]
struct DiffReport {
    added: Vec<PathBuf>,
    changed: Vec<PathBuf>,
    removed: Vec<PathBuf>,
    unchanged: Vec<PathBuf>,
}

impl DiffReport {
    /// Return `true` when something meaningful changed between runs.
    fn has_changes(&self) -> bool {
        !(self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty())
    }
}

/// A small summary returned by the top-level `run` function.
///
/// We separate the *work* from the final process exit code so the logic stays
/// easy to test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOutcome {
    changes_detected: bool,
}

impl RunOutcome {
    /// Convert the outcome into the CLI's exit status policy:
    /// - `0` means the scan completed and nothing changed
    /// - `1` means the scan completed and changes were detected
    pub fn exit_code(self) -> ExitCode {
        if self.changes_detected {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        }
    }
}

/// Parse CLI arguments, run the tool, and convert failures into exit codes.
///
/// We keep this wrapper public so `src/main.rs` can stay tiny and focused.
pub fn main_exit_code() -> ExitCode {
    let args = Args::parse();

    match run(args) {
        Ok(outcome) => outcome.exit_code(),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// The main application workflow:
/// 1. Resolve the directory and cache file paths
/// 2. Load the previous cache if it exists
/// 3. Walk the directory and hash every file
/// 4. Diff old vs. new hashes
/// 5. Print a report
/// 6. Persist the new cache atomically
pub fn run(args: Args) -> Result<RunOutcome> {
    let format = CacheFormat::from_args(&args);
    let scan_root = canonicalize_directory(&args.directory)?;
    let cache_path = resolve_cache_path(&scan_root, args.cache_file.as_deref(), format)?;

    let previous_cache = load_cache(&cache_path, format)?;
    let discovered_files = discover_files(&scan_root, &cache_path)?;
    let current_entries = hash_files(&discovered_files)?;
    let current_cache = StoredCache::new(current_entries);
    let diff = diff_caches(&previous_cache.entries, &current_cache.entries);

    print_report(&scan_root, &cache_path, format, &diff, args.quiet);
    save_cache(&cache_path, format, &current_cache)?;

    Ok(RunOutcome {
        changes_detected: diff.has_changes(),
    })
}

/// Convert the directory argument into a canonical absolute path.
///
/// Canonicalization resolves:
/// - `.` and `..`
/// - symlinked path segments
/// - relative paths from the current working directory
///
/// Using a canonical root makes path comparisons more reliable later.
fn canonicalize_directory(directory: &Path) -> Result<PathBuf> {
    let metadata = fs::metadata(directory).with_context(|| {
        format!(
            "failed to read metadata for directory '{}'",
            directory.display()
        )
    })?;

    if !metadata.is_dir() {
        bail!("'{}' is not a directory", directory.display());
    }

    fs::canonicalize(directory)
        .with_context(|| format!("failed to canonicalize directory '{}'", directory.display()))
}

/// Decide where the cache file lives.
///
/// If the user passed `--cache-file`, we respect it. Relative cache paths are
/// interpreted relative to the current working directory, which matches normal
/// CLI expectations.
///
/// If no cache path was given, we place the cache *inside* the scanned
/// directory using a format-specific default filename.
fn resolve_cache_path(
    scan_root: &Path,
    cache_file: Option<&Path>,
    format: CacheFormat,
) -> Result<PathBuf> {
    match cache_file {
        Some(path) if path.is_absolute() => Ok(path.to_path_buf()),
        Some(path) => Ok(std::env::current_dir()
            .context("failed to determine the current working directory")?
            .join(path)),
        None => Ok(scan_root.join(format.default_filename())),
    }
}

/// Walk the target directory recursively and collect files to hash.
///
/// A few details matter here:
/// - We skip directories because only file contents are hashed
/// - We skip symlinks because following them can create surprising behavior
/// - We skip the cache file itself so the tool does not hash its own output
/// - We print warnings for traversal errors instead of failing the entire run
fn discover_files(scan_root: &Path, cache_path: &Path) -> Result<Vec<DiscoveredFile>> {
    let mut discovered = Vec::new();

    for entry in WalkDir::new(scan_root) {
        match entry {
            Ok(entry) => {
                let path = entry.path();

                if path == cache_path {
                    continue;
                }

                let file_type = entry.file_type();

                if !file_type.is_file() {
                    continue;
                }

                let relative_path = path.strip_prefix(scan_root).with_context(|| {
                    format!(
                        "failed to strip '{}' from '{}'",
                        scan_root.display(),
                        path.display()
                    )
                })?;

                discovered.push(DiscoveredFile {
                    relative_path: relative_path.to_path_buf(),
                    absolute_path: path.to_path_buf(),
                });
            }
            Err(error) => {
                eprintln!("warning: failed to read a directory entry: {error}");
            }
        }
    }

    discovered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(discovered)
}

/// Hash every discovered file and return a map of `relative path -> SHA-256 hex digest`.
///
/// We keep this loop explicit instead of trying to be clever so the flow is easy
/// to study:
/// 1. open a file
/// 2. hash it
/// 3. insert it into the map
fn hash_files(files: &[DiscoveredFile]) -> Result<BTreeMap<PathBuf, String>> {
    let mut entries = BTreeMap::new();

    for file in files {
        let digest = hash_file(&file.absolute_path)?;
        entries.insert(file.relative_path.clone(), digest);
    }

    Ok(entries)
}

/// Compute the SHA-256 hash of one file.
///
/// This function demonstrates a common systems-programming pattern in Rust:
/// buffered, chunked I/O.
///
/// Why not `fs::read(path)`?
/// - `fs::read` loads the whole file into memory
/// - that is fine for small files
/// - but unnecessary and less scalable for large files
///
/// By reading fixed-size chunks, we keep memory usage predictable.
fn hash_file(path: &Path) -> Result<String> {
    let file =
        File::open(path).with_context(|| format!("failed to open file '{}'", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_SIZE];

    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .with_context(|| format!("failed while reading '{}'", path.display()))?;

        if bytes_read == 0 {
            break;
        }

        hasher.update(&buffer[..bytes_read]);
    }

    let digest = hasher.finalize();
    Ok(format!("{digest:x}"))
}

/// Load the previous cache from disk.
///
/// If the file does not exist, that is not an error. It simply means this is the
/// first run, so we return an empty cache.
fn load_cache(cache_path: &Path, format: CacheFormat) -> Result<StoredCache> {
    if !cache_path.exists() {
        return Ok(StoredCache::new(BTreeMap::new()));
    }

    let bytes = fs::read(cache_path)
        .with_context(|| format!("failed to read cache file '{}'", cache_path.display()))?;

    let cache = match format {
        CacheFormat::Bincode => {
            let config = bincode::config::standard();
            let (cache, _bytes_read): (StoredCache, usize) =
                bincode::serde::decode_from_slice(&bytes, config).with_context(|| {
                    format!(
                        "failed to deserialize bincode cache '{}'",
                        cache_path.display()
                    )
                })?;
            cache
        }
        CacheFormat::Json => serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "failed to deserialize JSON cache '{}'",
                cache_path.display()
            )
        })?,
    };

    validate_cache(cache, cache_path)
}

/// Basic validation for the cache we just loaded.
///
/// In small tools, lightweight validation goes a long way toward producing
/// helpful errors instead of mysterious behavior.
fn validate_cache(cache: StoredCache, cache_path: &Path) -> Result<StoredCache> {
    if cache.version != CACHE_FORMAT_VERSION {
        bail!(
            "cache file '{}' uses unsupported version {} (expected {})",
            cache_path.display(),
            cache.version,
            CACHE_FORMAT_VERSION
        );
    }

    if cache.algorithm != "sha256" {
        bail!(
            "cache file '{}' uses unsupported algorithm '{}'",
            cache_path.display(),
            cache.algorithm
        );
    }

    Ok(cache)
}

/// Compare the previous and current hash maps.
///
/// The algorithm is intentionally straightforward:
/// - loop over the new map to find added, changed, and unchanged files
/// - loop over the old map to find removed files
///
/// Because we use sorted maps, the resulting vectors also come out in a stable order.
fn diff_caches(
    previous: &BTreeMap<PathBuf, String>,
    current: &BTreeMap<PathBuf, String>,
) -> DiffReport {
    let mut diff = DiffReport::default();

    for (path, current_hash) in current {
        match previous.get(path) {
            None => diff.added.push(path.clone()),
            Some(previous_hash) if previous_hash == current_hash => {
                diff.unchanged.push(path.clone())
            }
            Some(_) => diff.changed.push(path.clone()),
        }
    }

    for path in previous.keys() {
        if !current.contains_key(path) {
            diff.removed.push(path.clone());
        }
    }

    diff
}

/// Print a human-readable report to standard output.
///
/// This is deliberately plain text rather than structured output because the
/// goal of the project is learning the mechanics first.
fn print_report(
    scan_root: &Path,
    cache_path: &Path,
    format: CacheFormat,
    diff: &DiffReport,
    quiet: bool,
) {
    println!("Scan root : {}", scan_root.display());
    println!("Cache file: {}", cache_path.display());
    println!(
        "Format    : {}",
        match format {
            CacheFormat::Bincode => "bincode",
            CacheFormat::Json => "json",
        }
    );
    println!();

    print_path_section("Added", &diff.added);
    print_path_section("Changed", &diff.changed);
    print_path_section("Removed", &diff.removed);

    if !quiet {
        print_path_section("Unchanged", &diff.unchanged);
    }

    println!("Summary");
    println!("  added    : {}", diff.added.len());
    println!("  changed  : {}", diff.changed.len());
    println!("  removed  : {}", diff.removed.len());
    println!("  unchanged: {}", diff.unchanged.len());

    if diff.has_changes() {
        println!();
        println!("Changes detected.");
    } else {
        println!();
        println!("No changes detected.");
    }
}

/// Helper for printing one list of paths in a consistent format.
fn print_path_section(label: &str, paths: &[PathBuf]) {
    println!("{label}");

    if paths.is_empty() {
        println!("  (none)");
        println!();
        return;
    }

    for path in paths {
        println!("  {}", path.display());
    }

    println!();
}

/// Persist the cache to disk using an atomic write pattern.
///
/// Atomic writes are a good habit whenever you rewrite an important file:
/// 1. serialize the content in memory
/// 2. write it to a temporary file in the same directory
/// 3. rename the temporary file into place
///
/// Writing in the same directory matters because `rename` is most reliable when
/// source and destination share a filesystem.
fn save_cache(cache_path: &Path, format: CacheFormat, cache: &StoredCache) -> Result<()> {
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directory for cache '{}'",
                cache_path.display()
            )
        })?;
    }

    let serialized = match format {
        CacheFormat::Bincode => {
            let config = bincode::config::standard();
            bincode::serde::encode_to_vec(cache, config)
                .context("failed to serialize cache to bincode")?
        }
        CacheFormat::Json => {
            serde_json::to_vec_pretty(cache).context("failed to serialize cache to JSON")?
        }
    };

    let temp_path = temporary_cache_path(cache_path);
    let mut temp_file = File::create(&temp_path).with_context(|| {
        format!(
            "failed to create temporary cache file '{}'",
            temp_path.display()
        )
    })?;

    temp_file.write_all(&serialized).with_context(|| {
        format!(
            "failed to write temporary cache file '{}'",
            temp_path.display()
        )
    })?;

    temp_file.flush().with_context(|| {
        format!(
            "failed to flush temporary cache file '{}'",
            temp_path.display()
        )
    })?;

    drop(temp_file);

    if cache_path.exists() {
        fs::remove_file(cache_path).with_context(|| {
            format!("failed to remove old cache file '{}'", cache_path.display())
        })?;
    }

    fs::rename(&temp_path, cache_path).with_context(|| {
        format!(
            "failed to move temporary cache '{}' into '{}'",
            temp_path.display(),
            cache_path.display()
        )
    })?;

    Ok(())
}

/// Build the temporary filename used during atomic writes.
fn temporary_cache_path(cache_path: &Path) -> PathBuf {
    let file_name = cache_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hash_cache".to_string());

    cache_path.with_file_name(format!("{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Write a file inside a temporary directory and create parents if needed.
    fn write_file(root: &Path, relative_path: &str, contents: &str) {
        let absolute_path = root.join(relative_path);

        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).expect("failed to create test parent directory");
        }

        fs::write(&absolute_path, contents).expect("failed to write test file");
    }

    #[test]
    fn hash_file_matches_known_sha256_digest() {
        let temp_dir = tempdir().expect("failed to create tempdir");
        let file_path = temp_dir.path().join("sample.txt");
        fs::write(&file_path, "abc").expect("failed to write sample file");

        let digest = hash_file(&file_path).expect("hash_file should succeed");

        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn discover_files_skips_cache_file() {
        let temp_dir = tempdir().expect("failed to create tempdir");
        write_file(temp_dir.path(), "notes.txt", "hello");
        write_file(temp_dir.path(), "nested/data.txt", "world");
        write_file(temp_dir.path(), ".hash_cache.bin", "do not hash me");

        let discovered = discover_files(temp_dir.path(), &temp_dir.path().join(".hash_cache.bin"))
            .expect("discover_files should succeed");

        let paths: Vec<PathBuf> = discovered
            .into_iter()
            .map(|file| file.relative_path)
            .collect();

        assert_eq!(
            paths,
            vec![PathBuf::from("nested/data.txt"), PathBuf::from("notes.txt")]
        );
    }

    #[test]
    fn diff_caches_classifies_added_changed_removed_and_unchanged_files() {
        let previous = BTreeMap::from([
            (PathBuf::from("alpha.txt"), "old-alpha".to_string()),
            (PathBuf::from("beta.txt"), "same-beta".to_string()),
            (PathBuf::from("removed.txt"), "removed".to_string()),
        ]);

        let current = BTreeMap::from([
            (PathBuf::from("alpha.txt"), "new-alpha".to_string()),
            (PathBuf::from("beta.txt"), "same-beta".to_string()),
            (PathBuf::from("gamma.txt"), "new-gamma".to_string()),
        ]);

        let diff = diff_caches(&previous, &current);

        assert_eq!(diff.added, vec![PathBuf::from("gamma.txt")]);
        assert_eq!(diff.changed, vec![PathBuf::from("alpha.txt")]);
        assert_eq!(diff.removed, vec![PathBuf::from("removed.txt")]);
        assert_eq!(diff.unchanged, vec![PathBuf::from("beta.txt")]);
    }

    #[test]
    fn save_and_load_cache_round_trip_in_json() {
        let temp_dir = tempdir().expect("failed to create tempdir");
        let cache_path = temp_dir.path().join(".hash_cache.json");

        let original = StoredCache::new(BTreeMap::from([
            (PathBuf::from("a.txt"), "111".to_string()),
            (PathBuf::from("nested/b.txt"), "222".to_string()),
        ]));

        save_cache(&cache_path, CacheFormat::Json, &original).expect("save_cache should succeed");
        let loaded = load_cache(&cache_path, CacheFormat::Json).expect("load_cache should succeed");

        assert_eq!(loaded, original);
    }

    #[test]
    fn save_and_load_cache_round_trip_in_bincode() {
        let temp_dir = tempdir().expect("failed to create tempdir");
        let cache_path = temp_dir.path().join(".hash_cache.bin");

        let original = StoredCache::new(BTreeMap::from([
            (PathBuf::from("a.txt"), "111".to_string()),
            (PathBuf::from("nested/b.txt"), "222".to_string()),
        ]));

        save_cache(&cache_path, CacheFormat::Bincode, &original)
            .expect("save_cache should succeed");
        let loaded =
            load_cache(&cache_path, CacheFormat::Bincode).expect("load_cache should succeed");

        assert_eq!(loaded, original);
    }

    #[test]
    fn run_reports_changes_on_first_scan() {
        let temp_dir = tempdir().expect("failed to create tempdir");
        write_file(temp_dir.path(), "file.txt", "hello");

        let args = Args {
            directory: temp_dir.path().to_path_buf(),
            cache_file: None,
            json: true,
            quiet: true,
        };

        let outcome = run(args).expect("run should succeed");

        assert!(outcome.changes_detected);
        assert!(temp_dir.path().join(".hash_cache.json").exists());
    }
}
