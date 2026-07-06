# TODO — Ordered Work Queue

This file tracks active implementation work only. The headless collaboration
infrastructure slice described in `docs/PLANS/headless-collab-infra.md` is now
implemented to its planned foundation boundary.

## Active Work

No active headless-collab foundation tasks remain.

Before opening the next implementation slice, decide which deferred product
surface is next:

1. Desktop or web product client.
2. Hosted auth hardening beyond local/token auth, such as OIDC or keypair auth.
3. Deeper query/result collaboration, such as full result broadcast streams and
   observer lag recovery.
4. Daily-driver database IDE features, such as schema search, autocomplete,
   export, explain plans, and edit workflows.

## Completed Headless-Collab Foundation

- Metadata runtime hardening:
  - synchronous SQLite work is isolated from async handlers;
  - blocking metadata work has explicit backpressure;
  - multi-row metadata mutations use transactions where needed;
  - credential replacement and deletion clean old secret handles on a
    best-effort basis.
- `sift-doc`:
  - snapshot/text extraction helpers;
  - backend-agnostic text operation apply API;
  - operation tests for replacement, insert/delete, and UTF-8 boundaries.
- Client SDK:
  - typed metadata/auth methods for tenants, rooms, members, documents,
    profiles, credentials, tokens, history, and profile-backed connections;
  - room document-operation WebSocket helper.
- Room runtime:
  - room WebSocket class for attachment, presence, and document operations;
  - in-memory attachment/presence runtime;
  - document operations apply through `sift-doc` and persist snapshots;
  - room-aware operation audit entries.
- Room-aware result handling:
  - direct session execution remains compatible;
  - room/profile context records query history;
  - room WebSocket receives query result summaries without broadcasting full
    result streams.
- API contract:
  - OpenAPI includes typed metadata/auth and room WebSocket schemas;
  - CI covers format, clippy, tests, and cargo-deny.
