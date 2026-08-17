# 当前验证记录

本文件只记录精简架构的当前验证，不继承已删除的 KVM、Profile、独立 Signer、Archiver、Capability、告警或长期证据链结论。

## 2026-08-18：本地完整快速验证

- `./scripts/test-all.sh` 通过；
- Rust：Controller 49、Domain 13、Protocol 4、Repository 6、Worker 20、其他组件 23，共 115 项；
- 前端类型检查、6 个 Vitest 用例和 Vite 生产构建通过；
- Compose 安全检查通过；
- Repository 测试实际调用 Arch `repo-add`/`repo-remove`、`makepkg` 和 GPG 生成 keyring 包。

## 2026-08-18：生产数据库副本迁移演练

- 使用 SQLite `.backup` 从生产 Controller 得到一致性副本，SHA-256 为 `1b1dbacfd6ceb5013532cfceffb7e0fcf788bc6e604960560f8b38f316ca7585`；
- 当前迁移已在该副本上再次执行：61 条订阅（49 条直接订阅）、192 条 Revision、240 条 Agent 运行和 103 条已提交 Release 数量保持不变；
- `PRAGMA integrity_check` 返回 `ok`，`PRAGMA foreign_key_check` 无结果；旧 events、alerts、archive、profile、TransferCapability、ReleaseAuthorization 表及 `releases.writer_epoch` 已删除；
- 应用 `0032_fixed_runtime.sql` 后，`PRAGMA foreign_key_check` 无结果，`PRAGMA integrity_check` 为 `ok`；
- 管理员 1、直接订阅 49、revision 192、Agent run 240、current release 指针 1，迁移前后相同；
- 迁移窗口中活动发布、上传和 Build 均为 0；旧终态上传与报告没有进入新运行表。

## 待本轮完成

- 精简镜像部署到真实公网设备与家庭 Builder；
- 真实 AUR full/diff 审查、Docker Build、rrsync、Publisher 直接签名与 pacman 安装全流程；
- 部署后 current/previous、回退和终态工作区检查。
