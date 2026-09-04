# Reconstruction Notes

_Started 2026-09-04._

## Context

This stack is being reconstructed on a new system. The previous working tree is
gone, so everything below is derived from what is committed here — not from a
running install. Treat the repositories as the source of truth and assume no
host-local state survives.

The stack-level note lives in
[`infernal-law/NOTES.md`](https://github.com/BenjaminGrandstaff/infernal-law/blob/main/NOTES.md);
this file records what is specific to `infernal-librarian-simple`.

Four services in the stack carry a PostgreSQL database — `infernal-law`,
`infernal-librarian-simple`, `infernal-pf2e-rules-simple` and
`infernal-pf2e-parser-simple`. All four use `r2d2` + `r2d2_postgres` (a
**blocking** pool), a `migrations/` directory of zero-padded SQL, and their own
role, database and container hostname. The remaining repositories hold no
database and reach these over HTTP.

## Goal: make the install self-healing

The intent is for a service to bring itself to a correct state on start rather
than depending on a host that was hand-prepared once and never reproduced. Some
of that property already exists here:

- **Schema ships inside the binary.** `migrate()` in `src/database.rs`
  applies both migrations (`0001_documents.sql`, `0002_search.sql`) through `include_str!` in a single `batch_execute` during
  startup. There is no external migration runner and no separate deploy step to
  forget.
- **Every migration is re-runnable.** All DDL is `CREATE TABLE / INDEX IF NOT
  EXISTS`, so the batch executes on every boot without error.

Because there is **no schema-version tracking table**, that idempotency is
load-bearing rather than incidental: the entire batch replays at every start.
Any future migration that is not written to be re-runnable will break startup on
an existing database, not just on a fresh one.

## Blocker: move and stabilize the install first

- **No PostgreSQL extension is required.** Search is built on the built-in
  `tsvector` type with a GIN index (`0002_search.sql`), not `pgvector`. A plain
  upstream PostgreSQL image is enough — see "Database boundary" in the README.
  This is the one place the stack differs from `infernal-law`, which *does*
  require `pgvector` and fails closed without it.
- `containers/postgres.env.example` carries the expected connection
  environment; there is no extension init script to run.
- **`infernal-client-rs` is a git dependency pinned to a rev** (`be3244a`).
  Reconstruction needs network access to GitHub, and bumping the client is an
  explicit rev change in `Cargo.toml`.
- **A database and role must exist before the service starts.** Nothing in this
  repository provisions them.

Only once a clean host reaches a running, provisioned state reproducibly does it
make sense to add self-healing behavior on top — otherwise the healing logic
cannot be distinguished from the setup it is meant to repair.

## Known rough edge

The migration list in `src/database.rs` is maintained by hand: adding a file to
`migrations/` does nothing until its `include_str!` line is added too. The
counts currently agree, but they can drift silently, and the failure mode is a
missing table at runtime rather than a build error.
