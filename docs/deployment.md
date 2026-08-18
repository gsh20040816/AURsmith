# 两台设备部署

## 公网设备

公网设备使用：

- 单一 `deploy/controller/compose.yaml`，同时运行 Controller/Web、Publisher、三个 low Runner、一个 high Runner 和 credential gateway；
- 现有宿主 Caddy，反代片段见 `deploy/netcup/Caddyfile.snippet`。

部署时同时加载 `runtime/deployment/controller.env` 与 `runtime/deployment/publisher.env`。Controller 必须配置公开 Origin、Builder Bearer token 的 SHA-256、全局更新和构建并发参数、三个 low 与一个 high 的真实 provider/model。Publisher 的本地控制面只使用共享 Unix Socket，不配置 SSH 凭据；只有家庭 Builder 向 Publisher 推送构建产物时使用受限 SSH forced-command。Publisher 必须配置该 forced-command 凭据和仓库 GPG 公私钥。多余的旧 Controller→Publisher SSH key、signing key、Worker verifying key、writer epoch、Signer、pacoloco、Archiver、通知和备份配置应删除。

```sh
docker compose --env-file runtime/deployment/controller.env \
  --env-file runtime/deployment/publisher.env \
  --env-file runtime/deployment/fixed-runtime.env \
  -f deploy/controller/compose.yaml up -d --build
```

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

Build image 默认依次启用 Arch 官方 `core/extra/multilib` 与 `archlinuxcn`，后者使用 `AURSMITH_ARCHLINUXCN_MIRROR` 配置的 HTTPS 镜像，并通过 `archlinuxcn-keyring` 校验软件包签名。官方仓库排列在前，普通同名依赖仍优先使用官方包；当版本约束排除官方包时，pacman 才会选择后续仓库中满足约束的 Provider，例如用 `cmake3` 满足 `cmake<4.4`。

## 恢复顺序

1. 停止公网单一 Compose 栈和家庭 Builder 的写入；
2. 恢复 GPG 私钥及严格权限；
3. 恢复 Publisher 仓库和 Journal；
4. 恢复 Controller SQLite；
5. 启动公网单一 Compose 栈，验证 Publisher 的 current/previous Manifest、包和 GPG 签名；
6. 验证 Controller 和 Publisher 健康后，再启动 Builder；
7. 用独立 Arch 客户端执行 keyring 核对、`pacman -Sy` 和一次安装验证。

同机 SQLite 副本不是灾备。备份周期、异机存储和密钥托管由宿主工具负责。
