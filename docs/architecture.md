# 系统架构

## 信任边界

- Controller 是策略权威，负责签发内部授权。
- 第一版信任 Worker 宿主机和 Worker daemon。
- AUR 文件、下载源码、构建 Guest、构建产物和软件包元数据均不可信。
- Publisher 负责校验产物，但不能访问仓库签名私钥。
- Signer 完全断网，只接受已签名的 `ReleaseAuthorization`。
- Archiver 独立保存不可变 Release 和回执，归档状态不影响 Release 已发布状态。

## 部署

系统由 Controller、Builder、Publisher 和 Archiver 四套 Docker Compose Stack 组成，每个 Worker 实例只承担一个角色。同一物理设备可以运行多套 Stack，但不能共享可写服务状态。

Builder daemon 在容器中通过 `/dev/kvm` 直接启动 QEMU，不获得 Docker Socket、libvirt Socket、TUN 或 privileged 权限。受限联网的 Fetch Guest 通过 Publisher 代理获取源码；全新的 Build Guest 不带网卡，只接收不可变且已经审计的输入。

第一版 Publisher 提供独立的无特权 Squid source-proxy 容器。QEMU 只把 Guest 内固定的 `10.0.2.100:8080` 转发到该代理；代理只转发 HTTP/HTTPS 并在 DNS 解析后拒绝私网、回环、链路本地和保留目标。它不与 Publisher Worker 或仓库共享可写卷，也不承担 pacoloco 的性能缓存职责。

控制流使用固定 host key 和 forced command 的 OpenSSH。大文件使用 rsync 在 Builder 与 Publisher、Publisher 与 Archiver 之间直接传输，并由短期有效的 Controller 签名 `TransferCapability` 授权。

每个 Worker 首次启动时在 SQLite Journal 中生成持久化实例 UUID，Controller 注册前必须通过固定 host key 探测并采用该 UUID；名称、角色、协议、Publisher writer epoch 或后续心跳 UUID 不一致时标记为 incompatible。TransferCapability 绑定源/目标 UUID、Attempt generation、writer epoch、完整文件清单和期限，Publisher 必须核对本地配置的 writer epoch。Builder 把清单中逐个复验的文件复制到只读 export 目录；SSH forced command 只接受固定 rsync sender 参数和 `/jobs/transfers/<capability UUID>`。Publisher 通过静态 UUID→SSH 端点映射和独立只读拉取密钥直接 rsync，落入 partial 目录，全部摘要复验后才 rename 为 landing。Controller 不转发包字节。

ReleaseAuthorization 包含上一稳定 Release 中未变化的 Artifact 与当前批次新 Artifact，因此每次交给 Signer 的都是完整仓库而非增量片段。Publisher 只把经 TransferCapability 验证的新包和上一已签名 hot set 中摘要一致的旧包送入 Signer。Signer 用 GPG 私钥和官方 repo-add 生成完整不可变输出；Publisher 仅持公钥，复验包、数据库、files 数据库与 Manifest 签名后，先提交 Release 目录和包文件，再更新签名与 files 链接，最后原子替换仓库 DB 链接。相同包名、版本但摘要不同会在 hot set 接管时失败关闭。

Release 提交后，Controller 从 Publisher 读取只含路径、大小和摘要的 Release 文件清单，签发绑定 Publisher、Archiver、writer epoch 和 Release ID 的 TransferCapability。Archiver 使用静态 Publisher UUID→SSH 地址及独立只读拉取密钥直接拉取，按完整文件集合复验后通过 `rsync --link-dest` 创建不可变快照。每个 Worker 首次启动还会在本地 Journal 生成持久化 Ed25519 身份密钥；Controller 注册时固定公钥，ArchiveReceipt 必须由对应 Archiver 签署并与 Release Manifest 及 Capability 文件集合完全一致。Controller 只有在 Receipt 验证通过后才调用源端导出清理，清理失败会保留待重试状态，不能通过提前删除释放空间。

服务端回滚使用短期 ReleaseRollbackAuthorization，只允许当前 writer epoch 的 Publisher 激活已经存在的不可变 Release。Publisher 在切换前重新验证 Manifest、包、db/files 数据库和全部 GPG 签名，不重新构建、运行 repo-add 或调用 Signer。Controller 单独保存当前 Release 指针，并从目标 Release Artifact 清单生成客户端 `pacman -U` 命令；服务端操作不会被描述成客户端自动降级。

Publisher 启动时从只读 secret 导入仓库 GPG 公钥，计算完整指纹并通过 Worker 身份响应交给 Controller 固定，同时把公钥作为普通只读下载文件发布到仓库。客户端引导 API 只在 Publisher 指纹已经注册后提供 pacman 配置和导入命令，页面始终要求用户在本地签名前人工核对完整指纹；AURsmith 仓库配置明确放在官方仓库之后。

Fetch Guest 实测官方依赖下载耗时并随 FetchResult 返回，Controller 按依赖记录下载字节、耗时、月使用次数和最近二十次构建出现率。优化器每七天最多评估一次：前二十次成功构建只观察；满足出现率或月使用次数且预计节省至少六十秒的官方依赖，连续两个周期后进入加入建议；已固化依赖连续三个低使用周期或三十天未使用后进入移除建议。AUR 依赖从不进入 Profile。UI 只生成可解释建议，实际不可变 Profile 仍必须由完整同步的 profile-builder 重建并通过 KVM fixture 后人工激活，失败时继续使用上一 Profile。

## 状态模型

`Revision`、`Job`、`Attempt`、`Artifact`、`Release` 和 `ArchiveCopy` 是相互独立的聚合。已提交的 Release 不会因为 ArchiveCopy 等待或失败而退回未发布状态。任务采用至少一次投递，Attempt token 用于保证结果接收幂等并拒绝迟到结果。

## 运维健康与背压

Controller 定期通过已经固定 host key 的 Worker `status` 命令获取角色、协议、实例身份、UTC 时间、cgroup v2、Builder KVM 能力以及角色数据卷所在文件系统的总量和可用量。Worker 只以固定参数执行 `/usr/bin/df`，不会接收 Controller 提供的命令或路径。原始状态快照和计算出的时钟偏差写入控制面，Doctor 将至少一个在线 Builder、Publisher、Archiver、已验证 Profile、仓库 GPG 指纹和四个 Agent Runner 作为运行前条件。

可用空间低于 15% 时产生 warning，低于 10% 时产生 critical。活动 Publisher 低于 10% 时，持久化的 `publication_backpressure` 同时阻止新 Job 调度和新 Release 授权，已经提交的仓库继续提供服务；恢复到阈值以上后自动解除。未获得有效 ArchiveReceipt 的已发布 Artifact 合计超过 20 GiB 时独立告警，不能通过把 Release 改回失败来隐藏欠归档状态。Worker 不可达、时钟偏差超过 60 秒、缺少 cgroup v2 或 Builder 缺少 KVM 都使用稳定 fingerprint 更新同一告警。

告警具有 `open → acknowledged → resolved` 生命周期。页面可查看和确认告警，结构化容器日志记录新打开的运维故障。可选 Webhook 使用 JSON 负载和 `X-AURsmith-Signature: sha256=<HMAC-SHA256>`，可选 ntfy 使用固定目标 URL；目标不能包含 URL 用户名或密码。通知先写 SQLite outbox，每个状态和通道只投递一次，失败最多尝试三次并保留最后错误，不会因为通知服务故障阻塞调度器。

`/api/v1/metrics` 汇总任务状态、成功 Attempt 的阶段平均耗时、Agent 调用/失败/成本、依赖下载与缓存命中，以及归档副本状态。第一版由认证后的 Web UI 消费该 JSON，不另行引入 Prometheus、Redis 或消息系统。

Controller 每 24 小时或按管理员请求执行一次控制面一致性备份。备份使用 SQLite `VACUUM INTO` 从 WAL 数据库生成单文件快照，随后执行 `PRAGMA integrity_check`、计算 SHA-256，并用 Controller Ed25519 身份签署版本化 `ControlPlaneBackup`。数据库文件和签名 Envelope 先在同一文件系统暂存、同步，再以目录 rename 提交；失败记录不会冒充 verified。控制面数据库保存密码哈希和业务状态但不保存 GPG、SSH、CA 或 Agent API 私钥，因此这些 secret 仍必须按首次向导要求另行离线备份。

每个 verified 控制面备份还会进入独立归档调度。Controller 根据自身 Ed25519 公钥确定一个稳定、非秘密的传输源 UUID，签发同时绑定 Backup ID、Archiver UUID、两份文件摘要和期限的 TransferCapability，并把最小导出目录只读暴露给同 Stack 的 `backup-ssh`。该 SSH sidecar 与 Worker 一样禁止 Shell、PTY 和转发，只允许 rsync sender 读取数据库和备份 Envelope。Archiver 通过静态 UUID→SSH 端点主动拉取，既复验 Capability 文件集合，又验证内部 `ControlPlaneBackup` 确由当前 Controller 签署，随后保存到 `control-plane-backups/<Backup ID>` 并返回自身签名的 `BackupArchiveReceipt`。Controller 只有核对 Receipt 身份、Backup ID 和完整文件集合后才标记独立归档完成并清理临时导出。

离线恢复命令先核对当前 Controller 公钥、Envelope、固定文件名、大小、摘要和 SQLite 完整性，再复制到目标文件系统复验。替换前把原数据库及 WAL/SHM 一并移动到带 UTC 时间和 Backup ID 的 `recovery` 目录，恢复中途失败时尝试放回原数据库。恢复要求先停止 Controller；在线 API 不提供数据库替换能力。

Archiver 每周对所有 Release ArchiveReceipt 和 BackupArchiveReceipt 执行一次集合巡检：复验 Receipt 自身签名，确认每个快照的文件集合、普通文件类型和大小完全一致。每九十天执行完整摘要巡检，在相同检查上重新计算所有文件 SHA-256。Archiver 用自身持久化 Ed25519 身份签署 `ArchiveInventory`；Controller 固定核对 Worker UUID、身份公钥和请求的巡检级别后才保存报告。发现任一损坏会产生 critical 告警，不能以更新 Receipt 或忽略多余文件来制造通过。

## AUR 同步与依赖闭包

Controller 不直接访问 AUR。浏览器请求由 Controller 认证后，经固定 argv 的 OpenSSH forced command 发给在线 Publisher；Publisher Worker 才能调用 AUR RPC 和 AUR Git。Builder 或 Archiver 收到同类命令会以角色错误拒绝。

搜索使用 AUR RPC v5。订阅时，Publisher 先执行有界浅克隆，以 40 位 AUR Git commit 固定 Revision，并通过 `git show HEAD:.SRCINFO` 读取静态元数据；该过程不执行或 `source` PKGBUILD。`.SRCINFO` 被折叠为 pkgbase、全部 split outputs、依赖类型、架构、Provider 和 source 清单。

Controller 在写数据库前遍历最多 64 个 AUR pkgbase 的依赖闭包。精确同名 AUR 依赖成为隐式订阅；虚拟依赖查询 `provides`，唯一候选可以解析，多个候选进入 `awaiting_provider_selection`。全部上游输入获取成功后，直接订阅、隐式引用、不可变 Revision、依赖边和 ReleaseBatch 才进入同一个控制面事务。循环依赖进入 `blocked_cycle`，不会猜测顺序。

单用户 Web UI 可以直接注册 Builder、Publisher 和 Archiver：提交名称、角色、SSH 端点、已人工核对的 host key 指纹和标签后，Controller 通过严格 known_hosts 连接并核对 Worker 持久化 UUID、角色、协议和身份签名公钥。依赖存在多个 Provider 时，包详情页展示候选并允许固定其中一个；选择会写入直接订阅并重新生成绑定 Provider 摘要的 Revision。

普通包的 AUR commit 变化就产生新 Revision。`-git` 包还从 `.SRCINFO` 的 `git+https` source 查询上游 commit；查询前拒绝私网、回环、链路本地和保留地址，并禁用 Git 重定向及 file/ext 协议。AUR commit、VCS commit 或固定 Provider 变化都会产生新 Revision，未开始发布的旧 Revision 标记为 `superseded`。split outputs 始终整体固定和构建，用户选择只表示客户端关注项。

Publisher 同时包装 Arch 官方仓库 JSON 接口。新订阅若与官方包同名会被拒绝；周期检查发现已有订阅进入官方仓库时，会暂停后续 AUR 更新、保留当前私有版本，并生成迁移告警和独立事件。

Controller 每六小时读取当前 Release 中 Artifact 构建时记录的官方依赖名称、版本和包摘要，再通过 Publisher 查询当前官方版本。版本变化只生成“建议重建”，明确不把版本比较描述成 ABI 兼容性证明；建议默认积累七天后合并为一个 ReleaseBatch，也可按包立即调度或关闭。重建覆盖受影响包及其反向依赖闭包，每个节点派生新的不可变 Revision，重新进入 Fetch、Source Manifest、三 Agent 审计和无网 Build 流程，绝不复用旧官方依赖快照。

每个 Build Job 在 Controller 签名的 JobSpec 中固定发布 `pkgrel`。某个完整上游版本第一次构建保留原 `pkgrel`；相同上游版本再次构建时，按历史成功产物派生 `.1`、`.2` 等单调递增后缀。Guest Agent 只改写 VM 内复制出的 PKGBUILD 工作副本，要求它恰好有一个可确定的顶层 `pkgrel=` 赋值；原始 AUR 快照不变。Controller 收到 BuildResult 后核对所有 split output 的 `.PKGINFO` 版本都与授权值一致，之后才把 `published_version` 写入 Revision。

每次成功刷新还会把上一份 package/revision 元数据与新快照比较。维护者变化、进入或离开 orphan 状态，以及规范化后的 source 域名集合变化分别写入 append-only 包事件；source 名称别名、本地补丁和 URL 大小写不会制造域名变化。AUR RPC 已找不到原 pkgbase 时，系统只报告“可能已删除、重命名或合并”，使用稳定告警 fingerprint 并保留当前 Release，不在缺少 AUR 合并证据时猜测具体目标。包详情 UI 同时展示 Revision、split outputs、依赖解析和这些生命周期事件。

## 发布安全

受影响的依赖闭包组成一个 `ReleaseBatch`。系统完整暂存该批次，根据完整 Manifest 签名并验证，然后最后切换仓库数据库。失败批次不能修改当前 Release。

Signer 是 Publisher Stack 内独立且 `network_mode: none` 的容器。Publisher 只能向只写 inbox 投递软件包和 Controller 签名的 `ReleaseAuthorization`，不能访问 Signer 的 GPG home；Signer 只能只读 inbox 并写独立 signed volume。Signer 再次验证 Controller Ed25519 公钥、授权期限、writer epoch 对应的授权内容、每个包的相对路径、大小、SHA-256 和 `.PKGINFO`，随后用固定 argv 调用 GPG 与官方 `repo-add`。包、仓库数据库和 Release Manifest 均生成 GPG 分离签名，完整 staging 最后通过目录 rename 提交。Publisher 仍须在公开前复验签名并执行 hot set/数据库切换；Signer 自身不拥有公开仓库卷。

Publisher 在把 Artifact 交给 Signer 前还会独立读取归档清单和 `.PKGINFO`：路径逃逸、重复条目、设备文件、FIFO、Socket，以及缺失或重复的 `.PKGINFO`、`.BUILDINFO`、`.MTREE` 会失败关闭；包名、版本和架构必须与 BuildResult 一致。INSTALL 脚本、pacman hook、systemd 单元、setuid/setgid 文件和内核模块作为风险事实记录，不因类别本身宣称恶意。结构化 `artifact-inspections.json` 随 Signer 输入进入断网边界，Signer核对报告数量和大小，并把其摘要写入 GPG 签名的 Release Manifest；因此 Archiver 会与 Release 一起保存这份发布前检查证据。

## Builder KVM 执行内核

Builder Worker 的 Journal 保存完整签名 JobSpec。执行循环以条件更新认领 queued Attempt，重启后会继续处理尚未认领的任务；同一 generation 的重放仍由 Journal 幂等规则约束。任务分为 Fetch、Build 和 ProfileFixture 三种，协议字段带默认值仅用于同一 major 版本内读取早期 Build 任务。

Profile 目录名是内容摘要，固定包含 `root.qcow2`、`vmlinuz-linux`、`initramfs-linux.img` 和 `profile-envelope.json`。Envelope 由 Controller Ed25519 密钥签署，内容摘要由三个文件的 Manifest、已安装包清单和创建时间确定，不采用无法实现的“包含自身摘要再求哈希”。Builder 在启动 VM 前验证签名、文件类型、大小和 SHA-256；Profile 的任何单字节变化都会拒绝任务。

每个 Attempt 使用独立 runtime 目录和 qcow2 overlay。QEMU 参数由 Rust 结构逐项构造，不经过 Shell。输入和输出分别由两个 virtiofsd 进程提供，输入为只读，输出为独立可写目录；控制通道使用 virtio-serial。Build 与 ProfileFixture 明确传入 `-nic none`。Fetch 使用 QEMU user networking 的 `restrict=on`，并且只有一条指向固定 Publisher source proxy IP:端口的 `guestfwd`，Guest 不能任意访问局域网或互联网。

Guest 完成后写出带类型的 FetchResult 或 BuildResult。Builder 重新核对 Job、Attempt、Revision、任务类型以及每个输出的安全相对路径、大小和摘要，才把 runtime 原子移动到 completed 区。超时、QEMU/virtiofsd 失败、取消和结果不匹配都会终止子进程并清理 staging/runtime；成功目录等待后续 TransferCapability 接管。Controller 对账时同时匹配 Attempt ID 和 generation，拒绝迟到或未知结果。

Worker 将 QEMU stdout/stderr 写入 Attempt runtime。失败时只把 QEMU 日志、Guest 结构化错误和 makepkg 日志复制到小型 `failed/<attempt>` 诊断目录，再删除 overlay、virtiofs Socket 和临时输入；成功时日志随 completed 结果保存。这样不会为排错长期保留大型写时复制磁盘，也不会把 Guest 错误压缩成无法定位的 `VM_FAILED`。

在 Builder 间 `TransferCapability` 传输尚未完成前，一个 ReleaseBatch 固定到第一个接单的 Builder。审计批准后的 Build Job 必须引用该节点上已经完成的 Fetch Attempt；Builder 再次验证 FetchResult、Source Manifest 和 completed 文件树后，才将 prepared source 复制进新的只读输入目录。这是显式的安全亲和策略：缺少原 Fetch Attempt 时任务保持不可调度，不允许 Build VM 重新联网获取源码。后续跨 Builder 调度只能通过同一摘要约束的 rsync Capability 扩展，不能绕过这条不变量。

Guest Agent 作为 Profile 根文件系统的 PID 1 运行，从内核命令行读取 Controller 公钥并再次验证只读输入中的 JobSpec Envelope。Fetch 任务只给 `makepkg --verifysource` 注入固定代理，复制并摘要准备后的完整源码树；Build 任务没有网卡，也不注入代理，以普通 `builder` 用户运行 `makepkg --cleanbuild`。输入中的特殊文件和越界符号链接会被拒绝。Guest 生成结果并同步输出卷后强制关机；失败时只写结构化错误，不尝试降级为宿主构建。

JobSpec 同时固定直接运行、构建和检查依赖及其来源。Fetch Guest 只对 `official` 依赖使用 pacman 下载，包文件进入 prepared source、完整 Source Manifest 和解析后的名称/版本/摘要清单；AUR 依赖不会伪装成官方依赖下载。Controller 使用 Fetch 实际结果替换预估的依赖快照摘要。按 DAG 构建时，Builder 从同批次已成功 Build Attempt 中重新验证并复制直接 AUR 依赖产物。Build Guest 以 PID 1 身份先用 pacman 离线安装两类依赖，再降权执行 makepkg；依赖缺失时确定性失败，绝不临时添加网卡。

Profile 构建器是按需启用的一次性 Compose 服务，不是常驻裸机工具。镜像构建阶段安装完整且同步的 Arch 根文件系统、嵌入 Guest Agent、生成显式包含 virtio 块设备、控制台和 virtiofs 驱动的 initramfs，并通过 `mkfs.ext4 -d` 和 `qemu-img` 生成 qcow2，无需 privileged 或宿主文件系统挂载。导出阶段断网、只读、零 capability，只产生固定四个文件和待 Controller 授权的 candidate。

开发期 KVM 冒烟使用协议 crate 下的 `prepare_kvm_fixture` example。它只使用固定测试密钥，在用户指定的临时目录中复用正式 Envelope 代码签署 ProfileFixture，不参与镜像或生产 Stack。验证结束必须删除临时运行目录；正式环境只能由 Controller 签发 Profile 和 JobSpec。

## 审计流水线与 Agent 边界

每个 Revision 首先形成只包含 AUR Git 跟踪文件、文件摘要、source 声明和确定性发现的不可变 `AuditPreScan`。预扫描命中路径逃逸、摘要不一致、私网 source URL 等绝对阻断时直接停止；未阻断时只允许创建 Fetch Job，绝不提前创建 Agent 调用。

Fetch VM 完成下载和校验后生成完整 Source Manifest，清单显式区分普通文件、目录和符号链接，并附带按固定规则选择的构建入口、脚本、安装、网络、权限和持久化相关文本。Builder 验证完整结果，Controller 再核对 Journal 中的结果摘要、Job、Attempt 和 Revision 身份，消费一次 `AuditPreScan`，形成内容寻址且不可再修改的最终 `AuditBundle`。此时才创建三个低成本 Agent 任务。覆盖说明必须明确列出完整清单和 Agent 实际读取的文件，并声明风险选读不能证明全部上游源码安全。

三个低成本 Runner 各自独立读取同一 Bundle。三票通过时直接批准；恰好两票通过时只创建一次高成本任务；不超过一票通过时转入人工队列。Runner 超时、不可用、非零退出、非法 JSON 都按未通过处理，每个调用仅重试一次。高成本 Runner 只收到原始 Bundle 和低成本报告的规范化异议，不接收隐藏推理过程，并且只有明确 `approve` 才能批准。人工决定绑定 Revision、Bundle 摘要和策略版本。

Runner 只支持 `codex` 与 `claude_code` 两种适配器，不接受用户提供任意可执行文件或 Shell 命令。两种 CLI 都从固定绝对路径和结构化 argv 启动，并使用 JSON Schema 约束最终输出。Codex 使用临时 `CODEX_HOME`、忽略用户配置与规则、只读 sandbox 和非交互审批；Claude Code 使用 bare/safe 配置、禁用全部工具、MCP、slash command、会话持久化和非必要遥测。一次性目录中只有输出 Schema，不挂载 Controller 数据库、仓库、Worker、SSH/GPG 密钥或 Docker Socket。

真实 provider API key 不进入 Runner 容器。独立 `agent-credential-gateway` 挂载 low/high 两份 Docker secret，Runner 只连接内部 Agent 网络中的网关路径并使用无权限占位令牌。网关按静态配置的 upstream Base URL 转发流式响应，删除调用方的认证头后重新注入对应的 bearer 或 `x-api-key`，且不接受请求指定目标主机。这样既支持 Codex/Claude Code 的自定义 provider 与 Base URL，也避免模型通过工具或环境读取真实密钥。网关是唯一拥有 Agent 外网的容器。

每次报告保存适配器、provider 名称、模型、CLI 版本、文件阅读范围、结构化发现、原始结构化输出、起止时间、退出状态、成本和报告摘要。API key、认证头和内部凭据不进入日志、数据库或报告。每日/月度调用次数与月度成本任一达到上限时，剩余任务进入人工队列，不会跳过审计。
