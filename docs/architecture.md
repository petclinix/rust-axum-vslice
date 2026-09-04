# Architecture

This is the structural reference for `rust-axum-vslice`: the module layout, the
on-disk data model, the rules that govern how slices depend on each other, and the
conventions that follow from them. For the *why* behind the non-obvious ones — with
real code — see [`architecture-internals.md`](architecture-internals.md), referenced
throughout below as `§N`.

## Concept

Vertical slices instead of layers, files instead of a database:
`features/<slice>/{mod.rs, handlers.rs, model.rs}`, each slice owning its own HTTP
handlers, on-disk record shapes, and file I/O end to end. This is the direct opposite
of the sibling PetcliniX implementations — `java-springboot-react-mtier`'s
`Controller → Service → Repository → JPA → MariaDB` and `php-twig-mtier`'s
`Controller → Service → Repository → Domain → PDO/MariaDB` — which both organize by
*layer* (every controller, then every service, then every repository) and persist to
a real database. Here the organizing axis is the *feature*: `pets/handlers.rs` and
`pets/model.rs` sit next to each other, not in separate top-level `controllers/` and
`repositories/` trees, and `pets/model.rs` talks to JSON files directly — there is no
Repository abstraction, no ORM, and no shared "data access layer" spanning every
entity type.

A slice reaches into another slice only through that slice's own public `model`
functions — never a generic cross-entity repository, never direct file access into a
directory another slice owns. See Design Constraint 2 below.

## Module Layout

```
src/
  main.rs                 # startup: config, tracing, admin seeding, serve
  lib.rs                  # build_router() — merges every slice's router
  config.rs                # env-based Config, doubles as the axum State
  error.rs                 # AppError enum -> {"error", "code"} JSON + status code
  domain.rs                 # types shared by >1 slice: Role, AppointmentStatus, TimeRange
  storage/                   # atomic_write / read_json / list_dir_json / FileLock
  auth/
    password.rs                # argon2 hash/verify
    token.rs                    # HS256 JWT issue/verify
    extractor.rs                 # AuthUser: axum FromRequestParts over the bearer token
  features/
    registration/          # register + login, for both owner and vet roles
    vets_directory/          # read-only: list vets + specialties
    pets/                      # add/list/get/update; owner-scoped; picture upload
    availability/                # a vet's weekly schedule + one-off exceptions
    appointments/                  # the core slice: booking, state machine, the flock lock
    visits/                          # a vet's diagnosis/vaccination/notes on a completed appointment
    admin/                             # user list/deactivate, activity log, stats
```

Every slice's `mod.rs` exposes exactly one `pub fn router() -> Router<Config>`;
`lib.rs::build_router` merges them and attaches `Config` as shared state. Nothing
outside a slice ever calls its `handlers` directly — `handlers` stays a private
module; only `model` (and, for `admin`, `activity`) is `pub`, per Design Constraint 2.

## Slice Responsibilities

- **`registration`** — user/owner/vet records, argon2 password hashing, JWT
  issuance. Owns `users/`, `owners/`, `vets/`.
- **`vets_directory`** — read-only pass-through over `registration`'s `Vet` records,
  for an owner picking who to book with. No `model.rs` of its own.
- **`pets`** — CRUD scoped to the calling owner, plus the inline base64
  picture upload/download (§6). Owns `pets/` and `uploads/pets/`.
- **`availability`** — a vet's recurring weekly schedule and one-off date
  exceptions. Owns `availability/` and `availability_exceptions/`.
- **`appointments`** — the core slice: `slots::derive_free_slots` (pure), the
  booking write path, the state machine, cancel/reschedule, and the `vet-<id>.lock`
  every write path shares (§1). Owns `appointments/`.
- **`visits`** — a vet's diagnosis/vaccination/note on a completed appointment;
  owner-facing history. Owns `visits/`.
- **`admin`** — user list/deactivate, the append-only activity log other slices
  write into, and on-demand stats. Owns `activity_log/`.

## On-Disk Data Layout

```
data/
  users/<user_id>.json
  owners/<owner_id>.json
  vets/<vet_id>.json
  pets/<pet_id>.json
  uploads/pets/<pet_id>/picture.<ext>
  availability/<vet_id>/<availability_id>.json
  availability_exceptions/<vet_id>/<id>.json
  appointments/<vet_id>/<appointment_id>.json
  visits/<appointment_id>.json           # filename *is* the appointment id — 0..1 per appointment
  activity_log/<yyyy-mm-dd>.ndjson       # append-only, one JSON object per line
  locks/
    vet-<vet_id>.lock                      # appointment writes for that vet (§1)
    availability-<vet_id>.lock              # that vet's own schedule/exceptions
    users.lock                               # register-time username-uniqueness check + write
```

- **Filenames are the primary key.** A record's id is its filename; "does X exist" /
  "read X" are direct path lookups, never a directory scan, except where the domain
  genuinely needs a set (all appointments for one vet, all pets for one owner,
  admin stats) — and those scans stay scoped to one subdirectory, never `data/` as a
  whole, with two documented exceptions in `appointments/model.rs` (§8).
- **Atomic writes.** Every record write is write-tmp → `fsync` → `rename` — POSIX
  rename is atomic, so a crash mid-write never leaves a half-written record. One
  primitive, `storage::atomic_write`, used everywhere; nothing calls `File::create`
  on a final path directly.
- **Appointments and locks are partitioned by `vet_id`.** A booking attempt only
  ever locks and scans one vet's own directory, never the whole dataset — this is
  what makes the concurrency story tractable (§1).
- **`visits/<appointment_id>.json`** — the filename *is* the appointment id, so
  "at most one visit per appointment" is a property of the layout itself, not a
  separate check (§1 in spirit, though this write needs no lock at all — see the
  file's own doc comment).

## Design Constraints

**1. No database.** All persistent state is JSON files under `data/`
(`DATA_DIR`), one file per record except the activity log (append-only NDJSON).

**2. No cross-entity repository abstraction.** Each slice owns the read/write
functions for the directories it's responsible for. Only primitive, entity-agnostic
helpers are shared (`storage::atomic_write`, `read_json`, `list_dir_json`,
`FileLock`). A slice that needs another slice's data calls that slice's own `model`
function directly — e.g. `pets::handlers` calling
`visits::model::find_all_for_appointments` — never a generic `Repository<T>`.

**3. Correctness comes from OS-level advisory locks (`flock`), not an in-process
`Mutex`.** An in-process mutex would be simpler but would silently stop being
correct the moment a second process touched the same `data/` volume; `flock`
degrades gracefully instead (§1).

**4. The filesystem is the single source of truth.** No long-lived in-memory copy
of the dataset — every request that needs data reads it from files, under the
appropriate lock. A decoded JWT's claims for the current request are fine; a second
copy of the appointment ledger is not.

**5. Vertical slices, not layers.** A feature module is its handlers + record
shapes + file I/O, in one place. Cross-slice reads call the other slice's public
`model` functions directly (e.g. `appointments::handlers` calling
`availability::model::read_weekly`) — no service layer, no trait-object indirection
unless something is genuinely polymorphic.

**6. Single self-contained binary, no bundled frontend.** One Docker image, one
container, one mounted volume — see the root `README.md` for the opt-in React
frontend profile, which talks to this API over plain HTTP, not a build-time
dependency of this binary.

**7. Availability stays derived, not stored.** Free slots are computed on read
from `weekly availability − that date's exception − active appointments`, never
persisted as their own file (§5).

## Auth Design

Reshaped to match `docs/petclinix-openapi-snapshot.json`, the wire contract this
API is being made compatible with.

- **Register** (`POST /api/users/register`, public): owner or vet self-registers
  with `username` + password (argon2-hashed) + `type` (`OWNER`/`VET`). `Admin` is
  rejected here — see below. The target contract carries no profile fields beyond
  that, so the internal `Owner`/`Vet` profile's `name` defaults to `username` and
  `phone`/`specialty` stay blank — nothing in the target contract ever reads them
  back.
- **Login** (`POST /api/auth/login`, public): verifies the password, issues an
  HS256 JWT (1h expiry, claims `sub`=user id, `role`). No refresh tokens, no
  sessions on disk. Response is `{token, type: "Bearer"}`.
- **`GET /api/users/aboutme`**: returns the caller's own `{id, username, role}`
  from their verified bearer token.
- **`AuthUser`** (`auth/extractor.rs`): every protected handler takes this as an
  argument; it verifies the bearer token and yields `{id, role}`. Role checks are
  plain `if auth.role != Role::X` per handler, not a declarative middleware stack.
- **Admin is seeded, never self-registered** (`lib::seed_admin_if_needed`, called
  from `main` before the server starts listening): username `admin@petclinix.local`
  / password `admin12345` if no admin account exists yet — the same fixed
  credentials `php-twig-mtier` seeds its admin with, for direct comparison across
  the PetcliniX implementations.
- **Wire IDs**: every response's `id` is a stable `int64` derived from the
  internal `Uuid` (`domain::wire_id`), not the `Uuid` itself — the target contract
  types every id as `integer(int64)`; storage keeps using `Uuid` throughout, and
  path params resolve the derived id back to it.
- **Role casing on the wire** is `UPPERCASE` (`OWNER`/`VET`/`ADMIN`), matching the
  target contract; internal `Role` variants are unchanged.

## API Surface

Base path `/api`. All bodies JSON; timestamps and dates use `time`'s
human-readable serde encoding (`"YYYY-MM-DD"`, `"YYYY-MM-DD HH:MM:SS.f"`).

| Method & Path | Role | Slice |
|---|---|---|
| `POST /api/users/register` | public | registration |
| `POST /api/auth/login` | public | registration |
| `GET /api/users/aboutme` | any authenticated | registration |
| `GET /api/vets` | owner | vets_directory |
| `GET /api/pets` | owner | pets |
| `POST /api/pets` | owner | pets |
| `GET /api/pets/{id}` | owner | pets (includes visit history, §6) |
| `PUT /api/pets/{id}` | owner | pets |
| `POST /api/vets/availability` | vet | availability |
| `POST /api/vets/availability/exceptions` | vet | availability |
| `GET /api/vets/{id}/slots?date=` | owner | appointments |
| `POST /api/appointments` | owner | appointments |
| `GET /api/appointments` | owner, vet | appointments ("mine": own pets' / own calendar) |
| `POST /api/appointments/{id}/cancel` | owner, vet | appointments (cutoff-gated) |
| `POST /api/appointments/{id}/reschedule` | owner | appointments |
| `POST /api/appointments/{id}/confirm` | vet | appointments |
| `POST /api/appointments/{id}/complete` | vet | appointments |
| `POST /api/appointments/{id}/no-show` | vet | appointments |
| `POST /api/appointments/{id}/visit` | vet | visits |
| `GET /api/pets/{id}/visits` | owner | visits |
| `GET /api/admin/users` | admin | admin |
| `POST /api/admin/users/{id}/deactivate` | admin | admin |
| `GET /api/admin/activity` | admin | admin |
| `GET /api/admin/stats` | admin | admin |

Appointment state machine: `Booked → Confirmed → Completed/Cancelled/NoShow`.
`AppointmentStatus::can_transition_to` (`domain.rs`) is the single source of truth
for which moves are legal; a handler that hits an illegal one returns
`AppError::InvalidTransition`, never a generic 400.

## What Is Intentionally Not Here

- **No ORM-equivalent, no query language, no schema migrations** — the on-disk
  layout above *is* the schema, reviewable as a diff like any other code.
- **No in-process cache/`HashMap` of the dataset** — Design Constraint 4.
- **No DI container** — every slice's dependencies are plain function imports;
  there is nothing to wire.
- **No bundled frontend** — this is a JSON API only; see the root `README.md` for
  the opt-in compose profile that points `java-springboot-react-mtier`'s React
  frontend at it.
- **No horizontal-scaling story beyond "would still be correct if it existed"** —
  `flock` is chosen specifically because it would stay correct across multiple
  processes sharing one mounted volume, but standing up multiple replicas isn't
  part of this showcase.

## Testing

Three layers — see §9 for the full reasoning:

- **Unit/pure-function tests**, colocated per module (`#[cfg(test)] mod tests`),
  each on a fresh `tempfile::tempdir()`. `appointments::slots::derive_free_slots`
  gets the heaviest coverage here — no filesystem, no async, exact-boundary and
  zero-duration edge cases included.
- **In-process HTTP tests**, colocated per slice (`features/*/tests.rs`), driving
  the real `Router` via `tower::ServiceExt::oneshot` — fast, no real sockets.
- **Black-box tests** (`tests/` at the crate root), spawning a real `axum::serve`
  instance on a random port and driving it purely over HTTP via `reqwest` — one
  broad journey per role, proving the whole binary's wiring end to end.

`cargo clippy -- -D warnings` and `cargo fmt --check` run in CI on every push/PR,
alongside a SonarQube scan.
