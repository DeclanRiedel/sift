#!/usr/bin/env sh
set -eu

keyfile="${1:-${SIFT_METADATA__SECRET_KEY_FILE:-$PWD/.sift/dev-secret.key}}"

if [ ! -f "$keyfile" ]; then
  mkdir -p "$(dirname "$keyfile")"
  umask 077
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32 > "$keyfile"
  else
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$keyfile"
  fi
  chmod 600 "$keyfile"
  echo "sift: generated dev secret keyfile at $keyfile" >&2
fi

printf '%s\n' "$keyfile"
