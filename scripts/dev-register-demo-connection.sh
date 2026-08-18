#!/usr/bin/env sh
# Register the seeded demo Postgres as a sift connection profile against an
# already-running sift-server, and print the profile id on stdout.
#
# Usage: dev-register-demo-connection.sh <base_url> [pgport] [profile_name]
#
# Idempotent: the named profile is reconciled on every run, so runtime schema
# changes repair an older demo profile instead of silently reusing stale JSON.
set -eu

base_url="${1:?usage: dev-register-demo-connection.sh <base_url> [pgport] [name]}"
pgport="${2:-${SIFT_DEMO_PG_PORT:-5433}}"
name="${3:-Demo Postgres}"
tenant_id="${SIFT_DEMO_TENANT_ID:-1}"

protocol_version="$(
  curl -fsS -X POST "$base_url/v1/handshake" \
    -H 'content-type: application/json' \
    -d '{
      "client_version":"sift-demo-postgres",
      "client_kind":"automation",
      "protocol":{"minimum":1,"maximum":1}
    }' \
    | jq -er .selected_protocol
)"

profile_payload="$(
  jq -n --argjson port "$pgport" --argjson tenant "$tenant_id" --arg name "$name" '{
    tenant_id: $tenant,
    name: $name,
    provider_id: "sift/postgres",
    configuration: {
      host: "127.0.0.1",
      port: $port,
      database: "sifttest",
      user: "sift",
      password: null,
      ssl_mode: "disable",
      engine_specific: {
        engine: "postgres",
        search_path: ["lab"],
        application_name: "sift-demo"
      }
    },
    credential_mode: "shared",
    tags: ["demo", "seeded", "lab.people"]
  }'
)"

curl -fsS -X POST "$base_url/v1/metadata/connections" \
  -H 'content-type: application/json' \
  -H "x-sift-protocol-version: $protocol_version" \
  -d "$profile_payload" \
  | jq -er .id
