# API token verification hot path

## Issue

Bearer-token verification uses Argon2 password verification and updates `last_used_at` on every authenticated request.

## Current proof

- `crates/metadata/src/lib.rs` `verify_api_token` runs `Argon2::default().verify_password(...)`.
- The same method immediately writes `UPDATE api_token SET last_used_at = ..., updated_at = ...`.
- `crates/server/src/http.rs` resolves auth context through metadata for bearer-authenticated requests.

## Failure mode

Every GET can consume tens to hundreds of milliseconds of CPU and then serialize on a SQLite write. Throughput is bounded by Argon2 cost, metadata blocking permits, and the single SQLite mutex.

## Changelist

- Replace Argon2 for API tokens with keyed HMAC-SHA256 over high-entropy random token material.
- Keep a lookup prefix for indexed lookup and use constant-time comparison for the MAC.
- Debounce or asynchronously batch `last_used_at` updates.
- Add a migration path for existing Argon2 tokens or explicitly invalidate them before a release boundary.
