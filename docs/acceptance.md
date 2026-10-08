# 验收清单

本地可验证项（见 `verification.md`）：

- [x] `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 通过；
- [x] 前端 `npm ci && npm test && npm run build` 通过；
- [x] `scripts/check-compose.py` 静态检查通过（两个公网服务、加固、signer 无网络且唯一持有 GPG 私钥、仅回环端口、Builder 只持有 builder_token）；
- [x] 2+1 判定矩阵、严格 JSON 报告解析（未知字段 / 多余文本 / 缺字段失败关闭）；
- [x] 签名器：校验 staging（篡改产物被拒且旧 `current` 不变）、`repo-add`、签名、原子切换、回滚；
- [x] Builder：租约 → 构建 → 分片上传 → 完成；按服务器偏移续传、输入篡改在 docker 前失败、重启恢复（BUILDER_RESTARTED）、服务器拒绝回报时丢弃作业目录；
- [x] 状态导出 → 导入 → 校验往返；旧库 `legacy-export`（测试夹具）。

部署时必须真实验证（尚未验证，不得在 `verification.md` 标记为通过）：

- [ ] 生产旧库 `legacy-export` → `import` → `verify`，管理员、订阅、Provider 选择、baseline 计数与旧库一致；
- [ ] 真实两个 low + 一个 high 模型返回符合 schema 的报告；任一失败进入人工审批；
- [ ] 家庭 Builder 经 Cloudflare/Caddy 完成真实构建与 32 MiB 分片上传；
- [ ] signer 在 `network_mode: none` 下用生产 GPG 私钥签名并切换 `current`；
- [ ] 独立 Arch 客户端核对指纹、`pacman -Sy` 并安装验证签名；
- [ ] `signer --rollback` 真实回滚后客户端仍可同步；
- [ ] 未登录管理 API、Origin/CSRF、Builder Bearer 在生产反代后行为正确。
