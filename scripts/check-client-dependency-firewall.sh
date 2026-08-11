#!/bin/sh

set -eu

for client_package in sift-client-sdk sift-ui sift-workspace-ui sift-desktop; do
  forbidden="$({ cargo tree -p "$client_package" --prefix none; } | sed -n \
    -e '/^sift-metadata /p' \
    -e '/^sift-server /p' \
    -e '/^sift-driver-api /p' \
    -e '/^sift-driver-postgres /p' \
    -e '/^sift-driver-sqlserver /p')"

  if [ -n "$forbidden" ]; then
    echo "client dependency firewall failed for $client_package:" >&2
    echo "$forbidden" >&2
    exit 1
  fi
done

echo "client dependency firewall: clean"
