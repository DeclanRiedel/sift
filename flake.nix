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
      });
}
