#!/usr/bin/env bash
# Build the WebAssembly module and its TypeScript declarations into dist/.
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli --version <the
# wasm-bindgen version in Cargo.lock> (the CLI and the library must match exactly).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p wirefold_bindings --features wasm --target wasm32-unknown-unknown --release
target="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')"
rm -rf dist
wasm-bindgen --target nodejs --out-dir dist --out-name wirefold_bindings \
  "$target/wasm32-unknown-unknown/release/wirefold_bindings.wasm"
echo "wrote dist/"
