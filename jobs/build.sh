#!/usr/bin/env bash
# Builds every job to WebAssembly. The node's integration tests need these.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
for dir in */; do
    [[ -f "$dir/Cargo.toml" ]] || continue
    echo "building ${dir%/}…"
    ( cd "$dir" && cargo build --release --target wasm32-unknown-unknown --quiet )
done
echo
echo "built:"
find . -name '*.wasm' -path '*release*' -printf '  %p  (%s bytes)\n'
