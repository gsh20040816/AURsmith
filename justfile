set shell := ["bash", "-euo", "pipefail", "-c"]

# 运行当前单服务核心的完整本地门禁。
test:
    bash scripts/test-all.sh

# 渲染并检查唯一 Compose 服务与通用反代边界。
compose-check:
    bash scripts/check-compose-security.sh

# 确认旧 crate、迁移和构建链没有重新进入源码树。
residual-check:
    bash scripts/check-residuals.sh
