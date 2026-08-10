# 验证记录

本文件记录实际执行过的集成验证。单元测试数量和外部软件版本会随提交变化，因此每条记录必须绑定源码提交或工作树状态，不能把局部冒烟描述成完整验收。

## 2026-08-10：KVM ProfileFixture

- 验证对象：提交 `8f9dc06` 之后的工作树，包含随后待提交的 QEMU 内存与诊断修复。
- 宿主能力：`/dev/kvm` 可用，QEMU、qemu-img 和 `/usr/lib/virtiofsd` 均由宿主提供。
- Profile：通过非 privileged、断网导出容器重建 Arch rootfs、Linux 内核、initramfs 和最新版 Guest Agent。
- 授权：`prepare_kvm_fixture` 使用正式 Ed25519/CBOR Envelope 实现和固定测试密钥，在临时目录生成 Profile 与 ProfileFixture JobSpec；没有使用生产密钥。
- 执行：真实 Worker daemon 创建 qcow2 overlay、两个只限 Attempt 目录的 virtio-9p 通道和无网卡 KVM VM；Guest 再次验证 Controller 签名，以普通 `builder` 用户执行 `makepkg`。
- 结果：Job `33246641-748c-42d7-ad33-017a3223d307` 成功，Attempt `e238c934-c38b-4799-9579-5f418f3cff6e` 返回 `profile_fixture`；生成 `aursmith-profile-fixture-1-1-any.pkg.tar.zst`，大小 1255 字节，SHA-256 为 `a09eedb98a2fdb0630730a23afc5d57c3b8ce45636f359be3436d43714aaa2e7`；provenance 明确记录 `network=none`。
- 实际发现并修复：QEMU memfd/NUMA 后端缺少匹配的 `-m`；最小 Arch rootfs 的 os-release 位于 `/usr/lib/os-release`；失败 VM 日志此前会随 runtime 清理而丢失。
- 未覆盖：这次只验证 ProfileFixture，不代表 Fetch 代理、真实 AUR source、批次内 AUR 依赖、Publisher、Signer 或 Archiver 已完成端到端验证。
- 清理：四个本次创建的临时 Profile/runtime 目录已移动到桌面环境回收站，可恢复；未保留运行中的 QEMU、virtiofsd 或 Worker 进程。

## 2026-08-10：Fetch 到离线 Build 接力

- 验证对象：提交 `c8d7851` 之后加入精确依赖快照的工作树。
- 输入：测试 PKGBUILD 不含外部 source 和依赖；它通过签名 JobSpec 的内联输入进入 Worker，不从宿主共享可写目录注入。
- Fetch：Job `42a6f6c4-2b19-44b3-9a32-e2c86c9b8d0d`、Attempt `59d2c20c-f0a0-4b80-a43b-b1d44ecba210` 在受限联网 KVM 中成功；Source Manifest 摘要为 `22f6a17734aea5df41ef3498c2bdf79fd5cb4d13e858b3c65cc5d428d0be957a`，并完整记录 PKGBUILD、src 目录和 Agent 风险选读文本。
- Build：新的 Job `74881cb7-c2be-49e4-96e7-f6a7b4b0f07c` 只引用上述 Fetch Attempt 和摘要；Worker 从 completed 目录重新验证并创建新 overlay。Build VM 使用 `network=none`，成功生成 `aursmith-fetch-fixture-1-1-any.pkg.tar.zst`，解析到包名、版本 `1-1`、架构 `any`，SHA-256 为 `744859e3eceb7675962040dc91150b1cef219936e1ad4a11bec5d630119ee24b`。
- 边界：该早期 fixture 没有实际下载官方依赖，只验证了依赖为空时的快照与离线安装路径；后续“Fetch KVM 真实官方依赖下载”已补齐 pacman 经 source proxy 的真实网络与验签行为。
- 清理：Worker 和临时监听器已停止；本次 Profile 与 runtime 目录移动到回收站后再确认无 QEMU/virtiofsd 残留。

## 2026-08-10：离线 Signer

- 输入：复用上一条 KVM Build 产生的真实 Arch 包；`prepare_release_fixture` 使用正式 Envelope 代码和固定测试 Controller 密钥生成十分钟有效的 ReleaseAuthorization。
- 密钥：在临时 GPG home 中生成一天有效的 Ed25519 测试密钥并导出私钥文件，只交给 Signer 进程；没有使用仓库或用户生产密钥。
- 执行：Signer 复验包 SHA-256、大小以及 `.PKGINFO` 的包名、版本和架构；生成包 `.sig`、`aursmith.db.tar.gz`、数据库 `.sig`、`release-manifest.json` 和 Manifest `.sig`，再原子提交完整 Release 目录。
- 验证：对最终 `release-manifest.json.sig` 实际执行 `gpg --verify`，签名有效，测试指纹为 `2AB6 48B7 402E 9526 9411 4A92 BCD2 FD6F 30E9 C801`。
- 边界：尚未覆盖 Publisher 从 Builder 拉取 Artifact、公开 hot set 切换、客户端 pacman 安装或生产 GPG 指纹引导。
- 清理：Signer 进程已停止；包含临时测试私钥、GPG home、inbox 和 signed Release 的目录已整体移动到回收站，可恢复。

## 2026-08-10：Builder 受限 rsync 导出

- 部署：使用 Builder Compose 启动真实 Worker 与永久降权 SSH sidecar，SSH 端口只绑定回环地址；Worker Journal 报告实例 UUID `35fe7758-e306-4fcb-ba68-ecefbeed397c`。
- 授权：固定测试 Controller 密钥签发十分钟有效的 TransferCapability，绑定源 Worker、随机目标 Worker、Job、Attempt 和单个 KVM 构建包的路径、大小、SHA-256。
- 导出：Builder 从 completed Attempt 重新读取并验证 Artifact，只把授权文件复制到 `/jobs/transfers/e2f9e278-e34e-4d40-bd53-f1e7c5fcc61e`。
- SSH：通过真实 OpenSSH forced command 执行 rsync sender；任意 Shell 仍被拒绝，rsync 只能读取上述 Capability 目录。
- 结果：接收文件与原 KVM Artifact 的 SHA-256 均为 `744859e3eceb7675962040dc91150b1cef219936e1ad4a11bec5d630119ee24b`。
- 边界：本条验证了 Builder export 与真实 SSH/rsync sender；Publisher 自动拉取另见下一条。尚未验证 Controller 调度器跨两端自动推进整个状态机。
- 清理：测试 Compose 的容器、网络和全部卷已经删除；客户端/host SSH 密钥及接收目录已移动到回收站。

## 2026-08-10：Publisher 能力绑定拉取

- 部署：Builder 使用 Compose 中的 Worker 与永久降权 SSH sidecar，Publisher 使用真实 Worker daemon；两端实例 UUID 分别为 `619c4763-1aba-49c4-a0a4-638b2e2f4326` 和 `845d862c-6e4e-473a-b7f2-f817feabde20`。
- 授权：TransferCapability `9108d8ad-7931-4398-a405-7eceae3e35fa` 同时绑定 Builder、Publisher、Build Job、Attempt generation、writer epoch 和唯一 Artifact 的路径、大小及 SHA-256。Builder 静态 SSH 地址由 Publisher 配置按源实例 UUID 解析，不接受 Capability 自带网络地址。
- 传输：Publisher 以固定 argv 启动 rsync，启用 `partial` 与 `delay-updates`；自定义远程 Shell 只接受固定 rsync sender 形态，OpenSSH 再由 Builder forced command 对 Capability 目录二次授权。
- 接管：文件先进入 `.9108d8ad-7931-4398-a405-7eceae3e35fa.partial`，完整核对文件集合、普通文件类型、大小和摘要后才原子改名到 landing 目录。Worker 返回 `IMPORT_VERIFIED`，文件 SHA-256 为 `744859e3eceb7675962040dc91150b1cef219936e1ad4a11bec5d630119ee24b`，与原 KVM Artifact 一致。
- 实际发现并修复：rsync 3.4.4 调用自定义远程 Shell 时使用 `-l 用户 主机` 参数形态；包装器最初安全失败关闭，随后改为显式识别该形态并继续严格拒绝未知远端命令。
- 边界：尚未覆盖 Controller 定时调度实际签发 Capability，也未覆盖 Publisher 调用 Signer、公开 hot set 原子切换和 Archiver Receipt。

## 2026-08-10：Publisher 与离线 Signer 原子发布

- 输入：复用 KVM 构建并经 TransferCapability 落地的真实 Arch 包；测试 Controller 分别签发两个完整 ReleaseAuthorization，均绑定 writer epoch、Artifact 元数据、Revision/Audit 摘要和源码提交。
- 隔离：Signer 只读取 inbox、写 signed output，并使用测试 GPG 私钥；Publisher 只导入对应公钥，未读取私钥。Signer 使用官方 `repo-add` 生成 `.db.tar.gz` 和 `.files.tar.gz`，两者、软件包及 Release Manifest 均生成并验证分离签名。
- 首次发布：Release `9ee7eedf-6fc2-4715-b624-95d6c9750f6d` 返回 `published`，Manifest SHA-256 为 `b6141c81d2d98e7f69ce97e9247161fbbc1bf0efd277bfa804cedd13f6ba7b98`。Publisher 先提交不可变 Release 目录和包 hot set，再依次切换数据库签名、files 数据库，最后原子切换 `aursmith.db`。
- 再次发布：相同软件包进入 Release `ba886982-3484-47e4-9e06-caed2f5d5955`，Manifest SHA-256 为 `cd443f23412dc70a73ce1f256e4a7f39e0cd50df9d2bee1bdf16b7a25abfaef9`。包签名因重新签署可能不同，Publisher 复验并保留已公开的有效旧签名；数据库链接成功指向新 Release，前一 Release 目录仍完整保留。
- 实际覆盖：真实 `gpg --verify`、`repo-add`、Publisher Journal、Signer inbox 原子接管、签名输出复验、同名包摘要冲突保护、Release 目录持久化和数据库最后切换。
- 容器：修改后的 Publisher Worker 与 Signer 镜像均通过实际 Docker 构建；Publisher Worker 镜像包含验签所需的 GnuPG，Compose 安全检查确认它只挂载公钥，而断网 Signer 只挂载私钥且不挂载公开仓库。
- 边界：尚未用独立 Arch 客户端执行 `pacman -Syu`，也未完成服务端回滚、30 天兼容窗口清理和 Archiver Receipt。

## 2026-08-10：Publisher 到 Archiver 不可变快照

- 拓扑：Publisher Worker 与 OpenSSH forced-command sidecar 运行在独立容器边界，Archiver Worker 作为目标端主动拉取；Release 文件没有经过 Controller。
- 授权：TransferCapability `c7a251dc-3de3-4dab-9019-45e3811f5c3c` 绑定 Publisher `10cca597-8121-475c-82c6-2b4d2b0d18af`、Archiver `7f423b9d-bb34-45fe-bc64-914aa3a2a800`、writer epoch、Release `ba886982-3484-47e4-9e06-caed2f5d5955` 和 9 个文件的路径、大小及摘要。
- 传输与快照：Publisher 仅导出能力目录；Archiver 通过固定 host key 和独立拉取密钥执行 rsync，复验完整文件集合后使用本地 `rsync --link-dest` 创建不可变 Release 快照。首次快照没有前代可去重，后续 Release 才会对同路径同内容文件形成硬链接。
- Receipt：Archiver 使用首次启动后保存在 Journal 的 Ed25519 身份密钥签署 ArchiveReceipt，Receipt SHA-256 为 `1bbdeb0c0f8d7156b80475533915e155ad903390ad15983d322d43a4880b0374`。再次提交相同 Capability 返回 `IDEMPOTENT_ARCHIVE` 和同一 Receipt。
- 结果：归档与 Publisher 的 `release-manifest.json` SHA-256 均为 `cd443f23412dc70a73ce1f256e4a7f39e0cd50df9d2bee1bdf16b7a25abfaef9`；快照包含包、包签名、db/files 数据库及签名、ReleaseAuthorization 和签名 Manifest。
- 边界：尚未执行第二个不同 Release 的硬链接 inode 去重验证、Controller 自动调度 Receipt 对账、每周库存巡检或从归档恢复控制面数据。

## 2026-08-10：既有签名 Release 回滚

- 输入：复制包含两个已签名 Release 的真实 Publisher 仓库，通过测试 Controller 分别签发五分钟有效、绑定 writer epoch 的 ReleaseRollbackAuthorization。
- 回滚：Publisher 对目标 Release 的 Manifest、db/files 数据库、全部包及分离签名重新执行 GPG 验证，未调用 Signer、repo-add 或 Builder。仓库 DB 链接从较新 Release `ba886982-3484-47e4-9e06-caed2f5d5955` 原子切换到旧 Release `9ee7eedf-6fc2-4715-b624-95d6c9750f6d`，返回 Manifest SHA-256 `b6141c81d2d98e7f69ce97e9247161fbbc1bf0efd277bfa804cedd13f6ba7b98`。
- 恢复当前版本：随后使用相同流程切回较新 Release，确认 DB 链接重新指向 `releases/ba886982-3484-47e4-9e06-caed2f5d5955/aursmith.db.tar.gz`；两个不可变目录均未被修改或删除。
- 客户端语义：Controller API 根据目标 Release 的 Artifact 清单生成逐包 HTTPS `pacman -U` 命令；UI 明确显示服务端回滚不会自动降低客户端已安装版本。
- 边界：尚未在独立 Arch 客户端实际执行生成的 `pacman -U` 命令。

## 2026-08-10：动态依赖 Profile 迟滞

- 测试：在真实 SQLite migration 后创建二十个成功 Build Job，并让官方依赖 `cmake` 出现在其中六次，每次记录六十秒下载耗时。
- 首周期：优化器返回 `suggest_add`，没有越过“两周期达标”的迟滞规则。
- 次周期：把统计周期推进八天后重新评估，返回 `add`；实现同时保留前二十次构建只观察、三低周期/三十天移除和 AUR 依赖永不固化的领域测试。
- 数据来源：Fetch Guest 已记录官方依赖下载总耗时，Controller 分摊到本次解析出的依赖观察；UI 展示最近二十次、月使用次数、预计节省和连续冷热周期。
- 边界：该验证是控制面统计测试，尚未用一个真实高频依赖完成“建议→重建 Profile→KVM fixture→激活→后续构建命中”的完整性能闭环；pacoloco 命中率仍未接入代理指标。

## 2026-08-10：运维健康、背压和告警界面

- 验证对象：提交 `5d6be0d` 之后的运维工作树。
- 自动测试：执行 `bash scripts/test-all.sh`；Rust workspace 共运行 68 个单元、领域、协议和 Journal 测试，全部通过；前端类型检查、1 个 Vitest、生产构建和四套 Compose 安全检查全部通过。
- 回归：首次运行发现旧 Doctor mock 缺少 `checks` 时 Dashboard 崩溃；实现改为让核心 Dashboard 与可选 Doctor 请求独立加载，异常或旧响应不再阻断需求总账和 Worker 概览。
- 覆盖：SQLite 迁移在空数据库实际执行；测试验证发布背压默认关闭且能读取持久化值，通知 URL 拒绝内嵌凭据和非 HTTP(S) scheme；Compose 检查继续确认无 privileged、Docker/libvirt Socket，Builder 只获得 `/dev/kvm`，Signer 断网且私钥不进入 Publisher。
- 未覆盖：本条没有把真实磁盘压到 15%/10%，没有制造 20 GiB 未归档数据，也没有向外部 Webhook 或 ntfy 实际投递；因此这里只确认状态机、静态安全边界和 UI 构建，不声称外部通知端到端已经验收。

## 2026-08-10：控制面一致性备份和离线恢复

- 测试在临时文件数据库完成全部 migration，写入 `before` 状态后执行正式备份实现；备份由 `VACUUM INTO` 生成，经过 `integrity_check`、SHA-256 和测试 Controller Ed25519 密钥签名。
- 随后把在线数据库状态改为 `after`、关闭连接池，并执行正式 `restore-control-plane` 内核逻辑。重新连接后读到签名快照中的 `before`，证明恢复的不是修改后的主数据库。
- 测试确认被替换数据库进入 `recovery` 目录；另有路径测试拒绝内存数据库和带查询参数的恢复目标，避免把不明确 URL 当成文件覆盖。
- 未覆盖：本条使用临时目录和测试密钥，没有在 Docker volume 上执行人工停机恢复演练；远端 Archiver 传输由后续条目覆盖静态路径，但仍需要一次真实跨设备恢复演练。

## 2026-08-10：Archiver 周期库存巡检

- 快速测试总数增加为 71 个 Rust 测试并全部通过；前端类型检查、Vitest、生产构建和 Compose 安全检查同时通过。
- 单元测试创建一个与 Receipt 完全匹配的归档目录，确认每周浅巡检和完整摘要巡检均通过；随后把文件改为相同长度的不同内容，浅巡检仍只负责集合/大小，而完整巡检按 SHA-256 明确失败，锁定两种检查的职责差异。
- Worker 只允许 Archiver 角色执行 inventory，并以本地 Journal 身份密钥签署版本化报告。Controller 调度实现会核对固定身份公钥、Worker UUID 和 full-digest 请求，失败报告进入稳定 fingerprint 的 critical 告警；归档 UI 可查看报告历史。
- 未覆盖：没有在包含上一轮真实 Release 的 Archiver 容器上实际运行季度全量扫描，也未进行损坏后从第二副本恢复演练；当前测试覆盖算法、协议、调度静态路径和 UI 构建。

## 2026-08-10：控制面备份独立归档路径

- 快速测试共运行 72 个 Rust 测试，前端和 Compose 检查全部通过。控制面备份测试额外验证最小导出目录只包含 `controller.db` 与 `backup-envelope.json`，目录名绑定随机 Capability，传输源 UUID 对同一 Controller 身份保持稳定；后续库存报告分别统计 Release 与控制面备份，二者都进入每周/季度巡检。
- Archiver 输入测试使用测试 Controller 密钥签署 `ControlPlaneBackup`，确认 Backup ID、数据库摘要、Capability 文件集合和 Controller 公钥全部一致时才接受；替换为其他公钥会失败关闭。
- Controller、backup-ssh 共用镜像以及修改后的 Archiver Worker 镜像均已通过真实 Docker build。Controller 镜像包含 OpenSSH server、rsync 和永久降权工具；Compose 安全检查确认 sidecar 仍为 `cap_drop: ALL`、只增加启动后永久降权所需能力、只读挂载导出卷且不获得 Controller 数据库写权限。
- 未覆盖：本次没有配置一组真实 Controller/Archiver SSH key 完成跨容器 rsync 与 BackupArchiveReceipt 冒烟，也没有从 Archiver 副本执行停机恢复；因此独立归档的协议、文件校验、容器镜像和调度代码已验证，但跨设备端到端验收仍未完成。

## 2026-08-10：AUR 生命周期事件与包详情

- 成功刷新会在同一控制面事务中比较旧 package/revision 元数据，记录维护者、orphan 和 source 域名集合变化；域名单元测试确认 `git+` 前缀、source 别名和主机名大小写被规范化，本地 source 不被误认为网络域名。
- AUR RPC 找不到原 pkgbase 时会打开稳定告警并只追加一次 `package_missing_from_aur` 事件，payload 明确保留 deleted/renamed/merged 三种可能；当前订阅和稳定 Release 不会因此被删除。
- Web 包页面补齐详情入口，展示 split outputs、不可变 Revision、Provider/依赖解析和 append-only 事件，并补上与退订语义分开的“清除”操作。
- 未覆盖：尚未对真实发生 AUR merge 的包验证上游是否提供可可靠固定的目标线索；VCS 历史重写仍需要单独实现与验收。

## 2026-08-10：官方依赖变化建议与本地重建版本

- 控制面保存每个 Artifact 构建时实际解析的官方依赖名称、版本和包摘要；周期检查只把版本变化建模为保守重建建议，并在 UI 明示这不能证明 ABI 兼容性。建议支持立即调度、按包关闭和七天自动合批。
- 重建测试确认系统不会复用旧 Fetch Attempt：它为受影响闭包派生新的 `rebuild_generation`、Revision 和 AuditPreScan，在没有活动 Profile 时停在可解释的 `awaiting_profile`，后续仍必须经过新 Fetch、Agent 审计和 Build。
- 版本测试确认首次发布保留上游 `pkgrel`，同一上游完整版本的新 AUR commit 或依赖重建派生 `.1` 后缀；支持带 epoch 的版本。签名 JobSpec 把派生 `pkgrel` 交给 Guest，Guest 单测确认只改写构建工作副本，并拒绝多个或歧义 `pkgrel` 赋值。Controller 还会拒绝 `.PKGINFO` 版本与授权值不一致的 BuildResult。
- 已运行 `cargo test -p aursmith-domain -p aursmith-protocol -p aursmith-guest-agent -p aursmith-controller`；补充单个动态 `pkgrel` 失败关闭及上游元数据缺失不误清除建议的用例后，四个相关 crate 共 59 个测试全部通过。随后执行 `bash scripts/test-all.sh`，全仓库 83 个 Rust 测试、前端类型检查、Vitest、生产构建和四套 Compose 安全策略检查全部通过。
- 未覆盖：尚未用真实官方包升级触发一次完整的“检测→七天合批→KVM 重新下载依赖→Agent 审计→pacman 客户端升级”端到端流程，因此当前验证覆盖领域规则、数据库状态机、协议约束和 Guest 工作副本改写，不声称真实 ABI 或客户端升级已经验收。

## 2026-08-10：Publisher 软件包归档检查

- Publisher 在复制到 Signer inbox 后独立调用固定路径的 `bsdtar`，核对归档普通路径、文件类型、唯一必需元数据及 `.PKGINFO` 中包名、版本、架构。单元测试覆盖路径逃逸和字符设备阻断，也确认 INSTALL、pacman hook、systemd 单元、setuid 和内核模块只记录为结构化事实。
- 集成测试实际创建含 `.PKGINFO`、`.BUILDINFO` 和 `.MTREE` 的 tar 软件包，再通过正式检查入口读取三份清单和元数据；这不是只对手工构造的字符串测试解析器。
- `artifact-inspections.json` 被复制进 Signer 输出，其大小和报告数量由断网 Signer 复验，文件摘要进入 GPG 签名的 Release Manifest。Publisher 激活或回滚 Release 时重新核对该摘要，归档的完整 Release 文件清单自然包含检查报告。
- 执行 `bash scripts/test-all.sh` 后，全仓库 86 个 Rust 测试、前端类型检查、Vitest、生产构建和 Compose 安全策略检查全部通过；修改后的 Worker 与 Signer 镜像分别实际构建为 `aursmith-worker:test` 和 `aursmith-signer:test`。
- 未覆盖：当前检查尚未解析 ELF `DT_NEEDED`、文件 capabilities 或实际执行 namcap；这些仍是 R01/B03 后续实现项。也尚未用含上述风险内容的真实 pacman 包完成 Publisher→Signer→Archiver 端到端演练。

## 2026-08-10：Fetch source-proxy 容器

- 新增基于 Arch 官方 squid 包的最小镜像，进程使用包内 `proxy` 用户运行；Compose 固定只读根文件系统、清空 capabilities、启用 no-new-privileges，并只给 PID、日志和临时文件分配 tmpfs。
- 第一次真实镜像构建发现预想的 `squid` 用户不存在，依据镜像实际创建的 UID/GID 15 `proxy` 用户修正；第二次构建成功。
- 使用与 Compose 相同的只读和降权参数启动真实容器，通过代理访问 `https://archlinux.org/` 返回 HTTP 200，访问 `127.0.0.1:8080` 返回 HTTP 403；测试容器随后删除。
- 修复真实上游冒烟脚本，使其生成一次性 GPG 测试公钥并配置 Publisher 的仓库、Signer 和 Journal 临时目录。随后实际通过 AUR RPC 搜索 `visual-studio-code-bin`、固定其 40 位 AUR Git commit、读取 `paru-git` 上游 commit，并从 Arch 官方接口查到 `pacman`；临时密钥和数据库均已清理。
- 边界：第一版代理执行全局公共网络策略，尚未由 Controller 按 Revision 自动生成精确 source 域名 allowlist，也没有引入 pacoloco 缓存统计。

## 2026-08-10：Worker 注册和 Provider 处置界面

- Worker 页面新增名称、固定角色、SSH 端点、host key 指纹和调度标签表单；注册请求仍由 Controller 执行真实 SSH 探测并核对 Worker UUID、名称、角色、协议和身份签名公钥，不在浏览器侧伪造在线状态。
- 包详情页对 `needs_selection` 依赖展示全部 Provider 候选。用户选择后调用既有受认证 API，重新同步依赖闭包并刷新详情，选择只绑定新 Revision，不形成永久信任。
- 前端类型检查、两个 Vitest 用例和生产构建通过；新增用例实际进入 Worker 页面并检查注册表单的可访问标签和提交按钮。

## 2026-08-10：Profile candidate Web 授权

- Profile 页面新增 JSON 文件选择入口，浏览器解析 profile-builder 生成的 candidate 后调用现有授权 API；成功响应显示重新计算的 Profile 摘要并可下载 `profile-envelope.json`。
- qcow2、内核和 initramfs 不上传 Controller，继续按摘要复制到 Builder 持久卷；这避免让控制面承担大文件传输，也保留 Builder 必须实际发现 Profile 后才能执行 fixture 的约束。
- 前端类型检查、三个 Vitest 页面用例和生产构建通过。当前 UI 测试覆盖入口和文件控件；真实 candidate 的 API 签名与 fixture 状态机由 Controller 测试覆盖，尚未在浏览器自动化中选择本地文件。

## 2026-08-10：构建镜像源配置

- Profile 协议新增可选 `repository_mirror`；新值参与内容摘要，缺少该字段的旧 Profile 仍按旧摘要读取。Controller 拒绝非 HTTPS、内嵌凭据、查询参数或片段的镜像地址。
- profile-builder 使用 `AURSMITH_ARCH_MIRROR` 同时配置自身 pacman 和 Guest mirrorlist，并把实际 Base URL 交给 candidate 导出。Compose 默认使用 `https://geo.mirror.pkgbuild.com`。
- Rust 工作区 86 个测试、前端 3 个测试与生产构建、Compose 安全检查均通过；另用一次预期失败的 Docker 构建确认 `http://mirror.example.org` 在执行 pacman 前即被拒绝。随后实际设置 `AURSMITH_ARCH_MIRROR=https://mirrors.tuna.tsinghua.edu.cn/archlinux` 构建当前源码的 Profile 镜像；pacman 从该镜像完成仓库同步和约 442 MiB 的包下载，profile-builder 成功生成 qcow2、内核与 initramfs，并导出包含 218 个已安装包的 candidate。
- 导出的 `profile-candidate.json` 明确记录镜像地址，Profile 摘要为 `48c8ed63192a249bc69a12a6be6061a7cd662957e80a48b506705a653d5c79d3`；最终构建镜像内的 `repository-mirror` 也与 candidate 一致。本轮尚未把该 Profile 授权后启动 Fetch VM 下载一个真实官方依赖，因此 Guest 内 pacman 的运行时命中仍属于后续端到端验证范围。
- 安全边界：该选项只影响 Profile 制作和 Fetch Guest 的官方依赖下载，Build Guest 仍固定 `-nic none`，不会因选择镜像而获得网络。

## 2026-08-10：非特权 KVM Fetch→Build 复验

- 验证对象：提交 `e2a4257` 之后、采用 QEMU virtio-9p 和 overlay 成功清理的待提交工作树；Profile 继续使用清华 Arch 镜像构建，摘要为 `e6561917eb5d627de6ae55d355d42f5a8855cb68c9f37f646fea12cab616bc0f`。
- 根因：真实运行发现 Rust virtiofsd 1.14 在非 root、`cap_drop: ALL` 容器中以 `Permission denied` 退出；启动 Socket 文件还存在一个更早被观察到的就绪竞态。没有通过增加 root、privileged、capability 或 Docker Socket 掩盖问题，而是改用 QEMU 内置 9p `mapped-xattr`，继续对输入启用只读并保持 Attempt 输出目录隔离。
- Fetch：Job `ac061190-8847-45c7-9244-b8811ab2f1b1`、Attempt `ce08fd35-0016-43d8-a1ba-cb3ef909996c` 在真实 KVM 中成功；Guest 再次验证 Controller 签名，Source Manifest 摘要为 `22f6a17734aea5df41ef3498c2bdf79fd5cb4d13e858b3c65cc5d428d0be957a`。
- Build：Job `54c0fc91-fc4b-438c-be16-e78e51312046`、Attempt `b39ed7d7-19d8-483a-92ba-839d3fc6b6be` 只引用上述 Fetch 结果，在 `network=none` 的新 VM 中生成 `aursmith-fetch-fixture-1-1-any.pkg.tar.zst`；包为 1251 字节，SHA-256 为 `d6eb9572caeafaa60749c641889b2dab30c06c32009cd648d6318376bbbf2233`，实际读取 `.PKGINFO` 得到版本 `1-1`、架构 `any`。
- 清理缺陷：首次成功复验发现 completed 目录仍包含 qcow2 overlay，随后改为结果验证成功后、原子接管前删除 overlay 和控制 Socket。使用修复后的 Worker 再次执行 Fetch Job `812b36c9-163f-42a2-92ae-c9d02a4223d0` 和 Build Job `c9acb9bf-596f-4e12-82d9-ba5930e5f6af`，两者成功；Build Attempt `7857d4f3-f425-4048-89d1-3ba73a60ea84` 生成 1252 字节、SHA-256 为 `30eaae8d9f82972b9c3c209862b8f6077cf8aa4cca88b329007be03f9bd25b79` 的包。随后实际检查全部 completed 目录，不再存在 overlay 或 Socket，也没有残留 QEMU/virtiofsd 进程。
- 测试结束后已停止并删除临时 Worker、source proxy 和 Docker 网络；两个仅用于本轮 Profile 导出的 Docker volume 已删除且不可恢复，缓存目录已移动到桌面环境回收站，可恢复。
- 边界：测试 PKGBUILD 不含外部 source 和官方依赖；代理 TCP 路径和镜像构建已实际运行，但 Fetch Guest 通过代理下载真实 source/官方依赖仍未在这条 fixture 中覆盖。

## 2026-08-10：跨角色发布、归档与 pacman 客户端闭环

- 拓扑：在同一临时 Docker 网络中分别启动 Builder、Builder SSH、Publisher、Publisher SSH、断网 Signer、仓库 Caddy、Archiver、Controller、Web 和独立 Arch Linux 客户端；大文件均由目标角色使用 OpenSSH forced command 与 rsync 拉取，没有经过 Controller。
- Artifact 传输：Builder 完成目录中的 `aursmith-e2e-fixture` 通过绑定 Attempt、源/目标 Worker、文件摘要和 writer epoch 1 的 TransferCapability 进入 Publisher。首次使用旧 fixture 固定的 epoch 0 被 Publisher 以 `WRITER_EPOCH_MISMATCH` 拒绝；fixture 随后改为显式接收 epoch，正确授权返回 `IMPORT_VERIFIED`，证明失败没有被静默降级。
- 发布：第一个不可变 Release 为 `b14e27db-d7ac-4894-bd90-c4096ab07747`，Manifest SHA-256 为 `ad7be669a97c8e05f949f68ff90b5ee8fae4ee6cec4dedb4c9aa3b48e4314210`；第二个为 `dca84e41-b05e-41a6-a796-5cf9453e1d67`，Manifest SHA-256 为 `a4d1e9987dc5abbdba22a27c0718b95ee518ea94e43b05b7dbf16e2ee8c3ea22`。Publisher 对软件包归档重新检查，断网 Signer 使用测试 GPG 指纹 `7FDE0628CE05A1E752E80C8C8F4E27E3DC73CD20` 执行 repo-add 和签名，Publisher 再验后原子切换数据库。
- 归档：两个 Release 均经独立 Capability 拉取并返回 `ARCHIVE_VERIFIED`。第一次完整库存巡检统计 1 个 Release、10 个文件、8157 字节、0 个失败；第二个 Release 也形成独立不可变快照。
- 客户端：独立 Arch 容器从仓库下载公钥，人工核对后导入并本地信任；pacman 配置使用 `SigLevel = Required DatabaseRequired`。客户端先安装 `1.0-1`，第二次 `pacman -Syu` 升级到 `1.1-1`。Publisher 使用签名回滚授权切回第一个 Release 后，保留 1.1 状态的客户端通过历史 Release 的显式 `pacman -U` 命令降级到 `1.0-1`；pacman 明确输出 downgrade 警告，没有把服务端回滚误报成客户端自动降级。
- 控制面：Controller 在只读、零 capability、no-new-privileges 容器中完成一次性令牌初始化和管理员登录；鉴权后的 `/api/v1/requirements` 返回包括 B05 与 O02 的完整总账。实际创建并再次验证控制面备份 `9bfa4fcd-2018-4f57-ae65-e96333713daa`，数据库为 483328 字节，SHA-256 为 `a8479d97a7120f2c9825b13a512d72c91ea8706684dee79afc640a9290dc19b2`。
- 容器缺陷：首次按 Compose 安全策略运行仓库 Caddy 时，因官方二进制携带 `cap_net_bind_service` 而在 exec 阶段得到 `operation not permitted`；派生镜像移除文件 capability、使用 UID/GID 10001 并监听非特权端口后，真实仓库请求成功。Controller 首次运行也因空命名卷把 `/run/aursmith` 变为 root 属主而失败；在镜像内预创建并交给 UID 10001 后，`/healthz` 返回正常。Web Caddy 在相同限制下返回 HTTP 200 和预期 CSP、DENY frame、nosniff 响应头。
- 清理：验证结束后已停止并删除前缀为 `aursmith-release-e2e` 的 10 个临时容器、16 个临时卷和独立 Docker 网络；卷不可恢复，包含测试密钥和 fixture 的临时目录已移动到桌面环境回收站，可恢复。
- 边界：本条把真实 Artifact 传输到 pacman 升级/降级串成一个闭环，但软件包在该临时拓扑中由 Arch 容器生成；KVM Fetch→Build 已在上一条独立真实验证。Controller 尚未自动调度这次完整链路，真实 Codex/Claude provider 也因没有可用 API key 未调用，因此不能把两条相邻验证合并宣称为订阅到发布的无人值守端到端验收。

## 2026-08-10：split outputs、check 策略与 namcap

- 协议：JobSpec 新增向后可读取的 `expected_outputs` 和默认启用的 `allow_check`。Controller 在 Build Job 创建时从不可变 Revision 快照固定完整 split outputs，并读取按包策略；不会使用用户仅关注的输出子集。
- Guest：`allow_check=true` 使用 makepkg 默认检查流程，显式 false 才增加 `--nocheck`。构建后核对 `.PKGINFO` 包名集合与授权集合完全相等，再以固定 argv 对全部软件包运行 namcap；check 状态和 namcap 日志摘要进入 provenance。
- Web：软件包详情显示当前策略，可显式禁用或重新启用 `check()`，并提示禁用会降低验证覆盖且只影响后续 Job。每次变更写入 append-only 事件日志。
- 自动测试：Controller 测试在真实 SQLite migration 后创建含两个 split outputs 的 Revision，把策略设为禁用并完成 Fetch/Audit 前置状态，确认生成的 Build Job 固定两个输出且 `allow_check=0`；Guest 测试确认 `--nocheck` 只在禁用时出现，缺失 split output 会失败。前端测试还实际打开详情并提交禁用操作。全仓库 90 个 Rust 测试、TypeScript 检查、4 个前端测试、生产构建和 Compose 安全检查通过。
- 纠正记录：前端命令第一次从仓库根目录运行，因那里没有 `package.json` 而得到 ENOENT；随后在 `web/` 目录重新执行并通过。该失败不是前端代码测试通过的证据，最终结论只采用纠正后的运行结果。
- 边界：本轮没有重新制作 KVM Profile 并实际启动 Guest，因此 namcap 的固定调用、协议和结果处理已通过静态编译与单元测试，但真实 VM 中的 namcap 输出仍由下一次完整 KVM 构建复验。Publisher 对 ELF `DT_NEEDED` 与文件 capability 的独立解析仍未实现。

## 2026-08-10：清除软件包与空仓库

- 根因：清除 API 原先只创建 `queued_removal` 批次，没有任何调度器消费该状态，因此 UI 返回成功但仓库不会变化；这是业务流程断链，不是展示问题。
- 修复：发布调度器现在接受清除批次，汇总目标 pkgbase 全部历史 Revision 的 split outputs，并从控制面当前激活 Release 删除这些名称，再冻结进 ReleaseAuthorization。测试还固定“服务端回滚后，新 Release 必须以显式回滚目标为基线”，不能按 committed_at 误选时间较新的停用 Release。Publisher 与 Signer 同时拒绝重复、非法或仍存在于最终 Artifact 集合中的清除名称。
- 空仓库：如果清除最后一个包，Signer 为 db/files 各创建一个空 gzip tar，继续执行 GPG 签名、Manifest、原子发布和归档流程。没有 Artifact 且没有清除目标的普通空授权仍被拒绝。
- 验证：单元测试覆盖同时清除两个 split outputs、保留其他包以及清除后为空；Signer 测试确认空数据库是可读归档。另在独立 Arch Linux 容器中，用真实 pacman 对该空数据库执行 `-Sy` 成功，随后 `-Sl aursmith` 正常返回空列表。第一次在宿主直接运行因 pacman 要求 root 被明确拒绝，改为一次性容器后通过；临时目录因 `/tmp` 不支持桌面回收站而在确认精确路径后删除。
- 边界：本条尚未重新启动完整 Publisher/Signer Stack 执行一次带 GPG `DatabaseRequired` 的最后一包清除；已验证调度数据、签名端构造逻辑、空归档和 pacman 无签名解析，签名发布链继续依赖上一条已通过的真实 Release E2E。

## 2026-08-10：设置页与运行时 Agent 预算

- API：新增认证设置读写接口，只接受每日调用、每月调用和每月成本三个非负上限；更新写入 SQLite 并追加事件。返回值只包含 Runner 数量、Codex/Claude Code 支持状态、预算使用量、通知是否配置和仓库公开信息，明确 `api_keys_exposed=false`。
- 调度：预算判定改为优先读取持久化覆盖值，而不是只读取启动环境；测试把每日上限写为 0 后确认新 Agent 调用不可用。
- Web：设置页不再只有客户端引导，新增预算表单、当前使用量、provider 配置方式、通知和 30 天仓库兼容窗口；页面明确 provider/Base URL/API key 必须通过 Compose 与 secret 修改。前端测试打开设置页，确认预算值可见、Codex/Claude Code 状态可见且不存在类似 API key 的内容。
- 验证：认证路由测试实际登录后 PUT 设置并核对响应中的新限额及 `api_keys_exposed=false`；Controller 33 个测试、TypeScript 检查、5 个前端测试和生产构建通过。第一次把 npm 串在仓库根目录命令后再次得到 ENOENT，随后在 `web/` 目录纠正并通过；该重复操作失误不计为验证成功。
- 边界：设置页不热更新 Agent 容器的 provider 与 Base URL，也不接收 API key；这是刻意的 secret 边界。尚未用真实 provider key 执行模型调用，Agent 外部服务 E2E 仍未验证。

## 2026-08-10：Publisher ELF 与 file capability 检查

- ELF：软件包检查在完整归档门禁通过后，对可执行文件、共享库和内核模块候选使用 bsdtar 单独提取；单文件上限 1 GiB。确认 ELF magic 后以固定 argv 调用 readelf，单份输出上限 16 MiB，并把路径到 `DT_NEEDED` 的映射写入检查报告。
- capability：Publisher 不需要 root 或 `CAP_SETFCAP`。检查器让 bsdtar 针对可执行归档项重建有限的 pax 头流，只读取前 128 KiB，并识别 `LIBARCHIVE.xattr.security.capability`；随后终止子进程，不把整个大文件读入内存。命中只记录路径，不自动判为恶意。
- 测试：真实 tar fixture 加入宿主 `/usr/bin/true` 的副本，正式检查入口识别 ELF 并生成依赖映射；另有 pax 头识别回归。修改后的 Worker 镜像实际构建为 `aursmith-worker:inspection-test`，镜像内非 root 用户执行 `/usr/bin/readelf --version` 返回 GNU Binutils 2.47；验证后该临时镜像标签及其独占层已删除，可由 Dockerfile 重新构建。
- 边界：测试用纯函数覆盖 capability pax 头识别，且已在独立 Arch 容器中确认 bsdtar 重建会保留真实 `security.capability` 头；尚未通过完整 Publisher Release API 提交一个带 capability 的 pacman 包。Controller 当前保存的是构建时官方依赖快照，签名检查报告中的 ELF 映射已归档但还没有单独进入 ABI 建议查询表。

## 2026-08-10：Attempt 基础设施重试与 uncertain

- 分类：新增严格基础设施白名单；VM/Worker/结果传输类错误可重试，`INPUT_INVALID`、`PROFILE_DIGEST_MISMATCH` 和审计拒绝等确定性错误测试确认不会重试。随后修正 QEMU 外层分类：Guest 已写出错误时归为确定性 `GUEST_BUILD_FAILED`/`GUEST_FETCH_FAILED`；Build 日志出现典型 DNS、路由或连接错误时为 `NETWORK_DURING_BUILD`，两者均不进入白名单。只有没有 Guest 错误证据的 QEMU 异常才保留为 `VM_FAILED`。
- 次数：初始 generation 0 失败后等待 5 秒，generation 1 失败后等待 10 秒，generation 2 失败后终止。每次仍由正常调度生成新 Attempt ID、签名 JobSpec 和 token；旧 Attempt 保持 failed，迟到结果继续因 ID/generation 不匹配被拒绝。
- uncertain：提交或运行中 SSH 查询失败只把 Job 标为 uncertain；查询选择器在三十分钟内跳过它。三十分钟后仍不可达才把当前 Attempt 记为失败并进入上述重试；第三次打开 `job-uncertain:<job-id>` 告警并终止批次。
- 界面：任务 API 和构建页新增阶段、Attempt 数量和下次重试时间，用户可以区分排队、退避、状态不确定和确定性失败。
- 测试：真实 SQLite migration 上连续模拟 generation 0、1、2 三次不可达，前两次回到 queued 且具有退避时间，第三次进入 failed 并只创建一个去重告警。全量测试还覆盖 migration、旧 Journal 幂等和迟到 Attempt。
- 边界：同一 ReleaseBatch 的 Build 因 Fetch 产物亲和性仍不能任意切换到其他 Builder；原 Worker 永久丢失时会在重试耗尽后告警，而不是绕过签名传输规则到其他节点重新联网构建。

## 2026-08-10：认证 SSE 实时进度

- 后端：新增只接受管理员会话 cookie 的 `/api/v1/events`。流比较 append-only event sequence、Job/Release/Archive 更新时间和未解决告警数，状态未变化不重复发送；十五秒 keep-alive 由 Axum 生成。
- 前端：登录成功后建立同源 EventSource，侧栏显示“实时连接正常/重试中”；收到变化帧后增加版本号，构建页重新读取 `/jobs` 权威状态。页面不从 SSE payload 拼装业务对象，因此断线重连不会漏掉最终状态。
- 测试：SQLite 测试确认添加 Job 与告警会改变实时快照；TypeScript 与生产构建验证浏览器端 EventSource 生命周期。jsdom 没有 EventSource 时页面安全跳过连接，不用假对象伪造实时成功。
- 边界：第一版只让高频构建页自动刷新，其他页面仍保留手工刷新按钮；SSE 是状态变化流，不是逐行构建日志传输。Worker 当前保存 build/fetch/QEMU 日志，按需日志流 API 仍是后续缺口。

## 2026-08-10：Release 签名证据链

- Controller 在成功 Attempt 对账时，把完整 GuestResult 与摘要写入独立 `job_evidence` 表；生成 ReleaseAuthorization 时同时收集批次图、Revision 快照、AuditBundle 覆盖范围、确定性发现、成功 Agent 原始结构化输出和 Job provenance。
- Signer 原样复制 Controller 的 `authorization.json`，把它作为 ManifestEntry 写入 GPG 签名的 Release Manifest。Publisher 在公开前复验文件摘要，并把文件字节与数据库中保存的 Controller Envelope 比较；归档文件枚举会自动包含它。
- Web Release 页面通过管理员认证 API 读取并验证当前 Controller 签名后，显示证据类型、身份和摘要；API 不向未登录用户暴露 Agent 输出。
- 定向验证执行 `cargo test -p aursmith-protocol -p aursmith-controller -p aursmith-signer -p aursmith-worker`：65 个测试通过。随后执行 `bash scripts/test-all.sh`，全仓库 99 个 Rust 测试、前端类型检查、6 个 Vitest 用例、生产构建和 Compose 安全策略检查全部通过。
- 该轮当时未覆盖大型证据文件；后续“完整 Release 大文件证据与恢复”验证已补齐，旧结论不再代表当前状态。

## 2026-08-10：Git VCS 历史重写门禁

- Publisher 快照请求新增可选上一 commit。当前 ref 仍由固定公共 IP 的 smart HTTP 广告取得；commit 变化时，再以禁用 file/ext 协议、禁重定向、固定 curl DNS 解析和无交互凭据的 Git fetch 获取无 blob 历史，使用 `merge-base --is-ancestor` 判定，不执行上游代码。
- Controller 对 root 包和隐式 AUR 依赖都执行同一门禁。非祖先关系不会生成 Revision，而是创建稳定 critical 告警、独立 `vcs_history_rewrite_detected` 事件和 ManualAction。Web 包详情要求至少 8 个字符的理由，批准或拒绝只绑定精确 previous/current commit 对。
- 单元测试用真实临时 Git 仓库验证正常子 commit 为祖先、orphan 分支不是祖先；SQLite 测试验证首次重写进入 pending、精确批准后放行、不同 current commit 再次回到 pending。前端用例实际提交批准请求并确认待处理面板消失。
- `scripts/smoke-upstream.sh` 扩展后，从真实 `paru-git` source 克隆深度 2 的当前历史，取父 commit 交给正式 Worker；Worker 返回 `vcs_ancestor_of_current=true`，AUR 搜索、普通快照和官方 `pacman` 查询也通过。第一次执行在上游请求阶段只返回汇总的“Worker 返回失败”，未作为通过证据；随后的 `bash -x` 复跑完整通过并由 trap 清理临时 Worker、GPG home、数据库和 Git clone。
- 最终执行 `bash scripts/test-all.sh`：全仓库 102 个 Rust 测试、前端类型检查、7 个 Vitest 用例、生产构建和 Compose 安全策略检查全部通过。额外回归确认同步期 ancestry 观察值不会进入不可变 Revision 摘要。
- 边界：真实上游冒烟验证了网络 fetch 的快进路径；历史重写分支由本地 Git 与控制面测试验证，没有要求公共项目实际 force-push 来制造破坏性测试。

## 2026-08-10：无付费 Agent 与 Fetch Doctor

- Agent Runner 新增 `/healthz`，只读取 adapter/provider/model 配置、检查选定的 `/usr/local/bin/codex` 或 `/usr/local/bin/claude` 文件，并在三秒内连接凭据网关；不会构造 AuditBundle、调用 CLI 或访问模型 provider。Controller 对三个低成本和一个高成本 endpoint 分别探测并保留各自失败原因。
- Publisher Worker 新增 `publisher-doctor` forced command。它验证 AUR RPC，并通过无内嵌凭据、查询参数或片段的 `AURSMITH_SOURCE_PROXY_URL` 请求 `https://archlinux.org/robots.txt`；Controller 将 AUR 与代理拆成两个 Doctor 结果。
- 单元测试覆盖 Agent 健康响应探测和 source proxy URL 拒绝规则。实际以只读、`cap_drop: ALL`、`no-new-privileges` 的 Squid 容器运行扩展后的 `scripts/smoke-upstream.sh`，AUR 与 source proxy 两项均通过；测试容器由 trap 删除，临时镜像标签移除。
- 实际构建 Agent Runner 镜像，并以只读、零 capability 容器分别启动 Codex 与 Claude Code adapter；两者 `/healthz` 均返回 200，且模拟凭据网关 TCP 可达。第一次用 `nc` 模拟网关没有得到 JSON，脚本又缺少最终失败判定，因此不计为通过；改用明确 HTTP 监听和失败关闭后分别复验成功。排查期间一个前台测试容器未随非 TTY 中断退出，随后按精确 ID 删除；测试镜像及独占层也已删除，可由 Dockerfile 重建。
- 最终执行 `bash scripts/test-all.sh`：全仓库 104 个 Rust 测试、前端类型检查、7 个 Vitest 用例、生产构建和 Compose 安全策略检查全部通过。此前直接运行 `docker compose config` 未提供 Stack 强制要求的 Controller 公钥和传输映射，因此在变量插值阶段按设计拒绝；该次不计为配置验证，最终结论采用统一脚本注入测试值后的通过结果。
- 边界：Doctor 证明配置、CLI、内部凭据网关、AUR 和 source proxy 路径可用，不证明 provider API key 有效，也不产生任何审计结论。真实 provider 审计仍保留为部署验收项；Fetch VM 内官方依赖下载已由后续独立 KVM 验收补齐。

## 2026-08-10：有界 Job 日志与证据详情

- Builder 成功目录中的 QEMU stdout/stderr、fetch、build 和 namcap 日志，以及失败诊断目录中的 QEMU、Guest 错误和 makepkg 日志，会形成路径白名单内的结构化证据。普通文件保存完整大小、SHA-256、截断标记、最多前 128 KiB 的 Base64 和可用的 UTF-8；超过 64 MiB 时不重新读取，并明确写入省略原因。
- Controller 拒绝未知或重复路径、无效摘要、超过 1 MiB 的日志响应、Base64/UTF-8 不一致和完整小日志摘要不匹配。成功和失败 Job 都写入 `job_evidence`；ReleaseEvidence 额外收集本批次实际 Profile 的 Controller 签名 Envelope 和包清单。
- 新增管理员 Job evidence API，构建页可以查看成功或失败的日志/provenance；Release 页面从证据摘要继续展开完整结构化文档。日志内容仍按不可信文本展示，不作为 HTML 执行。
- 单元测试覆盖日志摘要、128 KiB 截断、路径逃逸和完整内容摘要校验；前端 8 个用例中分别覆盖失败 Job 日志查看和 Release 证据展开。最终执行 `bash scripts/test-all.sh`：全仓库 106 个 Rust 测试、前端类型检查、8 个 Vitest 用例、生产构建和 Compose 安全策略检查全部通过。
- 该轮只验证了有界在线日志；后续“完整 Release 大文件证据与恢复”验证已把原始字节补入归档。

## 2026-08-11：完整 Release 大文件证据与恢复

- Builder 对每个成功 Build 生成 `profile.tar.zst`、`source.tar.zst` 和 `build-records.tar.zst`。单元测试实际生成包含 1 MiB 文件的 zstd tar，再由 bsdtar 列出并验证固定三文件 Manifest；最终 Arch Worker 镜像也以 UID 10001 实际完成压缩和读取。
- Controller migration `0026_release_evidence_files.sql` 保存固定 Attempt 路径、完整大小和 SHA-256。缺少任一归档时 ReleaseBatch 不能进入 Artifact 传输；证据与包共用 Controller 签名的 TransferCapability，Publisher 只接受摘要完全匹配的已验证导入。
- 断网、只读根文件系统、`cap_drop: ALL` 的实际 Signer 容器处理了含嵌套证据的测试 Release。输入和输出证据 SHA-256 一致，GPG 签名的 Release Manifest 列出同一清单；Publisher Worker 随后实际验签、提交并通过 `release-files` 返回该嵌套路径。
- Archiver 恢复测试调用生产 rsync `--link-dest` 快照路径，签名 ArchiveReceipt 的递归文件集合包含 `evidence/attempt/source.tar.zst`，并从不可变 Release 目录逐字节恢复 `complete source bytes`。
- 最终执行 `bash scripts/test-all.sh`：全仓库 108 个 Rust 测试、前端 TypeScript 检查、8 个 Vitest 用例、生产构建和 Compose 安全策略检查全部通过。容器测试所用临时 GPG 密钥和测试包只用于本地验证，不属于仓库发布密钥。

## 2026-08-11：pacoloco 与缓存指标

- 使用上游 1.8 镜像的固定 digest 构建 AURsmith 派生镜像，强制 UID/GID 65532、只读根文件系统、`cap_drop: ALL`、`no-new-privileges` 和独立缓存卷；命名卷首次挂载后实际确认该用户可写缓存。
- Caddy 配置验证通过。在临时 Docker 网络中经正式 `/arch-cache/core/os/x86_64/core.db` 路由连续请求两次，pacoloco `/metrics` 实际返回 requests 2、miss 1、hit 1。
- Publisher Worker 只允许无凭据、无参数的内部 HTTP `/metrics` URL，解析并聚合请求、命中、未命中、错误、缓存字节和包数量；单元测试覆盖多 repo 聚合和外部 HTTPS/内嵌凭据拒绝。Controller 指标从活动 Publisher 的签名状态快照读取该全局统计。

## 2026-08-11：Fetch KVM 真实官方依赖下载

- 使用 `AURSMITH_ARCH_MIRROR=https://mirrors.tuna.tsinghua.edu.cn/archlinux` 从头生成不可变 Profile。构建补齐根文件系统 `pacman.conf`、Arch Linux keyring，以及 initramfs 中的 `9pnet_virtio` 和 `virtio_net`；导出的镜像选择和文件摘要继续进入签名 Profile 声明。
- Builder 在只读根文件系统、`cap_drop: ALL`、`no-new-privileges` 且只挂载 `/dev/kvm` 的容器内启动 Fetch VM。QEMU user network 保持 `restrict=on`，唯一 guestfwd 指向 Attempt 独占的 Builder 回环中继，中继再连接无特权 Squid source proxy；Build VM 的 `-nic none` 逻辑没有改变。
- 测试 Revision 的 `makedepends=('tree')`。Fetch Guest 实际下载并通过 Arch keyring 验签 `tree-2.3.2-1-x86_64.pkg.tar.zst`，最终 Job `e4d002d5-85bb-4d76-87a1-adcd4f82a03f` 成功；FetchResult 记录 `tree 2.3.2-1`、44562 字节、SHA-256 `8a6230468cc31a2c984a41c092035dd16bf97e737ec3241490724a5419903739`，下载阶段为 901 毫秒，包及分离签名均进入 Source Manifest。
- 排查中先后真实暴露并修复了旧 Profile 缺少 9p 网络模块、`pacman.conf`、virtio 网卡驱动和初始化 keyring的问题。最后一个失败来自误把 pacman 的非 Query 选项用于 `-Qp`；正式实现改为用固定 argv 的 bsdtar 从已验签包读取 `.PKGINFO`，并排除 `.sig` 文件。只有上述最终成功 Attempt 计入 B02/B05 验收。
- 最终执行 `bash scripts/test-all.sh`：全仓库 110 个 Rust 测试、前端 TypeScript 检查、8 个 Vitest 用例、生产构建和 Compose 安全策略检查全部通过。
- 边界：该 fixture 没有额外 AUR source URL，因此本条严格证明的是同一个 Fetch KVM 内的受限网络、官方包下载、签名校验和溯源记录；AUR source 的实际 HTTPS 路径由同一代理机制和既有 Publisher Doctor覆盖，但不把二者合并声称为任意上游源码均已安全审计。
