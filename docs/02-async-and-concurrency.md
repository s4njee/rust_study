# Async And Concurrency

This doc covers the patterns used when the code is doing many things at once.

## Tokio Runtime

In plain English: Tokio is the engine that keeps lots of waiting tasks moving.
It is especially useful when your program spends time waiting on files, network
calls, or databases.

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

What to remember:

- `async fn` returns a future
- `.await` pauses the task, not the whole OS thread
- `#[tokio::main]` sets up the runtime so `main` can be async

## `JoinSet` For Collecting Spawned Tasks

In plain English: `JoinSet` is a bag of background jobs you started and still
want to hear back from.

```rust
let mut write_tasks = JoinSet::new();

write_tasks.spawn(async move {
    let _permit = write_sem.acquire_owned().await?;
    insert_parsed_vote(&pool, &changed_vote.parsed_vote).await?;
    Ok::<_, anyhow::Error>(changed_vote)
});
```

Later:

```rust
while let Some(result) = write_tasks.join_next().await {
    match result {
        Ok(Ok(changed_vote)) => { /* success */ }
        Ok(Err(err)) => { /* work failed */ }
        Err(err) => { /* task panicked */ }
    }
}
```

This shows up in `csearch-rscraper/src/votes.rs`.

## `Semaphore` For Concurrency Limits

In plain English: a semaphore is a bouncer at the door. It lets only a certain
number of jobs run at once.

That matters when the program could otherwise overload:

- the database
- the CPU
- the file system
- an external service

The repo uses it to cap concurrent DB writes and parsing tasks.

## `spawn_blocking` For CPU-Heavy Work

In plain English: async is great for waiting, but bad for heavy crunching. If a
job is going to chew CPU for a while, move it off the main async lane.

```rust
tokio::task::spawn_blocking(move || {
    parse_vote_job(job, &known_hashes)
}).await?
```

Use this for:

- hashing large files
- parsing big JSON or XML payloads
- decoding or resizing images

Without `spawn_blocking`, one heavy task can make the async runtime feel stuck.

## `Arc<T>` For Shared Ownership

In plain English: `Arc<T>` is how several tasks can point at the same data
safely.

```rust
let known_hashes = Arc::new(hashes.snapshot());
let parse_sem = Arc::new(Semaphore::new(WORKER_LIMIT));
```

Cloning an `Arc` does not copy the underlying data. It just increases the
reference count.

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

If you ever wonder why Rust insists on `move`, it is usually protecting you from
leaving a task holding references to data that no longer exists.

## Explicit Type Hints In Async Blocks

In plain English: async blocks sometimes hide too much information from the
compiler, so you help it out.

```rust
Ok::<_, anyhow::Error>(changed_vote)
```

That line tells Rust exactly what kind of success value and error value the task
returns.

## Rayon For Data Parallelism

In plain English: Rayon is the "make this CPU loop use multiple cores" tool.
Where Tokio helps with waiting, Rayon helps with pure computation.

Example from the thumbnail generator:

```rust
items.into_par_iter().map(f).collect()
```

That is often the entire change from sequential to parallel processing.

Rayon is a good fit when:

- each item can be processed independently
- the work is CPU-heavy
- you do not need async I/O inside the parallel loop

See `PHASE1/1.3-IMAGE-THUMBNAIL-GENERATOR/src/lib.rs`.

## Mental Shortcut

Use this rule of thumb:

- Tokio: "many tasks spend time waiting"
- `spawn_blocking`: "this task is heavy and should leave the async lane"
- Rayon: "this is CPU work that can fan out across cores"
