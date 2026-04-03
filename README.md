# rust_study

A Rust learning repo centered on building real projects instead of isolated exercises.

The core idea is to port parts of an existing stack into Rust, then branch into adjacent projects that build the same skills: CLI tooling, async services, data pipelines, APIs, WASM, and systems-style problem solving.

## Start Here

The detailed roadmap lives in [STUDY_GUIDE.md](/Users/sanjee/Documents/projects/rust_study/STUDY_GUIDE.md).

If you want the full plan, project rationale, and crate recommendations, start there.

## Repo Structure

- [STUDY_GUIDE.md](/Users/sanjee/Documents/projects/rust_study/STUDY_GUIDE.md): the full learning roadmap
- [docs](/Users/sanjee/Documents/projects/rust_study/docs): split study notes for Rust patterns used in the repo
- [PHASE1](/Users/sanjee/Documents/projects/rust_study/PHASE1): early fundamentals projects
- [csearch-rscraper](/Users/sanjee/Documents/projects/rust_study/csearch-rscraper): longer-form port work and planning

## Learning Path

### Phase 1: Foundations

Build comfort with ownership, borrowing, error handling, traits, CLI design, and basic concurrency.

Current starter projects:
- CLI hash cache
- Markdown sanitizer library
- Image thumbnail generator

### Phase 2: Port Real Projects

Apply Rust to real systems pulled from an existing stack:
- scraper/data pipeline work
- HTTP APIs
- embeddings and vector pipelines
- WASM state machines

### Phase 3: Adjacent Projects

Expand into Rust-native problem spaces that reinforce the same skills:
- async job runners
- SSE streaming servers
- ranking/fusion libraries
- graphics with `wgpu`
- config-driven tooling
- full-stack web apps with `axum` and `sqlx`

## How To Use This Repo

1. Pick one project from the guide.
2. Create or update a `PLAN.md` for that project.
3. Build the smallest working version first.
4. Add stretch goals only after the core version works.

## Current Focus

The most natural starting point is Phase 1.1, the CLI hash cache, because it builds file I/O, hashing, serialization, error handling, and a simple command-line interface in one contained project.
