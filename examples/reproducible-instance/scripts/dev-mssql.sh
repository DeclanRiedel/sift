#!/usr/bin/env sh
#
# Local SQL Server container helper for live-mssql tests and demos.
#
# Source of truth for the SA password is $ENV_FILE (default .env in the
# repo root). If it isn't set there yet, we generate one and write it in
# on first `start`. The container is always launched with the value
# read from that file, so `.env` and the container never drift.
#
# Container data is kept in a named docker volume so `stop` preserves
# it; use `reset` to nuke everything and start over.
#
# Usage:
#   scripts/dev-mssql.sh start      # ensure pw + start container
#   scripts/dev-mssql.sh stop       # stop + remove container (keep data)
#   scripts/dev-mssql.sh reset      # stop + remove + wipe volume + start fresh
#   scripts/dev-mssql.sh password   # print current SA password
#   scripts/dev-mssql.sh status     # running | stopped | not created

set -eu

CMD="${1:-start}"
ENV_FILE="${SIFT_ENV_FILE:-$PWD/.env}"
CONTAINER="${SIFT_MSSQL_CONTAINER:-sift-mssql}"
VOLUME="${SIFT_MSSQL_VOLUME:-sift-mssql-data}"
IMAGE="${SIFT_MSSQL_IMAGE:-mcr.microsoft.com/mssql/server:2022-latest}"
HOST_PORT="${SIFT_MSSQL_PORT:-1433}"

require_docker() {
  if ! command -v docker >/dev/null 2>&1; then
    echo "sift-mssql: docker not found on PATH" >&2
    exit 1
  fi
}

require_env_file() {
  if [ ! -f "$ENV_FILE" ]; then
    echo "sift-mssql: $ENV_FILE not found — copy .env.example and try again" >&2
    exit 1
  fi
}

current_pw() {
  # Take the last *uncommented* assignment. Blank if only commented lines exist.
  grep -E '^SIFT_MSSQL_PASSWORD=' "$ENV_FILE" 2>/dev/null \
    | tail -1 | sed -E 's/^SIFT_MSSQL_PASSWORD=//' || true
}

gen_pw() {
  # Generate a random password that satisfies MSSQL's default policy:
  # 8+ chars, upper + lower + digit + non-alphanumeric symbol.
  # Filter base64 output to avoid characters that need shell escaping
  # inside `.env` (=, /, +, whitespace), then append !Aa1 to guarantee
  # all four character classes.
  base=$(openssl rand -base64 24 | tr -d '\n=/+' | head -c 20)
  printf '%s!Aa1\n' "$base"
}

write_pw() {
  new="$1"
  tmp="$(mktemp)"
  if grep -qE '^#? *SIFT_MSSQL_PASSWORD=' "$ENV_FILE"; then
    awk -v pw="$new" '
      /^#? *SIFT_MSSQL_PASSWORD=/ { print "SIFT_MSSQL_PASSWORD=" pw; next }
      { print }
    ' "$ENV_FILE" > "$tmp"
    mv "$tmp" "$ENV_FILE"
  else
    printf '\nSIFT_MSSQL_PASSWORD=%s\n' "$new" >> "$ENV_FILE"
  fi
  chmod 600 "$ENV_FILE"
}

ensure_pw() {
  pw="$(current_pw)"
  if [ -z "$pw" ]; then
    pw="$(gen_pw)"
    write_pw "$pw"
    echo "sift-mssql: generated new SA password in $ENV_FILE" >&2
  fi
  printf '%s\n' "$pw"
}

wipe_pw() {
  if grep -qE '^SIFT_MSSQL_PASSWORD=' "$ENV_FILE"; then
    tmp="$(mktemp)"
    awk '
      /^SIFT_MSSQL_PASSWORD=/ { print "# SIFT_MSSQL_PASSWORD="; next }
      { print }
    ' "$ENV_FILE" > "$tmp"
    mv "$tmp" "$ENV_FILE"
    chmod 600 "$ENV_FILE"
  fi
}

container_state() {
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -qx "$CONTAINER"; then
    echo running
  elif docker ps -a --format '{{.Names}}' 2>/dev/null | grep -qx "$CONTAINER"; then
    echo stopped
  else
    echo missing
  fi
}

start_container() {
  pw="$1"
  case "$(container_state)" in
    running)
      echo "sift-mssql: container already running on 127.0.0.1:${HOST_PORT}" >&2
      ;;
    stopped)
      docker start "$CONTAINER" >/dev/null
      echo "sift-mssql: restarted existing container on 127.0.0.1:${HOST_PORT}" >&2
      ;;
    missing)
      docker run -d --name "$CONTAINER" \
        -e "ACCEPT_EULA=Y" \
        -e "MSSQL_SA_PASSWORD=$pw" \
        -v "$VOLUME:/var/opt/mssql" \
        -p "127.0.0.1:${HOST_PORT}:1433" \
        "$IMAGE" >/dev/null
      echo "sift-mssql: created container on 127.0.0.1:${HOST_PORT}" >&2
      ;;
  esac
}

case "$CMD" in
  start)
    require_docker
    require_env_file
    pw="$(ensure_pw)"
    start_container "$pw"
    ;;
  stop)
    require_docker
    docker stop "$CONTAINER" >/dev/null 2>&1 || true
    docker rm "$CONTAINER" >/dev/null 2>&1 || true
    echo "sift-mssql: stopped and removed container (volume $VOLUME preserved)" >&2
    ;;
  reset)
    require_docker
    require_env_file
    docker stop "$CONTAINER" >/dev/null 2>&1 || true
    docker rm "$CONTAINER" >/dev/null 2>&1 || true
    docker volume rm "$VOLUME" >/dev/null 2>&1 || true
    wipe_pw
    pw="$(ensure_pw)"
    start_container "$pw"
    echo "sift-mssql: reset complete" >&2
    ;;
  password)
    require_env_file
    ensure_pw
    ;;
  status)
    if command -v docker >/dev/null 2>&1; then
      state="$(container_state)"
    else
      state="docker-missing"
    fi
    echo "container: $state"
    if [ -f "$ENV_FILE" ]; then
      pw="$(current_pw)"
      if [ -n "$pw" ]; then
        echo "password: set in $ENV_FILE (${#pw} chars)"
      else
        echo "password: not set in $ENV_FILE"
      fi
    else
      echo "env file:  $ENV_FILE missing"
    fi
    ;;
  *)
    echo "usage: $0 {start|stop|reset|password|status}" >&2
    exit 1
    ;;
esac
