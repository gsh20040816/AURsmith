# 系统架构

## 信任边界

- Controller 是策略权威，负责签发内部授权。
- 第一版信任 Worker 宿主机和 Worker daemon。
- AUR 文件、下载源码、构建 Guest、构建产物和软件包元数据均不可信。
- Publisher 负责校验产物，但不能访问仓库签名私钥。
- Signer 完全断网，只接受已签名的 `ReleaseAuthorization`。
- Publisher 在原子发布之外负责短期历史保留，保留策略失败不影响当前 Release 服务。

## 部署

第一版由 Controller、Builder 和 Publisher 三套 Docker Compose Stack 组成，每个 Worker 实例只承担一个角色。外部 Archiver 协议保留为以后可选能力，但默认不部署也不参与调度。

Controller 进程同时提供 JSON API、SSE 和编译后的 React 静态页面；Publisher Worker 同时提供只读仓库 HTTP。两者只映射到宿主回环地址，由部署节点已有的 Caddy 统一终止 TLS。AURsmith 容器不读取宿主证书私钥，也不再维护内层 CA、Web Caddy 或仓库 Caddy。

Builder daemon 在容器中通过 `/dev/kvm` 直接启动 QEMU，不获得 Docker Socket、libvirt Socket、TUN 或 privileged 权限。受限联网的 Fetch Guest 通过 Publisher 代理获取源码；全新的 Build Guest 不带网卡，只接收不可变且已经审计的输入。

第一版 Publisher 保留 pacoloco 作为 Arch 官方包缓存，以 UID/GID 65532、只读根文件系统和独立缓存卷运行，不接触 source、Artifact 或签名密钥。宿主 Caddy 在 `/arch-cache/` 下反向代理 pacoloco。Fetch VM 使用 QEMU user networking 直接访问公网，不部署 HTTP source proxy；Build Guest 是否联网由 Builder 独立配置。

公网节点内的控制流继续使用固定 host key 和 forced command 的 OpenSSH。家庭网络中的 Builder 不暴露 SSH、HTTP 或任何公网入站端口，也不要求路由器端口映射：Builder 只通过 HTTPS 出站长轮询 Controller，使用持久 Ed25519 Worker 身份签署领取和上报消息；Controller 返回的任务仍是原有签名 `JobSpec`。大文件不经过 Controller，Builder 获得短期有效的 Controller 签名 `TransferCapability` 后，主动通过受限 rsync/SSH 推送到 Publisher 公网入口。Publisher 在公网节点本地提交完整不可变 Release，不再为第一版启动同机 Archiver。

每个 Worker 首次启动时在 SQLite Journal 中生成持久化实例 UUID 和 Ed25519 身份。公网可达 Worker 注册时由 Controller 通过固定 host key 探测；反向 Builder 则由管理员在 UI 中录入容器本地显示的 UUID、身份公钥、标签和 Profile，并完成一次挑战签名后启用。名称、角色、协议或后续身份不一致时标记为 incompatible。反向请求绑定 Worker UUID、随机 nonce、请求类型和短有效期；Controller 持久化 nonce，重复请求不能再次领取任务或推进状态。

`TransferCapability` 继续绑定源/目标 UUID、Attempt generation、writer epoch、完整文件清单和期限。反向 Builder 把清单中逐个复验的文件复制到只读 export 目录，通过 Publisher 的 OpenSSH forced command 主动上传到 Capability 专属 partial 目录。服务端直接复用 rsync 官方 `rrsync -wo /landing` 限制写入根目录和危险参数，AURsmith 不复制 rsync 的命令行协议解析；Capability 专属目录由传输前的准备操作创建，Publisher 在传输后复验完整清单与摘要，才 rename 为 landing。Controller 不代理或落盘包字节。公网可达源节点仍可沿用目标主动拉取模式，两种方向共用同一 Capability 数据模型和摘要门禁。

Publisher 的稳态存储以公开 hot store 为唯一包内容存储。Release 目录中的包名是同一文件系统内指向 hot store 内容的硬链接，只额外保存仓库数据库、Manifest、签名和纯文本检查记录。landing、Signer inbox 和 Signer output 都是短生命周期工作区；Release 完成原子提交并经 Publisher 复验后立即删除，若进程在提交与清理之间重启，则由 Journal 中的 `published` 状态幂等补做清理。这样正常稳态空间接近保留包集合本身，而不是发布阶段副本数量的倍数。

没有进入 Release 的 TransferCapability 在过期后自动回收。Publisher 清理前会读取所有 `queued` 和 `awaiting_signer` 授权，仍被活动 Release 完整引用的 transfer 即使已到期也暂不删除；其余 `receiving` 或 `verified` 工作区删除后在 Journal 标记为 `expired`，避免失败、被替代版本或早期部署遗留的传输永久占用磁盘。

ReleaseAuthorization 始终描述目标 Release 的完整最终 Artifact 清单，但 Publisher 只把经 TransferCapability 验证的变化包送入 Signer；未变化包只引用上一已提交 hot set。Signer 先验证当前仓库 db/files 的 GPG 签名并复制这两个小型数据库，再用官方 `repo-remove` 删除目标清单中已不存在的包，用官方 `repo-add` 只加入本批次变化包。首次发布才从空数据库加入全部包。Signer 最后只读取新数据库中的小型 `desc` 元数据，要求包名、版本和文件名与完整 ReleaseAuthorization 精确一致，然后重新签署 db/files 和 Manifest。Publisher 仅持公钥，复验变化包与新数据库签名后，先提交不可变 Release 目录和变化包，再更新签名与 files 链接，最后原子替换仓库 DB 链接。相同包名、版本但摘要不同仍在 hot set 接管时失败关闭。

清除操作同样创建不可变 Release，而不是删除当前仓库中的文件。Controller 汇总目标 pkgbase 全部历史 Revision 声明过的 split outputs，再从当前激活的 Release 移除这些名称，把清除清单写入 ReleaseAuthorization 和签名 Manifest；这样 output 改名后旧名称也不会残留。当前 Release 由控制面显式指针确定，服务端回滚后不会错误地以时间上更新但已停用的 Release 为基线。Signer 根据当前数据库与目标完整清单的差集调用官方 `repo-remove`，其他条目保持不变。清除最后一个包时生成标准空 gzip tar 数据库和 files 数据库后照常签名并原子切换；旧包文件仍按兼容窗口保留，但不再出现在当前仓库数据库中。

Release 提交并原子切换后，Publisher 从全部 GPG 签名 Manifest 计算保留集合：当前数据库指向的 Release 永久进入集合；其他 Release 只有同时满足“提交时间在最近 30 天内”和“包含对应 `pkgname` 最新 3 个不同 `package_version` 之一”才保留。超过 30 天的历史版本即使不足 3 个也删除，30 天内超过最新 3 个的更旧版本同样删除。证据文件随对应 Release 一起保留。任一 Manifest、签名、目录 ID 或当前数据库链接异常时，本轮清理整体停止，不能按文件名猜测后删除。

服务端回滚使用短期 ReleaseRollbackAuthorization，只允许当前 writer epoch 的 Publisher 激活已经存在的不可变 Release。Publisher 在切换前重新验证 Manifest、包、db/files 数据库和全部 GPG 签名，不重新构建、运行 repo-add 或调用 Signer。Controller 单独保存当前 Release 指针，并从目标 Release Artifact 清单生成客户端 `pacman -U` 命令；服务端操作不会被描述成客户端自动降级。

Publisher 启动时从只读 secret 导入仓库 GPG 公钥，计算完整指纹并通过 Worker 身份响应交给 Controller 固定，同时把公钥作为普通只读下载文件发布到仓库。客户端引导 API 只在 Publisher 指纹已经注册后提供 pacman 配置和导入命令，页面始终要求用户在本地签名前人工核对完整指纹；AURsmith 仓库配置明确放在官方仓库之后。

Fetch Guest 实测官方依赖下载耗时并随 FetchResult 返回，Controller 按依赖记录下载字节、耗时、月使用次数和最近二十次构建出现率。优化器每七天最多评估一次：前二十次成功构建只观察；满足出现率或月使用次数且预计节省至少六十秒的官方依赖，连续两个周期后进入加入建议；已固化依赖连续三个低使用周期或三十天未使用后进入移除建议。AUR 依赖从不进入 Profile。UI 只生成可解释建议，实际不可变 Profile 仍必须由完整同步的 profile-builder 重建并通过 KVM fixture 后人工激活，失败时继续使用上一 Profile。

## 状态模型

`Revision`、`Job`、`Attempt`、`Artifact`、`Release` 和 `ArchiveCopy` 是相互独立的聚合。已提交的 Release 不会因为 ArchiveCopy 等待或失败而退回未发布状态。任务采用至少一次投递，Attempt token 用于保证结果接收幂等并拒绝迟到结果。

## 运维健康与背压

Controller 对公网可达 Worker定期执行固定 host key 的 `status`；反向 Builder 则在每次长轮询中附带签名状态快照，空闲时默认 15 秒长轮询并在网络失败后指数退避，最长不超过 60 秒。快照包含角色、协议、实例身份、UTC 时间、cgroup v2、KVM 能力以及数据卷空间。Controller 根据最后上报时间判断 online/degraded/offline，不尝试回连 Builder。Doctor 将至少一个近期上报且具备 KVM 的 Builder、Publisher、已验证 Profile、仓库 GPG 指纹和四个 Agent Runner 作为运行前条件。

可用空间低于 15% 时产生 warning，低于 10% 时产生 critical。活动 Publisher 低于 10% 时，持久化的 `publication_backpressure` 同时阻止新 Job 调度和新 Release 授权，已经提交的仓库继续提供服务；恢复到阈值以上后自动解除。Worker 不可达、时钟偏差超过 60 秒、缺少 cgroup v2 或 Builder 缺少 KVM 都使用稳定 fingerprint 更新同一告警。

告警具有 `open → acknowledged → resolved` 生命周期。页面可查看和确认告警，结构化容器日志记录新打开的运维故障。可选 Webhook 使用 JSON 负载和 `X-AURsmith-Signature: sha256=<HMAC-SHA256>`，可选 ntfy 使用固定目标 URL；目标不能包含 URL 用户名或密码。通知先写 SQLite outbox，每个状态和通道只投递一次，失败最多尝试三次并保留最后错误，不会因为通知服务故障阻塞调度器。

`/api/v1/metrics` 汇总任务状态、成功 Attempt 的阶段平均耗时、Agent 调用/失败/成本、依赖下载与缓存命中，以及归档副本状态。第一版由认证后的 Web UI 消费该 JSON，不另行引入 Prometheus、Redis 或消息系统。

`/api/v1/events` 使用与普通 API 相同的管理员会话认证，并以 SSE 发送控制面增量快照。Controller 每两秒比较事件序号、Job/Release/Archive 更新时间和未解决告警数，只有状态变化时才发送 data frame；每十五秒发送 keep-alive 注释。浏览器断线使用 EventSource 原生重连，构建页收到变化后重新读取权威 JSON，不把 SSE 数据本身当作可写状态或完整日志存储。尚未确认的 open 告警同时显示在导航计数、全局横幅和总览待处理区；用户确认后从全局提醒移除，但仍保留在告警历史中。AUR 包从 RPC 索引消失时，UI 明确说明当前已发布版本仍保留，并提示确认删除、重命名或合并后迁移订阅。

Controller 每 24 小时或按管理员请求执行一次控制面一致性备份。备份使用 SQLite `VACUUM INTO` 从 WAL 数据库生成单文件快照，随后执行 `PRAGMA integrity_check`、计算 SHA-256，并用 Controller Ed25519 身份签署版本化 `ControlPlaneBackup`。数据库文件和签名 Envelope 先在同一文件系统暂存、同步，再以目录 rename 提交；失败记录不会冒充 verified。控制面数据库保存密码哈希和业务状态但不保存 GPG、SSH、CA 或 Agent API 私钥，因此这些 secret 仍必须按首次向导要求另行离线备份。

每个 verified 控制面备份由 SQLite 原生快照、摘要和 Controller 签名 Envelope 组成，保存在 Controller 持久卷。第一版不自动复制到独立故障域，因此初始化向导和设置页必须继续提示离线保存 Controller、GPG、CA 私钥和管理员恢复材料。

Doctor 不通过付费模型请求伪造“Agent 可用”。每个 Agent Runner 的 `/healthz` 只验证 Codex/Claude Code 固定 CLI 文件、adapter/provider/model 配置，以及到凭据网关的 TCP；凭据网关在启动时已经验证 API key secret 和 provider HTTPS URL。Controller 实际请求三个低成本和一个高成本 Runner 的健康端点。Publisher 的 `publisher-doctor` 同时执行无结果也合法的 AUR RPC 查询、经配置的 source proxy 请求公开 Arch HTTPS 文件，并读取 pacoloco `/metrics`；它不执行 PKGBUILD，也不给 Build VM 网络。

离线恢复命令先核对当前 Controller 公钥、Envelope、固定文件名、大小、摘要和 SQLite 完整性，再复制到目标文件系统复验。替换前把原数据库及 WAL/SHM 一并移动到带 UTC 时间和 Backup ID 的 `recovery` 目录，恢复中途失败时尝试放回原数据库。恢复要求先停止 Controller；在线 API 不提供数据库替换能力。

外部 Archiver 的 TransferCapability、ArchiveReceipt 和库存巡检协议仍保留在代码中，只有显式设置 `AURSMITH_EXTERNAL_ARCHIVER_ENABLED=true` 才调度，属于第一版默认拓扑之外的可选能力。

## AUR 同步与依赖闭包

Controller 不直接访问 AUR。浏览器请求由 Controller 认证后，经固定 argv 的 OpenSSH forced command 发给在线 Publisher；Publisher Worker 才能调用 AUR RPC 和 AUR Git。Builder 或 Archiver 收到同类命令会以角色错误拒绝。

搜索使用 AUR RPC v5。订阅时，Publisher 先执行有界浅克隆，以 40 位 AUR Git commit 固定 Revision，并通过 `git show HEAD:.SRCINFO` 读取静态元数据；该过程不执行或 `source` PKGBUILD。`.SRCINFO` 被折叠为 pkgbase、全部 split outputs、依赖类型、架构、Provider 和 source 清单。

Controller 在写数据库前遍历最多 64 个 AUR pkgbase 的依赖闭包。精确同名 AUR 依赖成为隐式订阅；虚拟依赖查询 `provides`，唯一候选可以解析，多个候选进入 `awaiting_provider_selection`。全部上游输入获取成功后，直接订阅、隐式引用、不可变 Revision、依赖边和 ReleaseBatch 才进入同一个控制面事务。循环依赖进入 `blocked_cycle`，不会猜测顺序。

AUR 依赖以 `subscription_references` 保存有向边，隐式订阅的引用数由这些边计算。退订直接订阅时，Controller 从全部仍处于 active/paused 的直接订阅重新计算可达闭包；不可达隐式节点的下游边一并移除，整个不可达闭包进入 `retained_without_references` 保留期。共享依赖只要仍可从任一直接订阅到达，就继续保持 active，不能因删除其中一个引用方而误删。

单用户 Web UI 第一版直接注册 Builder 和 Publisher：提交名称、连接模式、SSH 端点、已人工核对的 host key 指纹和标签后，Controller 核对 Worker 持久化 UUID、角色、协议和身份签名公钥。依赖存在多个 Provider 时，包详情页展示候选并允许固定其中一个；选择会写入直接订阅并重新生成绑定 Provider 摘要的 Revision。

普通包的 AUR commit 变化就产生新 Revision。`-git` 包还从 `.SRCINFO` 的 `git+https` source 查询上游 commit；查询前拒绝私网、回环、链路本地和保留地址，并禁用 Git 重定向及 file/ext 协议。AUR commit、VCS commit 或固定 Provider 变化都会产生新 Revision，未开始发布的旧 Revision 标记为 `superseded`。split outputs 始终整体固定和构建，用户选择只表示客户端关注项。

依赖分类同时检查 Provider 解析结果和当前 pkgbase 的完整 split output 集合。依赖名称若由同一 pkgbase 的另一个 output 提供，只作为本次 split 构建的内部关系，不交给 pacman 从官方仓库下载，也不创建对自身的隐式订阅；其他 AUR 依赖仍由批次内已完成的依赖 Artifact 提供。

第一版构建架构固定为 x86_64。解析 `.SRCINFO` 时，`depends`、`makedepends`、`checkdepends`、`optdepends` 和 `provides` 分别与对应的 `_x86_64` 字段合并，明确忽略 `_i686`、`_aarch64` 等其他架构字段；这样 CrossOver 等声明 `lib32-*` 运行依赖的 x86_64 包会在 Fetch 阶段获得完整官方依赖闭包。

Agent 审计身份只由 `pkgbase`、AUR Git commit（即完整 AUR 包装仓库文件）、固定 VCS commit、Provider 选择和审计策略版本组成。以上内容不变且已有自动批准结果时，后续手工重建直接复用该审计；Build Profile、官方依赖快照、下载缓存、内部 GPG 公钥包和其他 Fetch 实现元数据变化不触发重新审计。它们仍进入构建 provenance 和确定性校验，但不属于“AUR 打包脚本是否变化”的判定。

Git VCS commit 变化时，Controller 把上一 Revision 的 commit 交给 Publisher。Publisher 先用固定 IP 的 smart HTTP 广告取得当前 ref，再以 `protocol.file/ext=never`、禁重定向、`GIT_TERMINAL_PROMPT=0` 和 `http.curloptResolve` 固定同一公共地址，仅获取该 ref 的无 blob 历史并执行 `merge-base --is-ancestor`。正常快进自动继续；上一 commit 不存在或不是祖先时，不创建新 Revision，而是写入 `vcs_history_rewrite_detected` 事件、critical 告警和待处理人工动作。管理员在包详情中批准或拒绝精确的 previous/current commit 对；批准不能永久信任包，下一次不同重写仍重新阻断。

Publisher 同时包装 Arch 官方仓库 JSON 接口。新订阅若与官方包同名会被拒绝；周期检查发现已有订阅进入官方仓库时，会暂停后续 AUR 更新、保留当前私有版本，并生成迁移告警和独立事件。

Controller 每六小时读取当前 Release 中 Artifact 构建时记录的官方依赖名称、版本和包摘要，再通过 Publisher 查询当前官方版本。版本变化只生成“建议重建”，明确不把版本比较描述成 ABI 兼容性证明；建议默认积累七天后合并为一个 ReleaseBatch，也可按包立即调度或关闭。重建覆盖受影响包及其反向依赖闭包，每个节点派生新的不可变 Revision，重新进入 Fetch、Source Manifest、三 Agent 审计和按 Builder 配置选择网络模式的 Build 流程，绝不复用旧官方依赖快照。

用户也可以从包详情手工重建。控制面不会复活旧失败 Job 或覆盖 Attempt，而是以 `manual_rebuild` 原因创建新的 ReleaseBatch 和递增 rebuild Revision；它重新执行 Fetch、审计和 Build，并使用调度时最新的已验证 Profile。这样更换 Profile、构建网络策略或工具链后可以安全重试，同时保留旧失败证据。

每个 Build Job 在 Controller 签名的 JobSpec 中固定发布 `pkgrel`。某个完整上游版本第一次构建保留原 `pkgrel`；相同上游版本再次构建时，按历史成功产物派生 `.1`、`.2` 等单调递增后缀。Guest Agent 只改写 VM 内复制出的 PKGBUILD 工作副本，要求它恰好有一个可确定的顶层 `pkgrel=` 赋值；原始 AUR 快照不变。Controller 收到 BuildResult 后核对所有 split output 的 `.PKGINFO` 版本都与授权值一致，之后才把 `published_version` 写入 Revision。

每次成功刷新还会把上一份 package/revision 元数据与新快照比较。维护者变化、进入或离开 orphan 状态，以及规范化后的 source 域名集合变化分别写入 append-only 包事件；source 名称别名、本地补丁和 URL 大小写不会制造域名变化。AUR RPC 已找不到原 pkgbase 时，系统只报告“可能已删除、重命名或合并”，使用稳定告警 fingerprint 并保留当前 Release，不在缺少 AUR 合并证据时猜测具体目标。包详情 UI 同时展示 Revision、split outputs、依赖解析和这些生命周期事件。

## 发布安全

`authorize-release` 只验证 Controller 签名、writer epoch 和 Release 元数据，将不可变授权写入 Publisher 的 SQLite Journal 后立即返回。包内容检查、Signer inbox 物化、签名结果校验和原子提交由 Publisher 单实例后台对账循环异步执行；Controller 通过 `query-release` 查询 `queued → awaiting_signer → published/failed`。因此大包检查不会占用 SSH 控制请求，也不需要为控制通道设置分钟级超时或引入额外队列。

受影响的依赖闭包组成一个 `ReleaseBatch`。系统完整暂存该批次，根据完整 Manifest 签名并验证，然后最后切换仓库数据库。失败批次不能修改当前 Release。

Signer 是 Publisher Stack 内独立且 `network_mode: none` 的容器。Publisher 只能向只写 inbox 投递变化软件包和 Controller 签名的 `ReleaseAuthorization`，不能访问 Signer 的 GPG home；Signer 只能只读 inbox、当前公开仓库和写独立 signed volume。Signer 再次验证 Controller Ed25519 公钥、授权期限、writer epoch、变化包的相对路径、大小、SHA-256 和 `.PKGINFO`，并验证作为增量基线的当前 db/files GPG 签名；未变化的大包不重复读取。随后以固定 argv 调用 GPG、官方 `repo-add` 和 `repo-remove`。变化包、仓库数据库和 Release Manifest 均生成 GPG 分离签名，完整 staging 最后通过目录 rename 提交。Publisher 仍须在公开前复验变化内容和数据库签名并执行 hot set/数据库切换；Signer 的公开仓库卷保持只读。

Release 明确授权 Signer 保证仓库中存在 `aursmith-keyring` 系统包。Signer 从当前仓库私钥导出公钥，把 `aursmith.gpg`、`aursmith-trusted` 和空的 `aursmith-revoked` 安装到 `/usr/share/pacman/keyrings/`；`.INSTALL` 在安装和升级时调用 `pacman-key --populate aursmith`。若当前 hot store 中已有包内公钥指纹与仓库私钥一致的有效 keyring，后续 Release 直接复用原包及签名，不改变版本；只有首次发布或仓库公钥、信任内容实际变化时才生成新版本。定期检查只核对内容，内容不变不发布空更新。Publisher 不盲信该派生产物：它从包内重新读取 `.PKGINFO`、公钥主指纹、ownertrust、revoked 清单和安装脚本，并要求主指纹与启动时固定的仓库公钥一致，之后才复制到公开 hot set。`aursmith-keyring` 是保留包名，AUR Artifact 不能覆盖。首次客户端引导仍必须人工核对一次指纹；keyring 包负责之后的持久安装和密钥轮换，不能替代信任根引导。

Publisher 不重复审查 `makepkg` 已生成的软件包内容。Builder 导出和 Publisher 接收变化包时各在信任边界核对一次 Controller 授权、Attempt、普通文件、大小和 SHA-256；接收目录原子接管后，后续 inbox、Release 和 hot set 物化只使用大小、Journal 状态及本地硬链接，不再重复顺序读取同一大文件。Signer 核对变化输入和 ReleaseAuthorization，并交给官方 `repo-add/repo-remove` 更新仓库数据库。工具能接受且签名成功的包即可发布，不额外扫描 ELF、INSTALL、hook、systemd、setuid、capability 或内核模块。兼容旧 ReleaseManifest 的 `artifact-inspections.json` 字段暂保留为空数组，不参与门禁。

Controller 在 Attempt 对账事务中保存完整 GuestResult 和有界诊断日志，而不是只保留最终 Artifact 行。Builder 对 QEMU stdout/stderr、fetch、build 和 Guest 错误文件记录完整大小与 SHA-256；每个普通日志最多内嵌前 128 KiB UTF-8/Base64 内容，超过 64 MiB 的异常日志明确记录省略原因，不能让不可信输出撑爆控制消息。创建 ReleaseAuthorization 时，系统收集 ReleaseBatch、参与 Revision、AuditBundle、成功 Agent 报告、Controller 签名 Profile Envelope 和 Job 证据，形成版本化 `ReleaseEvidence`；每条记录都有基于规范 JSON 的 SHA-256。结构化证据最多 10000 条、序列化后最多 16 MiB，超限时阻止发布并要求人工拆分，不能静默截断。

成功 Build 不再为每个包重复归档完整 Profile 和 source tree。Controller 保存限长的 Build/QEMU 纯文本日志、结构化 BuildResult、签名 JobSpec 摘要、Profile digest、Source Manifest digest 和 AUR/VCS commit；Publisher 传输和保留二进制包、签名、仓库数据库、Release Manifest 与这些小型结构化引用。相同 digest 的 Profile 和 source 可在 Builder 缓存中复用，但不作为每个 Release 的大体积附件上传。

Publisher 和断网 Signer 不接收每包重复的 Profile/source 归档。Release 保存包、签名、数据库、Manifest、检查报告及小型结构化 digest 引用；管理员从 Job 页面查看有界在线日志。

## Builder KVM 执行内核

Builder Worker 的 Journal 保存完整签名 JobSpec。执行循环以条件更新认领 queued Attempt，重启后会继续处理尚未认领的任务；同一 generation 的重放仍由 Journal 幂等规则约束。任务分为 Fetch、Build 和 ProfileFixture 三种，协议字段带默认值仅用于同一 major 版本内读取早期 Build 任务。

Profile 目录名是内容摘要，固定包含 `root.qcow2`、`vmlinuz-linux`、`initramfs-linux.img` 和 `profile-envelope.json`。Envelope 由 Controller Ed25519 密钥签署，内容摘要由三个文件的 Manifest、已安装包清单、创建时间和可选的官方仓库镜像确定，不采用无法实现的“包含自身摘要再求哈希”。旧 Profile 未声明镜像时仍按原摘要读取；新 Profile 一旦声明镜像，修改地址就必须产生新摘要。Builder 在启动 VM 前验证签名、文件类型、大小和 SHA-256；Profile 的任何单字节变化都会拒绝任务。

每个 Attempt 使用独立 runtime 目录和 qcow2 overlay。QEMU 参数由 Rust 结构逐项构造，不经过 Shell。输入和输出通过 QEMU 内置 virtio-9p 的 `mapped-xattr` 模式提供，输入 `fsdev` 额外固定 `readonly=on`，输出只指向该 Attempt 的独立目录；不启动需要 root 或额外 capability 的宿主文件共享 daemon。控制通道使用 virtio-serial。ProfileFixture 明确传入 `-nic none`；Fetch 使用直接 QEMU user networking；Build 根据配置使用 `-nic none` 或直接 QEMU user networking。

Guest 完成后写出带类型的 FetchResult 或 BuildResult。Builder 重新核对 Job、Attempt、Revision、任务类型以及每个输出的安全相对路径、大小和摘要，删除 overlay 与控制 Socket 后，才把日志和输出原子移动到 completed 区。超时、QEMU 失败、取消和结果不匹配都会终止子进程并清理 staging/runtime；成功目录只保留到依赖它的 ReleaseBatch 终止。成功 Attempt 由 Controller 在反向租约中明确返回可释放 UUID；失败或取消 Attempt 没有可发布 Artifact，在结果被 Controller 确认接收后由 Builder 本地释放。Builder 均幂等删除对应工作目录并保留 Journal 终态。活动审计或构建仍引用的 Fetch/依赖产物不会按时间猜测清理。Controller 对账同时匹配 Attempt ID 和 generation，拒绝迟到或未知结果。

Controller 对基础设施错误和 `FETCH_DEPENDENCY_DOWNLOAD_FAILED` 自动创建新 Attempt：`BUILDER_INFRASTRUCTURE`、VM 超时/异常、Guest 结果缺失、结果暂不可读、Worker 不可达或 Fetch Guest 的 pacman 下载耗尽。Profile 固定的是基础 packages 名称列表和 pacman、makepkg、systemd、Guest Agent 等配置，不固定软件包版本。共享 `root.qcow2` 只是启动缓存；Fetch Guest 每次从当前镜像刷新数据库，并用同一次 pacman 事务下载完整系统升级集合以及本次 `depends`、`makedepends`、`checkdepends` 的最新闭包。Build Guest 在任务私有 overlay 中离线安装整个集合后再运行 makepkg，因此基础系统和构建依赖都跟随任务开始时的镜像上游，同时不让并发任务共同修改启动缓存。该下载最多尝试三次，全部失败后才报告专用错误码；其他 `GUEST_FETCH_FAILED` 仍视为确定性错误，不自动循环。Build 日志出现 DNS、路由、连接失败或 NuGet `NU1301` 时单独标记 `NETWORK_DURING_BUILD`，同样不自动循环构建。generation 0 和 1 分别在 5 秒、10 秒后重试；generation 2 再失败即终止，因此一个 Fetch Job 最多三个 Attempt。输入摘要、Profile、身份、审计和确定性 Build 错误不会自动循环。SSH 提交或对账状态不明确时，Job 先进入 `uncertain`，三十分钟内不重新派发；届时再次查询 Journal，仍不可达才按同一 generation 上限重试，耗尽后打开稳定 fingerprint 告警。

Guest 的最小 pacman 配置固定启用 Arch 官方 `core`、`extra` 和 `multilib`，因此 x86_64 AUR 包声明的 `lib32-*` 依赖仍从配置的同一镜像获取。Profile 固定仓库集合与配置，但每次 Fetch 继续同步这些仓库的最新数据库和依赖版本。

反向 Builder 第一版每次只租约一个未结束 Job。只要同一 Worker 仍有 `dispatched`、`running` 或 `uncertain` Job，Controller 就保留后续任务为 queued，不提前签发 JobSpec。这样 JobSpec 的十分钟有效期只覆盖传输和启动，不会在 Worker 本地等待队列中消耗；前一结果完成对账后才签发下一任务。

第一版自动创建的 Build Job 默认分配 4 个 vCPU、8 GiB 内存、32 GiB 临时磁盘和 1 小时超时；Fetch Job 仍保持 1 个 vCPU。QEMU 按签名 JobSpec 生成 `-smp 4`，Ninja、CMake 等会根据 Guest 可见 CPU 数自动并行。8 GiB 内存用于避免大型并行 C++ 编译在无 Swap Guest 中因页回收失去进展。已经启动的 VM 不热改资源，默认值只作用于修改后新创建的 Build Job。

Worker 将 QEMU stdout/stderr 写入 Attempt runtime。失败时只把 QEMU 日志、Guest 结构化错误和 makepkg 日志复制到小型 `failed/<attempt>` 诊断目录，再删除 overlay、控制 Socket和临时输入；成功时日志暂随 completed 结果保存，overlay 同样先删除。Controller 持久化有界日志和最终结果；上报确认后立即释放失败/取消工作区，批次终止后释放成功工作区，避免桌面 Builder 逐次累积源码与二进制副本，也不会把 Guest 错误压缩成无法定位的 `VM_FAILED`。

在 Builder 间 `TransferCapability` 传输尚未完成前，一个 ReleaseBatch 固定到第一个接单的 Builder。审计批准后的 Build Job 必须引用该节点上已经完成的 Fetch Attempt；Builder 再次验证 FetchResult、Source Manifest 和 completed 文件树后，才将 prepared source 复制进新的只读输入目录。Build VM 即使启用公网，也不能省略或替代已审计的 Fetch 输入。后续跨 Builder 调度只能通过同一摘要约束的 rsync Capability 扩展，不能绕过这条不变量。

Guest Profile 是安装了 `base`、`linux`、`base-devel` 和 `devtools` 的通用 Arch Linux 构建机，并由动态 Profile 额外预装高频官方构建依赖。标准 systemd 作为 PID 1，负责设备初始化、挂载和孤儿进程回收；Guest Agent 只是一个一次性 systemd service，不实现 init。Guest Agent 从内核命令行读取 Controller 公钥并再次验证只读输入中的 JobSpec Envelope。Fetch Guest 与 Build Guest 按部署配置直接访问公网。Fetch 执行 `makepkg --verifysource` 和官方依赖下载；Build 以普通 `builder` 用户运行 `makepkg --cleanbuild`。首次发布完全使用 PKGBUILD 自己的 pkgrel；只有同上游版本的本地重建才在工作副本生成 `.1`、`.2` 子版本。`makepkg` 成功且产生预期 split outputs 即视为构建成功，不再执行 `.SRCINFO` 对账、namcap 或 Publisher 内容复检。Guest Agent 生成结果并同步输出卷后关机；失败时只写结构化错误，不尝试降级为宿主构建。

JobSpec 同时固定直接运行、构建和检查依赖及其来源。Fetch Guest 只对 `official` 依赖使用 pacman 下载，并使用不可变 Profile 内已授权的 Arch HTTPS 镜像；包文件进入 prepared source、完整 Source Manifest 和解析后的名称/版本/摘要清单。AUR 依赖不会伪装成官方依赖下载。Controller 使用 Fetch 实际结果替换预估的依赖快照摘要。按 DAG 构建时，Builder 从同批次已成功 Build Attempt 中重新验证并复制直接 AUR 依赖产物。Build Guest 以 PID 1 身份先用 pacman 安装两类已固定依赖，再降权执行 makepkg；联网模式允许构建系统按上游脚本获取 NuGet 等生态依赖，但不会改变已固定的 Arch/AUR 依赖快照。BuildResult 通过 Profile 摘要固定镜像配置，并单独记录网络模式。

pacoloco 暴露的请求、命中、未命中、错误、缓存字节和包数量由 Publisher Worker 读取并进入签名身份心跳，Controller 指标 API 展示当前活动 Publisher 的累计值。Profile 优化仍以每个 Fetch 的依赖出现频率、下载字节和耗时为决策依据；全局缓存命中率用于解释网络收益，不把无法精确归属到单包的全局计数伪造成逐包命中。

Profile 构建器是按需启用的一次性 Compose 服务，不是常驻裸机工具。镜像构建阶段使用部署者选择的 Arch HTTPS 镜像安装完整且同步的 Arch 根文件系统，把同一 mirrorlist 写入 Guest，嵌入 Guest Agent、生成显式包含 virtio 块设备、控制台和 9p 驱动的 initramfs，并通过 `mkfs.ext4 -d` 和 `qemu-img` 生成 qcow2，无需 privileged 或宿主文件系统挂载。导出阶段断网、只读、零 capability，只产生固定四个文件和待 Controller 授权的 candidate；candidate 明确记录镜像地址。

Profile 页面接受 profile-builder 生成的 `profile-candidate.json`，通过认证 API 得到 Controller 签名 Envelope 并提供下载；大体积 qcow2、内核和 initramfs 不经过浏览器，仍由管理员复制到 Builder Profile 卷。Builder 心跳发现对应摘要目录后才有资格接收 fixture Job，fixture 成功前 UI 和 API 都拒绝激活。已激活 Profile 可通过正式接口停用并记录事件，但系统至少保留一个 active Profile，避免后续任务全部失去可用构建环境。

开发期 KVM 冒烟使用协议 crate 下的 `prepare_kvm_fixture` example。它只使用固定测试密钥，在用户指定的临时目录中复用正式 Envelope 代码签署 ProfileFixture，不参与镜像或生产 Stack。验证结束必须删除临时运行目录；正式环境只能由 Controller 签发 Profile 和 JobSpec。

## 审计流水线与 Agent 边界

每个 Revision 首先形成只包含 AUR Git 跟踪文件、文件摘要、source 声明和确定性发现的不可变 `AuditPreScan`。预扫描命中路径逃逸、摘要不一致等可由输入本身证明的违规时直接停止；包装脚本或安装脚本中仅出现私网、回环地址文字属于可疑线索，交给 Agent 结合用途判断。Fetch 实际请求私网、回环、链路本地或未授权目标时仍由网络入口确定性拒绝。未阻断时只允许创建 Fetch Job，绝不提前创建 Agent 调用。

Fetch VM 完成下载和校验后生成完整 Source Manifest，清单显式区分普通文件、目录和符号链接，并附带按固定规则选择的构建入口、脚本、安装、网络、权限和持久化相关文本。Builder 验证完整结果，Controller 再核对 Journal 中的结果摘要、Job、Attempt 和 Revision 身份，消费一次 `AuditPreScan`，形成内容寻址且不可再修改的最终 `AuditBundle`。此时才创建三个低成本 Agent 任务。覆盖说明必须明确列出完整清单和 Agent 实际读取的文件，并声明风险选读不能证明全部上游源码安全。

三个低成本 Runner 各自独立读取同一 Bundle。三票通过时直接批准；恰好两票通过时只创建一次高成本任务；不超过一票通过时转入人工队列。Runner 超时、不可用、非零退出、非法 JSON 都按未通过处理，每个调用仅重试一次。高成本 Runner 只收到原始 Bundle 和低成本报告的规范化异议，不接收隐藏推理过程，并且只有明确 `approve` 才能批准。人工决定绑定 Revision、Bundle 摘要和策略版本。

Runner 只支持 `codex` 与 `claude_code` 两种适配器，不接受用户提供任意可执行文件或 Shell 命令。两种 CLI 都从固定绝对路径和结构化 argv 启动，并使用 JSON Schema 约束最终输出。Codex 使用临时 `CODEX_HOME`、忽略用户配置与规则、只读 sandbox 和非交互审批；可选思考强度只接受 `minimal/low/medium/high/xhigh/max` 白名单并作为独立 `model_reasoning_effort` 配置参数传入。Claude Code 使用 bare/safe 配置、禁用全部工具、MCP、slash command、会话持久化和非必要遥测。一次性目录中只有输出 Schema，不挂载 Controller 数据库、仓库、Worker、SSH/GPG 密钥或 Docker Socket。

真实 provider API key 不进入 Runner 容器。独立 `agent-credential-gateway` 挂载 low-1、low-2、low-3 和 high 四份 Docker secret；三个低成本路由各自固定 provider Base URL、认证方式和密钥。Runner 只连接内部 Agent 网络中的专属网关路径并使用无权限占位令牌。网关删除调用方认证头后重新注入对应凭据，且不接受请求指定目标主机。这样既确保三路审计配置彼此独立，也避免模型通过工具或环境读取真实密钥。网关是唯一拥有 Agent 外网的容器。

每次报告保存适配器、provider 名称、模型、CLI 版本、文件阅读范围、结构化发现、原始结构化输出、起止时间、退出状态、成本和报告摘要。API key、认证头和内部凭据不进入日志、数据库或报告。每日/月度调用次数与月度成本任一达到上限时，剩余任务进入人工队列，不会跳过审计。

设置 API 只允许在登录后读取安全摘要，修改三项 Agent 预算和随机高成本复查率。运行时覆盖值保存在 SQLite `system_settings`，Agent 调度每次执行前读取，因此无需重启 Controller；预算值为 0 表示立即停止新的自动 Agent 调用。随机复查以基点表示，默认 0；非零时按 AuditBundle SHA-256 确定性抽样，命中的三票通过项进入一次高成本审计，只有明确通过才放行。适配器、provider、模型和 Base URL 仍由 Compose 环境固定，API key 仍只来自凭据网关 secret。设置 API 只返回 Runner 数量、支持的适配器及“是否配置”状态，绝不返回 key、认证头或 secret 路径内容。

审计结果按固定内容复用。新 Revision 完成 Fetch 后，Controller 仅在 AUR commit、VCS commit、完整 Source Manifest、Provider 选择和审计策略版本都与历史自动通过项一致时复用结论，不创建新的 Agent Run；新 AuditBundle 明确记录来源 Bundle、复用原因和事件。人工批准仍只绑定原 Revision，不能被复用。任一固定输入或策略变化都必须重新执行三个低成本 Agent。

## 无特权 Web 与控制面容器

Controller 镜像在切换到 UID/GID 10001 前预创建 `/run/aursmith`，保证第一次挂载空的命名卷后仍能创建 Unix Socket；数据库、运行目录和备份目录分别使用独立卷。Web 与仓库服务使用 AURsmith 派生的 Caddy 镜像，构建时移除上游二进制的 `cap_net_bind_service` 文件 capability，并以同一固定非 root 用户运行。两者只监听 8080 等非特权端口，由宿主端口映射承担对外 80/443。

Controller、Publisher 和 Signer 常驻服务均保持只读根文件系统、`cap_drop: ALL` 和 `no-new-privileges`。Controller 与 Publisher 的 HTTP 监听位于相同业务进程中，只通过宿主回环端口暴露给外层 Caddy；不再存在需要额外可写目录、内部 CA 卷或公开仓库只读共享卷的 Caddy sidecar。
