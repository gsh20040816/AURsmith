# netcup 单机公网节点部署

该拓扑把 Controller、Agent、Publisher 和 Signer 放在 `netcup`，Builder 保留在运行 Docker Engine 的可信桌面机。第一版不部署独立 Archiver，历史版本由 Publisher 按保留策略管理。

## 网络

部署前一次性创建所有 Stack 共享的内部传输网络：

```bash
docker network create --internal aursmith-backbone
```

Controller 使用 `publisher-ssh:2222` 访问同机 Worker。该网络不替代各 Stack 原有 control、agent 和 egress 网络。

宿主只开放：

- 80/443：由现有宿主 Caddy 提供控制台和仓库 HTTPS；
- 12223/tcp：Builder 使用 forced-command SSH 推送 Artifact；

Controller 自己提供 Web/API 并绑定 `127.0.0.1:18443`，Publisher Worker 自己提供仓库并绑定 `127.0.0.1:18081`，pacoloco 绑定 `127.0.0.1:19129`。宿主 Caddy 追加 `Caddyfile.snippet` 后统一处理公网 TLS 和缓存头；AURsmith Stack 内没有第二层 Caddy 或 TLS。部署过程只准备并验证片段，不修改或 reload 宿主 Caddy。

## Compose override

启动时必须同时指定基础文件和本目录对应的 `compose.netcup.yaml`。示例：

```bash
docker compose --env-file runtime/deployment/publisher.env \
  -f deploy/publisher/compose.yaml \
  -f deploy/publisher/compose.netcup.yaml up -d
```

不得把 `aursmith-backbone` 配成非 internal 网络。Publisher 的 12223/tcp 只用于家庭 Builder 持 Capability 主动推送产物。

Publisher 默认保留最近 30 天全部 Release，并为每个包至少保留最近 3 个不同版本。可通过 `AURSMITH_RELEASE_RETENTION_DAYS` 和 `AURSMITH_RELEASE_RETENTION_MIN_VERSIONS` 调整；两个值都必须大于零。

Builder 先用 `docker compose -f deploy/builder/compose.yaml --profile build-image build build-image` 生成固定 Build image，再用不带该 profile 的 `up -d worker` 启动。Build 容器使用普通 bridge 网络；Builder 只通过 `netcup.shgao.top:12223` 主动推送 Artifact。
