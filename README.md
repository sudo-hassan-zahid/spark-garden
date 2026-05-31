# Spark Garden

Spark Garden is a full-stack Rust web app that runs entirely in Docker with no paid subscriptions or external services.

It is not a CRUD dashboard. It is a small daily ritual app: people can log a mood, grow a shared garden, pick tiny feel-good quests, and leave anonymous kind notes for the next visitor.

## Run

```powershell
docker compose up --build
```

Then open:

```text
http://localhost:8080
```

## What is Rust here?

- HTTP server: Rust standard library
- HTML rendering: Rust
- API routing: Rust
- Persistence: Rust file-backed TSV store
- Docker build/runtime: Rust binary in a Debian container

There are no third-party crates, no Node toolchain, no database service, and no paid APIs.
