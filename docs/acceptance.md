# 当前验收清单

- 全仓库 Rust、前端、构建和 Compose 安全测试通过；
- 生产数据库副本迁移后通过 foreign-key 与 integrity 检查，管理员、订阅、批准 baseline、Agent 结果和 current 指针计数不变；
- 真实三个 low 与按需 high 报告实际 provider/model，完整 full/diff 审查失败关闭；
- 家庭 Builder 完成联网 Docker Build，产物通过固定 rrsync 到 Publisher；
- Publisher 直接使用 GPG 私钥签名，`repo-add` 后原子切换 current，失败不破坏旧 current；
- 独立 Arch 客户端核对指纹后能安装仓库包并验证签名；
- 同版本手工重建保持原版本，Web 显示不会自动升级及同名制品竞态警告；
- current/previous 回退、Builder 重启幂等、重复上报和工作区清理经过真实验证；
- 未登录管理 API、Origin/CSRF、Builder Bearer 与浏览器 session 隔离测试通过。

尚未实际验证的项目不得在 `verification.md` 中标记为通过。
