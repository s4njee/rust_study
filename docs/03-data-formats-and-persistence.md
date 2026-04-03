# Data Formats And Persistence

This doc covers how the repo turns outside data into Rust types and back again.

## Serde Is The Common Language

In plain English: `serde` is the translation layer. It lets Rust structs speak
JSON, XML, and binary formats without hand-writing all the conversion code.

Serde works through two traits:

- `Serialize` — "this struct can be turned **into** an output format"
- `Deserialize` — "this struct can be built **from** an input format"

Common derive setup:

```rust
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
```

Each of those derives generates code at compile time:

| Derive | What it generates |
|---|---|
| `Debug` | A `{:?}` display implementation for printing |
| `Serialize` | Code to convert the struct into any serde-compatible format |
| `Deserialize` | Code to build the struct from any serde-compatible format |
| `Default` | A constructor that sets fields to their zero/empty values |
| `Clone` | A `.clone()` method for duplicating the struct |

Common field attributes:

| Attribute | What it means in plain English | Example |
|---|---|---|
| `#[serde(rename = "...")]` | "This field has a different external name" | `#[serde(rename = "bill_type")]` |
| `#[serde(default)]` | "If it is missing, use the `Default` value" | `#[serde(default)]` on a `String` → `""` |
| `#[serde(deserialize_with = "...")]` | "Use custom logic while reading" | Custom null-handling functions |
| `#[serde(skip)]` | "Don't include this field in serialization" | Internal bookkeeping fields |

## JSON Parsing With `serde_json`

In plain English: if the incoming JSON shape is mostly known, make a Rust struct
that mirrors it and let `serde_json` fill it in. This is the "typed parsing"
approach — you describe the expected structure, and serde validates the input
against it automatically.

Reading a JSON file:

```rust
let data = fs::read_to_string(path)?;
let vote_json: VoteJson = serde_json::from_str(&data)?;
```

That second line does a lot of work: it parses the JSON string, maps each field
to the struct, applies any rename/default attributes, and returns an error if
the JSON does not match the expected structure.

Example struct that mirrors a JSON file:

```rust
#[derive(Debug, Deserialize, Default, Clone)]
pub struct VoteJson {
    // The JSON uses "bill_type" as the key, but the Rust field is also
    // bill_type — rename is still useful here for documentation clarity
    #[serde(rename = "bill_type", default)]
    pub bill_type: String,

    // The JSON key is "type" which is a Rust keyword — rename lets us
    // call the field something valid in Rust
    #[serde(rename = "type", default)]
    pub votetype: String,

    // The votes field contains a map of party → list of members
    // serde_json::Value is used because the list items can be objects,
    // strings, or nulls depending on the data source
    #[serde(default)]
    pub votes: std::collections::HashMap<String, Vec<Value>>,
}
```

See `csearch-rscraper/src/models.rs` and `csearch-rscraper/src/votes.rs`.

## When JSON Is Messy

In plain English: sometimes the input is inconsistent. A field might contain
objects sometimes, strings other times, or `null` in legacy data. Real-world
data is often messier than its documentation suggests.

That is why the scraper uses `serde_json::Value` in some places — it is the
"accept anything" type:

```rust
match item {
    // Skip null values and plain strings — they're not valid members
    serde_json::Value::Null | serde_json::Value::String(_) => continue,
    // Anything else, try to parse it as a properly structured member
    value => members.push(serde_json::from_value(value)?),
}
```

This is the Rust version of saying, "inspect each item carefully before trusting
it." The `Value` enum has variants for every JSON type:

```rust
enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(Map<String, Value>),
}
```

**When to use `Value` vs a typed struct:**

- Use a typed struct when the JSON shape is known and stable — you get
  compile-time guarantees and better documentation
- Use `Value` when the shape varies or when you need to inspect the data
  before deciding how to parse it
- You can mix both: use `Value` for the messy parts and typed structs for the
  rest (like the `votes` field in `VoteJson`)

## Custom Deserialization

In plain English: sometimes the source format is technically valid but annoying,
so you clean it up while reading it. Custom deserialization functions let you
transform data during parsing.

```rust
fn null_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let value = Option::<T>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}
```

That helper turns "missing or null" into a normal default value. You attach it
to a field like this:

```rust
#[derive(Deserialize)]
pub struct Bill {
    #[serde(deserialize_with = "null_or_default")]
    pub title: String,  // will be "" instead of causing an error on null
}
```

This is particularly useful when:

- An API sometimes sends `null` and sometimes omits the field entirely
- A field should always have a value, but the source data is not that reliable
- You want to normalize inconsistent input at the boundary

## XML Parsing With `quick-xml`

In plain English: XML parsing here works a lot like JSON parsing. You still make
Rust structs that match the data shape, then let `serde` do the field mapping.
The key mindset shift: you are not parsing tag-by-tag manually. You are
**describing the structure** Rust should expect.

```rust
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename = "billStatus")]
pub struct BillXmlRootNew {
    #[serde(rename = "bill", default)]
    pub bill: BillXmlNew,
}
```

Parsing:

```rust
let file_contents = fs::read_to_string(path)?;
let parsed: BillXmlRootNew = quick_xml::de::from_str(&file_contents)?;
```

Notice how the API is nearly identical to `serde_json::from_str`. That is the
power of serde's format-agnostic design: the same struct works with JSON, XML,
bincode, TOML, or any other format that implements serde's traits.

**XML-specific gotchas:**

- XML attributes and child elements are mapped differently. `quick-xml` uses
  `#[serde(rename = "@attribute")]` for XML attributes
- XML does not have arrays — repeated elements are mapped to `Vec<T>`
- Namespaces can be tricky; the repo's bill XML uses simple tag names

## Binary Serialization With `bincode`

In plain English: JSON is friendly for people. `bincode` is friendly for compact
machine storage. It encodes Rust values into a minimal binary format with no
field names, no whitespace, and no formatting overhead.

This repo uses it for cache files because:

- it is **smaller** (no field names or formatting in the output)
- it is **faster** to read and write (no parsing overhead)
- humans do not need to edit it directly (it is a machine-to-machine format)

Reading a bincode cache:

```rust
let bytes = fs::read(cache_path)?;
let config = bincode::config::standard();
let (cache, _bytes_read): (StoredCache, usize) =
    bincode::serde::decode_from_slice(&bytes, config)?;
```

Writing a bincode cache:

```rust
let config = bincode::config::standard();
let serialized = bincode::serde::encode_to_vec(cache, config)?;
fs::write(cache_path, &serialized)?;
```

The scraper crate uses a slightly older API:

```rust
let PersistedHashes(hashes) = bincode::deserialize(&bytes)?;
let bytes = bincode::serialize(&PersistedHashes(self.hashes.clone()))?;
```

**Trade-off:** Bincode files are not portable across different struct layouts.
If you add or remove a field from your struct, old cache files become
unreadable. That is why the hash cache includes a version number:

```rust
fn validate_cache(cache: StoredCache, cache_path: &Path) -> Result<StoredCache> {
    if cache.version != CACHE_FORMAT_VERSION {
        bail!(
            "cache file '{}' uses unsupported version {} (expected {})",
            cache_path.display(),
            cache.version,
            CACHE_FORMAT_VERSION
        );
    }
    Ok(cache)
}
```

## Hashing As Persistence Metadata

In plain English: hashing is how the repo remembers whether a file really
changed instead of just guessing from timestamps. A hash is a fingerprint —
if even one byte changes, the hash will be completely different.

```rust
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 { break; }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}
```

There are several important details in this function:

1. **Chunked reading** (`8192` byte buffer): avoids loading the entire file into
   memory. A 500 MB file would use 500 MB of RAM with `fs::read()`, but only
   8 KB with this approach.
2. **`&buffer[..read]`**: only hash the bytes that were actually read — the last
   chunk may be smaller than the buffer.
3. **`{:x}` formatting**: converts the raw hash bytes into a lowercase hex
   string for human-readable storage.

The key idea is practical: read a file in chunks, compute its fingerprint, and
store that fingerprint so later runs can skip unchanged work.

## Stable, Human-Friendly Serialization Choices

In plain English: different formats serve different needs. Choosing the right
one depends on who (or what) will read the output.

| Need | Good choice in this repo | Why |
|---|---|---|
| People should inspect or diff it | JSON | Readable, universal tooling |
| Speed and compact storage matter | `bincode` | No parsing overhead, minimal size |
| Incoming third-party structured documents | XML | Many government data sources use XML |
| Change detection for files | SHA-256 hash | Deterministic, cheap to compare |
| Configuration files | TOML or `.env` | Human-editable, simple to parse |

The repo supports both JSON and bincode for the hash cache, controlled by a
`--json` flag. That is a good pattern: default to the efficient format, but
offer a human-readable option for debugging:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheFormat {
    Bincode,
    Json,
}

impl CacheFormat {
    fn default_filename(self) -> &'static str {
        match self {
            Self::Bincode => ".hash_cache.bin",
            Self::Json => ".hash_cache.json",
        }
    }
}
```

## Good Next Step

Read [`04-services-and-configuration.md`](04-services-and-configuration.md) next
if you want to see how these parsed values move into databases, Redis, and child
processes.
