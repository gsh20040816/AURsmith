# netcup 公网节点

公网节点固定运行 Controller/Web、Publisher、三个 low Runner、一个 high Runner 和 credential gateway。家庭 Builder 只主动连接公网节点。

Controller 绑定宿主 `127.0.0.1:18443`，Publisher 仓库绑定 `127.0.0.1:18081`，Publisher rrsync SSH 绑定生产指定端口。宿主 Caddy 使用 `Caddyfile.snippet` 提供公网 TLS；Compose 内不运行第二层 Caddy。

部署使用 `runtime/deployment/controller.env`、`publisher.env` 与 `fixed-runtime.env`，只启动 `deploy/controller/compose.yaml`。该单一 Compose 栈包含公网节点的全部固定服务；Controller 与 Publisher 通过共享 Unix Socket 通信，不再维护两个 Compose project 或 Controller→Publisher SSH 控制链路。

部署前后按 `docs/deployment.md` 执行数据库/仓库/GPG 备份、迁移副本检查、容器健康检查和独立 pacman 验证。旧的 Controller signing key、Worker verifying key、Signer、pacoloco 和 Archiver 配置不再使用。
