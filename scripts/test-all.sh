#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo test --workspace
(
  cd web
  npm run lint
  npm test
  npm run build
)
bash scripts/check-compose-security.sh

echo "全部快速测试通过"
