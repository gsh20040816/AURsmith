# 决策记录

## 已接受

- ADR-001：控制面使用 Rust、Axum、SQLx 和 SQLite，UI 使用 React 与 TypeScript。
- ADR-002：部署拆分为 Controller、Builder、Publisher 和 Archiver 四套 Docker Compose Stack。
- ADR-003：由非 privileged Builder 容器直接启动 QEMU/KVM。
- ADR-004：远程控制和批量传输使用 OpenSSH 与 rsync。
- ADR-005：确定性 CBOR payload 由包含 SHA-256 和 Ed25519 签名的 Envelope 承载。
- ADR-006：三个低成本 Agent；三票通过、恰好两票升级一个高成本 Agent、不超过一票转人工。
- ADR-007：热点依赖进入不可变 KVM Guest Profile，不进入服务容器镜像。
- ADR-008：AURsmith 自有代码使用 Apache-2.0 许可证。
- ADR-009：交付全过程在 `main` 上使用 Git。每个验证通过的阶段形成独立提交，提交标题使用英文 `<type>: <message>`；Release Manifest 记录源码 commit，签名设施可用后为发布版本创建带签名的 annotated tag。
- ADR-010：AUR RPC、AUR Git 和 `.SRCINFO` 获取只在 Publisher Worker 中执行。Controller 通过现有 OpenSSH forced command 请求小型结构化响应，不新增常驻集群协议；任何 PKGBUILD 动态求值仍留给后续隔离 Fetch VM。
- ADR-011：官方包晋升检查使用 Arch 官方仓库 JSON 接口，由 Publisher 发起。发现晋升时暂停 AUR 自动更新而不删除当前包，用户确认客户端迁移后再清理。
- ADR-012：Agent Runner 只实现 Codex CLI 与 Claude Code 两种固定适配器，不保留任意自定义命令入口。两者都支持自定义 provider 标签、模型和 Base URL；真实 API key 只进入独立凭据网关的 Docker secret，Runner 使用内部 Base URL 和占位认证值。这样保留自建兼容服务能力，同时避免模型通过 CLI 工具读取真实 key。
- ADR-013：Agent 凭据网关替代通用正向代理。它为 low/high 层分别固定唯一 HTTPS upstream 和认证方式，剥离调用方认证头并注入 secret，不能由请求选择目标主机。Runner 网络为内部网络，只有网关具有 provider 外网。
- ADR-014：KVM Profile 身份采用“文件 Manifest、已安装包清单和创建时间”的确定性内容摘要，Envelope 再对该声明签名。拒绝让 payload 包含自身哈希的循环定义。Fetch VM 只用 `restrict=on` 的单一 guestfwd 到固定 source proxy，Build VM 固定 `-nic none`。
- ADR-015：Profile 使用一次性 Compose 构建镜像生成，不给常驻 Builder 增加 root 或联网能力。Guest Agent 固定为 PID 1 并再次验证 Controller JobSpec；任何 Guest 失败都关闭 VM，不回退到宿主 makepkg。
- ADR-016：AUR 包装层扫描结果建模为 `AuditPreScan`，它不是 Agent 的最终审计输入。只有 Fetch VM 产生完整 Source Manifest、Builder 与 Controller 双重验证结果后，系统才创建内容寻址的 `AuditBundle` 和三路低成本 Agent 任务，避免把“尚未取得上游源码”误报成已完成审计。

## 已拒绝

- Fork AURCache 或复制 aurto、lilac 核心代码。
- 裸机 AURsmith 服务、privileged Docker-in-Docker，以及 Docker/libvirt Socket 挂载。
- Kubernetes、Redis、Publisher 自动选主和自研 mTLS 集群协议。
- 使用 `latest` 作为角色名称；网络敏感角色命名为 Publisher。
- 三个低成本 Agent 全部通过后，仅因为软件包风险分类而强制调用高成本 Agent。
- 在 Agent Runner 中执行用户提供的任意 CLI、Shell 命令或自定义适配脚本。
- 将 Agent API key 作为 Runner 环境变量、命令行参数或可被模型读取的挂载文件。
