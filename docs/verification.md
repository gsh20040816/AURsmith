# 验证记录

本文件记录实际执行过的集成验证。单元测试数量和外部软件版本会随提交变化，因此每条记录必须绑定源码提交或工作树状态，不能把局部冒烟描述成完整验收。

## 2026-08-10：KVM ProfileFixture

- 验证对象：提交 `8f9dc06` 之后的工作树，包含随后待提交的 QEMU 内存与诊断修复。
- 宿主能力：`/dev/kvm` 可用，QEMU、qemu-img 和 `/usr/lib/virtiofsd` 均由宿主提供。
- Profile：通过非 privileged、断网导出容器重建 Arch rootfs、Linux 内核、initramfs 和最新版 Guest Agent。
- 授权：`prepare_kvm_fixture` 使用正式 Ed25519/CBOR Envelope 实现和固定测试密钥，在临时目录生成 Profile 与 ProfileFixture JobSpec；没有使用生产密钥。
- 执行：真实 Worker daemon 创建 qcow2 overlay、两个 virtiofs 通道和无网卡 KVM VM；Guest 再次验证 Controller 签名，以普通 `builder` 用户执行 `makepkg`。
- 结果：Job `33246641-748c-42d7-ad33-017a3223d307` 成功，Attempt `e238c934-c38b-4799-9579-5f418f3cff6e` 返回 `profile_fixture`；生成 `aursmith-profile-fixture-1-1-any.pkg.tar.zst`，大小 1255 字节，SHA-256 为 `a09eedb98a2fdb0630730a23afc5d57c3b8ce45636f359be3436d43714aaa2e7`；provenance 明确记录 `network=none`。
- 实际发现并修复：QEMU memfd/NUMA 后端缺少匹配的 `-m`；最小 Arch rootfs 的 os-release 位于 `/usr/lib/os-release`；失败 VM 日志此前会随 runtime 清理而丢失。
- 未覆盖：这次只验证 ProfileFixture，不代表 Fetch 代理、真实 AUR source、批次内 AUR 依赖、Publisher、Signer 或 Archiver 已完成端到端验证。
- 清理：四个本次创建的临时 Profile/runtime 目录已移动到桌面环境回收站，可恢复；未保留运行中的 QEMU、virtiofsd 或 Worker 进程。

## 2026-08-10：Fetch 到离线 Build 接力

- 验证对象：提交 `c8d7851` 之后加入精确依赖快照的工作树。
- 输入：测试 PKGBUILD 不含外部 source 和依赖；它通过签名 JobSpec 的内联输入进入 Worker，不从宿主共享可写目录注入。
- Fetch：Job `42a6f6c4-2b19-44b3-9a32-e2c86c9b8d0d`、Attempt `59d2c20c-f0a0-4b80-a43b-b1d44ecba210` 在受限联网 KVM 中成功；Source Manifest 摘要为 `22f6a17734aea5df41ef3498c2bdf79fd5cb4d13e858b3c65cc5d428d0be957a`，并完整记录 PKGBUILD、src 目录和 Agent 风险选读文本。
- Build：新的 Job `74881cb7-c2be-49e4-96e7-f6a7b4b0f07c` 只引用上述 Fetch Attempt 和摘要；Worker 从 completed 目录重新验证并创建新 overlay。Build VM 使用 `network=none`，成功生成 `aursmith-fetch-fixture-1-1-any.pkg.tar.zst`，解析到包名、版本 `1-1`、架构 `any`，SHA-256 为 `744859e3eceb7675962040dc91150b1cef219936e1ad4a11bec5d630119ee24b`。
- 边界：fixture 没有实际下载官方依赖，因此验证了依赖为空时的快照与离线安装路径，但未验证 pacman 经 source proxy 下载真实官方包的网络行为。
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
