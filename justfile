set shell := ["bash", "-euo", "pipefail", "-c"]

# 运行全部快速测试，不包含需要 KVM Guest 镜像的集成测试。
test:
    bash scripts/test-all.sh
# 渲染并检查四套 Compose 配置。
compose-check:
    bash scripts/check-compose-security.sh
