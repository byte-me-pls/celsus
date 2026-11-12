#!/usr/bin/env bash
# Builds the desktop app, with its MCP server beside it, into what people install on the system this runs on:
#   Windows  target/release/bundle/nsis/*-setup.exe
#   macOS    target/universal-apple-darwin/release/bundle/dmg/*.dmg   (Apple silicon and Intel in one app)
#   Linux    target/release/bundle/{deb,rpm,appimage}/
# With TAURI_SIGNING_PRIVATE_KEY set, also the signed files an installed app updates itself from.
# Needs the Tauri CLI (cargo install tauri-cli --version "^2" --locked); on Linux also the packages in README.md.
set -euo pipefail
cd "$(dirname "$0")/.."

# Tauri takes a program to ship beside the app from a file named after the system it was built for.
side=crates/celsus-desktop/sidecar
rm -rf "$side"
mkdir -p "$side"

host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
  *-windows-*)
    cargo build --release --locked -p celsus-mcp
    cp target/release/celsus-mcp.exe "$side/celsus-mcp-$host.exe"
    args=(--bundles nsis)
    ;;
  *-apple-darwin)
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
      rustup target add "$target"
      cargo build --release --locked -p celsus-mcp --target "$target"
      # The app is built once for each processor before the two are joined, and each build looks for its own.
      cp "target/$target/release/celsus-mcp" "$side/celsus-mcp-$target"
    done
    lipo -create -output "$side/celsus-mcp-universal-apple-darwin" \
      target/aarch64-apple-darwin/release/celsus-mcp target/x86_64-apple-darwin/release/celsus-mcp
    args=(--target universal-apple-darwin --bundles app,dmg)
    ;;
  *-linux-*)
    cargo build --release --locked -p celsus-mcp
    cp target/release/celsus-mcp "$side/celsus-mcp-$host"
    # Programs in /usr/bin are named in lower case.
    args=(--bundles deb,rpm,appimage --config '{"mainBinaryName":"celsus"}')
    ;;
  *)
    echo "no installer is made for $host" >&2
    exit 1
    ;;
esac

# With the signing key at hand, each installer the app can update itself from gets a signature beside it
# (<file>.sig). Without the key the installers are built all the same; they just cannot be used for updating.
if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  args+=(--config '{"bundle":{"createUpdaterArtifacts":true}}')
fi

cd crates/celsus-desktop
cargo tauri build --config release.conf.json "${args[@]}"
