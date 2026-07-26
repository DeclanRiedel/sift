#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 7 ]]; then
  echo "usage: $0 <dist-dir> <channel> <sequence> <version> <origin> <published-at> <expires-at>" >&2
  exit 2
fi

dist_dir=$1
channel=$2
sequence=$3
version=${4#v}
origin=${5%/}
published_at=$6
expires_at=$7

[[ $channel =~ ^[A-Za-z0-9_-]+$ ]] || {
  echo "unsafe release channel" >&2
  exit 2
}
[[ $sequence =~ ^[1-9][0-9]*$ ]] || {
  echo "release sequence must be a positive integer" >&2
  exit 2
}
[[ $origin == https://* ]] || {
  echo "release origin must be explicitly configured HTTPS" >&2
  exit 2
}

targets='[]'
found=0
for artifact in "$dist_dir"/sift-server-*; do
  [[ -f $artifact ]] || continue
  found=1
  filename=${artifact##*/}
  target=${filename#sift-server-}
  length=$(stat -c '%s' "$artifact")
  digest=$(sha256sum "$artifact" | awk '{print $1}')
  artifact_url="$origin/$version/$filename"
  targets=$(jq \
    --arg target "$target" \
    --arg artifact_url "$artifact_url" \
    --argjson byte_length "$length" \
    --arg sha256 "$digest" \
    '. + [{
      target: $target,
      artifact_url: $artifact_url,
      byte_length: $byte_length,
      sha256: $sha256,
      archive_format: "raw",
      executable_path: "sift-server"
    }]' <<<"$targets")
done
[[ $found -eq 1 ]] || {
  echo "no sift-server target artifacts found" >&2
  exit 1
}

jq -n \
  --arg channel "$channel" \
  --argjson sequence "$sequence" \
  --arg release_version "$version" \
  --arg published_at "$published_at" \
  --arg expires_at "$expires_at" \
  --arg minimum_updater_version "$version" \
  --argjson targets "$targets" \
  '{
    schema_version: 1,
    channel: $channel,
    sequence: $sequence,
    release_version: $release_version,
    published_at: $published_at,
    expires_at: $expires_at,
    minimum_updater_version: $minimum_updater_version,
    protocol: {minimum: 1, maximum: 1},
    targets: $targets
  }' >"$dist_dir/manifest.json"
