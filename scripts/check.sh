#!/usr/bin/env bash
# Definition-of-done gate: format, lint, test.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
