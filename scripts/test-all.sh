#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked --workspace
bash scripts/check-residuals.sh
bash scripts/check-compose-security.sh

echo "全部快速测试通过"
