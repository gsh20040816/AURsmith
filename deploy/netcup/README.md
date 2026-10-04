# netcup 公网节点

公网节点固定运行 Controller/Web、Publisher、三个 low Runner、一个 high Runner 和 credential gateway。家庭 Builder 只主动连接公网节点。

Controller 绑定宿主 `127.0.0.1:18443`，Publisher 仓库绑定 `127.0.0.1:18081`，Publisher rrsync SSH 绑定生产指定端口。宿主 Caddy 使用 `Caddyfile.snippet` 提供公网 TLS；Compose 内不运行第二层 Caddy。

部署使用 `runtime/deployment/controller.env`、`publisher.env` 与 `fixed-runtime.env`，只启动 `deploy/controller/compose.yaml`。该单一 Compose 栈包含公网节点的全部固定服务；Controller 与 Publisher 通过共享 Unix Socket 通信，不再维护两个 Compose project 或 Controller→Publisher SSH 控制链路。

部署前后按 `docs/deployment.md` 执行数据库/仓库/GPG 备份、迁移副本检查、容器健康检查和独立 pacman 验证。旧的 Controller signing key、Worker verifying key、Signer、pacoloco 和 Archiver 配置不再使用。

## Arch 官方接口 IPv6 通道

当公网 IPv4 访问 `archlinux.org` 出现 TLS EOF，而宿主 IPv6 可达时，可运行
`arch-ipv6-proxy.py` 和 `aursmith-arch-ipv6-proxy.service`，将 Publisher 的
`AURSMITH_ARCH_HTTPS_PROXY` 设置为 `http://192.168.64.1:19443`。
默认不设置此变量，官方接口仍直接连接。此选项仅作用于官方元数据请求。

通道只绑定 netcup 的 Docker egress 网关 `192.168.64.1`，仅接受该网段访问，
仅允许 `CONNECT archlinux.org:443`，通过 DNS AAAA 地址连接上游。
TLS 握手和证书校验由 Publisher 完成，通道不解密 TLS。
如果 Docker egress 网段变化，应同步调整脚本中的监听地址和允许网段。

提交并推送修复后，在 netcup 同步该提交，再安装服务：

```sh
install -d -m 0755 /usr/local/libexec
install -m 0644 deploy/netcup/arch-ipv6-proxy.py /usr/local/libexec/aursmith-arch-ipv6-proxy.py
install -m 0644 deploy/netcup/aursmith-arch-ipv6-proxy.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now aursmith-arch-ipv6-proxy
```

将代理 URL 写入 `runtime/deployment/publisher.env`，使用标准部署命令仅重建
Publisher：`up -d --no-deps --build publisher`。回滚时先移除代理变量并重建
Publisher，再 `systemctl disable --now aursmith-arch-ipv6-proxy`。

宽泛的 AUR 名称和描述搜索遇到上游结果过多时会改用名称搜索；结果按精确
包名、包名族前缀、普通前缀、名称包含、描述匹配排序，最多返回 200 项。
