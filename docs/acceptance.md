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
| A03 | 部分验证 | Codex/Claude Code 固定 argv、凭据网关、Compose secret、预算 | 缺真实 provider key E2E 和 Runner Doctor 探测 |
| A04 | 已验证 | 覆盖范围、选读文件和“不证明全部源码安全”写入报告 | 无 |
| B01 | 已验证 | 非 privileged Builder 容器内真实 KVM Fetch→Build | 无 |
| B02 | 部分验证 | Build VM `-nic none` 实际验证；source proxy 外部冒烟 | Fetch Guest 内真实 source/官方依赖下载尚未在同一 KVM 用例验证 |
| B03 | 部分验证 | JobSpec、Profile、source/dependency/artifact 摘要、完整 GuestResult、审计和 Agent 报告已进入签名 ReleaseEvidence | 完整 build/fetch/QEMU 日志字节及 Profile/Source/License 文件包尚未随 Release 归档 |
| B04 | 部分验证 | 统计、迟滞、建议、Profile 授权/激活/回滚代码与测试 | 尚未跑“真实统计→重建 Profile→KVM fixture→命中”的完整闭环；pacoloco 命中率未接入 |
| B05 | 已验证 | HTTPS 镜像进入 Profile 摘要；清华镜像实际构建 Profile | Fetch Guest 命中该镜像下载真实依赖仍属于 B02 缺口 |
| W01 | 已验证 | 四套 Compose、无裸机服务、静态安全检查和真实容器启动 | 无 |
| W02 | 已验证 | Builder/Publisher/Archiver 独立 Stack 与静态端点 | 无 |
| W03 | 已验证 | OpenSSH forced command、固定 host key、rsync 跨容器传输 | 无 |
| W04 | 已验证 | Journal、幂等、迟到拒绝、uncertain 30 分钟、最多两次基础设施重试 | 跨 Builder 的 Build 输入迁移按计划未实现，当前失败关闭 |
| W05 | 已验证 | 多 Builder 调度、单 writer epoch、单主 Archiver | 无人值守 Publisher failover 明确延期 |
| R01 | 已验证 | Publisher 路径/元数据/ELF/capability 检查，断网 Signer 与 GPG | capability 包尚未走完整 Publisher→Signer E2E |
| R02 | 已验证 | 完整 Release、repo-add、签名和数据库最后原子切换实际验证 | 空仓库签名 E2E 尚未执行，但真实 pacman 已能读取空 DB |
| R03 | 部分验证 | Release/控制面备份、Receipt、库存和恢复内核已有实现；签名授权已携带 Audit、Agent 与 provenance 结构化证据 | 原始日志、Profile/Source/License 文件包尚未归档；未做跨设备控制面恢复演练 |
| R04 | 已验证 | 服务端签名 Release 回滚和真实客户端 `pacman -U` 降级 | 自动客户端降级明确延期 |
| U01 | 部分验证 | 搜索、包、审计、构建、Worker、Profile、Release、归档、告警、设置页面；Release 可查看签名证据摘要 | 构建阶段原始日志和证据文档详情仍受 B03/R03 缺口阻塞 |
| U02 | 部分验证 | 单管理员认证、一次性初始化、GPG/pacman 引导和稳定 URL | 内部 CA 证书导出/轮换仍主要依赖部署配置 |
| U03 | 已验证 | Worker/磁盘/时钟、Doctor、告警、Webhook/ntfy、备份与库存页面 | 外部 Webhook/ntfy 未使用真实服务冒烟 |
| O01 | 已验证 | 包装 makepkg、repo-add、pacman、QEMU、OpenSSH、rsync、GnuPG、Caddy | 无 |
| O02 | 部分验证 | 全过程直接在 main 使用独立英文提交；Release 记录源码 commit | 第一版未完成，因此尚未创建正式版本号和签名 Git tag |

## 第一版阻塞项

当前只把以下项目视为完成第一版前必须继续实现：

1. B03/R03：现有签名证据已包含 Revision、AuditBundle、Agent 报告、构建 provenance 和日志摘要；继续通过受限传输加入可取得的原始日志、Profile、Source Manifest 文件及 License bundle。暂时不能取得的内容必须列为缺失，不能伪装已归档。
2. U01：在现有 Release 证据摘要基础上，为构建 Job 提供原始日志读取，并允许展开证据文档；第一版不要求复杂日志搜索。
3. A03/B02：提供可在没有真实付费调用时执行的 Agent/Fetch Doctor；真实 provider 与真实 source E2E 仍需部署者凭据和网络，若发布前无法执行必须保留为未验证范围。

B04 的性能闭环、跨设备恢复演练和外部通知冒烟很重要，但第一版可以在代码主路径、风险边界和手工操作说明完整时作为部署验收项，不继续扩展为高可用或复杂观测系统。
