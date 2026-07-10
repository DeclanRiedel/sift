# Postgres cancel TLS and timeout

## Issue

Postgres cancel does not honor `SslMode::Require`, and cancel sockets have no internal timeout.

## Current proof

- `crates/driver-postgres/src/lib.rs` uses TLS only for `VerifyCa` and `VerifyFull`; all other modes use `NoTls`.
- `SslMode::Require` maps to TLS for normal pool connections in `conn.rs`, but cancel falls through to `NoTls`.
- `token.cancel_query(...).await` is not wrapped in `tokio::time::timeout`.

## Failure mode

Servers that require TLS for all host connections reject cancel requests for `ssl_mode=require`. Network partitions can leave the cancel future hanging.

## Changelist

- Route `SslMode::Require` through a TLS connector that skips certificate verification or use the closest supported rustls connector.
- Wrap cancel in a short timeout, for example five seconds.
- Add live or integration tests against a TLS-required Postgres fixture.
