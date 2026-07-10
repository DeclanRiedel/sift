# OS keychain blocking calls

## Issue

The `OsKeychainSecretStore` implements async trait methods but calls the `keyring` crate synchronously inside them.

## Current proof

- `crates/metadata/src/secrets/keychain.rs` calls `Entry::set_secret`, `Entry::get_secret`, and `Entry::delete_credential` directly inside async methods.
- Linux Secret Service and macOS Keychain calls can perform blocking IPC or OS calls.

## Failure mode

When the production-recommended OS keychain backend is enabled, a slow keychain daemon can block tokio workers during credential writes, deletes, or profile opens.

## Changelist

- Wrap each keychain operation in `tokio::task::spawn_blocking`.
- Avoid borrowing `namespace`, `handle`, or `secret` across the blocking boundary; clone owned data before spawning.
- Add tests under `--features os-keychain` for error propagation from the blocking wrapper.
