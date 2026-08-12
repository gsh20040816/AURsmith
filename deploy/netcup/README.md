# netcup 单机公网节点部署

该拓扑把 Controller、Agent、Publisher 和 Signer 放在 `netcup`，Builder 保留在具有 KVM 的桌面机。第一版不部署独立 Archiver，历史版本由 Publisher 按保留策略管理。

## 网络

部署前一次性创建所有 Stack 共享的内部传输网络：

```bash
docker network create --internal aursmith-backbone
```

Controller 使用 `publisher-ssh:2222` 访问同机 Worker。该网络不替代各 Stack 原有 control、agent 和 egress 网络。

宿主只开放：

- 80/443：由现有宿主 Caddy 提供控制台和仓库 HTTPS；
- 12223/tcp：Builder 使用 forced-command SSH 推送 Artifact；
- 13128/tcp：Fetch VM 的 Squid 入口，只允许 Builder 当前公网地址访问。

控制台容器绑定 `127.0.0.1:18443`，仓库绑定 `127.0.0.1:18081`。宿主 Caddy 追加 `Caddyfile.snippet`；其中内层 TLS 只用于回环链路，因此宿主反代明确忽略内层自签证书，客户端看到的仍是宿主 Caddy 的公开证书。

## Compose override

启动时必须同时指定基础文件和本目录对应的 `compose.netcup.yaml`。示例：

```bash
docker compose --env-file runtime/deployment/publisher.env \
  -f deploy/publisher/compose.yaml \
  -f deploy/publisher/compose.netcup.yaml up -d
```

不得把 `aursmith-backbone` 配成非 internal 网络。Publisher 的 12223/tcp 只用于家庭 Builder 持 Capability 主动推送产物。

Publisher 默认保留最近 30 天全部 Release，并为每个包至少保留最近 3 个不同版本。可通过 `AURSMITH_RELEASE_RETENTION_DAYS` 和 `AURSMITH_RELEASE_RETENTION_MIN_VERSIONS` 调整；两个值都必须大于零。
