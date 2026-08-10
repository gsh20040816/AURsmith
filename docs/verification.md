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
