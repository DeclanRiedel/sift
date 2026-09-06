#!/bin/sh
# Create the demo file once; repeated launches preserve changes made in Sift.
set -eu
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)
file=${1:-"$repo/examples/reproducible-instance/demo-data/demo.db"}
mkdir -p -- "$(dirname -- "$file")"
if [ -e "$file" ]; then
    echo "Using existing SQLite demo: $file" >&2
    exit 0
fi
seed_tmp=$(mktemp "$(dirname -- "$file")/.sift-sqlite-seed.XXXXXX")
trap 'rm -f -- "$seed_tmp"' EXIT HUP INT TERM
sqlite3 -bail "$seed_tmp" < "$repo/examples/reproducible-instance/sql/sqlite-demo.sql"
chmod 600 "$seed_tmp"
# Do not overwrite a file created by a concurrent launcher.
mv -n -- "$seed_tmp" "$file"
echo "SQLite demo ready: $file" >&2
