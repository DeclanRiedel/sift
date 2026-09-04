#!/usr/bin/env bash
set -euo pipefail

destination=${1:?usage: test-ssh-device.sh user@host [workspace]}
workspace=${2:-$(pwd)}
profile=${SIFT_SSH_REMOTE_PROFILE:-debug}
ready_timeout_secs=${SIFT_SSH_REMOTE_READY_TIMEOUT_SECS:-90}
binary_dir="$workspace/target/$profile"
helper_binary="$binary_dir/sift-remote"
server_binary="$binary_dir/sift-server"
scratch=$(mktemp -d)
helper_pid=
remote_state=

case "$profile" in
  debug|release) ;;
  *)
    echo "SIFT_SSH_REMOTE_PROFILE must be debug or release" >&2
    exit 2
    ;;
esac
if [[ ! $ready_timeout_secs =~ ^[1-9][0-9]*$ ]]; then
  echo "SIFT_SSH_REMOTE_READY_TIMEOUT_SECS must be a positive integer" >&2
  exit 2
fi
if [[ ! -x $helper_binary || ! -x $server_binary ]]; then
  echo "build sift-server and sift-remote for the $profile profile first" >&2
  exit 2
fi

cleanup() {
  if [[ -n ${helper_pid:-} ]]; then
    kill -INT "$helper_pid" 2>/dev/null || true
    wait "$helper_pid" 2>/dev/null || true
  fi
  if [[ -n ${remote_state:-} && $remote_state == .cache/sift-device-validation.* ]]; then
    ssh -o BatchMode=yes "$destination" \
      "python3 -c 'import json,os,signal; p=\"$remote_state/runtime/daemon.json\"; os.path.exists(p) and os.kill(json.load(open(p))[\"pid\"], signal.SIGTERM)' 2>/dev/null || true; rm -rf -- $remote_state" \
      >/dev/null 2>&1 || true
  fi
  rm -rf "$scratch"
}
trap cleanup EXIT

remote_state=$(ssh -o BatchMode=yes "$destination" \
  'mkdir -p .cache && chmod 700 .cache && mktemp -d .cache/sift-device-validation.XXXXXX')
if [[ ! $remote_state =~ ^\.cache/sift-device-validation\.[A-Za-z0-9]+$ ]]; then
  echo "remote mktemp returned an unsafe state path" >&2
  exit 1
fi
remote_binary="$remote_state/bin/sift-server"

start_helper() {
  local output=$1
  "$helper_binary" "$destination" \
    --local-server-binary "$server_binary" \
    --state-dir "$remote_state" \
    --remote-binary "$remote_binary" \
    >"$output" 2>"$output.err" &
  helper_pid=$!
}

wait_for_lines() {
  local output=$1
  local expected=$2
  local attempts=$((ready_timeout_secs * 10))
  for _ in $(seq 1 "$attempts"); do
    if [[ $(wc -l <"$output") -ge $expected ]]; then
      return
    fi
    if ! kill -0 "$helper_pid" 2>/dev/null; then
      wait "$helper_pid" || true
      cat "$output.err" >&2
      echo "SSH helper exited before readiness" >&2
      exit 1
    fi
    sleep 0.1
  done
  cat "$output.err" >&2
  echo "SSH helper did not publish readiness within ${ready_timeout_secs}s" >&2
  exit 1
}

assert_ready() {
  local json=$1
  jq -e '
    (.local_base_url | startswith("http://127.0.0.1:")) and
    (.access_token | type == "string" and length > 0) and
    (.instance_id | type == "string" and length > 0) and
    (.daemon_generation | type == "string" and length > 0) and
    (.selected_protocol | type == "number")
  ' <<<"$json" >/dev/null
}

probe_ready() {
  local json=$1
  local base token protocol
  base=$(jq -r .local_base_url <<<"$json")
  token=$(jq -r .access_token <<<"$json")
  protocol=$(jq -r .selected_protocol <<<"$json")
  chmod 700 "$scratch"
  printf 'header = "Authorization: Bearer %s"\n' "$token" >"$scratch/curl-auth"
  chmod 600 "$scratch/curl-auth"
  curl -fsS "$base/v1/health" | jq -e '.status == "ok"' >/dev/null
  curl -fsS "$base/v1/ready" | jq -e '.ready == true' >/dev/null
  curl -fsS --config "$scratch/curl-auth" \
    -H "x-sift-protocol-version: $protocol" \
    "$base/v1/auth/whoami" | jq -e '.principal.id > 0' >/dev/null
  local handshake
  handshake=$(curl -fsS -X POST \
    -H 'content-type: application/json' \
    -d "{\"client_version\":\"device-test\",\"client_kind\":\"sdk\",\"protocol\":{\"minimum\":$protocol,\"maximum\":$protocol}}" \
    "$base/v1/handshake")
  test "$(jq -r .instance_id <<<"$handshake")" = "$(jq -r .instance_id <<<"$json")"
}

start_helper "$scratch/first.jsonl"
wait_for_lines "$scratch/first.jsonl" 1
first=$(sed -n '1p' "$scratch/first.jsonl")
assert_ready "$first"
probe_ready "$first"
first_instance=$(jq -r .instance_id <<<"$first")
first_generation=$(jq -r .daemon_generation <<<"$first")

remote_pid=$(ssh -o BatchMode=yes "$destination" \
  "python3 -c 'import json; print(json.load(open(\"$remote_state/runtime/daemon.json\"))[\"pid\"])'")
ssh -o BatchMode=yes "$destination" "kill $remote_pid"
wait_for_lines "$scratch/first.jsonl" 2
rolled=$(sed -n '2p' "$scratch/first.jsonl")
assert_ready "$rolled"
test "$(jq -r .instance_id <<<"$rolled")" = "$first_instance"
test "$(jq -r .daemon_generation <<<"$rolled")" != "$first_generation"
probe_ready "$rolled"
rolled_generation=$(jq -r .daemon_generation <<<"$rolled")

kill -INT "$helper_pid"
wait "$helper_pid"
helper_pid=
start_helper "$scratch/reconnected.jsonl"
wait_for_lines "$scratch/reconnected.jsonl" 1
reconnected=$(sed -n '1p' "$scratch/reconnected.jsonl")
assert_ready "$reconnected"
test "$(jq -r .instance_id <<<"$reconnected")" = "$first_instance"
test "$(jq -r .daemon_generation <<<"$reconnected")" = "$rolled_generation"
probe_ready "$reconnected"

echo "SSH device validation passed for $destination"
