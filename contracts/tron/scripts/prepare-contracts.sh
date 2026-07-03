#!/usr/bin/env bash
# Copy the Solidity example contracts into a gitignored tree for tronbox to compile.
#
# Why this exists:
#   - The example contracts live under contracts/solidity/src/ and are built with
#     Foundry. We want tronbox (TRON's solc fork, "tronc") to compile the SAME sources to
#     prove they build for the TVM and to produce TVM-native artifacts.
#   - tronbox reads from its own contracts_directory, and we never want the build to dirty
#     the working tree, so we mirror src/*.sol into a gitignored ./generated/ here.
#   - Only src/ is copied; test/*.t.sol import forge-std and are not deployable contracts,
#     matching the `--skip "*.t.sol"` convention in solidity/Makefile.
#   - The example contracts have no external (e.g. OpenZeppelin) imports, so this is a plain
#     copy: no symlink/remapping dance is needed.
set -euo pipefail

tron_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src_dir="$tron_dir/../solidity/src"
gen_dir="$tron_dir/generated"

rm -rf "$gen_dir"
mkdir -p "$gen_dir"
cp "$src_dir"/*.sol "$gen_dir/"

echo "Copied to generated/:"
for f in "$gen_dir"/*.sol; do
    echo "  $(basename "$f")"
done
