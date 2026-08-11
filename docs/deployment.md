# 容器化部署

## 宿主机要求

- Docker Engine 与 Docker Compose。
- Builder 宿主机需要 x86_64 KVM 和可访问的 `/dev/kvm`。
- Controller、Publisher 和 Archiver 宿主机不要求 KVM。
- 每个角色使用独立持久卷、SSH host key 和 Controller 访问密钥。

AURsmith 不安装裸机 daemon。文档中的所有管理命令都通过构建产物或 `docker compose exec` 运行。

## 初始化密钥

运行 `cargo run -p aursmithctl -- generate-controller-key` 生成 Controller Ed25519 密钥对。输出中的私钥十六进制值写入 Controller 的 `controller_signing_key` secret；公钥值写入每个 Worker 的 `AURSMITH_CONTROLLER_VERIFYING_KEY_HEX`。

使用 `openssl rand -hex 32` 生成一次性设置令牌。使用 `ssh-keygen -t ed25519` 分别生成：

- Controller 访问 Worker 的客户端密钥；
- 每个 Worker 自己的 SSH host key。

Controller 使用严格的 `known_hosts`，不能配置 `StrictHostKeyChecking=no`。Worker 的 `authorized_keys` 只允许 Controller 公钥，实际命令仍由 `sshd_config` 中的 forced command 二次限制。

Worker 账户的 `/bin/sh` 只用于 OpenSSH 按其协议执行服务端 forced command；客户端提交的原始命令不会交给该 shell。`ForceCommand` 会无条件替换请求，`aursmithctl ssh-gateway` 再按固定语法白名单解析，且 PTY、转发、密码登录和交互会话均被禁用。

Publisher 的 `AURSMITH_AUR_BASE_URL` 默认固定为 `https://aur.archlinux.org/`。AUR 搜索、info、Provider 查询和 Git 快照均从 Publisher 发起；Controller Stack 不需要 AUR 外网出口。第一版单次订阅最多展开 64 个 AUR pkgbase，超过上限会明确失败并保持数据库不变。

开发机具备外网时，可以运行 `scripts/smoke-upstream.sh` 验证真实 AUR 搜索、普通包快照、Git VCS commit 固定和 Arch 官方包查询。该脚本使用临时 SQLite、Unix Socket 和 Publisher 进程，结束时清理全部临时状态，不接触生产配置。

Compose 的本地文件型 secret 不支持可靠设置容器内 `uid/gid/mode`。AURsmith 不依赖该行为：SSH 容器只在启动器复制 secret 时短暂使用 root，并且仅增加 `DAC_READ_SEARCH`、`CHOWN`、`SETGID`、`SETUID` 以及清空 capability 集合所需的 `SETPCAP`。启动器先校验 secret 是有界的普通文件，再以 `0600`、禁止覆盖的方式复制到 tmpfs、变更为服务 UID，随后通过 `setpriv` 清空 capability bounding/inheritable/ambient 集合、永久降权并以 `exec` 启动非 root sshd。Controller 也只把 SSH 客户端私钥物化到自己的私有 tmpfs。原始 secret 始终为只读挂载，容器重启后私有副本自动消失。

Compose file-backed secret 以 UID/GID `10001`、模式 `0400` 挂载。部署前使用 `docker compose config` 检查实际渲染结果，不要把 `deploy/*/secrets` 加入 Git。

## Agent provider 配置

Controller Stack 内固定运行三个低成本 Runner、一个高成本 Runner和一个凭据网关。Runner 镜像固定包含 Codex CLI `0.147.0` 与 Claude Code `2.1.226`；升级 CLI 必须修改镜像构建参数、重新构建并运行适配器回归测试，不能在运行中自动更新。

三个低成本 Runner 必须分别配置，变量前缀依次为 `AURSMITH_LOW_AGENT_1_*`、`AURSMITH_LOW_AGENT_2_*`、`AURSMITH_LOW_AGENT_3_*`；高成本 Runner 使用 `AURSMITH_HIGH_AGENT_*`。每套配置包括：

- `PROVIDER`：写入报告的 provider 标识，只允许字母、数字、连字符和下划线；
- `MODEL`：传给对应 CLI 的模型 ID；
- `REASONING_EFFORT`：Codex 思考强度，可为空或使用 `minimal/low/medium/high/xhigh`；
- `BASE_URL`：凭据网关访问的上游 HTTPS Base URL；
- `AUTH_STYLE`：`bearer` 或 `x-api-key`；Codex 兼容 provider 通常使用 `bearer`；
- `API_KEY_FILE`：宿主机上的独立 Docker secret 文件路径。

第一版的三个低成本 Runner 固定使用 Codex 适配器，但 model、provider、Base URL、API key 和思考强度彼此独立，避免把三票退化成同一配置的重复调用。自建兼容网关必须使用 HTTPS；第一版不允许明文 HTTP upstream。Runner 实际只看到凭据网关的 `/low-1/`、`/low-2/`、`/low-3/` 或 `/high/` 内部路径。

Web 设置页可以修改每日调用数、每月调用数和每月成本上限；这三项立即作用于后续调度并写入事件日志。设置页只显示 provider 配置来源和 Runner 状态。修改适配器、provider、模型或 Base URL 后需要重新创建 Agent Stack；更新 API key 时只替换对应 secret 并重启凭据网关，不能把 key 粘贴到 Web 表单。

将三个低成本 key 分别写入 `deploy/controller/secrets/low_agent_1_api_key`、`low_agent_2_api_key`、`low_agent_3_api_key`，高成本 key 写入 `high_agent_api_key`，权限设为仅部署账户可读。不要把 key 写进 `.env`、Compose environment、Controller 设置、Agent prompt 或日志。凭据网关读取 secret 后删除 Runner 发送的认证头，再按各自路由注入对应凭据；Runner 子进程中只有无权限占位令牌。

Codex 的自定义 provider 走 Responses API 兼容接口；Claude Code 的自定义 Base URL 需要提供 Anthropic Messages API 兼容接口。provider 名称只是可追踪标签，不会自动转换 API 协议。部署 Doctor 后续阶段会对两层分别执行不含软件包内容的结构化输出探测。

## 启动顺序

Controller Web 默认使用内部 CA。已有公网证书时，额外加载 `deploy/controller/compose.external-tls.yaml`，并通过 `AURSMITH_WEB_TLS_FULLCHAIN_FILE` 和 `AURSMITH_WEB_TLS_PRIVATE_KEY_FILE` 指向宿主证书副本。外部证书只作为 Web 容器的只读 secret，Controller 不挂载私钥；同时将 `AURSMITH_CLIENT_CA_CERTIFICATE_FILE` 设为空，客户端引导不再错误提示导入内部 CA。证书续期后必须替换 secret 副本并重建 Web 容器。

1. 启动 Publisher 和 Archiver Stack。
2. 启动至少一个 Builder Stack。
3. 启动 Controller Stack。
4. 通过 `docker compose exec controller aursmith-controller setup-token` 读取初始化令牌。
5. 在 Web 设置页创建管理员。
6. 注册三个角色的 Worker，并执行“探测”。
7. 运行 Doctor，确认 KVM、SSH、协议、仓库和归档存储状态。
8. 在订阅真实软件包前，确认四个 Agent Runner 都能返回符合 Schema 的测试报告，且报告与容器日志中不含 API key。

不同宿主机部署时只需要复制对应 Stack 的 Compose、镜像和该角色的 secret，不要复制其他角色的数据卷或私钥。

## Builder KVM 配置

Publisher Stack 自带最小 `source-proxy` 服务。跨设备部署时设置 `AURSMITH_SOURCE_PROXY_BIND=<Publisher 管理网 IP>:3128`，并在宿主防火墙上只允许 Builder 地址访问；默认 `127.0.0.1:3128` 只适合角色同机。代理只允许 80/443，拒绝 loopback、私网、link-local、运营商 NAT、文档网段、组播和其他保留地址，不提供磁盘缓存。

Publisher Stack 还自带独立 pacoloco。它只缓存 Arch 官方仓库，缓存卷为 `pacoloco-cache`，不应与 Publisher staging 或公开仓库卷合并。外部 Arch 上游由 Publisher Compose 的 `AURSMITH_ARCH_MIRROR` 构建参数配置，必须是无凭据和参数的 HTTPS Base URL。公开仓库 Caddy 把 `https://<稳定仓库域名>/arch-cache/` 转发为 pacoloco 的 `archlinux` 仓库；首次部署可连续请求同一 `core.db`，再在 Doctor 中确认 requests、misses 和 hits 递增。

Publisher Worker 在 Compose 内固定使用 `AURSMITH_SOURCE_PROXY_URL=http://source-proxy:3128` 执行 Doctor。该地址只用于 Publisher 自检，不替代 Builder 的 `AURSMITH_FETCH_PROXY=<Publisher 管理网 IP>:3128`；跨设备时仍需显式配置 Builder 看到的地址并由宿主防火墙限制来源。

Builder Stack 必须设置 `KVM_GID` 和 `AURSMITH_FETCH_PROXY`，后者填写上述 Publisher 代理的固定 `IP:端口`。不能填写域名、URL 或一组候选地址；这样 QEMU 参数不会在运行时进行不受控解析。容器只映射 `/dev/kvm`，不需要 privileged、TUN、Docker Socket 或 libvirt Socket。

每个可用 Profile 放在 `/profiles/<profile_sha256>/`，包含：

- `root.qcow2`；
- `vmlinuz-linux`；
- `initramfs-linux.img`；
- `profile-envelope.json`。

`profile-envelope.json` 的 payload type 必须是 `aursmith.build_profile`，并由当前 Controller 公钥签署。仅复制文件或修改目录名不能激活 Profile。构建前可通过 Builder Stack 的 `AURSMITH_ARCH_MIRROR` 选择 Arch 镜像，地址必须是无凭据、查询参数和片段的 HTTPS Base URL。该值同时用于构建 Profile 根文件系统，并写入 Guest 的 `/etc/pacman.d/mirrorlist`，因此 Fetch Guest 后续下载官方依赖时使用同一镜像；正式 Build Guest 仍然没有网卡。若使用内置缓存，应填写 `https://<稳定仓库域名>/arch-cache`；Publisher Stack 的同名变量则配置 pacoloco 自己访问的外部上游，二者不能形成循环。

下面示例使用清华大学开源软件镜像站构建并导出 base candidate；未设置变量时使用 `https://geo.mirror.pkgbuild.com`：

```bash
AURSMITH_ARCH_MIRROR=https://mirrors.tuna.tsinghua.edu.cn/archlinux \
  docker compose -f deploy/builder/compose.yaml --profile profile-build build profile-builder
docker compose -f deploy/builder/compose.yaml --profile profile-build run --rm profile-builder --name base
```

导出卷包含 `profile-candidate.json` 以及三个固定二进制文件。候选清单中的 `repository_mirror` 属于 Profile 内容摘要和后续 provenance；修改镜像必须重新构建、授权和验证 Profile，不能只改 Guest 内的 mirrorlist。管理员可在 Web 的 Profile 页面选择该 JSON，也可以提交到 `POST /api/v1/profiles`；Controller 会忽略候选中自报的摘要、重新计算内容摘要并返回签名 Envelope。页面提供 `profile-envelope.json` 下载。Envelope 和三个文件必须放入 `/profiles/<profile_sha256>/`。Profile 未通过启动、无网和固定 fixture build 前，激活 API 会返回 `PROFILE_NOT_VERIFIED`；不能用人工改数据库绕过。

已验证的容器能力边界是：Builder 镜像在 `--device /dev/kvm --cap-drop ALL --security-opt no-new-privileges` 下可以初始化 `q35,accel=kvm`。实际生成的 base Profile 已通过 QEMU 内置 virtio-9p 完成签名 Fetch Job 和无网 Build Job，生成可由 `bsdtar` 读取的 Arch 软件包；容器没有 privileged、额外 capability 或宿主 Docker/libvirt Socket。输入 fsdev 固定只读，输出绑定 Attempt 独立目录，Worker 在接管结果前复验文件摘要并删除 overlay。

## 告警通知

Web UI 和 JSON 结构化日志始终可用。若要把告警投递到通用 Webhook，配置 `AURSMITH_WEBHOOK_URL`，并用 `openssl rand -hex 32` 生成 `deploy/controller/secrets/webhook_hmac_secret`。接收端必须对原始 HTTP body 计算 HMAC-SHA256，并与 `X-AURsmith-Signature` 中的 `sha256=` 十六进制值执行常量时间比较；不要先解析再重新序列化 JSON。即使未启用 Webhook，也要创建一个权限为仅部署账户可读的随机 secret 文件，以满足 Compose 的固定 secret 挂载。

ntfy 使用 `AURSMITH_NTFY_URL=https://<服务器>/<主题>` 配置。第一版不把 ntfy token 放入 URL，也不支持 URL 内嵌用户名和密码；需要私有认证时应在受信任反向代理处为固定来源配置，或只使用 HMAC Webhook。通知失败不会改变构建、发布或归档状态，可在 `alert_notifications` 表和 Controller 结构化日志中查看三次尝试后的最后错误。

Doctor 页面显示每个 Worker 的在线状态、数据卷可用百分比和时钟偏差。部署前应确保 Worker 主机启用时间同步；偏差超过 60 秒会告警。Publisher 可用空间低于 10% 时新任务和新 Release 被背压，恢复到 10% 以上后下次心跳自动解除。不要通过修改 SQLite 设置绕过容量保护。

## 控制面备份与恢复

Controller 默认把每日一致性备份写入同一持久卷的 `/var/lib/aursmith/backups/<Backup ID>/`，随后通过独立的 `backup-ssh` sidecar 让 Archiver 主动拉取。不要只复制 `controller.db` 而丢失 `backup-envelope.json`；即使远端 Receipt 已验证，`controller_signing_key` 等 secret 仍必须另行离线备份。

为备份源单独生成 SSH host key，并把 Archiver 的只读拉取公钥写入 Controller Stack 的 `backup_authorized_keys`。启动 Controller 后取得稳定源 UUID：

```bash
docker compose -f deploy/controller/compose.yaml exec controller aursmith-controller transfer-source-id
```

把输出 UUID 加入 Archiver 的 `AURSMITH_TRANSFER_ENDPOINTS_JSON`，值为 `ssh://aursmith@<Controller 地址>:<backup-ssh 端口>`；Archiver 的 known_hosts 同时固定 Publisher 和 Controller backup-ssh 的 host key。Controller 默认只把 backup-ssh 绑定到 `127.0.0.1:2221`，跨设备部署时必须显式设置 `AURSMITH_BACKUP_SSH_BIND` 为受防火墙保护的管理网地址。此端口只允许 forced-command rsync，不应暴露到公网。

恢复时先停止 Controller 服务，确认没有其他容器打开数据库卷，然后以只挂载 Controller 数据卷和必要 secret 的一次性容器执行：

```bash
docker compose -f deploy/controller/compose.yaml stop controller
docker compose -f deploy/controller/compose.yaml run --rm --no-deps controller restore-control-plane --backup /var/lib/aursmith/backups/<Backup ID>
docker compose -f deploy/controller/compose.yaml start controller
```

命令会验证 Controller 签名、SHA-256 和 SQLite `integrity_check`。被替换的数据库、WAL 与 SHM 移入 `/var/lib/aursmith/recovery/<UTC 时间>-<Backup ID>/`，不会自动删除。恢复后必须运行 Doctor，并核对 Worker、当前 Release、ArchiveCopy 和管理员登录；确认无误前不要清理 recovery 目录。

Archiver 库存巡检由 Controller 自动调度：七天没有成功报告时执行文件集合与大小检查，九十天没有完整报告时重新计算全部摘要。巡检通过现有 forced-command SSH 发起，结果由 Archiver 身份密钥签名。归档页面显示最近报告的级别、Release/文件数量和失败数；任何失败都应先隔离存储故障并从其他已验证副本恢复，不要直接修改 Receipt 或控制面状态。

## 按包构建策略

软件包详情页默认显示“执行 `check()`”。只有确认上游测试在隔离构建环境中不可用时，才使用“禁用 `check()`”；该设置只影响之后创建的新 Build Job，不会修改已经签名或运行中的 Job。禁用状态会进入 JobSpec、provenance 和事件日志。恢复启用后同样只影响后续任务。

无论用户关注 split package 中的哪些输出，Builder 都会构建并核对该 pkgbase 声明的完整 outputs。缺少或多出产物会使 Job 失败，不能通过只发布成功的子包绕过批次原子性。

## Git 与发布

- 日常开发直接进入 `main`，每个独立且验证通过的改动形成一个英文提交。
- 发布前工作树必须干净，`just test` 必须通过。
- Release Manifest 必须记录 `git rev-parse HEAD` 的完整 commit。
- 发布标签使用 `vMAJOR.MINOR.PATCH` annotated tag；仓库 GPG 可用后对标签签名。
- 镜像发布使用相同版本标签和源码 commit label，不能使用漂移的 `latest` 作为可恢复版本依据。
