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
