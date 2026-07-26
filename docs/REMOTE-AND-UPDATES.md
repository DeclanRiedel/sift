# Remote development and signed updates

Phase H supports three lifecycle modes and a direct OpenSSH remote topology.
Runtime mode does not change deployment or authorization policy:

| Use | Mode | Transport | Update ownership |
| --- | --- | --- | --- |
| Foreground/local parent | `in-process` | usually `loopback` | `sift-launcher` validates and selects a staged bundle |
| Long-lived local or SSH server | `daemon` | `loopback`, `network`, or `ssh-proxy` | Explicit restart/bootstrap activates |
| Immutable image | `container` | `loopback` or `network` | Container orchestrator; self-update is refused |

## Connect through SSH

Build or install `sift-server` and `sift-remote` beside one another, then run:

```console
sift-remote my-ssh-config-host
```

`my-ssh-config-host` is resolved by the system OpenSSH client. Sift preserves
the user's host-key, agent, jump-host, and authentication policy. It never
adds an option that weakens host-key checks.

The helper probes the remote target, uploads a compatible bundled or
signed-cache server when needed, starts or reuses a detached daemon, opens a
local loopback forward, and prints one JSON readiness record. The record
contains a short-lived access token and must be handled as secret material.
It may print replacement records as the grant is renewed. Stopping the helper
closes the forward but deliberately leaves the daemon and durable state alive.

Useful explicit options are:

```console
sift-remote my-host \
  --local-server-binary /trusted/package/sift-server \
  --state-dir .local/state/sift/remote \
  --local-port 0
```

Remote paths are relative, deliberately restricted, and never expanded by
Sift. The default state directory is private to the remote OS account. A
personal instance maps that privately owned state to its bootstrapped local
principal. A team instance requires a registered Sift Ed25519 principal key:
register its public key through `POST /v1/auth/keys`, keep the raw 32-byte
signing seed as 64 lowercase hexadecimal characters in a mode-0600 local
file, and pass that file with `--sift-key-file`. The private key is read
locally and never uploaded.

The remote `sift.toml`, if required, lives in the remote state directory. A
team configuration must set `deployment = "team"`, a stable HTTPS
`auth.public_base_url` audience, and `metadata.bootstrap_local = false`.
Bootstrap always forces `mode = "daemon"`, `transport = "ssh-proxy"`, a
loopback ephemeral bind, durable metadata secrets, and
`auth.loopback_bypass = false`.

After a tunnel loss, reconnect the helper. The daemon, instance id, documents,
rooms, profiles, history, and audit survive. A changed daemon generation means
sessions, connections, transactions, cursors, presence, and result references
must be reopened. An interrupted write has an unknown outcome unless its API
can prove an idempotent result; clients must not replay it automatically.

## Configure signed staging

Signed checks are disabled by default. Official/distribution builds embed one
or more raw Ed25519 release public keys at compile time through
`SIFT_RELEASE_PUBLIC_KEYS`, a comma-separated base64url list. A binary without
embedded keys refuses to construct the updater.

Set an explicit channel and distribution-owned URLs in `sift.toml` or the
matching `SIFT_UPDATER__...` variables shown in `.env.example`:

```toml
[updater]
enabled = true
channel = "stable"
manifest_url = "<distribution-supplied HTTPS manifest URL>"
signature_url = "<distribution-supplied HTTPS signature URL>"
state_dir = ".sift/updates"
max_artifact_bytes = 536870912
initial_delay_secs = 30
check_interval_secs = 21600
jitter_secs = 600
```

The URL placeholders must be replaced with values supplied by the chosen
distribution. Sift never derives a release URL from an SSH,
database, request, or forwarding host.

The detached Ed25519 signature is checked over the exact manifest bytes before
JSON parsing. Channel sequence, expiry, updater/release semver, protocol range,
target, layout, URL, signed length, and SHA-256 are then checked. Downloads
stream into a private bounded staging file. Only the signed `raw` single-binary
layout with executable path `sift-server` is accepted; archive paths and links
therefore cannot enter the install tree.

Successful checks place immutable binaries under the updater state directory
and atomically select a pending candidate. They never overwrite or interrupt
the running process. Start local/daemon installations through `sift-launcher`;
it launches the pending candidate, requires `/ready`, an ADR-016 handshake,
and the signed release version, then commits the pointer. Failure terminates
the candidate, clears its selection, and launches the retained known-good
binary. SSH bootstrap applies the same commit gate after the remote handshake.
Daemons activate only through explicit launcher restart/bootstrap. Container
mode rejects `updater.enabled = true`.

## Release operator contract

`.github/workflows/release.yml` builds native Linux x86-64, Linux ARM64, and
macOS ARM64 server, launcher, remote-helper, and administration artifacts,
checks reproducibility for the server binary, emits checksums and CycloneDX
SBOMs, builds the raw manifest, signs it, verifies a fixture with the public
key, and publishes the assets for an existing tag.

The workflow intentionally requires repository configuration rather than
guessing distribution values:

- variable `SIFT_RELEASE_PUBLIC_KEYS`: comma-separated raw public keys in
  unpadded base64url, embedded in every binary;
- variable `SIFT_RELEASE_PUBLIC_KEY_PEM`: PEM public key used for the
  independent release-job verification;
- variable `SIFT_MINIMUM_UPDATER_VERSION`: oldest updater semver permitted to
  consume the manifest; this is intentionally independent from the new
  release version;
- variable `SIFT_RELEASE_ORIGIN`: exact HTTPS asset origin;
- secret `SIFT_RELEASE_SIGNING_KEY_PEM_B64`: base64-encoded Ed25519 private
  signing key PEM.

Key rotation first ships an overlapping old/new public-key set in a release
signed by the old key. Never put the private signing key, connection
credentials, access grants, `.env`, or runtime secret-store keys in source,
workflow logs, artifacts, or manifests.
