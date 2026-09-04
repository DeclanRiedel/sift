#!/usr/bin/env bash
set -euo pipefail

remote_device=${1:?usage: test-tailnet-hosted.sh user@tailnet-device [workspace]}
workspace=${2:-$(pwd)}
profile=${SIFT_SSH_REMOTE_PROFILE:-debug}
server_binary="$workspace/target/$profile/sift-server"
scratch=$(mktemp -d)
server_pid=
serve_owned=0

cleanup() {
  if [[ -n ${server_pid:-} ]]; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  if [[ $serve_owned == 1 ]]; then
    tailscale serve reset >/dev/null 2>&1 || true
  fi
  rm -rf "$scratch"
}
trap cleanup EXIT

for dependency in curl jq python3 scp ssh tailscale timeout; do
  if ! command -v "$dependency" >/dev/null; then
    echo "missing local dependency: $dependency" >&2
    exit 2
  fi
done
if ! ssh -o BatchMode=yes -o ConnectTimeout=10 "$remote_device" \
  'command -v bash curl jq python3 >/dev/null'; then
  echo "the remote device must be reachable non-interactively and provide bash, curl, jq, and python3" >&2
  exit 2
fi
if [[ ! -x $server_binary ]]; then
  echo "build sift-server for the $profile profile first" >&2
  exit 2
fi
if [[ $(tailscale serve status --json) != '{}' ]]; then
  echo "refusing to replace an existing Tailscale Serve configuration" >&2
  exit 2
fi

tailnet_name=$(tailscale status --json | jq -r '.Self.DNSName | rtrimstr(".")')
tailnet_ipv4=$(tailscale status --json | jq -r '.Self.TailscaleIPs[] | select(contains(":") | not)' | head -1)
if [[ -z $tailnet_name || -z $tailnet_ipv4 ]]; then
  echo "this device has no Tailnet DNS name or IPv4 address" >&2
  exit 2
fi
origin="https://$tailnet_name"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
token=$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')
python3 -c 'import secrets,sys; open(sys.argv[1], "w").write(secrets.token_hex(32))' "$scratch/secret.key"
chmod 600 "$scratch/secret.key"

server_environment=(
  SIFT_DEPLOYMENT=personal
  SIFT_TRANSPORT=network
  SIFT_MODE=in-process
  "SIFT_BIND=127.0.0.1:$port"
  "SIFT_RUNTIME__STATE_DIR=$scratch/runtime"
  SIFT_AUTH__LOOPBACK_BYPASS=false
  "SIFT_AUTH__BEARER_TOKEN=$token"
  "SIFT_AUTH__PUBLIC_BASE_URL=$origin"
  SIFT_METADATA__ENABLED=true
  "SIFT_METADATA__PATH=$scratch/metadata.sqlite"
  SIFT_METADATA__SECRET_BACKEND=file
  "SIFT_METADATA__SECRET_KEY_FILE=$scratch/secret.key"
  SIFT_METADATA__BOOTSTRAP_LOCAL=true
  SIFT_DRIVERS__MOCK=true
)

env "${server_environment[@]}" "$server_binary" migrate apply >"$scratch/migrate.log" 2>&1

start_server() {
  env "${server_environment[@]}" "$server_binary" >"$scratch/server.log" 2>&1 &
  server_pid=$!
  for _ in $(seq 1 300); do
    if curl -fsS --connect-timeout 1 --max-time 2 "http://127.0.0.1:$port/v1/ready" 2>/dev/null | jq -e '.ready == true' >/dev/null 2>&1; then
      return
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
      cat "$scratch/server.log" >&2
      echo "hosted test server exited before readiness" >&2
      exit 1
    fi
    sleep 0.1
  done
  cat "$scratch/server.log" >&2
  echo "hosted test server did not become ready" >&2
  exit 1
}

stop_server() {
  kill -TERM "$server_pid"
  local status=0
  wait "$server_pid" || status=$?
  if [[ $status != 0 && $status != 143 ]]; then
    echo "hosted test server exited unexpectedly during restart (status $status)" >&2
    exit 1
  fi
  server_pid=
}

probe_from_remote() {
  local expected_instance=$1
  local payload="$scratch/remote-input"
  printf '%s\n%s\n' "$token" "$expected_instance" >"$payload"
  chmod 600 "$payload"
  local remote_payload
  remote_payload=$(ssh -o BatchMode=yes -o ConnectTimeout=10 "$remote_device" 'umask 077; mktemp')
  if [[ ! $remote_payload =~ ^/tmp/[^/]+$ ]]; then
    echo "remote mktemp returned an unsafe credential path" >&2
    exit 1
  fi
  if ! scp -q -o BatchMode=yes -o ConnectTimeout=10 "$payload" "$remote_device:$remote_payload"; then
    ssh -o BatchMode=yes -o ConnectTimeout=10 "$remote_device" \
      'rm -f -- "$1"' sh "$remote_payload" || true
    echo "copying the private probe credential to the remote device failed" >&2
    exit 1
  fi
  ssh -o BatchMode=yes -o ConnectTimeout=10 "$remote_device" bash -s -- "$origin" "$remote_payload" <<'REMOTE_SCRIPT'
set -euo pipefail
origin=$1
payload=$2
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"; rm -f "$payload"' EXIT
chmod 700 "$scratch"
IFS= read -r token <"$payload"
expected_instance=$(sed -n '2p' "$payload")
handshake=$(curl -fsS --connect-timeout 5 --max-time 15 -X POST \
  -H 'content-type: application/json' \
  -d '{"client_version":"tailnet-test","client_kind":"sdk","protocol":{"minimum":1,"maximum":1}}' \
  "$origin/v1/handshake")
protocol=$(jq -r .selected_protocol <<<"$handshake")
test "$(jq -r .instance_id <<<"$handshake")" = "$expected_instance"
test "$(curl -sS --connect-timeout 5 --max-time 15 -o /dev/null -w '%{http_code}' \
  -H "x-sift-protocol-version: $protocol" "$origin/v1/auth/whoami")" = 401
printf 'header = "Authorization: Bearer %s"\n' "$token" >"$scratch/curl-auth"
chmod 600 "$scratch/curl-auth"
curl -fsS --connect-timeout 5 --max-time 15 "$origin/v1/health" | jq -e '.status == "ok"' >/dev/null
curl -fsS --connect-timeout 5 --max-time 15 "$origin/v1/ready" | jq -e '.ready == true' >/dev/null
curl -fsS --connect-timeout 5 --max-time 15 --config "$scratch/curl-auth" \
  -H "x-sift-protocol-version: $protocol" \
  "$origin/v1/auth/whoami" >"$scratch/whoami.json"
jq -e '.principal.id > 0 and (.memberships | length > 0)' "$scratch/whoami.json" >/dev/null
tenant_id=$(jq -r '.memberships[0].tenant_id' "$scratch/whoami.json")
room=$(curl -fsS --connect-timeout 5 --max-time 15 --config "$scratch/curl-auth" \
  -X POST -H 'content-type: application/json' \
  -H "x-sift-protocol-version: $protocol" \
  -d "{\"tenant_id\":$tenant_id,\"name\":\"tailnet transport probe\",\"kind\":\"shared\"}" \
  "$origin/v1/metadata/rooms")
room_id=$(jq -r .id <<<"$room")
python3 - "$origin" "$room_id" "$token" "$protocol" <<'PY'
import base64
import os
import socket
import ssl
import sys
import urllib.parse

origin, room_id, token, protocol = sys.argv[1:]
url = urllib.parse.urlsplit(origin)
port = url.port or 443
key = base64.b64encode(os.urandom(16)).decode("ascii")
request = (
    f"GET /v1/metadata/rooms/{room_id}/ws HTTP/1.1\r\n"
    f"Host: {url.hostname}\r\n"
    "Upgrade: websocket\r\n"
    "Connection: Upgrade\r\n"
    f"Sec-WebSocket-Key: {key}\r\n"
    "Sec-WebSocket-Version: 13\r\n"
    f"Authorization: Bearer {token}\r\n"
    f"x-sift-protocol-version: {protocol}\r\n\r\n"
).encode("ascii")
with socket.create_connection((url.hostname, port), timeout=15) as raw:
    with ssl.create_default_context().wrap_socket(raw, server_hostname=url.hostname) as secure:
        secure.settimeout(15)
        secure.sendall(request)
        response = b""
        while b"\r\n\r\n" not in response and len(response) < 16384:
            block = secure.recv(4096)
            if not block:
                break
            response += block
headers = response.decode("iso-8859-1").split("\r\n")
if not headers or " 101 " not in headers[0]:
    raise SystemExit(f"websocket upgrade failed: {headers[0] if headers else 'empty response'}")
selected = {
    line.split(":", 1)[0].lower(): line.split(":", 1)[1].strip()
    for line in headers[1:]
    if ":" in line
}.get("x-sift-protocol-version")
if selected != protocol:
    raise SystemExit("websocket response omitted the negotiated Sift protocol")
PY
curl -fsS --connect-timeout 5 --max-time 15 --config "$scratch/curl-auth" \
  -X DELETE -H "x-sift-protocol-version: $protocol" \
  "$origin/v1/metadata/rooms/$room_id" >/dev/null
REMOTE_SCRIPT
}

echo "Starting authenticated network transport on loopback" >&2
start_server
echo "Publishing the server through Tailnet HTTPS" >&2
serve_owned=1
if ! timeout 20s tailscale serve --bg --yes "http://127.0.0.1:$port" \
  >"$scratch/tailscale-serve.log" 2>&1; then
  cat "$scratch/tailscale-serve.log" >&2
  echo "Tailscale Serve could not be enabled within 20 seconds" >&2
  exit 1
fi
https_ready=0
for attempt in $(seq 1 180); do
  if curl -fsS --connect-timeout 2 --max-time 5 "$origin/v1/ready" 2>/dev/null \
    | jq -e '.ready == true' >/dev/null 2>&1; then
    https_ready=1
    break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$scratch/server.log" >&2
    echo "hosted test server exited while waiting for Tailnet HTTPS" >&2
    exit 1
  fi
  if (( attempt % 10 == 0 )); then
    echo "waiting for Tailnet certificate and HTTPS listener (${attempt}s/180s)" >&2
  fi
  sleep 1
done
if [[ $https_ready != 1 ]]; then
  tailscale status >&2 || true
  tailscale serve status >&2 || true
  echo "Tailnet HTTPS did not become ready within 180 seconds" >&2
  exit 1
fi
first=$(curl -fsS -X POST \
  -H 'content-type: application/json' \
  -d '{"client_version":"tailnet-test","client_kind":"sdk","protocol":{"minimum":1,"maximum":1}}' \
  "http://127.0.0.1:$port/v1/handshake")
first_instance=$(jq -r .instance_id <<<"$first")
first_generation=$(jq -r .daemon_generation <<<"$first")
echo "Probing HTTPS and authentication from $remote_device" >&2
probe_from_remote "$first_instance"

echo "Restarting the hosted server generation" >&2
stop_server
start_server
second=$(curl -fsS -X POST \
  -H 'content-type: application/json' \
  -d '{"client_version":"tailnet-test","client_kind":"sdk","protocol":{"minimum":1,"maximum":1}}' \
  "http://127.0.0.1:$port/v1/handshake")
second_instance=$(jq -r .instance_id <<<"$second")
second_generation=$(jq -r .daemon_generation <<<"$second")
if [[ $second_instance != "$first_instance" ]]; then
  echo "hosted restart changed immutable instance identity" >&2
  exit 1
fi
if [[ $second_generation == "$first_generation" ]]; then
  echo "hosted restart reused daemon generation $first_generation" >&2
  exit 1
fi
echo "Probing the restarted generation from $remote_device" >&2
probe_from_remote "$first_instance"

echo "Tailnet HTTPS hosted validation passed from $remote_device to $tailnet_name ($tailnet_ipv4)"
