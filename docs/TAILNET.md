# Tailnet database connections

Sift can connect directly across Tailscale, or manage an SSH tunnel to a
database that listens only on the remote machine's loopback address. An
optional Serve workflow exposes that loopback service to permitted tailnet
clients. It never enables public Tailscale Funnel.

## Where it runs

All commands and connections run on **Sift's backend**, including when using a
hosted instance. Install and sign in to Tailscale there. Backend PATH must
contain `tailscale`, `ssh`, and `ssh-keyscan`. OpenSSH uses the backend account's
agent/default identity files; it does not forward your desktop agent into a
hosted server. SSH config files and interactive password prompts are disabled.
These controls require instance-administrator access, not just tenant membership.
This implementation uses the installed Tailscale daemon and OS networking; it
does not embed a second tailnet node or manage Tailscale sign-in.

## Connect

1. Open **Add PostgreSQL connection**, paste your database URL, and choose a
   network mode. Keep the URL host as the remote tailnet device, even for tunnels.
2. **Refresh devices** lists peers visible to the backend. Selecting one replaces
   the URL host without changing the database or credentials.
3. Choose **Tailnet direct**, **SSH tunnel**, or **Auto + SSH fallback**. Automatic
   mode probes direct TCP first; it only falls back when TCP is unavailable,
   never after database authentication failure.
4. For tunnels, enter the remote SSH user. The database target inside the tunnel
   is remote `127.0.0.1`, using the database port from the URL.
5. **Read SSH fingerprint**, verify it independently on the database server
   (`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`), then explicitly trust it.
   Scanning is not authentication. Pins are public keys, not private keys. Without
   a pin, OpenSSH's existing backend known_hosts trust is used strictly.
6. **Diagnose network**, then **Add & connect**. Validation happens before saving
   new profiles and replacements. Database passwords remain in SecretStore.

For saved profiles use the connection row menu's **Connection settings…**.
The regular details form includes the same transport controls. Existing profiles
with omitted passwords reuse server-held credentials; changing their settings
while reusing secrets requires tenant-admin access. Manifest-managed profiles
remain subject to manifest lifecycle protection.

Disconnect closes connection-owned listeners and SSH channels. Reopen creates
fresh tunnels. The existing idempotent reconnect path recreates failed tunnels;
SQL statements and transactions are never automatically replayed by this feature.
For TLS `verify_full`, use direct access: tunnel hostname substitution is rejected
until separate TLS server-name support is available. SQL Server certificate
validation is likewise not weakened to make a tunnel work.

## Optional Tailscale Serve

This changes access on the database server. Review both tailnet access policy and
PostgreSQL's **localhost authentication rules** first: the proxied database
connection originates locally on the server. A localhost `trust` rule may permit
access without the database password you expect.

The remote POSIX server needs Python 3, Tailscale Serve, and an SSH account with
Tailscale operator permissions. Sift does not run interactive sudo or install
packages. **Preview / inspect Serve** shows whether the database port already
has a rule. Explicitly confirm reviewed access to enable forwarding.

The helper records ownership in the remote SSH user's private
`~/.local/state/sift/tailnet-serve/<port>.json`, bound to Sift's instance ID.
It never adopts or overwrites an existing rule. Preview/inspection creates only
the private journal directory/lock when needed; it does not enable forwarding.
Apply checks the preview revision and rejects devices with Funnel enabled.
Unrelated rules are preserved. Removal requires the current configuration to
match the recorded post-apply revision and the exact expected rule. Changes
made outside Sift invalidate automatic removal; inspect them manually.

An interrupted apply leaves a pending journal. Sift deliberately refuses to
guess whether it owns a rule after an uncertain outcome. Inspect
`tailscale serve status --json` and the journal on the server before manual
recovery. Removing a database profile does **not** remove Serve: other tailnet
clients may rely on it. Use the explicit removal confirmation first if desired.

## Non-secret profile configuration

The server stores transport metadata under `configuration.sift_network`:

```json
{"mode":"automatic","ssh_user":"db-access","ssh_port":22}
```

Optional `host_key` contains the verified `ssh-ed25519 ...` public key. Custom
SSH ports are supported in the desktop form and API. Backend
endpoints are `/v1/tailnet/status`, `/probe`, `/host-key`, and `/serve` (the latter
three under the same `/v1/tailnet` prefix). SDK methods mirror these operations.
`POST /v1/metadata/connections/validate` tests a candidate without creating
metadata profiles or sessions. Validation and save are separate operations:
if the database fails after successful validation, the saved profile can remain.

References: [Tailscale Serve](https://tailscale.com/docs/reference/tailscale-cli/serve),
[Tailscale CLI](https://tailscale.com/docs/reference/tailscale-cli).
