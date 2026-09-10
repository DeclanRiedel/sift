# Tailnet database connections

## Design

Sift's backend owns network access. Desktop and remote clients use the same
control API; installing Tailscale only on the desktop does not join a hosted
backend to its tailnet. Database credentials remain in SecretStore.

Connection methods: direct (existing behaviour), tailnet direct, SSH tunnel,
and automatic (probe direct TCP, then tunnel only if direct TCP is unavailable).
Never fall back after database authentication failure or replay SQL. A tunnel
targets the SSH host's loopback interface. OpenSSH agent/key authentication and
strict known-host verification are backend-owned. Unknown host keys require
explicit verification outside an unattended connection attempt.

Network administration is instance-admin-only: a tenant must not gain access
to a hosted server's SSH identity or tailnet inventory. Commands are bounded,
non-interactive, use validated arguments, and never include database passwords.
Connection lifetime owns tunnel lifetime, including failed opens and reconnects.

Serve setup is a separate, explicitly confirmed remote mutation. Never enable
Funnel. Warn about tailnet policy and localhost PostgreSQL authentication;
require acknowledgement before exposing a service. Inspect existing Serve
configuration, preserve unrelated rules, and remove only a matching Sift-owned
rule. Ownership must survive restart; conflicting or unverifiable rules fail
closed. No remote setup is performed during development/testing.

## Milestones

- [x] Record architecture, security boundaries, and acceptance checklist.
- [x] Backend device discovery and direct TCP diagnostics.
- [x] Managed SSH tunnel setup, failure cleanup, reconnect and disconnect.
- [x] Persist non-secret transport settings; enforce administration boundary.
- [x] Desktop device picker, transport controls, tests and staged diagnostics.
- [x] Validate existing-profile replacements before writes.
- [x] Opt-in remote Serve preview/apply/inspect/remove with durable ownership.
- [x] Document setup, hosted-backend limitations, authentication and recovery.
- [x] Run formatting, strict workspace Clippy and workspace tests.

## Acceptance

- [ ] No Tailscale daemon, offline peer, TCP refusal/timeout, SSH failure and
      database authentication failure are distinguishable.
- [ ] Tailnet-direct and tunnel profiles reopen without manual terminals.
- [x] Unknown/changed SSH keys fail closed; no auto-accept or password logging.
- [x] Failed new save leaves no requested profile; failed replacement preserves
      prior settings and credentials.
- [ ] Disconnect/failure/cancellation releases local listeners and child processes.
- [x] Serve apply requires explicit exposure acknowledgement; existing rules and
      externally changed rules cannot be overwritten or deleted.
- [ ] UI supports Vim and presents backend location and selected network method.

Implementation includes Alt-key controls alongside the existing Vim workspace,
backend location labels, bounded forwarding tasks, and connection-lifetime
cancellation. Five isolated Python tests cover remote Serve ownership and
interruption; Rust/GPUI tests cover pins, listener cleanup, replacement
preservation, target changes, and administrative access. Full workspace tests
passed, including 478 UI and 79 API tests. Live SSH/Serve acceptance on a user's
tailnet is intentionally not marked complete: no remote configuration was
changed during implementation. Verify the real SSH identity and remote
prerequisites using [the setup guide](../TAILNET.md).

Final integration verification also passed all 262 server unit tests and the
79 API tests after isolating schema-cache keys by transport and effective
direct/tunnel selection. Strict workspace Clippy and formatting checks passed.
