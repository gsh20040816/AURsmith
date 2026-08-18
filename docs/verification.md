# 当前验证记录

本文件只记录精简后的固定两设备架构，不继承已删除的 KVM、Profile、独立 Signer、Archiver、Capability、告警或长期证据链结论。

## 2026-08-18：本地验证

- `./scripts/test-all.sh` 通过；Rust 共 123 项：Agent Gateway 2、Agent Runner 11、Controller 52、Domain 13、Guest Agent 5、Protocol 5、Repository 6、Worker 23、CLI 6；
- 前端类型检查、6 个 Vitest 用例和 Vite 生产构建通过；
- `cargo clippy --workspace --all-targets -- -D warnings` 通过；
- Compose 安全检查通过；Repository 测试实际调用 Arch `repo-add`/`repo-remove`、`makepkg` 和 GPG 生成 keyring 包；
- 真实 AUR/官方仓库/VCS/doctor 上游 smoke 与 Builder SSH smoke 通过；
- 在发现生产 Docker 不发布纯 internal 网络容器端口后，新增 `publisher-ssh` 必须同时加入 `publisher-control` 和 `edge` 的回归检查；该改动后的 Compose 安全检查再次通过。
- 两台设备完成原位迁移后，删除 Worker 启动时对 TransferCapability、旧 Publisher Release 表和旧签名信封的永久迁移探测，并移除 `transfer`、`completed_transfers`、`capability_id` 等旧协议字段别名；当前 Builder 消息对未知字段失败关闭。

## 2026-08-18：生产迁移

- 迁移前备份位于 `/opt/aursmith/runtime/deployment-backups/20260818T052316Z-fixed-runtime/`；Controller SHA-256 为 `8d51506c1a9a89b0b9f9b72e30df8ab2b90db2e21862002b87419bfce8ae96e1`，Publisher Worker SHA-256 为 `ddfbf76eb4c030e2bf8e3bd2612f9c43ad39005d19fba08fd47a2e6911d5f604`；
- 先在生产数据库副本执行迁移 0033、0034：管理员 1、订阅 62、Revision 195、Audit Bundle 175、Agent Run 247、Release 109，迁移前后数量一致；
- 副本和生产原库的 `PRAGMA integrity_check` 均返回 `ok`，`PRAGMA foreign_key_check` 均无结果；
- 迁移 0033 将公开 Evidence 收敛为有界 Job Log，并删除 Evidence 文件表；迁移 0034 删除通用 Worker、角色、Profile 和逐任务资源限制字段；生产 `_sqlx_migrations` 已确认两条迁移成功；
- 最终生产数据为管理员 1、订阅 62、Revision 196、Audit Bundle 176、Agent Run 250、Release 113（其中 109 个 committed）。端到端测试产生的历史 Revision/Audit 保留用于追溯，临时订阅本身已删除。

## 2026-08-18：真实部署与端到端验证

- 公网设备已合并为单个 `deploy/controller/compose.yaml` 项目；服务器主线、Controller、Publisher、Publisher SSH 与家庭 Builder 均部署 `930a552`。Agent 镜像为 `889f36e`，后续提交没有修改 Agent Runner；
- 生产 Doctor 返回 `ready: true`：Builder 最近轮询、仓库 GPG、TLS、三个低成本 Agent、高成本 Agent 和 Publisher AUR RPC 全部正常；
- keyring 失败恢复已真实验证：失败 Release `b81c0d0d-d6ed-465e-916f-3ff2a487ab3d` 通过显式 retry 创建新身份 `e567208f-0c61-4237-b539-92c6949744e3`，复用原已验证计划并成功提交，没有重新构建包；
- Publisher 已生成真实 `aursmith-keyring 1:1-1`。generation 为 1，固定指纹为 `BE59BEA40D9F50E7DA64BCBAFE313D9CC82D812D`，发布时间为 `2026-08-18T05:51:55.985818795Z`，下次到期时间为 `2026-09-17T05:51:55.985818795Z`；
- 临时加入真实 AUR 包 `rate-mirrors-bin 0.31.0-1`，创建 Revision `5d8344a3-261d-4592-a889-d313f537a1d2` 和 Audit Bundle `f8ac2d290b6e02de1622c94f1bf9ddd59ae5cc6deefacec23794ae303c7c2db0`；
- 三个低成本槽位同时运行并独立批准：`deepseek-v4-flash`、`gpt-5.6-luna`、`gpt-5.6-terra`。三票收齐后 Bundle 才转为 approved，未错误触发高成本审查；
- 家庭 Builder 完成 Job `c66519ad-bf62-4362-a8ae-16ea2ac6bd65` 的真实 AUR 拉取和隔离 Docker Build。首次 rrsync 暴露生产缺陷：`publisher-ssh` 只连接 internal 网络，容器内 sshd 正常但 Docker 没有安装 12223 宿主发布规则；
- 提交 `938cb29` 将该入口接入现有 `edge` 网络并保留内部 `publisher-control`。重建 SSH sidecar 后，Docker 显示 `0.0.0.0:12223->2222/tcp`，固定 Builder key 通过 forced-command 认证，原 Upload `88e869be-25e4-4835-9063-28eb174d14a5` 自动重试并转为 verified；
- Publisher 完成产物复验、GPG 签名和 `repo-add`，提交 Release `7e267811-8ae0-4548-84f0-e02b98ae0d07`，Manifest SHA-256 为 `1c155876b15e9747e96152f0177e4aadbc2deb3fcd837cd76b5dfde77eab80b4`；
- 使用全新 Arch 容器和空 pacman keyring，先核对并本地信任上述完整指纹，再从公网仓库同步。数据库签名和包签名均通过，安装得到 `rate-mirrors-bin 0.31.0-1`，程序报告 `rate-mirrors config 0.31.0`；
- 删除临时订阅后，清理 Release `fe5a20df-82b2-45bc-a9cc-5bd052c0a90e` 成功提交，Manifest SHA-256 为 `67acd9429bbe34636afd998d2b9f71d63469a31687ecb03ec841504111997ca2`，当前仓库数据库不再包含测试包；
- 真实回滚到 `7e267811-8ae0-4548-84f0-e02b98ae0d07` 后，公网数据库重新包含测试包；随后恢复 `fe5a20df-82b2-45bc-a9cc-5bd052c0a90e`，公网数据库再次移除测试包；
- Publisher 最终只保留上述 current/previous 两个 Release 目录，Controller current 指针为 `fe5a20df-82b2-45bc-a9cc-5bd052c0a90e`，临时订阅数量为 0；Controller 和 Publisher 数据库完整性检查通过。
- 严格协议版本部署后，家庭 Builder 健康检查为 healthy，生产 Doctor 再次返回 `ready: true` 且 Builder 最近轮询正常；current/previous API 与上述两个 Release 及 Manifest 摘要一致。

## 外部代理缓存说明

- 源站 Caddy 对仓库数据库、签名和包文件配置为重新验证；
- Cloudflare 公共响应仍被现有区级配置改写为 `max-age=14400, must-revalidate`。仓库数据库切换测试使用独立查询参数绕过边缘旧对象，并已与 Publisher 当前指针交叉核对；修改 Cloudflare 区级缓存规则仍需要相应账号权限。
