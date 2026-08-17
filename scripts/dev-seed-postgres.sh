#!/usr/bin/env sh
# Start (and if needed, create) the local demo Postgres instance and seed it
# with the `lab` schema every sift demo command expects.
#
# Idempotent: safe to run against an already-initialised, already-running
# cluster. Prints nothing on stdout except the port, so callers can capture it.
#
# Environment:
#   SIFT_DEMO_PGDATA           data dir            (default /tmp/sift-demo-pg)
#   SIFT_DEMO_PG_LOG           postmaster log      (default /tmp/sift-demo-pg.log)
#   SIFT_DEMO_PG_PORT          TCP port            (default 5433)
#   SIFT_DEMO_PG_SOCKET_DIR    unix socket dir     (default /tmp/sift-demo-pg-socket)
set -eu

pgdata="${SIFT_DEMO_PGDATA:-/tmp/sift-demo-pg}"
pglog="${SIFT_DEMO_PG_LOG:-/tmp/sift-demo-pg.log}"
pgport="${SIFT_DEMO_PG_PORT:-5433}"
pgsocket="${SIFT_DEMO_PG_SOCKET_DIR:-/tmp/sift-demo-pg-socket}"

mkdir -p "$pgsocket"

if [ ! -f "$pgdata/PG_VERSION" ]; then
  rm -rf "$pgdata"
  initdb -D "$pgdata" -U sift --auth=trust --no-locale --encoding=UTF8 >&2
  {
    echo "listen_addresses = '127.0.0.1'"
    echo "port = $pgport"
    echo "unix_socket_directories = '$pgsocket'"
    # Small demo cluster: do not reserve production-shaped memory on a laptop.
    echo "shared_buffers = '64MB'"
    echo "max_connections = 40"
  } >> "$pgdata/postgresql.conf"
fi
if ! grep -q "unix_socket_directories = '$pgsocket'" "$pgdata/postgresql.conf"; then
  echo "unix_socket_directories = '$pgsocket'" >> "$pgdata/postgresql.conf"
fi

if ! pg_ctl -D "$pgdata" status >/dev/null 2>&1; then
  pg_ctl -D "$pgdata" -l "$pglog" -w start >&2
fi

createdb -h 127.0.0.1 -p "$pgport" -U sift sifttest 2>/dev/null || true
psql -q -h 127.0.0.1 -p "$pgport" -U sift -d sifttest >&2 <<'SQL'
SET client_min_messages = warning;
CREATE SCHEMA IF NOT EXISTS lab;
CREATE TABLE IF NOT EXISTS lab.people (
  id integer PRIMARY KEY,
  name text NOT NULL,
  role text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO lab.people (id, name, role) VALUES
  (1, 'Ada', 'engineer'),
  (2, 'Grace', 'analyst'),
  (3, 'Linus', 'operator')
ON CONFLICT (id) DO UPDATE
  SET name = EXCLUDED.name,
      role = EXCLUDED.role;
SQL

printf '%s\n' "$pgport"
