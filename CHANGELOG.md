# 变更日志

本项目使用语义化版本号。正式版发布前以预发布版本记录可部署里程碑；所有已知缺口必须同时写入发布说明和验收矩阵。

## 0.1.0-alpha.1（2026-08-10）

首个单用户可部署预发布版本。

### 已实现

- AUR 搜索、订阅生命周期、pkgbase/split package、依赖闭包、Provider 和 ReleaseBatch。
- AUR 与 Git VCS 更新跟踪、历史重写门禁和精确人工审批。
- 三个低成本 Agent 与一个高成本 Agent 的固定投票规则；仅支持 Codex 和 Claude Code，并支持自定义 provider、Base URL 与文件型 API key。
- Docker Compose 四角色部署；Builder 在非 privileged 容器内使用 KVM 双 VM，Build VM 无网。
- 构建镜像源写入不可变 Profile；依赖统计、Profile 建议、授权、fixture、激活和回滚。
- Publisher 检查、断网 Signer、GPG、repo-add、完整 Release 原子发布、Archiver Receipt 与库存巡检。
- 实际 pacman 安装、升级、服务端回滚及客户端显式降级流程。
- Web 控制台、认证 SSE、告警、设置、Doctor、备份、Job 日志和 Release 签名证据。

### 已验证

- 全仓库 106 个 Rust 测试、8 个前端测试、TypeScript、生产构建和 Compose 安全策略检查通过。
- 真实 KVM Fetch→Build、真实 source proxy、真实 AUR/VCS/官方仓库访问、跨角色 rsync、Signer/Archiver 和 pacman 客户端流程分别完成冒烟或端到端验证。

### 已知限制

- 尚未把 Profile qcow2、完整 source tree/License bundle 和超限日志全文传输到 Archiver；结构化 Profile/Source Manifest、摘要和有界日志已归档。
- 真实 Codex/Claude provider 调用需要部署者 API key，本次没有产生付费模型调用。
- Fetch VM 内真实外部 source 与官方依赖下载尚未在同一 KVM 用例中验证。
- 尚无 Git remote，因此该预发布版本只创建本地 annotated tag，不包含 push 或托管平台 Release。
