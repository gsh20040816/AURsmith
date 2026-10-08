# 验证记录

旧架构（3+1 / Controller / Publisher / SSH Builder）的生产验证记录保留在 git 历史中（`8e0b098` 及之前的本文件），不适用于本次重构后的代码。

## 2026-10-08：架构简化后的本地验证

环境：Rust 1.90、Node（`web/package-lock.json` 锁定依赖）、宿主安装 `pacman`/`makepkg`/`repo-add`/`fakeroot`/`gpg`/`bsdtar`/`zstd`。

| 检查 | 结果 |
|------|------|
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 69 项全部通过：core 22、aursmithd 30、builder 17 |
| `web`: `npm ci && npm test && npm run build` | 8 个 Vitest 用例通过，TypeScript + Vite 生产构建通过 |
| `python3 scripts/check-compose.py` | 通过（YAML 解析 + 静态安全检查：公网 2 容器 / 1 网络；signer 无网络且唯一持有 GPG secret；Builder 只持有 builder_token） |

关键用例：

- **2+1 判定**：两低批准直接通过；低档拒绝 / 分歧 / 无效输出升级高档；高档拒绝 → 人工；首次添加即使 Agent 批准也人工（core 纯函数测试 + aursmithd 端到端测试，后者用本地 mock 的 OpenAI/Anthropic 端点）。
- **严格 JSON**：未知字段、多余文本、缺字段、`approve` 同时含 critical 发现均视为无效。
- **端到端流水线**：订阅 → revision → 人工批准 → 调度 → 模拟 Builder 回报与分片上传 → 期望状态计划 → 签名器（真实 `repo-add` + GPG）→ 原子切换 → VCS 前进后复用审查再发布 → **篡改 staging 产物被拒且 `current` 不变** → 回滚 → 退订收敛。
- **Builder**：mock 服务器 + 假 docker 脚本，覆盖成功路径、按服务器偏移 32 MiB 分片续传、guest 错误码透传、重启恢复 `BUILDER_RESTARTED`、服务器拒绝回报时丢弃、输入篡改在启动容器前失败、构建容器只读输入且无凭据。
- **迁移**：状态导出 → 导入 → 校验往返，非空目标拒绝导入；旧库（测试夹具）`legacy-export` 保留已批准 baseline。
- **认证**：管理 API 要求会话、Origin 与 CSRF 头。

未验证（本机无 Docker / 无生产凭据）：

- `docker compose config` / 镜像构建 / 容器实际运行；
- 真实 LLM 调用、真实家庭 Builder 经 Cloudflare 上传、生产 GPG 私钥签名、独立 pacman 客户端安装；
- 生产旧库的 `legacy-export` → `import` → `verify`。

以上须在部署时按 `acceptance.md` 逐项验证后再补记到本文件。
