# 第一版验收矩阵

本文只记录当前源码和已经执行的验证，不把“已有设计”当作“已经实现”。状态含义：

- `已验证`：存在实现，并有自动测试或真实集成验证；
- `部分验证`：主路径已实现，但仍有明确的第一版代码或端到端缺口；
- `延期`：已在需求总账中明确允许延期，不阻塞单用户第一版；
- `未完成`：仍阻塞第一版完成定义。

## 需求总账状态

| ID | 状态 | 当前证据 | 第一版剩余缺口 |
|---|---|---|---|
| P01 | 已验证 | 搜索、订阅、暂停、恢复、退订、清除 API 与 Web；清除通过完整 Release 生效 | 隐式依赖保留期目前只有状态，没有按天自动清理策略 |
| P02 | 已验证 | AUR commit、Git VCS commit 与真实祖先关系跟踪；`paru-git` 上游祖先检查冒烟 | 非 Git VCS 精细跟踪明确延期 |
| P03 | 已验证 | pkgbase DAG、完整 split outputs 构建与 Guest 强校验 | 无 |
| P04 | 已验证 | 依赖闭包、隐式引用、Provider 选择、循环阻断测试 | bootstrap 循环按计划人工处理 |
| P05 | 已验证 | ReleaseBatch 拓扑构建、私有依赖输入和整批发布状态机 | 尚未跑订阅到发布的单次无人值守 E2E |
| P06 | 已验证 | 本地 pkgrel 派生、Guest 工作副本改写和产物版本反验 | 动态 pkgrel 明确失败关闭，需人工处理 |
| P07 | 已验证 | AUR 消失、维护者/orphan/source 域名、官方晋升、依赖变化及 VCS 历史重写事件 | 无 |
| P08 | 已验证 | 新批次失败不改变当前 Release；发布故障与回滚实际验证 | 无 |
| A01 | 已验证 | 包装层扫描、Fetch 后 AuditBundle、三 Runner 调度 | 真实外部模型调用未验证 |
| A02 | 已验证 | 3/2/≤1、单次重试和高成本批准规则自动测试 | 随机复查率保持默认 0%，尚无 UI 配置 |
| A03 | 部分验证 | Codex/Claude Code 固定 argv、凭据网关、Compose secret、预算；四 Runner 无付费 Doctor 已实现并做容器冒烟 | 缺真实 provider key 审计 E2E，发布前必须保留为未验证范围 |
| A04 | 已验证 | 覆盖范围、选读文件和“不证明全部源码安全”写入报告 | 无 |
| B01 | 已验证 | 非 privileged Builder 容器内真实 KVM Fetch→Build | 无 |
| B02 | 部分验证 | Build VM `-nic none` 实际验证；Publisher Doctor 经 source proxy 转发 Arch HTTPS 冒烟 | Fetch Guest 内真实 source/官方依赖下载尚未在同一 KVM 用例验证 |
| B03 | 已验证 | JobSpec、签名 Profile、完整 source tree/License、完整 Build/Fetch/QEMU/namcap 日志、依赖、GuestResult 和 Artifact 均由摘要绑定并随 Release 传输 | 第一版会重复压缩相同 Profile，后续可按内容寻址优化空间 |
| B04 | 部分验证 | 统计、迟滞、建议、Profile 授权/激活/回滚代码与测试 | 尚未跑“真实统计→重建 Profile→KVM fixture→命中”的完整闭环；pacoloco 命中率未接入 |
| B05 | 已验证 | HTTPS 镜像进入 Profile 摘要；清华镜像实际构建 Profile | Fetch Guest 命中该镜像下载真实依赖仍属于 B02 缺口 |
| W01 | 已验证 | 四套 Compose、无裸机服务、静态安全检查和真实容器启动 | 无 |
| W02 | 已验证 | Builder/Publisher/Archiver 独立 Stack 与静态端点 | 无 |
| W03 | 已验证 | OpenSSH forced command、固定 host key、rsync 跨容器传输 | 无 |
| W04 | 已验证 | Journal、幂等、迟到拒绝、uncertain 30 分钟、最多两次基础设施重试 | 跨 Builder 的 Build 输入迁移按计划未实现，当前失败关闭 |
| W05 | 已验证 | 多 Builder 调度、单 writer epoch、单主 Archiver | 无人值守 Publisher failover 明确延期 |
| R01 | 已验证 | Publisher 路径/元数据/ELF/capability 检查，断网 Signer 与 GPG | capability 包尚未走完整 Publisher→Signer E2E |
| R02 | 已验证 | 完整 Release、repo-add、签名和数据库最后原子切换实际验证 | 空仓库签名 E2E 尚未执行，但真实 pacman 已能读取空 DB |
| R03 | 已验证 | 签名 Release 保存 Audit、Agent、provenance、完整 Profile/Source/License/日志证据；ArchiveReceipt 绑定递归文件集合，rsync 快照恢复测试逐字节通过；控制面备份恢复测试通过 | 跨物理设备恢复仍应在实际部署后按手册演练 |
| R04 | 已验证 | 服务端签名 Release 回滚和真实客户端 `pacman -U` 降级 | 自动客户端降级明确延期 |
| U01 | 已验证 | 搜索、包、审计、构建、Worker、Profile、Release、归档、告警、设置页面；Job 可查看成功/失败有界日志，Release 可展开签名证据文档 | 不要求第一版实现复杂日志搜索 |
| U02 | 部分验证 | 单管理员认证、一次性初始化、GPG/pacman 引导和稳定 URL | 内部 CA 证书导出/轮换仍主要依赖部署配置 |
| U03 | 已验证 | Worker/磁盘/时钟、Agent/Fetch Doctor、告警、Webhook/ntfy、备份与库存页面 | 外部 Webhook/ntfy 未使用真实服务冒烟 |
| O01 | 已验证 | 包装 makepkg、repo-add、pacman、QEMU、OpenSSH、rsync、GnuPG、Caddy | 无 |
| O02 | 已验证 | 全过程直接在 main 使用独立英文提交；Release 记录源码 commit；首个预发布版本使用本地 annotated tag 管理 | 仓库无 remote，未执行 push；正式 v0.1.0 尚未发布 |

## 第一版部署验收项

B03/R03 的代码阻塞已经关闭。真实 provider key、跨物理设备恢复、内部 CA 生命周期、外部通知和热点 Profile 性能收益仍依赖用户部署环境，必须在首次部署时由 Doctor 与恢复手册验收；它们不应被误写成当前开发机已经验证，也不继续扩展为高可用或复杂观测系统。
