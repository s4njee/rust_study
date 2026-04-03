# Data Formats And Persistence

This doc covers how the repo turns outside data into Rust types and back again.

## Serde Is The Common Language

In plain English: `serde` is the translation layer. It lets Rust structs speak
JSON, XML, and binary formats without hand-writing all the conversion code.

Common derive setup:

```rust
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
```

Common attributes:

| Attribute | What it means in plain English |
|---|---|
| `#[serde(rename = "...")]` | "This field has a different external name" |
| `#[serde(default)]` | "If it is missing, use the normal default value" |
| `#[serde(deserialize_with = "...")]` | "Use custom logic while reading" |

## JSON Parsing With `serde_json`

In plain English: if the incoming JSON shape is mostly known, make a Rust struct
that mirrors it and let `serde_json` fill it in.

```rust
let data = fs::read_to_string(path)?;
let vote_json: VoteJson = serde_json::from_str(&data)?;
```

Example struct:

```rust
#[derive(Debug, Deserialize, Default, Clone)]
pub struct VoteJson {
    #[serde(rename = "bill_type", default)]
    pub bill_type: String,
    #[serde(rename = "type", default)]
    pub votetype: String,
    #[serde(default)]
    pub votes: std::collections::HashMap<String, Vec<Value>>,
}
```

See `csearch-rscraper/src/models.rs` and `csearch-rscraper/src/votes.rs`.

## When JSON Is Messy

In plain English: sometimes the input is inconsistent. A field might contain
objects sometimes, strings other times, or `null` in legacy data.

That is why the scraper uses `serde_json::Value` in some places:

```rust
match item {
    serde_json::Value::Null | serde_json::Value::String(_) => continue,
    value => members.push(serde_json::from_value(value)?),
}
```

This is the Rust version of saying, "inspect each item carefully before trusting
it."

## Custom Deserialization

In plain English: sometimes the source format is technically valid but annoying,
so you clean it up while reading it.

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

That helper turns "missing or null" into a normal default value.

## XML Parsing With `quick-xml`

In plain English: XML parsing here works a lot like JSON parsing. You still make
Rust structs that match the data shape, then let `serde` do the field mapping.

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
let parsed: BillXmlRootNew = from_str(&file_contents)?;
```

The important mindset is: you are not parsing tag-by-tag manually. You are
describing the structure Rust should expect.

## Binary Serialization With `bincode`

In plain English: JSON is friendly for people. `bincode` is friendly for compact
machine storage.

This repo uses it for cache files because:

- it is smaller
- it is faster to read and write
- humans do not need to edit it directly

Examples:

```rust
let PersistedHashes(hashes) = bincode::deserialize(&bytes)?;
let bytes = bincode::serialize(&PersistedHashes(self.hashes.clone()))?;
```

And in the Phase 1 cache project:

```rust
let config = bincode::config::standard();
let serialized = bincode::serde::encode_to_vec(cache, config)?;
```

## Hashing As Persistence Metadata

In plain English: hashing is how the repo remembers whether a file really
changed instead of just guessing from timestamps.

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

The key idea is practical: read a file in chunks, compute its fingerprint, and
store that fingerprint so later runs can skip unchanged work.

## Stable, Human-Friendly Serialization Choices

In plain English: different formats serve different needs.

| Need | Good choice in this repo |
|---|---|
| People should inspect or diff it | JSON |
| Speed and compact storage matter | `bincode` |
| Incoming third-party structured documents | XML |
| Change detection for files | SHA-256 hash |

## Good Next Step

Read [`04-services-and-configuration.md`](04-services-and-configuration.md) next
if you want to see how these parsed values move into databases, Redis, and child
processes.
