#!/usr/bin/env bash
# The gate before a push: formatting, lints as errors, every test (the architecture ratchets
# included). Install as a hook: ln -s ../../tools/check.sh .git/hooks/pre-push
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
