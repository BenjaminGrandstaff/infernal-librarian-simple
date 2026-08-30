# infernal-librarian-simple

> Infernal Law owns authority, communication, and correctness.
> Librarian owns document data, search, and document semantics.

This is the first real domain service built on top of the
[infernal-law](https://github.com/BenjaminGrandstaff/infernal-law) governance
kernel. Every other repository in this ecosystem so far (`infernal-taskmaster-simple`,
`infernal-worker-simple`) is a *reference implementation of the kernel's own
contract* -- proving the kernel works, not proving anything useful can be
built on top of it. Librarian is different: it is a small, real document
service (put, get, search) whose actual business logic has nothing to do
with governance. It exists to answer one question honestly --

> Can a useful domain service store, version, retrieve, and search its own
> data while receiving governed work through Infernal Law, with no direct
> knowledge of peer services and without adding document or search
> semantics to the kernel?

-- and to be small enough that the answer stays legible either way. If
building this required kernel changes, or lateral communication with other
services, or Librarian reaching into the kernel's own database, that would
be a real architectural finding. It didn't (see "Kernel payload
limitations" below for the one honest exception, and how it was handled).

## Architectural boundary

This division is load-bearing and this repository preserves it exactly:

**Infernal Law kernel owns:**

- authenticated service identity
- communication admission
- authorization enforcement
- durable Requests and routes
- claims, leases, and fencing
- idempotency (of Request *acceptance* -- see "Domain idempotency" below
  for why this is not the same thing as Librarian's own idempotency)
- audit/evidence
- trusted communication between services

**Librarian owns:**

- document/artifact content
- document metadata
- domain persistence
- domain indexing
- search
- domain-specific validation
- document lifecycle semantics
- all Librarian-specific business rules

The kernel has gained no Librarian tables, search logic, document types,
chunk types, embeddings, or lifecycle rules, and never will as a result of
this repository. Librarian never reads or mutates kernel PostgreSQL tables,
and never communicates directly with Taskmaster, Inquisitor, workers,
producers, or any future peer domain service -- every cross-service
signal Librarian acts on arrives as an authenticated Infernal Law Request,
the same way `infernal-worker-simple`'s work arrives. The kernel contract
is infrastructure Librarian depends on; Librarian itself is independently
deployable, independently owned, and independently deletable (see
"Database boundary").

## What this project must not cause

- no document storage in Infernal Law
- no search implementation in Infernal Law
- no Librarian business rules in Infernal Law
- no direct service-to-service communication
- no scheduler authority over worker claims
- no shared domain/kernel database transaction
- no new kernel feature merely because it makes Librarian easier to
  implement

Nothing in this repository's history required touching `infernal-law`.
That is the point being tested, not an accident.

## Domain model

Deliberately small. A `Document` is not a generalized artifact platform --
it is exactly what `librarian.document.put`/`get`/`search` need and
nothing else.

Every document version (`document_versions`, keyed by `(document_id,
version)`) carries:

- `document_id`
- `content`
- `content_type`
- `title` (optional)
- `source_uri` (optional)
- a content digest (SHA-256 of `content`)
- a created timestamp

Documents are **immutable per version**: updating a document appends a new
row rather than overwriting one. The "current version" is always
`MAX(version)` for a `document_id` -- there is no separate head pointer,
kept deliberately simple. See `migrations/0001_documents.sql`.

### Domain idempotency

Infernal Law's own Request/route/claim machinery guarantees a governed
Request is *accepted* exactly once. It says nothing about how many times
Librarian itself might *process* that already-accepted Request -- for
example, a route reclaimed after Librarian crashes between committing its
own domain write and completing the kernel claim (see "Failure semantics"
below) delivers the same Request again on the next claim.

Librarian's own domain idempotency boundary is a `put_operations` table
keyed by the kernel's own stable `request_id` (`migrations/0001_documents.sql`).
Before performing a `librarian.document.put` mutation, the repository
checks whether this `request_id` was already processed; if so, it returns
the existing result untouched rather than writing anything new. This is
enforced in `PostgresDocumentRepository::put`, not assumed -- see
`tests/domain_repository.rs::repeated_put_with_the_same_request_id_does_not_create_a_duplicate_document`.

### Search

PostgreSQL full-text search (`tsvector`/`GIN`, `migrations/0002_search.sql`)
over each document version's own title and content. No vector embeddings,
no external search engine, no graph database, no RAG, no semantic
chunking -- those are all real, plausible future directions, and all
explicitly deferred until the service/kernel boundary this project exists
to prove is itself proven. Search belongs entirely to Librarian: the
kernel never inspects, ranks, or executes it.

## Kernel payload limitations

infernal-law's MVP `Request` carries only a namespaced `action`, a `scope`
string (bounded to 200 characters), and schema version references --
ILK-006 artifact/content mediation (a real payload channel) is explicitly
Future Kernel, not built yet (see
[minimum-viable-kernel.md](https://github.com/BenjaminGrandstaff/infernal-law/blob/main/docs/architecture/minimum-viable-kernel.md)
Section 8). This is a genuine, structural limitation of the kernel as it
exists today, not a Librarian design choice, and this repository does not
route around it with an invented side channel (a direct upload endpoint,
a second signed protocol, lateral communication with the caller). Instead
-- matching the same discipline the kernel's own architecture document
applies to results it cannot yet carry cleanly -- Librarian accepts the
constraint and documents it:

- `librarian.document.put`'s `scope` carries the document's raw text
  content directly. This caps a put's content at 200 bytes for now, and
  always uses `content_type = "text/plain"`; `title`/`source_uri` are not
  populated through this path (the domain layer supports both -- see
  `tests/domain_repository.rs` -- this is a kernel-adapter limitation,
  not a domain one).
- `librarian.document.get`'s `scope` carries a document ID, optionally
  followed by `@<version>` for a specific version (latest otherwise).
- `librarian.document.search`'s `scope` carries the raw query text.

When ILK-006 (or an equivalent real payload/result channel) lands in a
future kernel version, that is the correct place to carry real document
content and real query results -- not a Librarian-invented workaround
retrofitted into `scope`.

## Results

The same limitation applies in the other direction: the kernel's claim/
complete contract has no field for returning arbitrary result data to the
original caller. A `librarian.document.put`'s generated `document_id`, and
a `librarian.document.search`'s hits, are **not** delivered back through
the kernel to whoever submitted the Request -- there is nowhere in the
current contract for them to go.

This repository does not solve that by inventing a generalized result/
event framework, moving result storage into the kernel, or having
Librarian call the original caller directly (which would itself be the
forbidden lateral communication). Instead: Librarian persists every
result in its own database and completes the governed claim once that
persistence (or its already-processed recognition -- see "Domain
idempotency") succeeds. An operator or test harness with legitimate
direct access to Librarian's own database (never through the kernel) can
observe the result; the original caller, today, cannot retrieve it
through the same governed round trip that requested it.

Document the eventual cross-service result/request pattern (most likely
built on ILK-006 artifact mediation, or a dedicated ILK-0xx result
channel) as a future kernel integration need -- not something to implement
here by working around the gap.

## Infernal Law integration

Uses [`infernal-client-rs`](https://github.com/BenjaminGrandstaff/infernal-client-rs)
rather than reimplementing the signed request protocol -- the same crate
`infernal-worker-simple` and `infernal-taskmaster-simple` use.

At startup, Librarian:

1. enrolls as a normal Infernal Law service principal (ADR-0008, when
   `ENROLLMENT_CHALLENGE` is configured -- see Kubernetes section below);
2. idempotently ensures it has an active inclusive subscription for each
   of `librarian.document.put`, `librarian.document.get`, and
   `librarian.document.search` (`KernelClient::ensure_subscription`,
   `src/kernel_client.rs`) -- unlike `infernal-worker-simple`, which
   relies on a subscription being provisioned out of band, Librarian
   creates its own, and a restart never fails or duplicates one that
   already exists.

Then, in its main loop (`work_once`, `src/lib.rs`):

3. polls `GET /v1/routes/eligible`;
4. claims eligible work under its own authenticated service/instance
   identity (`POST /v1/routes/{route_id}/claims`);
5. reads the Request through `GET /v1/routes/{route_id}/request`;
6. interprets the namespaced action (`src/dispatch.rs`);
7. performs the domain operation against Librarian-owned PostgreSQL;
8. completes the claim through the kernel
   (`POST /v1/claims/{id}/complete`).

### Worker ownership

Librarian claims its own work, exactly like `infernal-worker-simple` and
for the same reason: the kernel takes both `worker_service` and
`worker_instance` from whichever caller signs the claim request, never
from a request body field, so there is no way for one process to claim
work and hand it to a different process to complete. Taskmaster may
recommend what should run next; it never claims on Librarian's behalf,
and the kernel remains the final claim/lease/fencing authority regardless
of what any scheduler recommends.

## Database boundary

Librarian owns its own PostgreSQL database and schema, entirely separate
from infernal-law's own database:

- Librarian's migrations (`migrations/`) live only in this repository.
  infernal-law's own migrations never reference Librarian's tables, and
  Librarian's migrations never reference infernal-law's.
- Librarian connects to its own database via `LIBRARIAN_DATABASE_URL`
  (`src/database.rs`) -- a distinct connection string from infernal-law's
  own `DATABASE_URL`, typically a different PostgreSQL instance entirely.
- Librarian can be deleted and rebuilt from its own migrations without
  affecting kernel correctness. The kernel can be replaced or restarted
  without ever becoming the authoritative store for Librarian data --
  restarting Librarian reconstructs its state entirely from its own
  PostgreSQL database plus whatever the kernel currently reports as
  eligible work (see "Failure semantics").

## Failure semantics

### domain commit succeeded, but kernel completion was not recorded

This is the first real distributed transaction boundary between
kernel-owned state and domain-owned state, and it is deliberately **not**
solved with a distributed database transaction between infernal-law and
Librarian. Instead: domain-level idempotency (see "Domain idempotency"
above) makes the operation safe to retry, and the existing successful
domain result is recognized rather than redone.

Concretely, in `work_once` (`src/lib.rs`): the domain operation runs
*before* the kernel claim is completed, and the claim is only ever
completed if the domain operation succeeded (or was already recognized as
done). If Librarian crashes after its own domain commit but before
calling `complete_claim`, the route's lease eventually expires, becomes
eligible again, and gets reclaimed -- by this same instance on restart,
or another. Reprocessing the same request_id doesn't create a second
document; `put_operations` recognizes it, returns the existing result,
and the *new* claim is what actually gets completed. Locked in by
`tests/kernel_adapter.rs::a_domain_repository_failure_never_completes_the_kernel_claim`
in the other direction: a failing domain write must never be followed by
a completion call.

### Everything else tested

- **Duplicate processing of the same domain command does not create a
  duplicate domain mutation** -- domain idempotency, above.
- **Process crash after claim but before domain commit** -- no domain
  side effect ever happened; the reclaimed route is processed fresh.
- **Process crash after domain commit but before kernel completion** --
  the scenario this section is about.
- **Stale claim/fencing loss is handled without corrupting Librarian
  state** -- a fenced completion attempt (`CompleteOutcome::Fenced`)
  never touches Librarian's database a second time; the domain write
  already happened and is left exactly as it was
  (`WorkOutcome::LostBeforeCompletion`).
- **Kernel unavailable causes Librarian to stop receiving new governed
  work but does not corrupt domain data** -- `work_once` fails the pass
  and logs it (`run`'s loop) before any domain write is attempted; no
  partial domain state is possible from a kernel-side failure.
- **Librarian database unavailable causes the operation to fail rather
  than report successful work completion** -- a domain repository error
  propagates out of `work_once` *before* `complete_claim` is ever called;
  see the same `a_domain_repository_failure_never_completes_the_kernel_claim`
  test above.
- **Restarting Librarian reconstructs its state entirely from its own
  PostgreSQL database plus current kernel work state** -- Librarian holds
  no other state; `Config::from_env` reconnects to both independently
  every time the process starts.

## Configuration

- `KERNEL_AUTHORITY` (required) -- the kernel's host (and, if needed,
  port), for example `infernal-law`. Never a scheme or path.
- `LIBRARIAN_SERVICE_ID` (required) -- this service's own `service_id`,
  as a UUID. Must already be provisioned as an `identities` row and
  enrolled with the kernel (ADR-0008), with communication admission
  enabled and an ILK-002 authority grant for each action this service
  subscribes to plus `subscription.create` -- deployment configuration,
  not something this scaffold performs itself.
- `LIBRARIAN_DATABASE_URL` (required) -- Librarian's own PostgreSQL
  connection string. Never infernal-law's `DATABASE_URL`.
- `CLAIM_LEASE_SECONDS` (default `300`) -- the lease duration proposed
  with each claim.
- `POLL_INTERVAL_SECONDS` (default `5`) -- how often to poll
  `GET /v1/routes/eligible`.
- `KERNEL_CA_CERT_PATH` (optional) -- path to a PEM-encoded certificate
  authority to trust in addition to the default public root store, for a
  kernel reachable only behind a private or self-signed certificate.
- `ENROLLMENT_CHALLENGE` (optional) -- a base64url-encoded, 32-byte
  ADR-0008 enrollment challenge from a kernel operator's own out-of-band
  challenge issuance (infernal-law has no self-service HTTP call for
  requesting one). When set, `SERVICE_ENDPOINT` and `POD_UID` become
  required, and `WORKLOAD_TOKEN_PATH` (default
  `/var/run/secrets/infernal-law-enrollment/token`) must point at this
  Pod's own projected `infernal-law-enrollment`-audience ServiceAccount
  token.
- `HEALTH_ADDRESS` (default `0.0.0.0:8090`) -- where the local-only
  `/health/live` and `/health/ready` endpoints listen (`src/health.rs`).

## Development

```sh
cargo build
cargo test
```

## Podman

```sh
podman network create infernal-law
podman build -t localhost/infernal-librarian-simple:latest .
```

### Librarian's own PostgreSQL

Librarian does not need `pgvector` or any other extension -- a plain
upstream image is enough:

```sh
cp containers/postgres/postgres.env.example containers/postgres/postgres.env
podman volume create infernal-librarian-postgres-data
podman run --detach --name infernal-librarian-postgres \
  --network infernal-law \
  --env-file containers/postgres/postgres.env \
  --publish 127.0.0.1:5433:5432 \
  --volume infernal-librarian-postgres-data:/var/lib/postgresql/data:Z \
  docker.io/library/postgres:17
```

```sh
podman run --rm --name infernal-librarian-simple --network infernal-law \
  --env KERNEL_AUTHORITY='infernal-law' \
  --env LIBRARIAN_SERVICE_ID='00000000-0000-4000-8000-000000000006' \
  --env LIBRARIAN_DATABASE_URL='postgres://infernal_librarian:YOUR_PASSWORD@infernal-librarian-postgres:5432/infernal_librarian' \
  localhost/infernal-librarian-simple:latest
```

## Kubernetes

The base manifests are in [`k8s/base`](k8s/base):

```sh
kubectl kustomize k8s/base
kubectl create secret generic infernal-librarian-database \
  --from-literal=url='postgres://USER:PASSWORD@DATABASE_HOST:5432/DATABASE_NAME'
kubectl apply -k k8s/base
```

The Deployment expects a Secret named `infernal-librarian-database` with a
`url` key; this repository does not commit database credentials. It also
expects a `infernal-law-ca-cert` ConfigMap (the same one
`infernal-taskmaster-simple`/`infernal-worker-simple` consume) if the
kernel's own TLS sidecar uses a self-signed certificate -- see
infernal-law's own README's Kubernetes section for how to generate and
publish that certificate.

Librarian requires no Kubernetes API privileges: it never calls the
Kubernetes API itself, only the kernel and its own database. No RBAC is
requested in these manifests, and none should be added unless a genuine
Librarian requirement appears -- not merely because the service runs in
Kubernetes.

### Provisioning a new Librarian identity

Enrollment, communication admission, and authorization are all
infernal-law's own out-of-band administrative concerns (see that
repository's README and architecture document) -- nothing here automates
them, matching the treatment every other reference service gets. Before
Librarian can do anything, an operator with direct database access to the
kernel's own PostgreSQL must:

1. insert an `identities` row for `LIBRARIAN_SERVICE_ID`;
2. insert an enrollment binding for Librarian's Kubernetes ServiceAccount
   and enable it;
3. enable `service_communication_admission` for that identity;
4. create an ILK-002 authority grant for each of
   `librarian.document.put`, `librarian.document.get`,
   `librarian.document.search`, and `subscription.create`;
5. issue a real ADR-0008 enrollment challenge and set
   `ENROLLMENT_CHALLENGE` to it before the next restart.

## Tests

Split by what each proves, matching the repository's own architectural
boundary:

- **Domain tests** (`tests/domain_repository.rs`, live PostgreSQL,
  `#[ignore]`d) -- document creation, immutable version behavior,
  retrieval, search, and domain idempotency, entirely without a kernel.
  Run with:
  ```sh
  export LIBRARIAN_DATABASE_URL='postgres://...'
  cargo test --test domain_repository -- --ignored --test-threads=1
  ```
- **Kernel adapter tests** (`tests/kernel_adapter.rs`) -- the mapping
  between a routed Request and a Librarian domain command, against fakes
  for both the kernel and the repository. No live kernel, no live
  database. This is also where the "domain commit succeeded, kernel
  completion did not" failure semantics are proven directly:
  `a_domain_repository_failure_never_completes_the_kernel_claim`.
- **Live vertical-slice test** (`tests/live_vertical_slice.rs`,
  `#[ignore]`d) -- against a real deployed infernal-law kernel and a real
  Librarian PostgreSQL: submits a real signed `librarian.document.put`
  Request, confirms the kernel authenticates, authorizes, and routes it,
  confirms Librarian claims, reads, stores, and completes it, restarts
  Librarian's own process state and confirms the document is still
  retrievable, and confirms directly against infernal-law's own database
  that no Librarian-specific table or row exists there.
- **Live requester-submission test** (`tests/live_requester_submission.rs`,
  `#[ignore]`d) -- the same live boundary proven the other way: a separate
  Requester identity submits real signed `put`, `get`, and `search`
  Requests against a real deployed kernel, and a genuinely independent,
  already-running `infernal-librarian-simple` Deployment claims, processes,
  and completes each one entirely through its own ordinary poll loop --
  not `work_once` called in-process the way `tests/live_vertical_slice.rs`
  does. Run 2026-08-30 against a real kind-deployed kernel, evaluator, and
  two isolated PostgreSQL instances: all three actions completed, the
  document was retrievable and searchable, and infernal-law's own database
  contained zero Librarian-specific tables afterward. This run also
  surfaced a real kernel-side limitation, not a Librarian one -- see
  "Kernel limitation observed" below.

### Kernel limitation observed: enrolled instance leases cannot be renewed

An enrolled instance's lease defaults to 60 seconds
(`infernal-law`'s `DEFAULT_LEASE_SECONDS`), after which the kernel's
`ServiceRequestVerifier` rejects every one of that instance's signed
calls with 401 -- including this service's own `GET /v1/routes/eligible`
poll. `infernal-law`'s own `kernel::handshakes` module and
`0005_instance_handshakes.sql` migration model a renewal concept, but no
HTTP route exposes it in the current MVP kernel, so no client of
`infernal-client-rs` -- this service included -- has any way to renew a
lease before it expires. Observed directly: a Librarian instance running
longer than a minute starts failing every poll with 401 until its process
restarts and re-enrolls. This is a kernel-side gap (ILK-005 instance
lifecycle is incomplete, not a Librarian concern) and is not something
this repository should work around -- there is no Librarian-side fix for
"the kernel will not let my instance keep talking to it." Filed against
`infernal-law` rather than papered over here.

## Scope discipline

Before proposing a change to `infernal-law` on this project's behalf,
stop and ask whether it protects authority, communication, or
correctness. If not, it belongs in Librarian. Nothing in this
repository's development required a kernel change -- the two genuine
kernel-side gaps found along the way (no real payload/result channel, and
no instance lease renewal route) are documented above, not routed around.

## License

MIT. See [LICENSE](LICENSE).
