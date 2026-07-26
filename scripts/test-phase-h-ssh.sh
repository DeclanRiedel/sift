#!/usr/bin/env bash
set -euo pipefail

workspace=${1:-$(pwd)}
scratch=$(mktemp -d)
sshd_pid=
user_created=0

cleanup() {
  if [[ -n ${sshd_pid:-} ]]; then
    kill "$sshd_pid" 2>/dev/null || true
  fi
  if [[ $user_created -eq 1 ]]; then
    sudo userdel --remove sift-phase-h 2>/dev/null || true
  fi
  rm -rf "$scratch"
}
trap cleanup EXIT

sudo useradd --create-home --shell /bin/bash sift-phase-h
user_created=1
ssh-keygen -q -t ed25519 -N '' -f "$scratch/client-key"
sudo install -d -m 700 -o sift-phase-h -g sift-phase-h /home/sift-phase-h/.ssh
sudo install -m 600 -o sift-phase-h -g sift-phase-h \
  "$scratch/client-key.pub" /home/sift-phase-h/.ssh/authorized_keys
sudo ssh-keygen -q -t ed25519 -N '' -f "$scratch/host-key"

cat >"$scratch/sshd_config" <<EOF
Port 2222
ListenAddress 127.0.0.1
HostKey $scratch/host-key
PidFile $scratch/sshd.pid
AuthorizedKeysFile .ssh/authorized_keys
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
PermitRootLogin no
AllowUsers sift-phase-h
Subsystem sftp internal-sftp
EOF
sudo /usr/sbin/sshd -D -e -f "$scratch/sshd_config" >"$scratch/sshd.log" 2>&1 &
sshd_pid=$!

for _ in $(seq 1 50); do
  if ssh-keyscan -p 2222 127.0.0.1 >"$scratch/known_hosts" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
test -s "$scratch/known_hosts"
touch "$scratch/empty_known_hosts"
cat >"$scratch/ssh_config" <<EOF
Host phase-h-good
  HostName 127.0.0.1
  Port 2222
  User sift-phase-h
  IdentityFile $scratch/client-key
  IdentitiesOnly yes
  StrictHostKeyChecking yes
  UserKnownHostsFile $scratch/known_hosts
Host phase-h-bad
  HostName 127.0.0.1
  Port 2222
  User sift-phase-h
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
exec /usr/bin/ssh -F "$scratch/ssh_config" "\$@"
EOF
cat >"$scratch/bin/scp" <<EOF
#!/usr/bin/env bash
exec /usr/bin/scp -F "$scratch/ssh_config" "\$@"
EOF
chmod 700 "$scratch/bin/ssh" "$scratch/bin/scp"

export PATH="$scratch/bin:$PATH"
if "$workspace/target/debug/sift-remote" phase-h-bad \
  --local-server-binary "$workspace/target/debug/sift-server" \
  >"$scratch/bad.out" 2>"$scratch/bad.err"; then
  echo "host-key rejection unexpectedly succeeded" >&2
  exit 1
fi

run_helper() {
  local output=$1
  "$workspace/target/debug/sift-remote" phase-h-good \
    --local-server-binary "$workspace/target/debug/sift-server" \
    >"$output" 2>"$output.err" &
  local helper_pid=$!
  for _ in $(seq 1 200); do
    if [[ -s $output ]]; then
      kill -INT "$helper_pid"
      wait "$helper_pid"
      return
    fi
    if ! kill -0 "$helper_pid" 2>/dev/null; then
      wait "$helper_pid"
      return 1
    fi
    sleep 0.1
  done
  kill "$helper_pid" 2>/dev/null || true
  wait "$helper_pid" 2>/dev/null || true
  echo "remote helper did not become ready" >&2
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
  /usr/bin/ssh -F "$scratch/ssh_config" phase-h-good \
    "python3 -c 'import json; print(json.load(open(\".local/state/sift/remote/runtime/daemon.json\"))[\"pid\"])'"
)
/usr/bin/ssh -F "$scratch/ssh_config" phase-h-good "kill $remote_pid"
for _ in $(seq 1 50); do
  if ! /usr/bin/ssh -F "$scratch/ssh_config" phase-h-good "kill -0 $remote_pid" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
rm -f "$scratch/restarted.json"
run_helper "$scratch/restarted.json"
test "$(jq -r .instance_id "$scratch/restarted.json")" = "$first_instance"
test "$(jq -r .daemon_generation "$scratch/restarted.json")" != "$first_generation"
