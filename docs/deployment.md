# 两台设备部署

## 公网设备

公网设备使用：

- `deploy/controller/compose.yaml` 与 `compose.netcup.yaml`；
- `deploy/publisher/compose.yaml` 与 `compose.netcup.yaml`；
- 现有宿主 Caddy，反代片段见 `deploy/netcup/Caddyfile.snippet`。

Controller 必须配置公开 Origin、固定 Publisher SSH endpoint、Builder Bearer token 的 SHA-256、Publisher SSH host key、三个 low 与一个 high 的真实 provider/model。Publisher 必须配置 SSH forced-command 凭据和仓库 GPG 公私钥。多余的旧 signing key、Worker verifying key、writer epoch、Signer、pacoloco、Archiver、通知和备份配置应删除。

部署前先确认没有 `issued/signing` 发布、`issued/export_ready` 上传或 `dispatched/running/uncertain` Build。使用 SQLite `.backup` 生成一致性数据库备份；仓库目录、Publisher Journal 和 GPG 私钥由宿主备份工具分别备份并核对权限与摘要。先在备份副本应用全部 migration，执行 `PRAGMA foreign_key_check` 和 `PRAGMA integrity_check`，再部署生产镜像。

## 家庭 Builder

Builder 使用 `deploy/builder/compose.yaml`。先构建固定 Build image，再启动 worker：

```sh
docker compose --env-file runtime/deployment/builder.env \
  --env-file runtime/deployment/fixed-runtime.env \
  -f deploy/builder/compose.yaml --profile build-image build build-image
docker compose --env-file runtime/deployment/builder.env \
  --env-file runtime/deployment/fixed-runtime.env \
  -f deploy/builder/compose.yaml up -d --build worker
```

Builder 的轮询 URL 必须精确指向 `/api/v1/builder/poll`；旧 `/api/v1/reverse-workers/poll` 不兼容且应从部署环境删除。Builder secret 文件至少包括 Controller Bearer token、Publisher write-only SSH key 和 known_hosts。Bearer token 与 SSH 私钥必须属于 `AURSMITH_SECRET_GID` 对应的宿主组且权限为 `0440`；known_hosts 可以是 `0444`。仅设为创建者 `0600` 会使以 UID 10001 运行的 Worker 无法读取。AURsmith 容器不开放入站端口。Builder jobs 路径必须是宿主绝对路径，Docker Socket 只挂载给 Builder daemon，不进入临时 Build 容器。

## 恢复顺序

1. 停止 Controller 与 Publisher 写入；
2. 恢复 GPG 私钥及严格权限；
3. 恢复 Publisher 仓库和 Journal；
4. 恢复 Controller SQLite；
5. 启动 Publisher，验证 current/previous Manifest、包和 GPG 签名；
6. 启动 Controller，再启动 Builder；
7. 用独立 Arch 客户端执行 keyring 核对、`pacman -Sy` 和一次安装验证。

同机 SQLite 副本不是灾备。备份周期、异机存储和密钥托管由宿主工具负责。
