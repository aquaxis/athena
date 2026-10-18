#!/bin/sh
# The Studio build pipeline (P7-01): Rust → WebAssembly, then the static assets land
# the runnable area. The commands mirror `athena_studio::BuildPlan::phase7`.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${1:-wasm32-unknown-unknown}"

cd "$root"

if ! rustup target list --installed 2>/dev/null | grep -q "$target"; then
    echo "build-studio: adding the $target toolchain"
    rustup target add "$target"
fi

cargo build -p athena-studio --target "$target" --release

out="target/$target/release"
echo "build-studio: artifacts at $out/athena_studio.wasm and $out/athena_studio.js"
ls -l "$out" | head -20
