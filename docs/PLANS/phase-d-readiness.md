# Phase D readiness review

Reviewed 2026-07-20 against the Phase D checklist, the five product goals in
`README.md`, the repository rules in `AGENTS.md`, and the public HTTP/SDK
surface.

## Verdict

Phase D is complete for its declared v0.1 headless-product scope. Phase E can
start with ADR-030, the hosted identity model. No remaining Phase D item is a
dependency for identity, token issuance, or local-mode behavior.

## Polish completed

- Transaction completion is single-flight. A failed commit or rollback keeps
  the transaction registered so the caller can retry or recover.
- Process termination reports the engine result; SQL Server self-termination
  is a reported no-op instead of a false success.
- CSV import into an existing table casts text through the authoritative
  target schema, preserving lexical values such as `001`.
- Every Phase D route is represented in OpenAPI and the reference SDK.
- DDL generation and query export have first-class `Operation` variants and
  participate in contextual capability enumeration.
- Phase D handlers record both successful and failed operation outcomes.
- Table DDL preserves defaults and identity behavior. PostgreSQL serial
  columns do not retain a reference to the source sequence, and foreign tables
  fail explicitly instead of producing misleading ordinary-table DDL.

## Accepted follow-ups

These are intentionally outside the Phase D completion bar and remain visible
in their owning plans:

- generated/computed-column exclusion and optional edit dry-run conflicts;
- eager search-index warming/invalidation, concurrent data fan-out,
  engine-native FTS, and non-text data search;
- SQL Server execution with `analyze=true`;
- the Priority 2-4 items in `docs/PLANS/ddl-gaps.md`, including standalone
  sequence/trigger/type DDL and a live SQL Server round-trip fixture;
- replacing the hand-authored OpenAPI document with generated schemas (Phase
  J). The Phase D route/schema drift is covered by a regression test meanwhile.

## Phase E entry conditions

1. Write ADR-030 before implementation. ADR-019 is already the audit
   durability decision and cannot be reused.
2. Preserve local mode's loopback bypass and bootstrapped local principal.
3. Propagate authenticated actor identity into operation audit records. Phase
   D records outcomes with no actor because hosted identity does not exist yet.
4. Decide whether the existing `principal_key` and `keypair_challenge` schema
   is adopted or removed before adding another keypair representation.
