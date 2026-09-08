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

        desktopRunner = pkgs.writeShellApplication {
          name = "sift-desktop-runner";
          runtimeInputs = [ pkgs.nix ];
          text = devCommand ''
            cargo build --profile release-dev -p sift-server --bins
            cargo run --profile release-dev -p sift-desktop -- "$@"
          '';
        };

        desktopItem = pkgs.makeDesktopItem {
          name = "dev.sift.Sift";
          desktopName = "Sift";
          genericName = "Database IDE";
          comment = "Native database IDE for PostgreSQL and SQL Server";
          exec = "${desktopRunner}/bin/sift-desktop-runner";
          icon = "${./crates/desktop/assets/sift-icon.png}";
          terminal = false;
          categories = [ "Development" "Database" ];
          startupNotify = true;
          startupWMClass = "dev.sift.Sift";
        };

        installDesktopItem = ''
          data_home="''${XDG_DATA_HOME:-$HOME/.local/share}"
          ${pkgs.coreutils}/bin/mkdir -p "$data_home/applications"
          ${pkgs.coreutils}/bin/install -m 0644 \
            "${desktopItem}/share/applications/dev.sift.Sift.desktop" \
            "$data_home/applications/dev.sift.Sift.desktop"
          ${pkgs.desktop-file-utils}/bin/update-desktop-database \
            "$data_home/applications" >/dev/null 2>&1 || true
        '';

        desktop = pkgs.writeShellApplication {
          name = "sift-desktop";
          text = ''
            ${installDesktopItem}
            exec "${desktopRunner}/bin/sift-desktop-runner" "$@"
          '';
        };

        # Seeded end-to-end desktop demo: local Postgres with a relational `lab` dataset, a
        # reproducible Sift instance root, and the GPUI client supervising the
        # real (non-mock) server from that root.
        desktopDemo = pkgs.writeShellApplication {
          name = "sift-desktop-demo";
          runtimeInputs = with pkgs; [ coreutils curl git gnugrep gnused jq nix openssl postgresql sqlite util-linux ];
          text = ''
            set -Eeuo pipefail

            ${installDesktopItem}

            phase_name="launcher validation"
            phase() {
              phase_name="$2"
              echo "[$1/7] $2"
            }
            demo_error() {
              status=$?
              # A failing command substitution is reported by its parent.
              if [ "$BASH_SUBSHELL" -gt 0 ]; then
                exit "$status"
              fi
              echo "Desktop demo failed during: $phase_name (exit $status)." >&2
              echo "Resolve the error above, then rerun the launcher." >&2
              exit "$status"
            }
            trap demo_error ERR

            phase 1 "Validate checkout and reserve the demo"
            repo="''${SIFT_REPO:-$PWD}"
            if [ ! -f "$repo/flake.nix" ] || [ ! -f "$repo/Cargo.toml" ]; then
              echo "Run this from the sift checkout, or set SIFT_REPO=/path/to/sift." >&2
              exit 1
            fi

            lock_file="''${TMPDIR:-/tmp}/sift-desktop-demo-$(id -u).lock"
            if [ "''${SIFT_DESKTOP_DEMO_LOCK_HELD:-0}" != "1" ]; then
              exec 9>>"$lock_file"
              if ! flock -n 9; then
                existing="$(head -n 1 "$lock_file" 2>/dev/null || true)"
                echo "A Sift desktop demo is already running''${existing:+ (launcher PID $existing)}." >&2
                echo "Stop that demo before starting another; this is the demo-process lock, not Cargo's build lock." >&2
                exit 1
              fi
              : >"$lock_file"
              printf '%s\n' "$$" >&9
            fi

            pgdata="''${SIFT_DEMO_PGDATA:-/tmp/sift-demo-pg}"
            pglog="''${SIFT_DEMO_PG_LOG:-/tmp/sift-demo-pg.log}"
            if [ -n "''${SIFT_DESKTOP_DEMO_INSTANCE_ROOT:-}" ]; then
              instance_root="$SIFT_DESKTOP_DEMO_INSTANCE_ROOT"
            else
              instance_root="''${TMPDIR:-/tmp}/sift-desktop-demo-instance-$(id -u)"
            fi
            mkdir -p "$instance_root"
            # Stable identity makes repeated demo launches replace the same
            # desktop inventory entry instead of accumulating throwaway roots.
            manifest_id="''${SIFT_DESKTOP_DEMO_MANIFEST_ID:-d35fd35e-3144-4dc2-98cf-38fb44db851b}"
            instance_state="''${XDG_STATE_HOME:-$HOME/.local/state}/sift/instances/$manifest_id"
            workspace_root="''${SIFT_DESKTOP_DEMO_WORKSPACE_ROOT:-$instance_root/workspace}"
            seed_server_pid=""

            run_in_dev() {
              if [ -n "''${IN_NIX_SHELL:-}" ]; then
                "$@"
              else
                nix develop "$repo" --command "$@"
              fi
            }

            cleanup() {
              if [ -n "$seed_server_pid" ]; then
                kill "$seed_server_pid" >/dev/null 2>&1 || true
                wait "$seed_server_pid" >/dev/null 2>&1 || true
              fi
              if [ "''${SIFT_DEMO_KEEP_POSTGRES:-0}" != "1" ]; then
                pg_ctl -D "$pgdata" -m fast -w stop >/dev/null 2>&1 || true
              fi
              # Keep saved queries, workspaces, and credentials across demo launches.
            }
            trap cleanup EXIT

            phase 2 "Seed demo Postgres and SQL Server"
            pgport="$(SIFT_DEMO_RESET=1 sh "$repo/examples/reproducible-instance/scripts/dev-seed-postgres.sh")"
            mssqlport="$(cd "$repo" && sh examples/reproducible-instance/scripts/dev-mssql.sh seed)"

            cd "$repo"
            # Build first, in the foreground: a cold GPUI build takes minutes,
            # and the desktop must be able to resolve its sibling server binary.
            phase 3 "Build the server and desktop (Cargo may wait for its own build lock)"
            run_in_dev cargo build --profile release-dev -p sift-server -p sift-desktop

            phase 4 "Prepare the reproducible instance"
            sh "$repo/examples/reproducible-instance/scripts/dev-seed-sqlite.sh" "$instance_root/demo-data/demo.db"
            cp "$repo/examples/reproducible-instance/sift.toml" "$instance_root/sift.toml"
            rm -f -- "$instance_root/sift.lock"
            sed -i \
              -e "s/b654b918-b1f1-4d70-924d-e4c1014f482f/$manifest_id/" \
              -e 's/name = "demo-sift"/name = "desktop-demo"/' \
              -e "s|127.0.0.1:5432/postgres|127.0.0.1:$pgport/sifttest|" \
              "$instance_root/sift.toml"
            mkdir -p "$workspace_root/queries"
            printf '%s\n\n%s\n' '# Sift Desktop Demo' 'Git-backed workspace for the seeded lab database.' >"$workspace_root/README.md"
            printf '%s\n' 'SELECT * FROM lab.order_summary ORDER BY placed_at DESC;' >"$workspace_root/queries/order-summary.sql"
            workspace_path_literal="$(jq -Rn --arg value "$workspace_root" '$value')"
            cat >>"$instance_root/sift.toml" <<EOF

[[connections]]
name = "demo/sql-server"
tenant = "demo"
provider = "sql-server"
connection_string = "Server=127.0.0.1,$mssqlport;Database=siftdemo;User Id=sa;Encrypt=true;TrustServerCertificate=true"
credential_mode = "shared"
credential = "credential:demo/sql-server/shared"
tags = ["demo", "local", "sql-server"]

[connections.policy]
allow_sql = true
allow_schema_read = true
allow_export = true

[connections.lifecycle]
prevent_destroy = true

[server.workspaces]
enabled = true

[[server.workspaces.roots]]
handle = "demo-postgres"
path = $workspace_path_literal
read_only = false

[server.vcs]
enabled = true
network_enabled = false
EOF

            # The demo credential is random and destination-local. It never
            # enters sift.toml, sift.lock, process arguments, or repository state.
            db_password="$(openssl rand -hex 24)"
            printf "ALTER ROLE sift PASSWORD '%s';\n" "$db_password" | \
              psql -q -h 127.0.0.1 -p "$pgport" -U sift -d sifttest

            phase 5 "Apply the instance and import its local credential"
            run_in_dev cargo run -q --profile release-dev -p sift-server --bin sift -- instance lock "$instance_root"
            run_in_dev cargo run -q --profile release-dev -p sift-server --bin sift -- instance apply "$instance_root"
            if grep -q 'credential = "credential:demo/postgres/shared"' "$instance_root/sift.toml"; then
              printf '%s\n' "$db_password" | jq -cnR '{password: input}' | \
                run_in_dev cargo run -q --profile release-dev -p sift-server --bin sift -- \
                  instance credentials import "$instance_root" \
                  --slot credential:demo/postgres/shared --stdin
            fi

            sh "$repo/examples/reproducible-instance/scripts/dev-mssql.sh" password | jq -cnR '{password: input}' | \
              run_in_dev cargo run -q --profile release-dev -p sift-server --bin sift -- \
                instance credentials import "$instance_root" \
                --slot credential:demo/sql-server/shared --stdin

            echo "Postgres: host=127.0.0.1 port=$pgport db=sifttest user=sift credential=<destination-local> ssl=prefer"
            echo "Sift instance root: $instance_root"
            echo "Sift connections: demo/postgres, demo/sql-server and demo/sqlite (managed by sift.toml)"
            echo "SQLite query: SELECT * FROM main.order_summary ORDER BY placed_at DESC;"
            echo "Seeded query: SELECT * FROM lab.order_summary ORDER BY placed_at DESC;"
            echo "Large result query: SELECT * FROM lab.large ORDER BY id;"
            echo "Postgres log: $pglog"
            echo "Desktop: supervising the applied auto-loopback instance"
            echo "The desktop can edit this run's sift.toml through the current-instance API."

            phase 6 "Initialize the Git-backed demo workspace"
            seed_server_log="''${TMPDIR:-/tmp}/sift-desktop-demo-seed-server-$(id -u).log"
            run_in_dev cargo run -q --profile release-dev -p sift-server --bin sift-server -- \
              --instance-root "$instance_root" >"$seed_server_log" 2>&1 &
            seed_server_pid=$!
            seed_base_url=""
            for _ in $(seq 1 200); do
              if [ -f "$instance_state/daemon.json" ]; then
                seed_endpoint="$(jq -er .endpoint "$instance_state/daemon.json" 2>/dev/null || true)"
                if [ -n "$seed_endpoint" ] && curl -fsS "http://$seed_endpoint/v1/health" >/dev/null 2>&1; then
                  seed_base_url="http://$seed_endpoint"
                  break
                fi
              fi
              if ! kill -0 "$seed_server_pid" >/dev/null 2>&1; then
                echo "Demo seed server exited before becoming ready. Log follows:" >&2
                sed -n '1,200p' "$seed_server_log" >&2
                exit 1
              fi
              sleep 0.1
            done
            if [ -z "$seed_base_url" ]; then
              echo "Demo seed server was not ready before timeout. Log follows:" >&2
              sed -n '1,200p' "$seed_server_log" >&2
              exit 1
            fi
            seed_protocol="$(curl -fsS -X POST "$seed_base_url/v1/handshake" \
              -H 'content-type: application/json' \
              -d '{"client_version":"sift-desktop-demo","client_kind":"automation","protocol":{"minimum":2,"maximum":2}}' \
              | jq -er .selected_protocol)"
            demo_tenant_id="$(curl -fsS "$seed_base_url/v1/metadata/tenants" \
              -H "x-sift-protocol-version: $seed_protocol" \
              | jq -er 'first(.[] | select(.tenant.name == "demo") | .tenant.id)')"
            demo_profile_id="$(curl -fsS "$seed_base_url/v1/metadata/connections?tenant=$demo_tenant_id" \
              -H "x-sift-protocol-version: $seed_protocol" \
              | jq -er 'first(.[] | select(.name == "demo/postgres") | .id)')"
            workspace_seed="$(SIFT_DEMO_TENANT_ID="$demo_tenant_id" \
              sh "$repo/examples/reproducible-instance/scripts/dev-seed-demo-workspace.sh" \
              "$seed_base_url" "$demo_profile_id")"
            kill "$seed_server_pid"
            wait "$seed_server_pid" || true
            seed_server_pid=""
            echo "Sift Git workspace: $workspace_root (repository $(printf '%s' "$workspace_seed" | jq -r .repository_id))"

            phase 7 "Start the desktop and supervised Sift server"
            run_in_dev cargo run --profile release-dev -p sift-desktop -- --instance-root "$instance_root" "$@"
          '';
        };

        demoSqlite = pkgs.writeShellApplication {
          name = "sift-demo-sqlite";
          runtimeInputs = with pkgs; [ coreutils sqlite ];
          text = ''
            repo="''${SIFT_REPO:-$PWD}"
            exec sh "$repo/examples/reproducible-instance/scripts/dev-seed-sqlite.sh" "$@"
          '';
        };

        desktopMetadata = pkgs.writeShellApplication {
          name = "sift-desktop-metadata";
          runtimeInputs = with pkgs; [ coreutils nix ];
          text = ''
            repo="''${SIFT_REPO:-$PWD}"
            source_root="''${1:-''${SIFT_DESKTOP_DEMO_INSTANCE_ROOT:-''${TMPDIR:-/tmp}/sift-desktop-demo-instance-$(id -u)}}"
            if [ "$#" -gt 0 ]; then shift; fi
            inspection_parent="$(mktemp -d "''${TMPDIR:-/tmp}/sift-metadata-inspection.XXXXXXXX")"
            inspection_root="$inspection_parent/instance"
            cd "$repo"
            nix develop "$repo" --command cargo build --profile release-dev -p sift-server -p sift-desktop
            nix develop "$repo" --command cargo run --profile release-dev -p sift-server --bin sift -- metadata inspect "$source_root" "$inspection_root"
            echo "Inspection snapshot retained at $inspection_root"
            exec nix develop "$repo" --command cargo run --profile release-dev -p sift-desktop -- --instance-root "$inspection_root" "$@"
          '';
        };

        website = pkgs.writeShellApplication {
          name = "sift-website";
          runtimeInputs = [ pkgs.nix ];
          text = ''
            echo "Wiki URL: http://''${1:-127.0.0.1:8787}/index.html"
          '' + devCommand ''cargo run -p sift-website --'';
        };

        wiki = pkgs.writeShellApplication {
          name = "sift-wiki";
          runtimeInputs = [ pkgs.nix ];
          text = ''
            echo "Wiki URL: http://''${1:-127.0.0.1:8787}/index.html"
          '' + devCommand ''cargo run -p sift-website --'';
        };

        desktopDemoWiki = pkgs.writeShellApplication {
          name = "sift-desktop-demo-wiki";
          runtimeInputs = with pkgs; [ coreutils curl python3 util-linux nix jq ];
          text = ''
            set -Eeuo pipefail

            phase_name="wiki launcher validation"
            wiki_error() {
              status=$?
              echo "Desktop demo + wiki failed during: $phase_name (exit $status)." >&2
              if [ -n "''${wiki_log:-}" ] && [ -s "$wiki_log" ]; then
                echo "Keyboard wiki server log:" >&2
                tail -n 40 "$wiki_log" >&2
              fi
              exit "$status"
            }
            trap wiki_error ERR

            repo="''${SIFT_REPO:-$PWD}"
            wiki="$repo/docs/keyboard-wiki"
            if [ ! -f "$repo/flake.nix" ] || [ ! -f "$wiki/index.html" ]; then
              echo "Run this from the sift checkout, or set SIFT_REPO=/path/to/sift." >&2
              exit 1
            fi

            bind="''${SIFT_DESKTOP_DEMO_WIKI_BIND:-127.0.0.1}"
            port="''${SIFT_DESKTOP_DEMO_WIKI_PORT:-8787}"
            echo "Wiki URL: http://$bind:$port/index.html"
            lock_file="''${TMPDIR:-/tmp}/sift-desktop-demo-$(id -u).lock"
            exec 9>>"$lock_file"
            if ! flock -n 9; then
              existing="$(head -n 1 "$lock_file" 2>/dev/null || true)"
              echo "A Sift desktop demo or demo wiki is already running''${existing:+ (launcher PID $existing)}." >&2
              echo "Stop the existing launcher before starting another." >&2
              exit 1
            fi
            : >"$lock_file"
            printf '%s\n' "$$" >&9

            phase_name="checking keyboard wiki port $bind:$port"
            if ! python3 -c 'import socket,sys; s=socket.socket(); s.bind((sys.argv[1], int(sys.argv[2]))); s.close()' "$bind" "$port" 2>/dev/null; then
              if curl -fsS --max-time 1 "http://$bind:$port/index.html" >/dev/null 2>&1; then
                echo "A keyboard wiki is already listening at http://$bind:$port." >&2
              else
                echo "Port $bind:$port is already used by another process." >&2
              fi
              echo "Set SIFT_DESKTOP_DEMO_WIKI_PORT to choose another port." >&2
              exit 1
            fi

            phase_name="building Sift website"
            cd "$repo"
            wiki_binary="$(nix develop "$repo" --command cargo build -p sift-website --message-format=json | jq -r 'select(.reason == "compiler-artifact" and .target.name == "sift-website" and .executable != null) | .executable')"
            if [ ! -x "$wiki_binary" ]; then
              echo "Website build did not produce an executable." >&2
              exit 1
            fi
            phase_name="starting Sift website and wiki"
            wiki_log="''${TMPDIR:-/tmp}/sift-desktop-demo-wiki-$(id -u).log"
            "$wiki_binary" "$bind:$port" >"$wiki_log" 2>&1 &
            wiki_pid=$!
            cleanup() {
              kill "$wiki_pid" >/dev/null 2>&1 || true
              wait "$wiki_pid" >/dev/null 2>&1 || true
            }
            trap cleanup EXIT
            trap 'exit 130' INT
            trap 'exit 143' TERM

            ready=0
            for _ in $(seq 1 50); do
              if curl -fsS --max-time 1 "http://$bind:$port/index.html" >/dev/null 2>&1; then
                ready=1
                break
              fi
              if ! kill -0 "$wiki_pid" >/dev/null 2>&1; then
                wait "$wiki_pid"
              fi
              sleep 0.1
            done
            if [ "$ready" != "1" ]; then
              echo "Keyboard wiki did not become ready at http://$bind:$port." >&2
              exit 1
            fi

            echo "Website: http://$bind:$port"
            echo "Wiki: http://$bind:$port/index.html"
            echo "Starting seeded Sift desktop demo..."
            phase_name="running seeded desktop demo"
            SIFT_DESKTOP_DEMO_LOCK_HELD=1 "${desktopDemo}/bin/sift-desktop-demo" "$@"
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
              sift-health               Curl /v1/health from the configured backend and pretty-print JSON.
              sift-smoke                Start a mock backend and exercise health/session/connection/schema/audit.
              sift-test                 Run cargo nextest for the whole workspace.
              sift-check                Run cargo check for the whole workspace.
              sift-desktop              Run the native GPUI desktop client.
              sift-desktop-demo         Seeded Postgres + SQL Server + SQLite + real backend + desktop.
              sift-desktop-demo-wiki    Run desktop demo + product page and wiki together.
              sift-website              Preview the Topcoat product page and wiki.
              sift-wiki                 Serve only the wiki, without the desktop demo.
              sift-demo-sqlite          Create the SQLite fixture once, preserving existing files.
              sift-desktop-metadata    Open a read-only inspection snapshot of Sift's metadata.
              sift-dev-secret-key       Generate the ignored local metadata secret key file.
              sift-dev-mssql            Manage a local SQL Server docker container for live-mssql tests.
                                        Sub: start | stop | reset | password | status. Password is
                                        generated and persisted to .env on first start.

            Typical flow:
              nix develop
              sift-help
              sift-desktop-demo

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
            health
            smoke
            test
            check
            desktop
            desktopDemo
            desktopDemoWiki
            website
            wiki
            demoSqlite
            desktopMetadata
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
          desktop = {
            type = "app";
            program = "${desktop}/bin/sift-desktop";
          };
          desktop-demo = {
            type = "app";
            program = "${desktopDemo}/bin/sift-desktop-demo";
          };
          sift-desktop-demo = {
            type = "app";
            program = "${desktopDemo}/bin/sift-desktop-demo";
          };
          sift-demo-sqlite = {
            type = "app";
            program = "${demoSqlite}/bin/sift-demo-sqlite";
          };
          sift-desktop-metadata = {
            type = "app";
            program = "${desktopMetadata}/bin/sift-desktop-metadata";
          };
          website = {
            type = "app";
            program = "${website}/bin/sift-website";
          };
          wiki = {
            type = "app";
            program = "${wiki}/bin/sift-wiki";
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
