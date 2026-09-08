#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# Match the toolchain used to validate the website without installing the
# workspace's desktop/Wasm development components on Cloudflare's build image.
website_rust_version=1.98.1

if command -v rustc >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1 &&
  [[ "$(rustc --version)" == "rustc ${website_rust_version} "* ]]; then
  exec cargo run --locked -p sift-website -- --export target/website
fi

website_cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ ! -x "$website_cargo_bin/rustup" ]]; then
  website_installer=$(mktemp)
  trap 'rm -f "$website_installer"' EXIT
  curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
    https://sh.rustup.rs -o "$website_installer"
  sh "$website_installer" --yes --profile minimal --default-toolchain none --no-modify-path
  rm -f "$website_installer"
  trap - EXIT
fi

"$website_cargo_bin/rustup" toolchain install "$website_rust_version" --profile minimal
exec "$website_cargo_bin/rustup" run "$website_rust_version" \
  cargo run --locked -p sift-website -- --export target/website
