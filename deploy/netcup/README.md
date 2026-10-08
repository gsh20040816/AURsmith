# netcup 公网节点

公网节点只运行 `deploy/server/compose.yaml` 中的两个容器：

- `aursmithd`：Web/API、AUR 同步、进程内 2+1 审查、构建调度与期望状态发布计划；绑定宿主 `127.0.0.1:18443`。
- `signer`：`network_mode: none`，唯一持有仓库签名私钥，通过共享的 `exchange` 卷收发计划与结果，写入宿主仓库目录。

宿主 Caddy 使用 `Caddyfile.snippet`：管理站点反代到 `aursmithd`，`repo.*` 直接以静态文件提供
`AURSMITH_REPOSITORY_HOST_DIR`（默认 `/srv/aursmith/repo`）。不再有仓库 HTTP 服务容器、SSH/rrsync
中继、agent runner 或凭据网关。家庭 Builder 只通过 HTTPS 主动连接 `aursmithd`。

部署、迁移与回滚步骤见 `docs/deployment.md`。

## Arch 官方接口 IPv6 通道（可选）

当公网 IPv4 访问 `archlinux.org` 出现 TLS EOF，而宿主 IPv6 可达时，可运行
`arch-ipv6-proxy.py` 和 `aursmith-arch-ipv6-proxy.service`，并在 `deploy/server/.env` 中设置
`AURSMITH_ARCH_HTTPS_PROXY=http://192.168.64.1:19443`。默认不设置，官方接口直接连接；此选项
仅作用于 Arch 官方元数据请求。

通道只绑定 Compose 网络 `aursmith` 的网关 `192.168.64.1`（`compose.yaml` 中固定网段
`192.168.64.0/20`），仅接受该网段访问，仅允许 `CONNECT archlinux.org:443`，通过 DNS AAAA 地址
连接上游。TLS 握手和证书校验由 aursmithd 完成，通道不解密 TLS。

```sh
install -d -m 0755 /usr/local/libexec
install -m 0644 deploy/netcup/arch-ipv6-proxy.py /usr/local/libexec/aursmith-arch-ipv6-proxy.py
install -m 0644 deploy/netcup/aursmith-arch-ipv6-proxy.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now aursmith-arch-ipv6-proxy
```

回滚时先移除代理变量并重建 `aursmithd`，再 `systemctl disable --now aursmith-arch-ipv6-proxy`。
