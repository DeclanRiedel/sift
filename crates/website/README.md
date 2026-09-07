# Sift website

A small Topcoat product page and the existing keyboard wiki. Preview from the
checkout with `nix run .#website`, then open `http://127.0.0.1:8787`.
Use `nix run .#website -- 127.0.0.1:8788` to choose another listening address.
`nix run .#sift-desktop-demo-wiki` serves the same site alongside the desktop demo.

The product page is rendered in Rust; documentation and CSS are embedded from
`docs/keyboard-wiki` at build time. Existing `.html` wiki URLs remain available.
Changes take effect after rebuilding. No JavaScript build, database, accounts,
or access to Sift's application state is needed.

Topcoat is pinned to 0.6.2 for compatibility with the Nix-pinned compiler. Topcoat
0.7 requires Rust 1.98; the current Nix shell provides Rust 1.96.1.
Custom CSS supplies the centered content frame and decorative SVG cell gutters.
The site lists no unverified binary download URLs.
