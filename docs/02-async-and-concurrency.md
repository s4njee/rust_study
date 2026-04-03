# Async And Concurrency

This doc covers the patterns used when the code is doing many things at once.

## Tokio Runtime

In plain English: Tokio is the engine that keeps lots of waiting tasks moving.
It is especially useful when your program spends time waiting on files, network
calls, or databases. Without Tokio (or a similar runtime), Rust has no built-in
way to run async code.

```rust
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!(error = %err, "scraper run failed");
            ExitCode::FAILURE
        }
    }
}
```

See `csearch-rscraper/src/main.rs`.

The `#[tokio::main]` macro does two things behind the scenes:

1. Creates a multi-threaded Tokio runtime (like calling `asyncio.run()` in
   Python)
2. Wraps your `async fn main()` so it runs inside that runtime

Without this macro, `main()` is synchronous and you cannot use `.await`. The
macro roughly expands to:

```rust
fn main() -> ExitCode {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async { /* your async main body */ })
}
```

What to remember:

- `async fn` returns a **future** — it does not run until someone `.await`s it
- `.await` pauses the current task, not the whole OS thread — other tasks keep
  running on the same thread while this one waits
- `#[tokio::main]` sets up the runtime so `main` can be async
- Tokio uses a **multi-threaded** executor by default, which means your futures
  may resume on a different OS thread than where they started

## `JoinSet` For Collecting Spawned Tasks

In plain English: `JoinSet` is a bag of background jobs you started and still
want to hear back from. It is the async equivalent of spawning threads and
collecting their handles.

**Spawning tasks into the set:**

```rust
let mut write_tasks = JoinSet::new();

write_tasks.spawn(async move {
    let _permit = write_sem.acquire_owned().await?;
    insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
    Ok::<_, anyhow::Error>(changed_vote)
});
```

**Draining results as tasks complete:**

```rust
while let Some(result) = write_tasks.join_next().await {
    match result {
        Ok(Ok(changed_vote)) => { /* work succeeded */ }
        Ok(Err(err)) => { /* the task's own logic returned an error */ }
        Err(err) => { /* task panicked or was cancelled */ }
    }
}
```

This shows up in `csearch-rscraper/src/votes.rs`.

The two levels of `Result` here are important to understand:

| Level | Type | What it means |
|---|---|---|
| Outer | `Result<T, JoinError>` | Did the spawned task complete without panicking? |
| Inner | `Result<ChangedVote, anyhow::Error>` | Did the task's own logic succeed? |

A `JoinError` happens when a task panics or is cancelled. The inner `Result` is
whatever your task logic returns. You need to handle both.

## `Semaphore` For Concurrency Limits

In plain English: a semaphore is a bouncer at the door. It lets only a certain
number of jobs run at once. Without limits, spawning thousands of tasks could
overload external resources.

That matters when the program could otherwise overload:

- the database connection pool
- the CPU (too many parallel decode/encode operations)
- the file system (too many open file handles)
- an external service (rate limits)

The repo uses semaphores to cap concurrent DB writes and parsing tasks:

```rust
// Create a semaphore allowing at most 4 concurrent operations
let write_sem = Arc::new(Semaphore::new(4));

// Each task acquires a permit before doing its work
write_tasks.spawn(async move {
    // This line blocks until a permit is available
    let _permit = write_sem.acquire_owned().await?;

    // Only 4 tasks can be here at the same time
    insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;

    Ok::<_, anyhow::Error>(changed_vote)
    // _permit is dropped here → releases the slot for another task
});
```

Key details:

- `acquire_owned()` takes a permit from the semaphore, blocking if none are
  available. The `_owned` variant works with `Arc<Semaphore>` so the permit can
  be moved into an async block.
- The permit is automatically returned when the `_permit` variable is dropped
  (RAII in action).
- The underscore prefix (`_permit`) signals "I need this to exist for its
  lifetime, but I will not use its value directly."

**Choosing the right limit:** In `csearch-rscraper`, the parse semaphore uses
64 (one per CPU core for parsing) and the write semaphore uses 4 (matching the
DB connection pool size). These numbers mirror the Go scraper's design.

## `spawn_blocking` For CPU-Heavy Work

In plain English: async is great for waiting, but bad for heavy crunching. If a
job is going to chew CPU for a while, move it off the main async lane so it
does not stall other waiting tasks.

```rust
tokio::task::spawn_blocking(move || {
    parse_vote_job(job, &known_hashes)
}).await?
```

Tokio's async threads use **cooperative scheduling** — tasks must yield control
voluntarily by hitting an `.await` point. A CPU-heavy function that never awaits
will monopolize the thread, starving all other tasks on that thread.

`spawn_blocking` moves the work to a separate, dedicated thread pool where
blocking is expected.

Use this for:

- hashing large files
- parsing big JSON or XML payloads
- decoding or resizing images
- any computation that takes more than a few milliseconds

Without `spawn_blocking`, one heavy task can make the async runtime feel stuck.

**When NOT to use it:** If the work involves `.await` (network calls, database
queries), keep it on the async executor. `spawn_blocking` is for code that would
block the thread without ever yielding.

## `Arc<T>` For Shared Ownership

In plain English: `Arc<T>` is how several tasks can point at the same data
safely. Each `Arc::clone()` bumps a reference count rather than copying the
data.

```rust
let known_hashes = Arc::new(hashes.snapshot());
let parse_sem = Arc::new(Semaphore::new(WORKER_LIMIT));
```

Cloning an `Arc` does not copy the underlying data. It just increases the
reference count. When the last clone is dropped, the data is freed.

This is necessary because each spawned task may outlive the scope that created
it. Without `Arc`, Rust would not let you pass a reference to data that might
be dropped while the task is still running:

```rust
for file in files {
    let pool = Arc::clone(&pool);       // bump the count (cheap)
    let sem = Arc::clone(&sem);

    tokio::spawn(async move {
        let _permit = sem.acquire().await?;
        process(&pool, &file).await
        // pool and sem are dropped here → their ref counts decrease
    });
}
// original pool and sem are dropped here
// the data is freed when the LAST task finishes
```

## `async move`

In plain English: `move` says, "put the captured values inside the task itself."
That is important because the task may keep running after the current scope has
finished.

```rust
write_tasks.spawn(async move {
    let _permit = write_sem.acquire_owned().await?;
    insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
    Ok::<_, anyhow::Error>(changed_vote)
});
```

Without `move`, the closure would try to **borrow** the captured variables. But
the spawned task might outlive the current scope, so borrowing is not allowed.
`move` transfers ownership into the task, satisfying the compiler.

If you ever wonder why Rust insists on `move`, it is usually protecting you from
leaving a task holding references to data that no longer exists.

**This is why `Arc` and `move` go together:** You cannot move the same value
into multiple tasks. Instead, you clone the `Arc` (cheap ref-count bump) and
move each clone into its respective task.

```rust
for file in files {
    let pool = Arc::clone(&pool);   // clone the Arc, not the pool
    tokio::spawn(async move {
        // pool (the Arc) was moved in here
        do_work(&pool, &file).await
    });
}
```

## Explicit Type Hints In Async Blocks

In plain English: async blocks sometimes hide too much information from the
compiler, so you help it out with a type annotation on the final expression.

```rust
Ok::<_, anyhow::Error>(changed_vote)
```

That line tells Rust exactly what kind of success value and error value the task
returns. Without this hint, the compiler may not be able to infer the error type
because the async block never explicitly returns an `Err(...)`.

You will see this pattern whenever an async block's only fallible operations use
`?` to propagate errors — the compiler needs to know what error type `?` should
convert into.

## Rayon For Data Parallelism

In plain English: Rayon is the "make this CPU loop use multiple cores" tool.
Where Tokio helps with **waiting**, Rayon helps with **pure computation**.

The transformation from sequential to parallel is often a single word change:

```rust
// Sequential — processes one item at a time:
let results: Vec<_> = items.into_iter().map(|x| work(x)).collect();

// Parallel — fans out across all CPU cores:
let results: Vec<_> = items.into_par_iter().map(|x| work(x)).collect();
```

From the thumbnail generator, the generic parallel batch helper:

```rust
#[cfg(feature = "rayon")]
pub fn par_batch<T, U, E, F>(items: Vec<T>, f: F) -> Vec<Result<U, E>>
where
    T: Send,
    U: Send,
    E: Send,
    F: Fn(T) -> Result<U, E> + Sync + Send,
{
    items.into_par_iter().map(f).collect()
}
```

See `PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs`.

Rayon is a good fit when:

- each item can be processed **independently** (no shared mutable state)
- the work is **CPU-heavy** (hashing, parsing, image resizing)
- you do **not** need async I/O inside the parallel loop
- tasks are roughly **uniform** in cost (rayon cannot rebalance mid-task)

Rayon is NOT a good fit when:

- tasks involve network calls or database queries (use Tokio instead)
- tasks vary wildly in duration and you need fine-grained scheduling

## Mental Shortcut

Use this rule of thumb:

| Tool | Best for | Example |
|---|---|---|
| **Tokio** | "Many tasks spend time waiting" | DB queries, Redis calls, HTTP requests |
| **`spawn_blocking`** | "This task is heavy and should leave the async lane" | Parsing XML, hashing files |
| **Rayon** | "This is CPU work that can fan out across cores" | Batch image resizing |

A common pattern in the repo combines all three:

1. Tokio orchestrates the overall pipeline (spawn tasks, manage concurrency)
2. `spawn_blocking` moves CPU-heavy parsing off the async threads
3. Rayon could parallelize within a single blocking task if the data is large
   enough

## Good Next Step

Read [`03-data-formats-and-persistence.md`](03-data-formats-and-persistence.md)
next if you want to see how the data flowing through these concurrent tasks
gets parsed and stored.
