#!/usr/bin/env bash
# The same checks CI would run, on this machine: tests and strict clippy.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo test"
cargo test --quiet
echo "==> cargo clippy (warnings are errors)"
cargo clippy --all-targets --quiet -- -D warnings
echo "==> all checks passed"
