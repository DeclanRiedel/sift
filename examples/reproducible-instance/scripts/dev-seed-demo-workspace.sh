#!/usr/bin/env sh
# Create the demo room/workspace, bind its filesystem projection, and
# initialize the projection as a Git repository through Sift's public API.
#
# Usage: dev-seed-demo-workspace.sh <base_url> <profile_id> [root_handle]
#
# Idempotent: existing named resources and bindings are reused.
set -eu

base_url="${1:?usage: dev-seed-demo-workspace.sh <base_url> <profile_id> [root_handle]}"
profile_id="${2:?usage: dev-seed-demo-workspace.sh <base_url> <profile_id> [root_handle]}"
root_handle="${3:-demo-postgres}"
tenant_id="${SIFT_DEMO_TENANT_ID:-1}"
room_name="${SIFT_DEMO_ROOM_NAME:-Demo Postgres}"
workspace_name="${SIFT_DEMO_WORKSPACE_NAME:-Postgres Lab}"

protocol_version="$(
  curl -fsS -X POST "$base_url/v1/handshake" \
    -H 'content-type: application/json' \
    -d '{
      "client_version":"sift-demo-workspace",
      "client_kind":"automation",
      "protocol":{"minimum":1,"maximum":1}
    }' \
    | jq -er .selected_protocol
)"

api_get() {
  curl -fsS "$base_url$1" -H "x-sift-protocol-version: $protocol_version"
}

api_post() {
  curl -fsS -X POST "$base_url$1" \
    -H 'content-type: application/json' \
    -H "x-sift-protocol-version: $protocol_version" \
    -d "$2"
}

api_put() {
  curl -fsS -X PUT "$base_url$1" \
    -H 'content-type: application/json' \
    -H "x-sift-protocol-version: $protocol_version" \
    -d "$2"
}

rooms="$(api_get "/v1/metadata/rooms?tenant=$tenant_id")"
room_id="$(printf '%s' "$rooms" | jq -er --arg name "$room_name" \
  'first(.[] | select(.name == $name) | .id) // empty' 2>/dev/null || true)"
if [ -z "$room_id" ]; then
  room_payload="$(jq -cn \
    --argjson tenant "$tenant_id" \
    --arg name "$room_name" \
    '{tenant_id:$tenant,name:$name,kind:"shared"}')"
  room_id="$(api_post /v1/metadata/rooms "$room_payload" | jq -er .id)"
fi

# Keep query execution context attached to the seeded database profile.
api_put "/v1/metadata/rooms/$room_id/connection" \
  "$(jq -cn --argjson profile "$profile_id" '{connection_profile_id:$profile}')" \
  >/dev/null

workspaces="$(api_get "/v1/metadata/rooms/$room_id/workspaces")"
workspace_id="$(printf '%s' "$workspaces" | jq -er --arg name "$workspace_name" \
  'first(.[] | select(.name == $name) | .id) // empty' 2>/dev/null || true)"
if [ -z "$workspace_id" ]; then
  workspace_id="$(api_post "/v1/metadata/rooms/$room_id/workspaces" \
    "$(jq -cn --arg name "$workspace_name" '{name:$name}')" | jq -er .id)"
fi

projection="$(api_get "/v1/metadata/workspaces/$workspace_id/projection")"
projection_id="$(printf '%s' "$projection" | jq -er '.id // empty' 2>/dev/null || true)"
if [ -z "$projection_id" ]; then
  projection_id="$(api_post "/v1/metadata/workspaces/$workspace_id/projection" \
    "$(jq -cn --arg root "$root_handle" '{root_handle:$root,mode:"read_write"}')" \
    | jq -er .id)"
fi

repository="$(api_get "/v1/metadata/workspaces/$workspace_id/repository")"
repository_id="$(printf '%s' "$repository" | jq -er '.id // empty' 2>/dev/null || true)"
if [ -z "$repository_id" ]; then
  repository_id="$(api_post "/v1/metadata/workspaces/$workspace_id/repository" \
    "$(jq -cn --argjson projection "$projection_id" \
      '{projection_id:$projection,initialize:true}')" | jq -er .id)"
fi

jq -cn \
  --argjson room_id "$room_id" \
  --argjson workspace_id "$workspace_id" \
  --argjson projection_id "$projection_id" \
  --argjson repository_id "$repository_id" \
  '{room_id:$room_id,workspace_id:$workspace_id,projection_id:$projection_id,repository_id:$repository_id}'
