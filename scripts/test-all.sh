#!/usr/bin/env bash
# 全部快速检查：Rust 格式/lint/测试、Web 测试与构建、Compose 静态检查。
# aursmithd 的签名器测试需要宿主机有 gpg、repo-add、makepkg、bsdtar、fakeroot、zstd。
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
(
  cd web
  npm ci
  npm test
  npm run build
)
python3 scripts/check-compose.py

echo "全部快速检查通过"
