# Rust Study Guide

A learning roadmap built around porting and extending the existing project stack (csearch-updater-root, csearch-nlp, eva, s8njee-web) into Rust, plus standalone projects for real-world exposure.

---

## Table of Contents

1. [Stack Overview](#stack-overview)
2. [Phase 1 — Foundations](#phase-1--foundations)
3. [Phase 2 — Port Projects](#phase-2--port-projects)
4. [Phase 3 — Adjacent Projects](#phase-3--adjacent-projects)
5. [Crate Reference](#crate-reference)

---

## Stack Overview

| Project | Language | What It Does |
|---------|----------|--------------|
| **csearch-updater-root** | Go + Python (scraper), Node/Fastify (API), Vue/Nuxt (frontend) | Ingests, stores, and serves U.S. Congressional bill/vote data. Go scraper parses XML/JSON, writes to Postgres, invalidates Redis. Fastify API serves read-only endpoints with Redis caching. |
| **csearch-nlp** | Python (FastAPI) | RAG service adding semantic search to csearch. Fetches bill XML from GovInfo, chunks text, embeds via OpenAI, stores vectors in Qdrant, fuses keyword + vector search with cross-encoder reranking, streams LLM answers via SSE. |
| **eva** | JavaScript/TypeScript (React + Three.js) | Portfolio homepage with three WebGL visualizations (3D model viewer, Matrix rain, chemistry/molecule viewer). Shared special-effects state machine, PubChem API integration. |
| **s8njee-web** | Python (Django 6.0) | Personal blog + photo gallery. Markdown rendering with HTML sanitization, image thumbnail generation, deployed via Docker Compose with Postgres + nginx. |

---

## Phase 1 — Foundations

Build comfort with ownership, borrowing, error handling, traits, and async before tackling ports. Each mini-project targets a concept you'll need later.

### 1.1 — CLI Hash Cache (1-2 days)

Reimplement the GOB-based SHA-256 hash cache from the csearch scraper as a Rust CLI tool.

**What you build:** A tool that walks a directory, hashes each file with SHA-256, persists the hash map to disk (bincode or JSON), and on subsequent runs reports which files changed.

**Concepts:** `std::fs`, `std::collections::HashMap`, `serde` + `bincode`, `sha2` crate, error handling with `anyhow`, CLI arg parsing with `clap`.

**Stretch:** Add concurrency with `rayon` for parallel hashing.

### 1.2 — Markdown Sanitizer Library (2-3 days)

Port the s8njee-web markdown pipeline (Markdown → HTML → sanitize dangerous tags) to a Rust library.

**What you build:** A library that takes markdown input, renders to HTML with `pulldown-cmark`, then sanitizes with `ammonia`. Expose as both a library crate and a CLI binary.

**Concepts:** Crate structure (lib + bin), string processing, `pulldown-cmark`, `ammonia`, unit testing with `#[cfg(test)]`.

**Stretch:** Add WASM compilation with `wasm-pack` so it could be called from JavaScript.

### 1.3 — Image Thumbnail Generator (2-3 days)

Port the s8njee-web Pillow-based thumbnail pipeline to Rust.

**What you build:** A CLI that takes an input image, detects format, generates a resized thumbnail (max 400px width), and writes it to disk. Support JPEG, PNG, WebP.

**Concepts:** The `image` crate, file I/O, enum-based format detection, `clap` for CLI args.

**Stretch:** Batch processing a directory of images with `rayon` or `tokio::fs`.

---

## Phase 2 — Port Projects

### 2.1 — csearch Scraper Core in Rust (2-3 weeks)

The Go scraper is the highest-value port target. It's a CPU-bound data pipeline with concurrency, XML/JSON parsing, Postgres writes, and Redis cache invalidation — all things Rust excels at.

**What you build:** A Rust binary that replicates the Go scraper's core loop:
1. Walk a directory of bill XML / vote JSON files
2. Hash each file, skip unchanged (reuse the hash cache from 1.1)
3. Parse XML into typed structs
4. Write to Postgres in batches
5. Invalidate Redis cache keys

**Key crates:**
- `quick-xml` or `roxmltree` — XML parsing
- `serde` + `serde_json` — JSON deserialization
- `sqlx` — async Postgres (compile-time query checking)
- `redis` — async Redis client
- `tokio` — async runtime, task spawning, concurrency control with `Semaphore`
- `tracing` + `tracing-subscriber` — structured JSON logging (replaces Go's `slog`)

**Architecture notes:**
- Model the 64-worker parse pool as a `tokio::task::JoinSet` with a semaphore limiting DB write concurrency to 4 (mirroring the Go design)
- Use `sqlx::query!` macros for compile-time checked SQL (replaces `sqlc`)
- Structured logging with `tracing` spans for per-file context

**Learning milestones:**
1. Parse a single bill XML into a Rust struct
2. Insert one bill into Postgres with `sqlx`
3. Add hash-based skip logic
4. Add concurrent parsing with `tokio::task::spawn`
5. Add Redis cache invalidation
6. Add structured logging and error context with `anyhow`/`thiserror`

### 2.2 — csearch API in Axum (2-3 weeks)

Replace the Fastify API with a Rust HTTP server. This gives deep exposure to async web development with `tokio` and `axum`.

**What you build:** A read-only REST API serving bills, votes, search, members, and committees from Postgres with Redis caching.

**Key crates:**
- `axum` — HTTP framework (tower-based, first-class `tokio` integration)
- `sqlx` — async Postgres connection pool
- `redis` — async cache reads
- `tower` — middleware (rate limiting, compression, CORS)
- `tower-http` — `CorsLayer`, `CompressionLayer`
- `serde` — request/response serialization
- `tracing` — per-request structured logging

**Key endpoints to port:**
| Route | Complexity | Notes |
|-------|-----------|-------|
| `GET /api/bills/latest/:type` | Low | Cached Postgres query |
| `GET /api/bills/:congress/:type/:number` | Low | Single-row lookup |
| `GET /api/bills/search?q=` | High | Full-text search with `websearch_to_tsquery`, trigram similarity, multi-stage ranking |
| `GET /api/votes/:chamber` | Low | Cached list query |
| `GET /api/explore/:queryId` | Medium | Dynamic SQL from predefined query set |

**Architecture notes:**
- Use `axum::extract::State` for shared Postgres pool + Redis connection
- Implement the fail-open Redis cache pattern: cache miss → query DB → cache result, cache error → query DB anyway
- Set `X-Cache: HIT/MISS` response header
- Use `tower::limit::RateLimitLayer` for rate limiting
- Graceful shutdown with `tokio::signal`

### 2.3 — csearch-nlp Chunker + Embedder Pipeline (2-3 weeks)

Port the Python RAG data pipeline to Rust. This covers XML parsing, text chunking with token counting, HTTP API calls, and vector DB operations.

**What you build:** A Rust binary that:
1. Fetches bill XML from GovInfo (or reads from local cache)
2. Chunks text with section-aware splitting, token counting, and overlap
3. Deduplicates chunks via SHA-256
4. Calls OpenAI embeddings API in batches
5. Upserts vectors to Qdrant

**Key crates:**
- `reqwest` — HTTP client for GovInfo + OpenAI API calls
- `quick-xml` — XML parsing for bill text extraction
- `tiktoken-rs` — token counting (cl100k_base, matching the Python `tiktoken`)
- `qdrant-client` — official Qdrant Rust client (gRPC-based)
- `tokio` — async runtime, buffered streams for batch processing
- `serde` — OpenAI API request/response structs

**Learning milestones:**
1. Parse one bill XML and produce chunks with token counts
2. Deduplicate with SHA-256 hashes
3. Call OpenAI embeddings API and deserialize response
4. Upsert a batch of vectors to Qdrant
5. Add checkpointing for fault tolerance (resume after crash)
6. Add concurrent fetching with `tokio::sync::Semaphore` rate limiting

### 2.4 — EVA Special Effects State Machine as WASM (1 week)

Port the shared special effects system from eva to a Rust library compiled to WebAssembly. This is a contained, well-defined state machine — a great intro to Rust-WASM interop.

**What you build:** A Rust library that manages effect state (cinematic mode, glitch, thermal, hue cycle, etc.), handles hotkey → action mapping, and exposes the current effect configuration to JavaScript via `wasm-bindgen`.

**Key crates:**
- `wasm-bindgen` — Rust ↔ JS interop
- `serde` + `serde-wasm-bindgen` — pass structs across the boundary
- `wasm-pack` — build tooling

**Concepts:** Enums as state, `match` expressions, no-std compatible logic, WASM size optimization.

---

## Phase 3 — Adjacent Projects

Standalone projects that build Rust skills in areas adjacent to your stack.

### 3.1 — Async Job Runner / Task Queue (1-2 weeks)

Build a lightweight task queue inspired by the csearch nightly CronJobs and csearch-nlp sync jobs.

**What you build:** A Rust service that:
- Accepts job definitions via HTTP (axum)
- Enqueues them in Redis (or an in-process queue)
- Executes jobs concurrently with configurable parallelism
- Reports job status via API
- Supports retry with exponential backoff

**Key crates:** `tokio` (timers, channels, `JoinSet`), `axum`, `redis`, `serde`, `tracing`.

**Why:** Teaches `tokio` channels, cancellation, timeouts, and structured concurrency — patterns you'll use in every async Rust project.

### 3.2 — SSE Streaming Search Server (1-2 weeks)

Build a server that mimics the csearch-nlp SSE streaming pattern: receive a query, stream back results token-by-token.

**What you build:** An axum server with an SSE endpoint that:
- Accepts a search query
- Retrieves context from Postgres (full-text search)
- Calls an LLM API (OpenAI) with streaming enabled
- Pipes the token stream back to the client as SSE events
- Includes source citations before the LLM stream

**Key crates:** `axum` (SSE via `Sse` extractor), `reqwest` (streaming response body), `tokio_stream`, `futures`, `serde`.

**Why:** SSE streaming is the csearch-nlp delivery pattern. This teaches `Stream` trait, backpressure, and real-time HTTP — skills directly applicable to porting the NLP service.

### 3.3 — Reciprocal Rank Fusion Library (3-5 days)

Extract the csearch-nlp fusion + reranking logic into a standalone Rust library.

**What you build:** A library crate that:
- Takes multiple ranked result lists (each with an ID + score)
- Fuses them with Reciprocal Rank Fusion (RRF)
- Supports pluggable reranking (trait-based)
- Returns a merged, reranked result set

**Key crates:** `serde` (for result types), `ordered-float` (for safe float sorting).

**Why:** Pure algorithm crate with no I/O. Great for learning generics, traits, iterators, and `cargo test` + `cargo bench`. Benchmarking with `criterion` teaches performance-aware Rust.

### 3.4 — WebGPU Particle System (2-3 weeks)

Build a native Rust graphics application inspired by eva's Three.js visualizations.

**What you build:** A desktop application that renders an interactive particle system (similar to Matrix rain or the Atom electron trails) using `wgpu`.

**Key crates:**
- `wgpu` — WebGPU API (runs on Metal/Vulkan/DX12, also compiles to WASM)
- `winit` — window management and input events
- `glam` — math (vectors, matrices)
- `bytemuck` — safe casting for GPU buffer data

**Why:** Bridges your Three.js/WebGL experience to Rust's graphics ecosystem. `wgpu` is the Rust equivalent of WebGPU and works both native and in-browser.

### 3.5 — Config-Driven Kubernetes Manifest Generator (1 week)

Build a CLI that generates Kubernetes YAML from a Rust config struct, inspired by the k8s manifests across csearch and s8njee-web.

**What you build:** A CLI that reads a TOML/YAML config describing a service (name, image, replicas, env vars, volumes, ports) and outputs valid Kubernetes Deployment + Service + Ingress YAML.

**Key crates:** `serde` + `serde_yaml`, `clap`, `toml`.

**Why:** Exercises serde deeply — custom serialization, nested structs, optional fields, enums as YAML tags. Practical for your deployment workflow.

### 3.6 — Blog Engine with Axum + SQLx (2-3 weeks)

A direct Rust rewrite of s8njee-web — the most complete port project.

**What you build:** A server-rendered blog + photo gallery:
- `axum` for HTTP routing
- `sqlx` with Postgres (or SQLite for dev)
- `askama` or `tera` for HTML templates
- `pulldown-cmark` + `ammonia` for markdown (reuse from 1.2)
- `image` crate for thumbnail generation (reuse from 1.3)
- Serve static files with `tower-http::ServeDir`

**Key learning areas:**
- Form handling and file uploads in axum
- Session/auth middleware
- Database migrations with `sqlx migrate`
- Template rendering (Rust's answer to Django templates)
- Static file serving and compression

**Why:** End-to-end web application development in Rust. Covers every layer: routing, middleware, database, templates, file handling.

---

## Crate Reference

Quick reference for the most important crates across all projects.

| Crate | Category | Used In |
|-------|----------|---------|
| `tokio` | Async runtime | 2.1, 2.2, 2.3, 3.1, 3.2 |
| `axum` | HTTP framework | 2.2, 3.1, 3.2, 3.6 |
| `sqlx` | Async database | 2.1, 2.2, 3.6 |
| `serde` | Serialization | Everything |
| `reqwest` | HTTP client | 2.3, 3.2 |
| `redis` | Redis client | 2.1, 2.2, 3.1 |
| `tracing` | Structured logging | 2.1, 2.2, 3.1 |
| `clap` | CLI argument parsing | 1.1, 1.3, 3.5 |
| `quick-xml` / `roxmltree` | XML parsing | 2.1, 2.3 |
| `pulldown-cmark` | Markdown → HTML | 1.2, 3.6 |
| `ammonia` | HTML sanitization | 1.2, 3.6 |
| `image` | Image processing | 1.3, 3.6 |
| `wasm-bindgen` / `wasm-pack` | WASM interop | 2.4 |
| `wgpu` | GPU graphics | 3.4 |
| `qdrant-client` | Vector DB | 2.3 |
| `tiktoken-rs` | Token counting | 2.3 |
| `rayon` | Data parallelism | 1.1, 1.3 |
| `anyhow` / `thiserror` | Error handling | Everything |
| `criterion` | Benchmarking | 3.3 |

---

## Suggested Order

```
Phase 1 (2 weeks)
├── 1.1 CLI Hash Cache
├── 1.2 Markdown Sanitizer
└── 1.3 Image Thumbnail Generator

Phase 2 (6-8 weeks, can overlap)
├── 2.1 csearch Scraper Core ← start here, biggest payoff
├── 2.2 csearch API in Axum
├── 2.3 csearch-nlp Chunker + Embedder
└── 2.4 EVA Effects WASM

Phase 3 (pick based on interest)
├── 3.1 Async Job Runner ← if you want deep tokio
├── 3.2 SSE Streaming Server ← if you want to port csearch-nlp serving
├── 3.3 RRF Library ← quick win, good for traits/generics
├── 3.4 WebGPU Particles ← if you want graphics in Rust
├── 3.5 K8s Manifest Generator ← quick win, serde practice
└── 3.6 Blog Engine ← full-stack Rust web dev
```

Phase 1 projects are intentionally small and reusable — their outputs feed directly into Phase 2 ports. Phase 3 projects are independent and can be picked up in any order based on what interests you most.
