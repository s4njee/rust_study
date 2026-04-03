# Types, Strings, And Iterators

This doc covers the smaller everyday patterns that make Rust code feel natural.
These are the building blocks you will reach for constantly.

## Enums As Real Modeling Tools

In plain English: enums are not just fancy constants. They let you describe a
value that can be **one of several meaningful shapes**, and each shape can carry
different data.

```rust
enum VoteParseOutcome {
    Changed(ChangedVote),   // carries the parsed vote data
    Skipped,                 // no data needed
    Missing,                 // no data needed
}
```

That is more expressive than using booleans or magic strings because the type
itself tells you the allowed states. The compiler will reject any code that
tries to create a `VoteParseOutcome` with a shape not listed here.

**Compare with other languages:**

- In Python, you might return a tuple like `("changed", vote_data)` or
  `("skipped", None)` — the compiler can't check that you handle all cases
- In TypeScript, you might use a discriminated union — similar to Rust enums,
  but TypeScript's exhaustiveness checking is optional (with `never`)
- In Go, you might use multiple return values or error sentinels — error-prone

Another enum in the repo serves as a configuration choice:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheFormat {
    Bincode,
    Json,
}
```

Even though `CacheFormat` carries no data in its variants, it is still better
than a `bool` — `CacheFormat::Json` is much clearer than `true`, especially
when reading code months later.

## `match` Makes State Handling Explicit

In plain English: `match` is how Rust asks you to handle **all** cases on
purpose. If you forget a variant, the code will not compile.

```rust
match outcome {
    VoteParseOutcome::Changed(vote) => { /* process the parsed vote */ }
    VoteParseOutcome::Skipped => { stats.votes_skipped += 1; }
    VoteParseOutcome::Missing => { stats.votes_missing += 1; }
}
```

This can feel strict at first, but it prevents "forgot to handle that case"
bugs. When you later add a new variant to the enum, the compiler shows you
**every** `match` that needs updating.

`match` also supports **guards** — additional conditions on a pattern:

```rust
match previous.get(path) {
    None => diff.added.push(path.clone()),
    Some(previous_hash) if previous_hash == current_hash => {
        diff.unchanged.push(path.clone())
    }
    Some(_) => diff.changed.push(path.clone()),
}
```

The `if previous_hash == current_hash` guard adds a condition to the `Some`
pattern. The final `Some(_)` matches any remaining `Some` values where the
hashes differ.

## `String` vs `&str`

In plain English: `String` and `&str` are the owned and borrowed versions of
text, just like `PathBuf` and `&Path` are for paths.

| Type | Ownership | Common use |
|---|---|---|
| `String` | Owned, heap-allocated, growable | Return values, struct fields |
| `&str` | Borrowed slice into a `String` or literal | Function parameters, string literals |

That is why function parameters often use `&str`, while functions that build new
text often return `String`:

```rust
// Takes &str — only needs to READ the name, doesn't need to own it
fn env_enabled(name: &str, default_value: bool) -> bool { /* ... */ }

// Returns String — the caller OWNS the constructed DSN
pub fn postgres_dsn(&self) -> String {
    format!(
        "postgres://{}:{}@{}:{}/{}",
        self.db_user, self.db_password, self.postgres_uri, self.db_port, self.db_name
    )
}
```

**Why two types?** `&str` avoids unnecessary allocations. If a function only
needs to read a string, accepting `&str` means callers can pass string literals,
`String` references, or slices without allocating a new `String`:

```rust
// All of these work when the parameter is &str:
env_enabled("RUN_VOTES", true);          // string literal → &str
env_enabled(&my_string, true);           // String → &str (via Deref)
env_enabled(&my_string[0..5], true);     // slice of String → &str
```

## Common String Conversions

In plain English: Rust is picky here because it wants to be clear about who owns
text and when conversions happen. The names are a little longer than in dynamic
languages, but they say exactly what they do.

```rust
// &str → String (allocates a new heap string)
"hello".to_string()
String::from("hello")    // equivalent

// String → &str (borrows — free, no allocation)
my_string.as_str()
&my_string               // equivalent, via Deref

// Path → string (lossy — replaces invalid Unicode with ?)
path.to_string_lossy()                // returns Cow<str>
path.to_string_lossy().into_owned()   // returns owned String

// Number → String
42.to_string()
format!("{}", 42)        // equivalent, more flexible

// String → Number (returns Result — parsing can fail)
"42".parse::<i32>()      // turbofish syntax specifies the target type
let port: u16 = value.parse().ok().unwrap_or(5432);
```

**`Cow<str>` explained:** `to_string_lossy()` returns a `Cow<str>` (Copy on
Write) because if the path is already valid UTF-8, no allocation is needed.
`Cow` borrows when it can and allocates only when it must. Call `.into_owned()`
when you need a guaranteed `String`.

## Raw String Literals

In plain English: raw strings are for text that would be annoying to escape,
especially SQL or JSON snippets. The `r#"..."#` syntax means "everything inside
is literal — no escape sequences."

```rust
sqlx::query(
    r#"
INSERT INTO votes (voteid, bill_type)
VALUES ($1, $2)
ON CONFLICT (voteid) DO UPDATE SET
    bill_type = $2
    "#,
)
```

Without `r#"..."#`, you would need to escape every `"` inside the SQL string,
making it much harder to read. You can add more `#` symbols if your string
itself contains `"#`:

```rust
r##"She said "hello" and typed r#"raw"#"##
```

## Iterator Chains

In plain English: iterator chains are Rust's way of describing a data
transformation step by step without lots of temporary containers. They are
**lazy** — no work happens until you call a consuming method like `.collect()`.

```rust
let paths: Vec<PathBuf> = discovered
    .into_iter()               // consume the vector, yielding owned items
    .map(|file| file.relative_path)  // transform: DiscoveredFile → PathBuf
    .collect();                // collect results into a new Vec
```

Read that as:

1. consume the list
2. transform each item (extract the `relative_path` field)
3. collect the results into a new vector

More complex chains from the repo:

```rust
// Filter, transform, and collect in one chain
let members: Vec<VoteMember> = items
    .into_iter()
    .filter(|item| !item.is_null())     // skip nulls
    .map(|item| parse_member(item))     // parse each one
    .collect::<Result<Vec<_>>>()?;      // collect, propagating errors with ?
```

**Why iterators instead of `for` loops?** Both work, but iterators:

- Communicate intent more clearly (filter/map/collect says what you're doing)
- Compose naturally (chain more operations without nesting loops)
- Enable rayon parallelism (swap `.into_iter()` for `.into_par_iter()`)

That said, `for` loops are perfectly fine when the logic is complex or has
side effects. The repo uses both styles.

## Method Chains

In plain English: method chains often show a "cleanup pipeline" where each step
slightly improves a value. Each method transforms the value or handles a
potential failure.

```rust
env::var("DB_PORT")
    .ok()                              // Result → Option (discard the error)
    .and_then(|value| value.parse().ok())  // try to parse if present
    .unwrap_or(5432)                   // use 5432 if any step failed
```

Read that step by step:

| Step | Type | What happens |
|---|---|---|
| `env::var("DB_PORT")` | `Result<String, VarError>` | Read the env var |
| `.ok()` | `Option<String>` | Convert to Option, discarding the error |
| `.and_then(\|v\| v.parse().ok())` | `Option<u16>` | Parse if present, None if parsing fails |
| `.unwrap_or(5432)` | `u16` | Extract the value, or use 5432 as default |

**`.and_then()` vs `.map()`:** Use `.map()` when the transformation always
succeeds (returns `T`). Use `.and_then()` when the transformation might fail
(returns `Option<T>`). `.and_then()` "flattens" — it prevents
`Option<Option<T>>`.

## Sorting And Stable Output

In plain English: the repo sorts output when stability matters, because stable
ordering is easier to debug and test.

Sorting a vector of structs by a field:

```rust
discovered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
```

Using `BTreeMap` for inherently sorted storage:

```rust
// BTreeMap keeps keys sorted at all times — iteration is always in order
entries: BTreeMap<PathBuf, String>
```

**`BTreeMap` vs `HashMap`:**

| Feature | `HashMap` | `BTreeMap` |
|---|---|---|
| Lookup speed | O(1) average | O(log n) |
| Iteration order | Random | Sorted by key |
| Use when | Speed matters, order doesn't | Deterministic output or range queries |

The hash cache uses `BTreeMap` so that:

- JSON output is deterministic (same keys always in the same order)
- Debug output is stable across runs
- Tests can compare caches without worrying about ordering

## `Vec::with_capacity`

In plain English: when you already know about how many items you are going to
store, reserve that space up front. This avoids repeated reallocations as the
vector grows.

```rust
let mut members = Vec::with_capacity(items.len());
for item in items {
    members.push(parse_member(item)?);
}
```

Without `with_capacity`, a `Vec` starts with zero capacity and doubles each
time it fills up. That means 10+ allocations and copies for a 1000-element
vector. `with_capacity` does it in one allocation.

It is a small performance hint, not a required optimization. Use it when:

- you know (or can estimate) the final size
- the collection will be large enough that reallocation is noticeable
- you are building performance-sensitive code

## Reserved Keywords With Raw Identifiers

In plain English: if external data uses a name Rust reserves (like `type`,
`match`, `fn`), you can still represent it using the `r#` prefix.

```rust
#[derive(Debug, Deserialize)]
pub struct VoteInfo {
    pub r#type: String,      // maps to the "type" field in JSON
    pub congress: i32,
}
```

`r#type` tells Rust "this is a regular identifier named `type`, not the `type`
keyword." You access it the same way: `vote_info.r#type`.

This comes up most often when:

- JSON or XML uses a key that happens to be a Rust keyword
- You want to name a field to match its source exactly

Alternative: use `#[serde(rename = "type")]` to map the external name to a
different Rust field name:

```rust
#[serde(rename = "type")]
pub votetype: String,  // Rust field is "votetype", JSON key is "type"
```

## The `let-else` Pattern

In plain English: `let ... else` is a way to destructure a value and
immediately bail out if the pattern does not match. It is cleaner than nesting
`if let` with `else` blocks.

```rust
pub fn env_enabled(name: &str, default_value: bool) -> bool {
    let Ok(value) = env::var(name) else {
        return default_value;
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default_value,
    }
}
```

The `else` block must **diverge** — it must `return`, `break`, `continue`, or
`panic!`. This guarantees that after the `let` statement, the variable is
always bound.

## Practical Lesson

These small patterns are where Rust starts feeling comfortable. Once you are no
longer surprised by `String` vs `&str`, `Option`, enums, iterator chains, and
`match`, the bigger project code becomes much easier to read. They show up in
every file across the repo.
