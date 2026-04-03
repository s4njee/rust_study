# Types, Strings, And Iterators

This doc covers the smaller everyday patterns that make Rust code feel natural.

## Enums As Real Modeling Tools

In plain English: enums are not just fancy constants. They let you describe a
value that can be one of several meaningful shapes.

```rust
enum VoteParseOutcome {
    Changed(ChangedVote),
    Skipped,
    Missing,
}
```

That is more expressive than using booleans or magic strings because the type
itself tells you the allowed states.

## `match` Makes State Handling Explicit

In plain English: `match` is how Rust asks you to handle all cases on purpose.

```rust
match outcome {
    VoteParseOutcome::Changed(vote) => { /* process */ }
    VoteParseOutcome::Skipped => { stats.votes_skipped += 1; }
    VoteParseOutcome::Missing => { stats.votes_missing += 1; }
}
```

This can feel strict at first, but it prevents "forgot to handle that case"
bugs.

## `String` vs `&str`

In plain English:

- `String` owns text
- `&str` borrows text

That is why function parameters often use `&str`, while functions that build new
text often return `String`.

Examples:

```rust
fn env_enabled(name: &str, default_value: bool) -> bool { /* ... */ }
pub fn postgres_dsn(&self) -> String { /* ... */ }
```

## Common String Conversions

In plain English: Rust is picky here because it wants to be clear about who owns
text and when conversions happen.

```rust
"hello".to_string()
string.as_str()
path.to_string_lossy()
path.to_string_lossy().into_owned()
```

The names are a little longer than in dynamic languages, but they say exactly
what they do.

## Raw String Literals

In plain English: raw strings are for text that would be annoying to escape,
especially SQL or JSON snippets.

```rust
r#"
INSERT INTO votes (voteid, bill_type)
VALUES ($1, $2)
"#
```

## Iterator Chains

In plain English: iterator chains are Rust's way of describing a data
transformation step by step without lots of temporary containers.

```rust
let paths: Vec<PathBuf> = discovered
    .into_iter()
    .map(|file| file.relative_path)
    .collect();
```

Read that as:

1. consume the list
2. transform each item
3. collect the results into a new vector

## Method Chains

In plain English: method chains often show a "cleanup pipeline" where each step
slightly improves a value.

```rust
env::var("DB_PORT")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(5432)
```

That means:

1. try to read the env var
2. if it exists, try to parse it
3. if any step fails, use `5432`

## Sorting And Stable Output

In plain English: the repo sorts output when stability matters, because stable
ordering is easier to debug and test.

```rust
discovered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
```

And for stored map keys:

```rust
entries: BTreeMap<PathBuf, String>
```

`BTreeMap` is useful when you want keys to come out in sorted order.

## `Vec::with_capacity`

In plain English: when you already know about how many items you are going to
store, reserve that space up front.

```rust
let mut members = Vec::with_capacity(items.len());
```

It is a small performance hint, not a required optimization.

## Reserved Keywords With Raw Identifiers

In plain English: if external data uses a name Rust reserves, you can still
represent it.

```rust
pub r#type: String
```

That lets Rust hold a field literally named `type` without confusing the parser.

## Practical Lesson

These small patterns are where Rust starts feeling comfortable. Once you are no
longer surprised by `String` vs `&str`, `Option`, enums, and iterator chains,
the bigger project code becomes much easier to read.
