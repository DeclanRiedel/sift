"""Fixed remote helper. Receives JSON on stdin, never executes user shell text.

Requires Python 3, flock, and a tailscale operator account on a POSIX server.
The private journal is conservative: interrupted mutations require inspection,
never adoption of an unknown pre-existing rule.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def status():
    result = subprocess.run(
        ["tailscale", "serve", "status", "--json"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=8, check=True,
    )
    if len(result.stdout) > 1048576:
        raise ValueError("Serve status exceeds limit")
    return parse_status(result.stdout)


def parse_status(raw):
    value = json.loads(raw)
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise ValueError("Invalid Serve configuration shape")
    return {key: item for key, item in value.items() if item is not None}


def rule(config, port):
    return (config.get("TCP") or {}).get(str(port))


def main():
    request = json.loads(sys.stdin.read(65536))
    port = request["port"]
    if type(port) is not int or not 1 <= port <= 65535:
        raise ValueError("Invalid port")
    owner = request["owner"]
    if not isinstance(owner, str) or not owner:
        raise ValueError("Missing instance identity")
    action = request["action"]
    if action not in ("preview", "inspect", "apply", "remove"):
        raise ValueError("Invalid action")
    root = Path.home() / ".local" / "state" / "sift" / "tailnet-serve"
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if root.is_symlink() or root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o077:
        raise ValueError("Serve journal directory must be private and owned by SSH user")
    journal = root / (str(port) + ".json")
    lock_fd = os.open(str(root / (str(port) + ".lock")), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(lock_fd, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        config = status()
        revision = digest(config)
        marker = None
        if journal.exists():
            with os.fdopen(os.open(str(journal), os.O_RDONLY | os.O_NOFOLLOW)) as source:
                marker = json.load(source)
        expected_rule = {"TCPForward": "127.0.0.1:" + str(port)}
        configured = rule(config, port) is not None
        owned = bool(marker and marker.get("owner") == owner and marker.get("phase") == "active"
                     and marker.get("revision") == revision and rule(config, port) == expected_rule)
        message = "Preview: forward tailnet TCP port %d to remote 127.0.0.1:%d. Review tailnet access policy and PostgreSQL localhost authentication first." % (port, port)
        if action in ("apply", "remove"):
            if not request.get("acknowledge_exposure"):
                raise ValueError("Explicit acknowledgement required")
            if request.get("expected_revision") != revision:
                raise ValueError("Serve configuration changed; inspect again")
            if action == "apply":
                if configured or marker:
                    raise ValueError("Existing rule or journal found; refusing overwrite or adoption")
                if any((config.get("AllowFunnel") or {}).values()):
                    raise ValueError("Funnel is enabled on this device; refusing exposure changes")
                with os.fdopen(os.open(str(journal), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "w") as target:
                    json.dump({"owner": owner, "phase": "pending", "revision": revision}, target)
                    target.flush()
                    os.fsync(target.fileno())
                subprocess.run(["tailscale", "serve", "--bg", "--tcp=" + str(port), "tcp://127.0.0.1:" + str(port)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=8, check=True)
                updated = status()
                expected = json.loads(json.dumps(config))
                expected.setdefault("TCP", {})[str(port)] = expected_rule
                if updated != expected:
                    raise ValueError("Unexpected Serve change; pending journal retained for manual inspection")
                revision = digest(updated)
                with os.fdopen(os.open(str(journal), os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW), "w") as target:
                    json.dump({"owner": owner, "phase": "active", "revision": revision}, target)
                    target.flush()
                    os.fsync(target.fileno())
                owned = configured = True
                message = "Sift-owned tailnet forwarding enabled. PostgreSQL authentication still applies."
            else:
                if not owned:
                    raise ValueError("Rule ownership or configuration cannot be verified; refusing removal")
                subprocess.run(["tailscale", "serve", "--tcp=" + str(port), "off"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=8, check=True)
                updated = status()
                if rule(updated, port) is not None:
                    raise ValueError("Rule still present; journal retained")
                journal.unlink()
                revision = digest(updated)
                owned = configured = False
                message = "Sift-owned forwarding removed. No other Serve rules were targeted."
        elif configured:
            message = "Sift-owned forwarding is active." if owned else "Existing or changed rule is not removable by Sift."
        elif marker:
            message = "Interrupted setup journal exists. Inspect server manually before retrying; Sift will not adopt a rule."
        print(json.dumps(dict(revision=revision, owned=owned, configured=configured, message=message)))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # Fixed diagnostics, no remote command output, environment, or secrets.
        message = str(error) if isinstance(error, ValueError) else "Remote setup failed: check Python 3, tailscale operator permissions, journal and daemon"
        print(json.dumps({"error": message}))
