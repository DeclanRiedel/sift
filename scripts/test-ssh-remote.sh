#!/usr/bin/env bash
set -euo pipefail

workspace=${1:-$(pwd)}
profile=${SIFT_SSH_REMOTE_PROFILE:-debug}
ready_timeout_secs=${SIFT_SSH_REMOTE_READY_TIMEOUT_SECS:-90}
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
binary_dir="$workspace/target/$profile"
helper_binary="$binary_dir/sift-remote"
server_binary="$binary_dir/sift-server"
scratch=$(mktemp -d)
sshd_pid=
remote_state=".cache/sift-ssh-remote-test-$$"
remote_binary="$remote_state/bin/sift-server"
test_user=$(id -un)
sshd_bin=$(command -v sshd)
ssh_bin=$(command -v ssh)
scp_bin=$(command -v scp)
test_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')

cleanup() {
  if [[ -s $scratch/ssh_config ]]; then
    "$ssh_bin" -F "$scratch/ssh_config" sift-ssh-good \
      "python3 -c 'import json,os,signal; p=\"$remote_state/runtime/daemon.json\"; os.path.exists(p) and os.kill(json.load(open(p))[\"pid\"], signal.SIGTERM)' ; rm -rf $remote_state" \
      >/dev/null 2>&1 || true
  fi
  if [[ -n ${sshd_pid:-} ]]; then
    kill "$sshd_pid" 2>/dev/null || true
  fi
  rm -rf "$scratch"
}
trap cleanup EXIT

ssh-keygen -q -t ed25519 -N '' -f "$scratch/client-key"
cp "$scratch/client-key.pub" "$scratch/authorized_keys"
chmod 600 "$scratch/authorized_keys"
ssh-keygen -q -t ed25519 -N '' -f "$scratch/host-key"

cat >"$scratch/sshd_config" <<EOF
Port $test_port
ListenAddress 127.0.0.1
HostKey $scratch/host-key
PidFile $scratch/sshd.pid
AuthorizedKeysFile $scratch/authorized_keys
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
PermitRootLogin prohibit-password
AllowUsers $test_user
Subsystem sftp internal-sftp
EOF
"$sshd_bin" -D -e -f "$scratch/sshd_config" >"$scratch/sshd.log" 2>&1 &
sshd_pid=$!

for _ in $(seq 1 50); do
  if ssh-keyscan -p "$test_port" 127.0.0.1 >"$scratch/known_hosts" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if [[ ! -s $scratch/known_hosts ]]; then
  cat "$scratch/sshd.log" >&2
  exit 1
fi
touch "$scratch/empty_known_hosts"
cat >"$scratch/ssh_config" <<EOF
Host sift-ssh-good
  HostName 127.0.0.1
  Port $test_port
  User $test_user
  IdentityFile $scratch/client-key
  IdentitiesOnly yes
  StrictHostKeyChecking yes
  UserKnownHostsFile $scratch/known_hosts
Host sift-ssh-bad
  HostName 127.0.0.1
  Port $test_port
  User $test_user
  IdentityFile $scratch/client-key
  IdentitiesOnly yes
  StrictHostKeyChecking yes
  UserKnownHostsFile $scratch/empty_known_hosts
EOF

mkdir "$scratch/bin"
cat >"$scratch/bin/ssh" <<EOF
#!/usr/bin/env bash
if [[ \${SIFT_TEST_DISABLE_MASTER:-0} == 1 && " \$* " == *" -M "* ]]; then
  exit 1
fi
exec "$ssh_bin" -F "$scratch/ssh_config" "\$@"
EOF
cat >"$scratch/bin/scp" <<EOF
#!/usr/bin/env bash
exec "$scp_bin" -F "$scratch/ssh_config" "\$@"
EOF
chmod 700 "$scratch/bin/ssh" "$scratch/bin/scp"

export PATH="$scratch/bin:$PATH"
if [[ ! -x $helper_binary || ! -x $server_binary ]]; then
  echo "SSH remote binaries are missing for profile $profile" >&2
  ls -lh "$binary_dir" >&2 2>/dev/null || true
  exit 1
fi

if "$helper_binary" sift-ssh-bad \
  --local-server-binary "$server_binary" \
  --state-dir "$remote_state" \
  --remote-binary "$remote_binary" \
  >"$scratch/bad.out" 2>"$scratch/bad.err"; then
  echo "host-key rejection unexpectedly succeeded" >&2
  exit 1
fi

dump_helper_diagnostics() {
  local output=$1
  local helper_pid=$2
  echo "SSH remote helper diagnostics:" >&2
  echo "  profile=$profile timeout=${ready_timeout_secs}s" >&2
  ls -lh "$helper_binary" "$server_binary" >&2 2>/dev/null || true
  ps -o pid,ppid,stat,etime,cmd -p "$helper_pid" >&2 2>/dev/null || true
  if [[ -s $output.err ]]; then
    echo "sift-remote stderr:" >&2
    cat "$output.err" >&2
  fi
  if [[ -s $scratch/sshd.log ]]; then
    echo "test sshd log:" >&2
    cat "$scratch/sshd.log" >&2
  fi
  "$ssh_bin" -F "$scratch/ssh_config" sift-ssh-good \
    "echo 'remote state:'; find $remote_state -maxdepth 3 -type f -printf '%p %s bytes\n' 2>/dev/null; test ! -f $remote_state/daemon.log || { echo 'remote daemon log:'; cat $remote_state/daemon.log; }" \
    >&2 2>/dev/null || true
}

run_helper() {
  local output=$1
  "$helper_binary" sift-ssh-good \
    --local-server-binary "$server_binary" \
    --state-dir "$remote_state" \
    --remote-binary "$remote_binary" \
    >"$output" 2>"$output.err" &
  local helper_pid=$!
  local attempts=$((ready_timeout_secs * 10))
  for attempt in $(seq 1 "$attempts"); do
    if [[ -s $output ]] && jq -e \
      'type == "object" and (.instance_id | type == "string") and (.daemon_generation | type == "string")' \
      "$output" >/dev/null 2>&1; then
      kill -INT "$helper_pid"
      wait "$helper_pid"
      return
    fi
    if ! kill -0 "$helper_pid" 2>/dev/null; then
      wait "$helper_pid" || {
        dump_helper_diagnostics "$output" "$helper_pid"
        return 1
      }
      dump_helper_diagnostics "$output" "$helper_pid"
      return 1
    fi
    if (( attempt % 300 == 0 )); then
      echo "waiting for SSH remote helper ($((attempt / 10))s/${ready_timeout_secs}s)" >&2
      ps -o pid,stat,etime,cmd -p "$helper_pid" >&2 2>/dev/null || true
    fi
    sleep 0.1
  done
  kill "$helper_pid" 2>/dev/null || true
  wait "$helper_pid" 2>/dev/null || true
  dump_helper_diagnostics "$output" "$helper_pid"
  echo "remote helper did not become ready within ${ready_timeout_secs}s" >&2
  return 1
}

run_helper "$scratch/first.json"
first_instance=$(jq -r .instance_id "$scratch/first.json")
first_generation=$(jq -r .daemon_generation "$scratch/first.json")
test -n "$first_instance"
test -n "$first_generation"

# The first helper closed its control master; the detached daemon must survive
# and a new helper must reconnect to the same generation.
run_helper "$scratch/reconnected.json"
test "$(jq -r .instance_id "$scratch/reconnected.json")" = "$first_instance"
test "$(jq -r .daemon_generation "$scratch/reconnected.json")" = "$first_generation"

# Refusing control-master creation must fall back to dedicated OpenSSH
# connections without weakening host-key policy.
SIFT_TEST_DISABLE_MASTER=1 run_helper "$scratch/fallback.json"
test "$(jq -r .instance_id "$scratch/fallback.json")" = "$first_instance"
test "$(jq -r .daemon_generation "$scratch/fallback.json")" = "$first_generation"

remote_pid=$(
  "$ssh_bin" -F "$scratch/ssh_config" sift-ssh-good \
    "python3 -c 'import json; print(json.load(open(\"$remote_state/runtime/daemon.json\"))[\"pid\"])'"
)
"$ssh_bin" -F "$scratch/ssh_config" sift-ssh-good "kill $remote_pid"
for _ in $(seq 1 50); do
  if ! "$ssh_bin" -F "$scratch/ssh_config" sift-ssh-good "kill -0 $remote_pid" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
rm -f "$scratch/restarted.json"
run_helper "$scratch/restarted.json"
test "$(jq -r .instance_id "$scratch/restarted.json")" = "$first_instance"
test "$(jq -r .daemon_generation "$scratch/restarted.json")" != "$first_generation"
