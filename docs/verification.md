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
