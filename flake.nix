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
          # GPUI Linux build/runtime surface (X11 + Wayland + Vulkan).
          fontconfig
          freetype
          libxkbcommon
          vulkan-loader
          mesa                  # Vulkan ICDs + libgbm/libdrm for the GPUI renderer
          wayland
          libx11
          libxcb
        ];

        # GPUI renders through Vulkan. The dev shell replaces LD_LIBRARY_PATH,
        # so on a non-NixOS host the system ICD JSONs under /usr/share/vulkan
        # still resolve but their relative `libvulkan_*.so` no longer do:
        # `vkCreateInstance: Found no drivers!`, surfacing in the client as
        # "Failed to create surface for any enabled backend". Point the loader
        # at Nix's mesa ICDs, whose JSONs carry absolute store paths.
        vulkanIcdNames =
          if pkgs.stdenv.hostPlatform.isx86_64 then [
            "intel_icd.x86_64.json"
            "intel_hasvk_icd.x86_64.json"
            "radeon_icd.x86_64.json"
            "nouveau_icd.x86_64.json"
            "virtio_icd.x86_64.json"
            "lvp_icd.x86_64.json"       # software fallback, keep last
          ] else [
            "panfrost_icd.aarch64.json"
            "freedreno_icd.aarch64.json"
            "broadcom_icd.aarch64.json"
            "asahi_icd.aarch64.json"
            "nouveau_icd.aarch64.json"
            "lvp_icd.aarch64.json"
          ];
        vulkanIcdFilenames = pkgs.lib.concatMapStringsSep ":"
          (name: "${pkgs.mesa}/share/vulkan/icd.d/${name}")
          vulkanIcdNames;

        # Rust + adjacent dev tooling.
        rustDeps = with pkgs; [
          rustToolchain
          rust-analyzer
          mold                  # linker: far less RAM + wall time than bfd/gold
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
          if [ -n "''${IN_NIX_SHELL:-}" ]; then
            exec ${command} "$@"
          fi
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
          runtimeInputs = with pkgs; [ curl nodejs nix ];
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

            ready=0
            for _ in $(seq 1 "''${SIFT_BACKEND_LAB_READY_TRIES:-480}"); do
              if curl -fsS "http://''${SIFT_BIND:-127.0.0.1:3000}/v1/health" >/dev/null 2>&1; then
                ready=1
                break
              fi
              if ! kill -0 "$backend_pid" >/dev/null 2>&1; then
                echo "sift backend exited before becoming ready. Log follows:" >&2
                sed -n '1,240p' "$backend_log" >&2
                exit 1
              fi
              sleep 0.25
            done
            if [ "$ready" != 1 ]; then
              echo "sift backend was not ready before timeout. Log follows:" >&2
              sed -n '1,240p' "$backend_log" >&2
              exit 1
            fi

            cd "$lab"
            if [ ! -d node_modules ]; then
              npm ci
            fi

            if [ "$#" -eq 0 ]; then
              set -- --host 0.0.0.0 --port 5177
            fi

            echo "Backend log: $backend_log"
            echo "Lab UI: http://0.0.0.0:5177 (open this host's IP or forwarded URL)"
            exec ./node_modules/.bin/vite "$@"
          '';
        };

        demoPostgres = pkgs.writeShellApplication {
          name = "sift-demo-postgres";
          runtimeInputs = with pkgs; [ curl jq nodejs nix postgresql ];
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

            pgdata="''${SIFT_DEMO_PGDATA:-/tmp/sift-demo-pg}"
            pglog="''${SIFT_DEMO_PG_LOG:-/tmp/sift-demo-pg.log}"
            backend_log="''${SIFT_BACKEND_LAB_BACKEND_LOG:-/tmp/sift-backend-lab-backend.log}"
            bind="''${SIFT_BIND:-127.0.0.1:3000}"
            base_url="http://$bind"

            pgport="$(sh "$repo/examples/reproducible-instance/scripts/dev-seed-postgres.sh")"

            cd "$repo"
            # Real (non-mock) backend: a fresh metadata database must be migrated first.
            nix develop "$repo" --command cargo run -q -p sift-server -- migrate apply
            nix develop "$repo" --command env SIFT_BIND="$bind" cargo run -p sift-server -- >"$backend_log" 2>&1 &
            backend_pid=$!
            cleanup() {
              kill "$backend_pid" >/dev/null 2>&1 || true
              wait "$backend_pid" >/dev/null 2>&1 || true
              pg_ctl -D "$pgdata" -m fast -w stop >/dev/null 2>&1 || true
            }
            trap cleanup EXIT

            ready=0
            for _ in $(seq 1 "''${SIFT_BACKEND_LAB_READY_TRIES:-480}"); do
              if curl -fsS "$base_url/v1/health" >/dev/null 2>&1; then
                ready=1
                break
              fi
              if ! kill -0 "$backend_pid" >/dev/null 2>&1; then
                echo "sift backend exited before becoming ready. Log follows:" >&2
                sed -n '1,240p' "$backend_log" >&2
                exit 1
              fi
              sleep 0.25
            done
            if [ "$ready" != 1 ]; then
              echo "sift backend was not ready before timeout. Log follows:" >&2
              sed -n '1,240p' "$backend_log" >&2
              exit 1
            fi

            profile_id="$(sh "$repo/examples/reproducible-instance/scripts/dev-register-demo-connection.sh" "$base_url" "$pgport")"

            cd "$lab"
            if [ ! -d node_modules ]; then
              npm ci
            fi

            echo "Postgres: host=127.0.0.1 port=$pgport db=sifttest user=sift password=<empty> ssl=disable"
            echo "Sift connection: Demo Postgres (profile $profile_id)"
            echo "Seeded query: SELECT * FROM lab.order_summary ORDER BY placed_at DESC;"
            echo "Large result query: SELECT * FROM lab.large ORDER BY id;"
            echo "Backend log: $backend_log"
            echo "Postgres log: $pglog"
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

        desktop = pkgs.writeShellApplication {
          name = "sift-desktop";
          runtimeInputs = [ pkgs.nix ];
          text = devCommand ''
            cargo build --profile release-dev -p sift-server --bin sift-launcher
            cargo run --profile release-dev -p sift-desktop -- "$@"
          '';
        };

        # Seeded end-to-end desktop demo: local Postgres with a relational `lab` dataset, a
        # reproducible Sift instance root, and the GPUI client supervising the
        # real (non-mock) server from that root.
        desktopDemo = pkgs.writeShellApplication {
          name = "sift-desktop-demo";
          runtimeInputs = with pkgs; [ gnused jq nix openssl postgresql util-linux ];
          text = ''
            set -euo pipefail

            repo="''${SIFT_REPO:-$PWD}"
            if [ ! -f "$repo/flake.nix" ] || [ ! -f "$repo/Cargo.toml" ]; then
              echo "Run this from the sift checkout, or set SIFT_REPO=/path/to/sift." >&2
              exit 1
            fi

            pgdata="''${SIFT_DEMO_PGDATA:-/tmp/sift-demo-pg}"
            pglog="''${SIFT_DEMO_PG_LOG:-/tmp/sift-demo-pg.log}"
            temporary_instance=0
            if [ -n "''${SIFT_DESKTOP_DEMO_INSTANCE_ROOT:-}" ]; then
              instance_root="$SIFT_DESKTOP_DEMO_INSTANCE_ROOT"
              mkdir -p "$instance_root"
            else
              instance_root="$(mktemp -d "''${TMPDIR:-/tmp}/sift-desktop-demo-instance.XXXXXX")"
              temporary_instance=1
            fi
            manifest_id="$(uuidgen)"
            instance_state="''${XDG_STATE_HOME:-$HOME/.local/state}/sift/instances/$manifest_id"

            run_in_dev() {
              if [ -n "''${IN_NIX_SHELL:-}" ]; then
                "$@"
              else
                nix develop "$repo" --command "$@"
              fi
            }

            pgport="$(SIFT_DEMO_RESET=1 sh "$repo/examples/reproducible-instance/scripts/dev-seed-postgres.sh")"

            cd "$repo"
            # Build first, in the foreground: a cold GPUI build takes minutes,
            # and the desktop must be able to resolve its sibling server binary.
            run_in_dev cargo build --profile release-dev -p sift-server -p sift-desktop

            cleanup() {
              if [ "''${SIFT_DEMO_KEEP_POSTGRES:-0}" != "1" ]; then
                pg_ctl -D "$pgdata" -m fast -w stop >/dev/null 2>&1 || true
              fi
              rm -rf -- "$instance_state"
              if [ "$temporary_instance" = "1" ]; then
                rm -rf -- "$instance_root"
              fi
            }
            trap cleanup EXIT

            cp "$repo/examples/reproducible-instance/sift.toml" "$instance_root/sift.toml"
            rm -f -- "$instance_root/sift.lock"
            sed -i \
              -e "s/b654b918-b1f1-4d70-924d-e4c1014f482f/$manifest_id/" \
              -e 's/name = "demo-sift"/name = "desktop-demo"/' \
              -e "s|127.0.0.1:5432/postgres|127.0.0.1:$pgport/sifttest|" \
              "$instance_root/sift.toml"

            # The demo credential is random and destination-local. It never
            # enters sift.toml, sift.lock, process arguments, or repository state.
            db_password="$(openssl rand -hex 24)"
            printf "ALTER ROLE sift PASSWORD '%s';\n" "$db_password" | \
              psql -q -h 127.0.0.1 -p "$pgport" -U sift -d sifttest

            run_in_dev cargo run -q --profile release-dev -p sift-server --bin sift -- instance lock "$instance_root"
            run_in_dev cargo run -q --profile release-dev -p sift-server --bin sift -- instance apply "$instance_root"
            if grep -q 'credential = "credential:demo/postgres/shared"' "$instance_root/sift.toml"; then
              printf '%s\n' "$db_password" | jq -cnR '{password: input}' | \
                run_in_dev cargo run -q --profile release-dev -p sift-server --bin sift -- \
                  instance credentials import "$instance_root" \
                  --slot credential:demo/postgres/shared --stdin
            fi

            echo "Postgres: host=127.0.0.1 port=$pgport db=sifttest user=sift credential=<destination-local> ssl=prefer"
            echo "Sift instance root: $instance_root"
            echo "Sift connection: demo/postgres (managed by sift.toml)"
            echo "Seeded query: SELECT * FROM lab.order_summary ORDER BY placed_at DESC;"
            echo "Large result query: SELECT * FROM lab.large ORDER BY id;"
            echo "Postgres log: $pglog"
            echo "Desktop: supervising the applied auto-loopback instance"
            echo "The desktop can edit this run's sift.toml through the current-instance API."

            run_in_dev cargo run --profile release-dev -p sift-desktop -- --instance-root "$instance_root" "$@"
          '';
        };

        desktopDemoWiki = pkgs.writeShellApplication {
          name = "sift-desktop-demo-wiki";
          runtimeInputs = [ pkgs.python3 ];
          text = ''
            set -euo pipefail

            repo="''${SIFT_REPO:-$PWD}"
            wiki="$repo/docs/keyboard-wiki"
            if [ ! -f "$repo/flake.nix" ] || [ ! -f "$wiki/index.html" ]; then
              echo "Run this from the sift checkout, or set SIFT_REPO=/path/to/sift." >&2
              exit 1
            fi

            bind="''${SIFT_DESKTOP_DEMO_WIKI_BIND:-127.0.0.1}"
            port="''${SIFT_DESKTOP_DEMO_WIKI_PORT:-8787}"
            echo "Sift keyboard wiki: http://$bind:$port"
            echo "Run the desktop demo in another terminal: nix run .#desktop-demo"
            exec python3 -m http.server "$port" --bind "$bind" --directory "$wiki"
          '';
        };

        devSecretKey = pkgs.writeShellApplication {
          name = "sift-dev-secret-key";
          runtimeInputs = with pkgs; [ coreutils openssl ];
          text = devCommand ''sh examples/reproducible-instance/scripts/dev-secret-key.sh'';
        };

        devMssql = pkgs.writeShellApplication {
          name = "sift-dev-mssql";
          # docker is intentionally not in runtimeInputs — the container
          # runtime is the user's host docker, not a nix-managed pkg.
          # The script checks for docker on PATH and errors out cleanly
          # if it's missing.
          runtimeInputs = with pkgs; [ coreutils openssl gawk ];
          text = devCommand ''sh examples/reproducible-instance/scripts/dev-mssql.sh "$@"'';
        };

        siftHelp = pkgs.writeShellApplication {
          name = "sift-help";
          text = ''
            set -euo pipefail

            cat <<'EOF'
            Sift commands available after `nix develop`:

              sift-help                 Show this TLDR.
              sift-server               Run sift-server with normal configured drivers.
              sift-server-mock          Run sift-server with the mock Postgres driver enabled.
              sift-backend-lab          Run the browser backend lab UI from .labs/sift-backend-lab.
              sift-backend-lab-backend  Run the backend for the lab on SIFT_BIND, mock mode by default.
              sift-backend-lab-stack    Run mock backend + network-bound lab UI together.
              sift-demo-postgres        Run seeded Postgres + backend + registered demo connection + lab UI.
              sift-health               Curl /v1/health from the configured backend and pretty-print JSON.
              sift-smoke                Start a mock backend and exercise health/session/connection/schema/audit.
              sift-test                 Run cargo nextest for the whole workspace.
              sift-check                Run cargo check for the whole workspace.
              sift-desktop              Run the native GPUI desktop client.
              sift-desktop-demo         Seeded Postgres + real backend + registered connection + desktop.
              sift-desktop-demo-wiki    Serve the keyboard-language wiki beside the desktop demo.
              sift-dev-secret-key       Generate the ignored local metadata secret key file.
              sift-dev-mssql            Manage a local SQL Server docker container for live-mssql tests.
                                        Sub: start | stop | reset | password | status. Password is
                                        generated and persisted to .env on first start.

            Typical flow:
              nix develop
              sift-help
              sift-backend-lab-stack

            Desktop UI flow:
              nix develop
              sift-desktop-demo            Opens the client on a seeded relational `lab` dataset.

            Environment:
              SIFT_REPO=/path/to/sift      Override checkout path for commands that need it.
              SIFT_BIND=127.0.0.1:3000     Override backend bind address where supported.
              SIFT_DEMO_PG_PORT=5433       Port for the seeded demo Postgres.
              SIFT_DESKTOP_DEMO_INSTANCE_ROOT=/path
                                            Override the reproducible demo root.
              SIFT_DEMO_KEEP_POSTGRES=1    Leave the demo cluster running after the desktop exits.
              .env.example                 Template for local env vars; never commit .env.
              sift.example.toml            Template for local sift.toml; never commit sift.toml.
            EOF
          '';
        };
      in
      {
        devShells.default = pkgs.mkShell {
          inherit nativeBuildInputs buildInputs;
          packages = rustDeps ++ [
            siftHelp
            server
            serverMock
            backendLab
            backendLabBackend
            backendLabStack
            demoPostgres
            health
            smoke
            test
            check
            desktop
            desktopDemo
            desktopDemoWiki
            devSecretKey
            devMssql
          ];

          # Point rust-analyzer + cargo at the right std sources.
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          RUST_BACKTRACE = "1";
          RUST_LOG = "sift=debug,info";

          # GPUI's Linux backend dlopens wayland-client/vulkan/etc at runtime,
          # and NixOS has no FHS /usr/lib. Expose every buildInput's lib dir
          # so `cargo run -p sift-desktop` can resolve them via dlopen.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath buildInputs;

          # Both spellings: the loader reads VK_DRIVER_FILES and falls back to
          # the deprecated VK_ICD_FILENAMES depending on its version.
          VK_DRIVER_FILES = vulkanIcdFilenames;
          VK_ICD_FILENAMES = vulkanIcdFilenames;

          # Keep sccache inside the repo so it survives GC and is machine-local.
          SCCACHE_DIR = "${toString ./.}/.cache/sccache";

          # Link with mold. Linking GPUI + server with the default linker is the
          # single largest memory spike in a build; mold cuts both peak RSS and
          # link time by a wide margin. Job count and debuginfo limits live in
          # the committed .cargo/config.toml so non-Nix hosts get them too.
          RUSTFLAGS = "-C link-arg=-fuse-ld=mold";

          # Generate a local dev keyfile for the encrypted-file secret backend
          # and export its path. Selecting the backend stays opt-in.
          shellHook = ''
            keyfile="$(sh "$PWD/examples/reproducible-instance/scripts/dev-secret-key.sh" "''${SIFT_METADATA__SECRET_KEY_FILE:-$PWD/.sift/dev-secret.key}")"
            export SIFT_METADATA__SECRET_KEY_FILE="$keyfile"
          '';
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
          demo-postgres = {
            type = "app";
            program = "${demoPostgres}/bin/sift-demo-postgres";
          };
          desktop = {
            type = "app";
            program = "${desktop}/bin/sift-desktop";
          };
          desktop-demo = {
            type = "app";
            program = "${desktopDemo}/bin/sift-desktop-demo";
          };
          sift-desktop-demo-wiki = {
            type = "app";
            program = "${desktopDemoWiki}/bin/sift-desktop-demo-wiki";
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
          dev-secret-key = {
            type = "app";
            program = "${devSecretKey}/bin/sift-dev-secret-key";
          };
          dev-mssql = {
            type = "app";
            program = "${devMssql}/bin/sift-dev-mssql";
          };
        };
      });
}
