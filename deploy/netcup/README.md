# netcup 公网节点

公网节点固定运行 Controller/Web、Publisher、三个 low Runner、一个 high Runner 和 credential gateway。家庭 Builder 只主动连接公网节点。

Controller 绑定宿主 `127.0.0.1:18443`，Publisher 仓库绑定 `127.0.0.1:18081`，Publisher rrsync SSH 绑定生产指定端口。宿主 Caddy 使用 `Caddyfile.snippet` 提供公网 TLS；Compose 内不运行第二层 Caddy。

部署使用 `runtime/deployment/controller.env` 与 `publisher.env`，并覆盖：

- `deploy/controller/compose.yaml` + `compose.netcup.yaml`；
- `deploy/publisher/compose.yaml` + `compose.netcup.yaml`。

部署前后按 `docs/deployment.md` 执行数据库/仓库/GPG 备份、迁移副本检查、容器健康检查和独立 pacman 验证。旧的 Controller signing key、Worker verifying key、Signer、pacoloco 和 Archiver 配置不再使用。
