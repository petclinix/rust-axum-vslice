# rust-axum-vslice

PetcliniX — files-instead-of-a-database showcase: Rust/axum, JSON files on disk,
vertical-slice module layout, JSON REST API only (no bundled frontend).

## Overview

No database, no layers-for-their-own-sake, minimal runtime footprint — the
deliberate opposite of the other two PetcliniX implementations
(`java-springboot-react-mtier`, `php-twig-mtier`), which are both DB-backed
layered architectures. Vertical slices instead of layers: each feature module
owns its HTTP handlers, on-disk record shapes, and file I/O end to end, with no
Repository abstraction spanning every entity type. Correctness for the one hard
concurrency rule — no double-booking — comes from an OS-level advisory file lock
per vet (`flock`), not a database transaction. See
[`docs/architecture.md`](docs/architecture.md) for the full design.

## Documentation

| Doc | What it covers |
| --- | --- |
| [`docs/architecture.md`](docs/architecture.md) | The structural reference: module layout, on-disk data layout, design constraints, auth design, the API route table, and what's intentionally not here. |
| [`docs/architecture-internals.md`](docs/architecture-internals.md) | Non-obvious design decisions — the `flock` concurrency model, why locking has to run in `spawn_blocking`, the exception-overrides-template rule for availability, the pet-picture wire contract, a real wire-format bug caught by a test, and the three-layer testing strategy — each as a problem/why-it-breaks/solution writeup with real code. |

## Quickstart

```
docker compose up --build
```

Then visit http://localhost:8080/health.

Owners and vets self-register via `POST /api/users/register`. The admin account is
seeded on first boot (never self-registered): username `admin@petclinix.local` /
password `admin12345`, the same fixed credentials `php-twig-mtier` seeds its admin
with.

### Optional: with the React frontend

This repo is JSON API only and doesn't bundle a frontend, but
`java-springboot-react-mtier`'s React frontend can be pointed at it to exercise
the pet-picture wire-contract compatibility described in
[`docs/architecture-internals.md`](docs/architecture-internals.md) §6. It's
opt-in via a compose profile, fronted by an nginx reverse proxy since the
frontend image calls relative `/api/...` paths with no configurable API base URL:

```
docker compose --profile frontend up --build
```

Then visit http://localhost:8090. Only the pet-picture wire contract is meant to
line up with that frontend — other routes/fields aren't guaranteed to match.

## Running tests

```
cargo test
```

Three layers run here: unit/pure-function tests, in-process HTTP tests per
slice, and black-box tests (`tests/`) that spawn a real instance and drive it
over HTTP via `reqwest`. See
[`docs/architecture.md`](docs/architecture.md#testing) for what each layer is
for.

## Quality tooling

```
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Both run in CI (`.github/workflows/build.yml`), along with a SonarQube scan.
