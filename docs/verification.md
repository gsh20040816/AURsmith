# 当前验证记录

本文件只记录精简架构的当前验证，不继承已删除的 KVM、Profile、独立 Signer、Archiver、Capability、告警或长期证据链结论。

## 2026-08-18：本地完整快速验证

- `./scripts/test-all.sh` 通过；
- Rust：Controller 49、Domain 13、Protocol 4、Repository 6、Worker 22、其他组件 23，共 117 项；
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

## 2026-08-18：真实部署与端到端验证

- Controller、三个低成本 Agent、一个高成本 Agent、凭据网关、Publisher 和家庭 Builder 均部署代码提交 `0101634`；Controller、Publisher 和 Builder 健康检查通过；
- 生产 Doctor 返回 `ready: true`：Builder 最近轮询、仓库 GPG、TLS、三个低成本 Agent、高成本 Agent 和 Publisher AUR RPC 全部正常；
- 对真实 AUR 包 `paru-alpm-bin 2.1.0.alpm16.1-1` 完成首次审查：`deepseek-v4-flash`、`gpt-5.6-luna`、`gpt-5.6-terra` 三个低成本槽位均独立批准，三份报告 SHA-256 各不相同；
- 同一 AUR commit 的最终重建创建 Revision `c3ff2ee7-4434-4bac-8ecd-6b142754c62b`，明确记录 `approved_wrapper_reuse`，复用来源为首次已批准审查包 `4b94caa3b77e584265109aca3b559a49fbed5f8cef7186e7891b0a3f958fd01b`；
- 最终重建完成真实 AUR 拉取、Docker Build、rrsync 上传、Publisher 本地 GPG 签名与 `repo-add`，提交 Release `938c52e6-880e-40fb-be10-0d3c1fbb5433`；
- 使用全新 Arch 容器和空 pacman keyring，导入并本地信任固定指纹 `BE59BEA40D9F50E7DA64BCBAFE313D9CC82D812D` 后，数据库签名与包签名校验通过；安装得到 `paru-alpm-bin 2.1.0.alpm16.1-1`，执行得到 `paru v2.1.0 - libalpm v16.0.1`；
- 真实回退到 Release `7ede37b4-2b87-4902-98bc-24c84e210ae5` 成功，Controller current 指针和 Publisher 数据库链接一致；全新 Arch 客户端确认该版本不含 `paru-alpm-bin`；随后恢复当前 Release 并再次完成安装验证；
- Publisher 最终只保留 current/previous 两个 Release，运行目录无遗留发布工作区；Publisher Journal 无旧版授权信封；
- Controller 与 Publisher 数据库 `PRAGMA integrity_check` 返回 `ok`，外键检查无错误。

## 外部代理缓存说明

- 源站 Caddy 对仓库数据库、签名和包文件返回 `Cache-Control: no-cache, must-revalidate`；
- Cloudflare 公共响应仍被现有区级配置改写为 `max-age=14400, must-revalidate`。当前项目保存的令牌只有 DNS 权限，对 Cache Rules 查询和单 URL purge 均返回 Cloudflare `10000 Authentication error`；应用侧与源站侧已验证，修改 Cloudflare 区级缓存规则需要额外账号权限。
