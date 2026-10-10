#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
node --check ui/app.js
node --check scripts/release.mjs
node --test tests/ui/*.test.mjs tests/release/*.test.mjs
node scripts/check-contrast.mjs
node scripts/release.mjs check-version
