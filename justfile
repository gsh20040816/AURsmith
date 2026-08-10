set shell := ["bash", "-euo", "pipefail", "-c"]

# 运行全部快速测试，不包含需要 KVM Guest 镜像的集成测试。
test:
    cargo fmt --all -- --check
    cargo test --workspace
    cd web && npm run lint && npm test && npm run build
    bash scripts/check-compose-security.sh
# 渲染并检查四套 Compose 配置。
compose-check:
    bash scripts/check-compose-security.sh
