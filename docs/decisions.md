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
- ADR-017：首个可运行的 Fetch/Build 接力以 ReleaseBatch 为单位固定 Builder 亲和性。Build Job 引用已验证的 Fetch Attempt 和 Source Manifest，不允许重新下载。跨 Builder 接力必须等待受签名 `TransferCapability` 约束的 rsync 路径完成，不能用共享可写卷或临时联网代替。
- ADR-018：官方依赖只允许 Fetch Guest 通过 pacman 和固定代理下载，并进入 Source Manifest；批次内 AUR 依赖只允许来自较早的成功 Build Attempt。Build Guest 在无网状态使用 pacman 安装两类包，禁止把 AUR 依赖固化进 Profile 或在构建失败后临时放开网络。
- ADR-019：Signer 使用独立断网容器和私有 tmpfs GPG home。Publisher 只能写入 inbox、只读 signed 输出，Signer 不挂载公开仓库；它必须自行验证 ReleaseAuthorization、Artifact Manifest 和 `.PKGINFO`，并用官方 repo-add 从完整包集合创建数据库。
- ADR-020：Worker UUID 由本地 Journal 首次生成并持久化，Controller 注册必须以远端报告值为准，不能在两端各自生成不相关 ID。Artifact 传输采用源端最小 export 目录、固定 rsync sender forced command 和目标端 partial+摘要复验；Publisher 的 Builder 端点来自静态 UUID 映射，不增加服务发现或自研传输协议。
- ADR-021：ReleaseAuthorization 始终描述完整仓库集合。新批次按 `pkgname` 覆盖上一稳定 Release 的对应 Artifact，未变化包从已签名 hot set 按摘要复用；Signer 每次重新生成完整 db/files 数据库。Publisher 只持仓库公钥，公开顺序固定为不可变 Release、包 hot set、数据库签名、files 链接和最后的 db 链接。
- ADR-022：ArchiveCopy 复用 TransferCapability 与 forced-command rsync，但使用 `release_id` 而不是 Build Attempt 绑定聚合。Archiver 主动拉取并用 `--link-dest` 建立快照；Receipt 由 Archiver Journal 中持久化的 Ed25519 身份密钥签署，Controller 在 Worker 注册和每次心跳时固定并核对身份公钥。源端 export 只在目标导入或 ArchiveReceipt 验证后由 Controller 触发幂等清理。
- ADR-023：服务端回滚只激活既有签名 Release，不复制数据库、不重新签名且不改变历史 Release。Controller 将服务端当前指针和客户端降级分开建模，后者始终以显式 `pacman -U` 命令交给用户执行。
- ADR-024：仓库公钥可以由 Publisher 公开下载，但私钥仍只存在于断网 Signer。Controller 不自行猜测指纹，而是在固定 Publisher 身份时保存其 GPG 完整指纹；客户端引导必须要求人工核对后才执行 `pacman-key --lsign-key`。
- ADR-025：动态依赖优化以七天统计周期和迟滞状态实现，不直接修改正在使用的 Guest。优化器只对官方包给出加入/移除计划；每个计划仍经过完整 Profile 重建、签名授权、KVM fixture 和人工激活，避免统计波动把未经验证的环境直接投入构建。
- ADR-026：Worker 容量和能力复用已有签名身份与 OpenSSH 心跳采集，不部署额外主机 Agent。Publisher 低于 10% 可用空间时使用 SQLite 持久化背压停止新任务和 Release，当前仓库仍可读；告警通知使用 SQLite outbox、HMAC Webhook或 ntfy，不为单用户部署引入消息队列。
- ADR-027：控制面备份使用 SQLite 原生 `VACUUM INTO`，而不是在 WAL 运行时复制主数据库文件。每份快照必须通过完整性检查并由 Controller Ed25519 签名；恢复只作为停机 CLI 提供，并先保留当前数据库，避免 Web 请求在线覆盖控制面。
- ADR-028：归档库存检查分为每周集合/大小复验和每九十天完整 SHA-256 巡检。报告由 Archiver 身份密钥签署并在 Controller 端核对，不把 SSH 传输成功或文件存在等同于长期数据完整。
- ADR-029：控制面备份进入 Archiver 时复用 OpenSSH、rsync 和 TransferCapability，但 Controller 仅为备份导出运行无 Shell 的容器化 SSH sidecar。传输源 UUID由 Controller 公钥确定，Archiver 仍使用静态端点和独立拉取凭据；数据库字节不经过 JSON 控制消息，也不引入新的集群协议。
- ADR-030：AUR 包不可见时建模为含删除、重命名、合并三种可能原因的生命周期事件，不从一次空查询猜测具体原因。维护者、orphan 和 source 域名变化使用快照差异独立记录，且不伪装成普通构建失败。
- ADR-031：官方依赖版本变化采用保守的重建建议，不宣称从包版本或 ELF 信息证明 ABI 已改变或兼容。建议默认七天合批，用户可立即执行或按包关闭；实际执行必须派生新 Revision 并重新 Fetch、审计和构建，禁止复用旧依赖快照。
- ADR-032：本地重建版本由 Controller 根据同一上游完整版本的成功历史单调派生，并通过签名 JobSpec 固定到 Build Guest。Guest 只改写工作副本中唯一、静态的顶层 `pkgrel`；动态或歧义赋值失败关闭。Controller 以产物 `.PKGINFO` 反向核验授权版本，避免只更新数据库字段却生成客户端无法升级的同版本软件包。
- ADR-033：Publisher 对不可信软件包执行独立归档检查。路径、文件类型和必需元数据属于确定性发布门禁；INSTALL、hook、服务、setuid 和内核模块只形成可追踪风险事实，不能脱离审计上下文直接判为恶意。检查报告由 Signer 纳入签名 Release Manifest，避免发布后只剩包文件而丢失 Publisher 的判断依据。
- ADR-034：第一版先用独立、无缓存的 Squid 容器补齐 Fetch VM 的实际外网出口，只允许 80/443 并拒绝全部本地和保留目标。pacoloco 命中率、按 Revision 自动下发精确域名 ACL 属于后续优化；Build VM 无网边界不因此变化。
- ADR-035：Arch 软件仓库镜像在不可变 Profile 构建时配置，而不是作为每个 Build Job 的可变参数。镜像必须是 HTTPS Base URL，同时写入 Guest mirrorlist 和签名 Profile 清单；Fetch Guest 用它下载官方依赖，Build Guest 继续使用已准备好的离线包。这样镜像选择可追溯，也不会破坏无网构建边界。

## 已拒绝

- Fork AURCache 或复制 aurto、lilac 核心代码。
- 裸机 AURsmith 服务、privileged Docker-in-Docker，以及 Docker/libvirt Socket 挂载。
- Kubernetes、Redis、Publisher 自动选主和自研 mTLS 集群协议。
- 使用 `latest` 作为角色名称；网络敏感角色命名为 Publisher。
- 三个低成本 Agent 全部通过后，仅因为软件包风险分类而强制调用高成本 Agent。
- 在 Agent Runner 中执行用户提供的任意 CLI、Shell 命令或自定义适配脚本。
- 将 Agent API key 作为 Runner 环境变量、命令行参数或可被模型读取的挂载文件。
