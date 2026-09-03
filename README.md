# rust-axum-vslice

PetcliniX — files-instead-of-a-database showcase: Rust/axum, JSON files on disk,
vertical-slice module layout, JSON REST API only (no bundled frontend).

## Overview

No database, no layers-for-their-own-sake, minimal runtime footprint — the deliberate
opposite of the other two PetcliniX implementations (`java-springboot-react-mtier`,
`php-twig-mtier`), which are both DB-backed layered architectures. See
[`PLAN.md`](PLAN.md) for the full design: on-disk data layout, the `flock`-based
concurrency model that replaces database transactions/row locks, and the module
layout.

**Status:** all feature slices in `PLAN.md` §6/§13 are implemented — registration,
pets, availability, appointments (including the `flock` concurrency stress test),
visits, and admin.

## Quickstart

```
docker compose up --build
```

Then visit http://localhost:8080/health.

Owners and vets self-register via `POST /api/auth/register`. The admin account is
seeded on first boot (never self-registered — PLAN.md §7): `admin@petclinix.local` /
`admin12345`, the same fixed credentials `php-twig-mtier` seeds its admin with.

### Optional: with the React frontend

This repo is JSON API only and doesn't bundle a frontend (see `PLAN.md` §9), but
`java-springboot-react-mtier`'s React frontend can be pointed at it to exercise the
pet-picture wire-contract compatibility called out there. It's opt-in via a compose
profile, fronted by an nginx reverse proxy since the frontend image calls relative
`/api/...` paths with no configurable API base URL:

```
docker compose --profile frontend up --build
```

Then visit http://localhost:8090. Only the pet-picture wire contract is meant to line
up with that frontend (see `PLAN.md` §9's scope note) — other routes/fields aren't
guaranteed to match.

## Running tests

```
cargo test
```

## Quality tooling

```
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Both run in CI (`.github/workflows/build.yml`), along with a SonarQube scan.
