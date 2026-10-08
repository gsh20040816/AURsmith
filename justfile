set shell := ["bash", "-euo", "pipefail", "-c"]

# Rust 格式/lint/测试 + Web 测试与构建 + Compose 静态检查。
test:
    bash scripts/test-all.sh

# 只做 Compose 静态安全检查（不需要 docker）。
compose-check:
    python3 scripts/check-compose.py
