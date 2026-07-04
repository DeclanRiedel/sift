{
  description = "sift — database IDE (Rust end-to-end)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self
    , nixpkgs
    , rust-overlay
    , flake-utils
    , ...
    }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Single source of truth: rust-overlay reads ./rust-toolchain.toml.
        # Same file is also honoured by rustup on non-Nix machines / CI.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # Native libs required to build Rust crates (openssl-sys, pq-sys, ...).
        nativeBuildInputs = with pkgs; [
          pkg-config
        ];

        buildInputs = with pkgs; [
          openssl
          postgresql.lib        # libpq headers — needed by tokio-postgres / sqlx at build time
          postgresql            # psql client + ability to run a local dev instance
        ];

        # Rust + adjacent dev tooling.
        rustDeps = with pkgs; [
          rustToolchain
          rust-analyzer
          sccache               # shared compile cache across checkouts / machines
          cargo-nextest         # faster, better test runner
          cargo-watch           # auto-rebuild on save
          cargo-deny            # advisories + license gates
          cargo-edit            # cargo upgrade / set-version
          just                  # task runner (justfile to come)
        ];

        devCommand = command: ''
          set -euo pipefail

          repo="''${SIFT_REPO:-$PWD}"
          if [ ! -f "$repo/flake.nix" ] || [ ! -f "$repo/Cargo.toml" ]; then
            echo "Run this from the sift checkout, or set SIFT_REPO=/path/to/sift." >&2
            exit 1
          fi

          cd "$repo"
          exec nix develop "$repo" --command ${command} "$@"
        '';

        server = pkgs.writeShellApplication {
          name = "sift-server";
          runtimeInputs = [ pkgs.nix ];
          text = devCommand ''cargo run -p sift-server --'';
        };

        serverMock = pkgs.writeShellApplication {
          name = "sift-server-mock";
          runtimeInputs = [ pkgs.nix ];
          text = devCommand ''env SIFT_DRIVERS__MOCK=true cargo run -p sift-server --'';
        };

        backendLab = pkgs.writeShellApplication {
          name = "sift-backend-lab";
          runtimeInputs = with pkgs; [ nodejs ];
          text = ''
            set -euo pipefail

            repo="''${SIFT_REPO:-$PWD}"
            lab="$repo/.labs/sift-backend-lab"
            if [ ! -f "$lab/package.json" ]; then
              echo "Missing $lab. Clone it with:" >&2
              echo "  git clone git@github.com:DeclanRiedel/sift-backend-lab.git $lab" >&2
              exit 1
            fi

            cd "$lab"
            if [ ! -d node_modules ]; then
              npm ci
            fi
            exec npm run dev -- "$@"
          '';
        };

        backendLabBackend = pkgs.writeShellApplication {
          name = "sift-backend-lab-backend";
          runtimeInputs = [ pkgs.nix ];
          text = ''
            set -euo pipefail

            repo="''${SIFT_REPO:-$PWD}"
            if [ ! -f "$repo/flake.nix" ] || [ ! -f "$repo/Cargo.toml" ]; then
              echo "Run this from the sift checkout, or set SIFT_REPO=/path/to/sift." >&2
              exit 1
            fi

            cd "$repo"
            if [ "''${SIFT_BACKEND_LAB_MOCK:-1}" = "1" ]; then
              exec nix develop "$repo" --command env SIFT_BIND="''${SIFT_BIND:-127.0.0.1:3000}" SIFT_DRIVERS__MOCK=true cargo run -p sift-server --
            fi
            exec nix develop "$repo" --command env SIFT_BIND="''${SIFT_BIND:-127.0.0.1:3000}" cargo run -p sift-server --
          '';
        };

        backendLabStack = pkgs.writeShellApplication {
          name = "sift-backend-lab-stack";
          runtimeInputs = with pkgs; [ nodejs nix ];
          text = ''
            set -euo pipefail

            repo="''${SIFT_REPO:-$PWD}"
            lab="$repo/.labs/sift-backend-lab"
            if [ ! -f "$repo/flake.nix" ] || [ ! -f "$repo/Cargo.toml" ]; then
              echo "Run this from the sift checkout, or set SIFT_REPO=/path/to/sift." >&2
              exit 1
            fi
            if [ ! -f "$lab/package.json" ]; then
              echo "Missing $lab. Clone it with:" >&2
              echo "  git clone git@github.com:DeclanRiedel/sift-backend-lab.git $lab" >&2
              exit 1
            fi

            cd "$repo"
            backend_log="''${SIFT_BACKEND_LAB_BACKEND_LOG:-/tmp/sift-backend-lab-backend.log}"
            if [ "''${SIFT_BACKEND_LAB_MOCK:-1}" = "1" ]; then
              nix develop "$repo" --command env SIFT_BIND="''${SIFT_BIND:-127.0.0.1:3000}" SIFT_DRIVERS__MOCK=true cargo run -p sift-server -- >"$backend_log" 2>&1 &
            else
              nix develop "$repo" --command env SIFT_BIND="''${SIFT_BIND:-127.0.0.1:3000}" cargo run -p sift-server -- >"$backend_log" 2>&1 &
            fi
            backend_pid=$!
            cleanup() {
              kill "$backend_pid" >/dev/null 2>&1 || true
              wait "$backend_pid" >/dev/null 2>&1 || true
            }
            trap cleanup EXIT

            cd "$lab"
            if [ ! -d node_modules ]; then
              npm ci
            fi

            echo "Backend log: $backend_log"
            echo "Lab UI: http://127.0.0.1:5177"
            exec npm run dev -- "$@"
          '';
        };

        smoke = pkgs.writeShellApplication {
          name = "sift-smoke";
          runtimeInputs = with pkgs; [ curl jq nix ];
          text = ''
            set -euo pipefail

            repo="''${SIFT_REPO:-$PWD}"
            if [ ! -f "$repo/flake.nix" ] || [ ! -f "$repo/Cargo.toml" ]; then
              echo "Run this from the sift checkout, or set SIFT_REPO=/path/to/sift." >&2
              exit 1
            fi

            cd "$repo"
            bind="''${SIFT_BIND:-127.0.0.1:7474}"
            base_url="http://$bind"
            log_file="''${SIFT_SMOKE_LOG:-/tmp/sift-smoke-server.log}"

            env SIFT_DRIVERS__MOCK=true SIFT_BIND="$bind" \
              nix develop "$repo" --command cargo run -p sift-server -- >"$log_file" 2>&1 &
            server_pid=$!
            cleanup() {
              kill "$server_pid" >/dev/null 2>&1 || true
              wait "$server_pid" >/dev/null 2>&1 || true
            }
            trap cleanup EXIT

            ready=0
            for _ in $(seq 1 "''${SIFT_SMOKE_READY_TRIES:-480}"); do
              if curl -fsS "$base_url/v1/health" >/dev/null 2>&1; then
                ready=1
                break
              fi
              if ! kill -0 "$server_pid" >/dev/null 2>&1; then
                echo "sift-server exited before becoming ready. Log follows:" >&2
                sed -n '1,240p' "$log_file" >&2
                exit 1
              fi
              sleep 0.25
            done

            if [ "$ready" != 1 ]; then
              echo "sift-server was not ready at $base_url before timeout. Log follows:" >&2
              sed -n '1,240p' "$log_file" >&2
              exit 1
            fi

            echo "health:"
            curl -fsS "$base_url/v1/health" | jq .

            session_id="$(
              curl -fsS -X POST "$base_url/v1/sessions" \
                -H 'content-type: application/json' \
                -d '{"tag":"flake-smoke"}' \
                | jq -r .id
            )"
            echo "session: $session_id"

            connection_id="$(
              curl -fsS -X POST "$base_url/v1/sessions/$session_id/connections" \
                -H 'content-type: application/json' \
                -d '{
                  "engine":"postgres",
                  "spec":{
                    "host":"mock.invalid",
                    "port":5432,
                    "database":"mock",
                    "user":"mock",
                    "password":null,
                    "ssl_mode":"disable",
                    "engine_specific":null
                  }
                }' \
                | jq -r .id
            )"
            echo "connection: $connection_id"

            echo "ping:"
            curl -fsS -X POST "$base_url/v1/sessions/$session_id/connections/$connection_id/ping" | jq .

            echo "schema:"
            curl -fsS "$base_url/v1/sessions/$session_id/connections/$connection_id/schema" | jq .

            echo "audit:"
            curl -fsS "$base_url/v1/audit" | jq .
          '';
        };

        health = pkgs.writeShellApplication {
          name = "sift-health";
          runtimeInputs = with pkgs; [ curl jq ];
          text = ''
            set -euo pipefail
            bind="''${SIFT_BIND:-127.0.0.1:7474}"
            curl -fsS "http://$bind/v1/health" | jq .
          '';
        };

        test = pkgs.writeShellApplication {
          name = "sift-test";
          runtimeInputs = [ pkgs.nix ];
          text = devCommand ''cargo nextest run --workspace'';
        };

        check = pkgs.writeShellApplication {
          name = "sift-check";
          runtimeInputs = [ pkgs.nix ];
          text = devCommand ''cargo check --workspace --all-targets'';
        };
      in
      {
        devShells.default = pkgs.mkShell {
          inherit nativeBuildInputs buildInputs;
          packages = rustDeps;

          # Point rust-analyzer + cargo at the right std sources.
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          RUST_BACKTRACE = "1";
          RUST_LOG = "sift=debug,info";

          # Keep sccache inside the repo so it survives GC and is machine-local.
          SCCACHE_DIR = "${toString ./.}/.cache/sccache";
        };

        apps = {
          default = self.apps.${system}.server-mock;
          server = {
            type = "app";
            program = "${server}/bin/sift-server";
          };
          server-mock = {
            type = "app";
            program = "${serverMock}/bin/sift-server-mock";
          };
          backend-lab = {
            type = "app";
            program = "${backendLab}/bin/sift-backend-lab";
          };
          backend-lab-backend = {
            type = "app";
            program = "${backendLabBackend}/bin/sift-backend-lab-backend";
          };
          backend-lab-stack = {
            type = "app";
            program = "${backendLabStack}/bin/sift-backend-lab-stack";
          };
          health = {
            type = "app";
            program = "${health}/bin/sift-health";
          };
          smoke = {
            type = "app";
            program = "${smoke}/bin/sift-smoke";
          };
          test = {
            type = "app";
            program = "${test}/bin/sift-test";
          };
          check = {
            type = "app";
            program = "${check}/bin/sift-check";
          };
        };
      });
}
