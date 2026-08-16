# 容器化部署

## 宿主机要求

- Docker Engine 与 Docker Compose。
- Builder 宿主机需要 Docker Engine，并允许可信 Builder Worker 访问 Docker Socket。
- 每个角色使用独立持久卷、SSH host key 和 Controller 访问密钥。

AURsmith 不安装裸机 daemon。文档中的所有管理命令都通过构建产物或 `docker compose exec` 运行。

## 初始化密钥与管理员

运行 `cargo run -p aursmithctl -- generate-controller-key` 生成 Controller Ed25519 密钥对。输出中的私钥十六进制值写入 Controller 的 `controller_signing_key` secret；公钥值写入每个 Worker 的 `AURSMITH_CONTROLLER_VERIFYING_KEY_HEX`。

使用 `ssh-keygen -t ed25519` 分别生成：

- Controller 访问 Worker 的客户端密钥；
- 每个 Worker 自己的 SSH host key。

Controller 使用严格的 `known_hosts`，不能配置 `StrictHostKeyChecking=no`。Worker 的 `authorized_keys` 只允许 Controller 公钥，实际命令仍由 `sshd_config` 中的 forced command 二次限制。

Controller 必须把 `AURSMITH_PUBLIC_ORIGIN` 配置为浏览器实际访问的固定 HTTPS Origin，例如 `https://aursmith.example.com`。该值不能包含凭据、路径、查询参数或片段。会话 Cookie 固定使用 `__Host-` 前缀和 Secure 属性，不提供 HTTP 或不安全 Cookie 降级开关。`AURSMITH_SESSION_IDLE_MINUTES` 与 `AURSMITH_SESSION_ABSOLUTE_HOURS` 分别控制服务端空闲和绝对过期时间；前者允许 1 分钟至 7 天，后者允许 1 小时至 365 天，非法配置会拒绝启动，且空闲期限不能长于绝对期限。

反向代理必须删除客户端传入的 `X-AURsmith-Client-IP`，再用当前 TCP 连接的真实客户端 IP 覆盖该请求头；禁止直接透传同名 header 或从任意 `X-Forwarded-For` 链猜测来源。Controller 只接受该 header 中的单个合法 IP，缺失或无效时统一进入 `direct` 登录节流桶。登录同时受每来源小桶和较高的全局硬上限约束。

管理员只能在公网核心设备本地创建。Controller 启动并完成数据库迁移后，通过安全管道向标准输入传入密码；密码不得作为命令行参数。CLI 会拒绝直接从可能回显的 TTY 读取密码，也可以改用权限为 `0600` 的 `--password-file`：

```bash
read -rs AURSMITH_ADMIN_PASSWORD
printf '%s\n' "${AURSMITH_ADMIN_PASSWORD}" | docker compose -f deploy/controller/compose.yaml exec -T controller \
  aursmithctl admin init --username admin
unset AURSMITH_ADMIN_PASSWORD
```

`reset-password` 同样从标准输入读取新密码，并在同一事务中吊销全部已有会话。只吊销会话而不改密时使用 `aursmithctl admin revoke-sessions`。这三个命令都直接访问本机 SQLite，不存在公网 setup/status 或 setup token。

Worker 账户的 `/bin/sh` 只用于 OpenSSH 按其协议执行服务端 forced command；客户端提交的原始命令不会直接交给该 shell。控制命令由 `aursmithctl ssh-gateway` 按固定语法解析；Publisher 的 rsync 收件命令则直接交给 rsync 官方随包提供的 `rrsync -wo /landing`，不由 AURsmith 解析 rsync 的内部参数。PTY、转发、密码登录和交互会话均被禁用。

同机部署 Controller 与 Publisher 时，两套 Compose 通过外部 Docker 网络 `aursmith-backbone` 连接；部署前仅需执行一次 `docker network create --internal aursmith-backbone`。Controller 和 Publisher SSH 服务在 Compose 中显式加入该网络，容器重建后仍保留 `publisher-ssh` 服务发现，不依赖部署后的手工 `docker network connect`。

Publisher 的 `AURSMITH_AUR_BASE_URL` 默认固定为 `https://aur.archlinux.org/`。AUR 搜索、info、Provider 查询和 Git 快照均从 Publisher 发起；Controller Stack 不需要 AUR 外网出口。第一版单次订阅最多展开 64 个 AUR pkgbase，超过上限会明确失败并保持数据库不变。

开发机具备外网时，可以运行 `scripts/smoke-upstream.sh` 验证真实 AUR 搜索、普通包快照、Git VCS commit 固定和 Arch 官方包查询。该脚本使用临时 SQLite、Unix Socket 和 Publisher 进程，结束时清理全部临时状态，不接触生产配置。

Compose 的本地文件型 secret 不支持可靠设置容器内 `uid/gid/mode`。AURsmith 不依赖该行为：SSH 容器只在启动器复制 secret 时短暂使用 root，并且仅增加 `DAC_READ_SEARCH`、`CHOWN`、`SETGID`、`SETUID` 以及清空 capability 集合所需的 `SETPCAP`。启动器先校验 secret 是有界的普通文件，再以 `0600`、禁止覆盖的方式复制到 tmpfs、变更为服务 UID，随后通过 `setpriv` 清空 capability bounding/inheritable/ambient 集合、永久降权并以 `exec` 启动非 root sshd。Controller 也只把 SSH 客户端私钥物化到自己的私有 tmpfs。原始 secret 始终为只读挂载，容器重启后私有副本自动消失。

Compose file-backed secret 以 UID/GID `10001`、模式 `0400` 挂载。部署前使用 `docker compose config` 检查实际渲染结果，不要把 `deploy/*/secrets` 加入 Git。

## Agent provider 配置

Controller Stack 内固定运行三个低成本 Runner、一个高成本 Runner和一个凭据网关。Runner 镜像固定包含 Codex CLI `0.147.0` 与 Claude Code `2.1.226`；升级 CLI 必须修改镜像构建参数、重新构建并运行适配器回归测试，不能在运行中自动更新。

三个低成本 Runner 必须分别配置，变量前缀依次为 `AURSMITH_LOW_AGENT_1_*`、`AURSMITH_LOW_AGENT_2_*`、`AURSMITH_LOW_AGENT_3_*`；高成本 Runner 使用 `AURSMITH_HIGH_AGENT_*`。每套配置包括：

- `PROVIDER`：写入报告的 provider 标识，只允许字母、数字、连字符和下划线；
- `MODEL`：传给对应 CLI 的模型 ID；
- `REASONING_EFFORT`：Codex 思考强度，可为空或使用 `minimal/low/medium/high/xhigh/max`；
- `BASE_URL`：凭据网关访问的上游 HTTPS Base URL；
- `AUTH_STYLE`：`bearer` 或 `x-api-key`；Codex 兼容 provider 通常使用 `bearer`；
- `API_KEY_FILE`：宿主机上的独立 Docker secret 文件路径。

第一版的三个低成本 Runner 固定使用 Codex 适配器，但 model、provider、Base URL、API key 和思考强度彼此独立，避免把三票退化成同一配置的重复调用。自建兼容网关必须使用 HTTPS；第一版不允许明文 HTTP upstream。Runner 实际只看到凭据网关的 `/low-1/`、`/low-2/`、`/low-3/` 或 `/high/` 内部路径。

Web 设置页可以修改每日调用数、每月调用数和每月成本上限；这三项立即作用于后续调度并写入事件日志。设置页只显示 provider 配置来源和 Runner 状态。修改适配器、provider、模型或 Base URL 后需要重新创建 Agent Stack；更新 API key 时只替换对应 secret 并重启凭据网关，不能把 key 粘贴到 Web 表单。

将三个低成本 key 分别写入 `deploy/controller/secrets/low_agent_1_api_key`、`low_agent_2_api_key`、`low_agent_3_api_key`，高成本 key 写入 `high_agent_api_key`，权限设为仅部署账户可读。不要把 key 写进 `.env`、Compose environment、Controller 设置、Agent prompt 或日志。凭据网关读取 secret 后删除 Runner 发送的认证头，再按各自路由注入对应凭据；Runner 子进程中只有无权限占位令牌。

Codex 的自定义 provider 走 Responses API 兼容接口；Claude Code 的自定义 Base URL 需要提供 Anthropic Messages API 兼容接口。provider 名称只是可追踪标签，不会自动转换 API 协议。部署 Doctor 后续阶段会对两层分别执行不含软件包内容的结构化输出探测。

## 启动顺序

Controller 自行提供 Web/API，Publisher 自行提供仓库 HTTP；两者默认只映射到宿主回环地址。公网部署应使用宿主现有反向代理统一提供 TLS，不复制证书到容器，也不在 Stack 内重复运行 Web 代理。

1. 启动 Publisher Stack。
2. 启动至少一个 Builder Stack。真实公网拓扑下 Builder 不启动 SSH sidecar，也不映射入站端口；它通过 `AURSMITH_CONTROLLER_POLL_URL` 主动领取任务。
3. 配置固定的 `AURSMITH_PUBLIC_ORIGIN` 后启动 Controller Stack。
4. 在公网核心设备本地执行 `aursmithctl admin init`；已存在管理员时不得重复初始化。
5. 通过 HTTPS Web 登录。
6. 注册 Builder 和 Publisher，并执行“探测”。
7. 运行 Doctor，确认 Docker daemon、绝对 jobs 目录、SSH、协议、仓库和保留策略状态。
8. 在订阅真实软件包前，确认四个 Agent Runner 都能返回符合 Schema 的测试报告，且报告与容器日志中不含 API key。

不同宿主机部署时只需要复制对应 Stack 的 Compose、镜像和该角色的 secret，不要复制其他角色的数据卷或私钥。

## Builder Docker 配置

Publisher Stack 还自带独立 pacoloco。它只缓存 Arch 官方仓库，缓存卷为 `pacoloco-cache`，不应与 Publisher staging 或公开仓库卷合并。外部 Arch 上游由 Publisher Compose 的 `AURSMITH_ARCH_MIRROR` 构建参数配置，必须是无凭据和参数的 HTTPS Base URL。宿主反向代理把 `https://<稳定仓库域名>/arch-cache/` 转发到 pacoloco；首次部署可连续请求同一 `core.db`，再在 Doctor 中确认 requests、misses 和 hits 递增。

Builder Stack 必须设置 `DOCKER_GID`、`AURSMITH_JOBS_DIR`、`AURSMITH_SECRET_GID`、`AURSMITH_CONTROLLER_POLL_URL` 和 `AURSMITH_REVERSE_PUBLISHER_ENDPOINT`。`DOCKER_GID` 是宿主 Docker Socket 所属组；`AURSMITH_JOBS_DIR` 必须是宿主绝对路径，并以同一路径 bind 到 Worker，使宿主 Docker daemon 能解析每个 Attempt 的 input/output bind。`AURSMITH_SECRET_GID` 是宿主部署密钥文件所属组，私钥应保持 `0440` 且仅允许该组读取。轮询地址必须是公网 Controller 的无凭据 HTTPS URL；Publisher 端点仍是只允许 Capability 接收与完成命令的 SSH forced-command 账户。

反向 Builder 首次注册时，在本地执行 `aursmithctl worker status` 取得持久实例 UUID 与 `identity_signing_key_hex`，由管理员在 Web UI 选择 `reverse` 模式录入。私钥只存在 Builder Journal 中，Controller 只保存公钥。注册完成后 Builder 每次轮询都签署 UUID、nonce、时间、状态和 Attempt Journal；Controller 不尝试连接家庭网络。Publisher 的推送 SSH 入口只接受固定 rsync receiver 路径 `/landing/.<Capability ID>.partial/` 与 `finalize-push-import`，不允许 Shell、PTY、转发或任意目标路径。

当 Controller 与 Publisher 同机部署到 netcup 时，使用 `deploy/*/compose.netcup.yaml` 把相关服务接入外部 internal 网络 `aursmith-backbone`。Controller 通过 `publisher-ssh:2222` 进行同机控制；该内部 SSH 端口不映射到宿主。公网入口和宿主 Caddy 示例见 `deploy/netcup/README.md`。

构建固定 Arch Build image 时可通过 `AURSMITH_ARCH_MIRROR` 选择无凭据、查询参数和片段的 HTTPS 镜像。先显式构建固定 tag，再启动 Worker：

```bash
AURSMITH_ARCH_MIRROR=https://mirrors.ustc.edu.cn/archlinux \
  docker compose -f deploy/builder/compose.yaml --profile build-image build build-image
docker compose -f deploy/builder/compose.yaml up -d worker
```

不要对 `up` 添加 `--profile build-image`：该服务只给 Compose 提供可重复的 build/tag 入口，不是常驻进程。可信 Worker 挂载 Docker Socket；它创建的 Build 子容器只获得只读 snapshot input、可写 Attempt output 和普通 bridge 网络，不获得 Docker Socket、Controller/SSH/GPG secret。Build 使用镜像内标准 `makepkg --syncdeps`/pacman，Worker 在接管结果前复验输入和产物摘要。

## 告警通知

Web UI 和 JSON 结构化日志始终可用。若要把告警投递到通用 Webhook，配置 `AURSMITH_WEBHOOK_URL`，并用 `openssl rand -hex 32` 生成 `deploy/controller/secrets/webhook_hmac_secret`。接收端必须对原始 HTTP body 计算 HMAC-SHA256，并与 `X-AURsmith-Signature` 中的 `sha256=` 十六进制值执行常量时间比较；不要先解析再重新序列化 JSON。即使未启用 Webhook，也要创建一个权限为仅部署账户可读的随机 secret 文件，以满足 Compose 的固定 secret 挂载。

ntfy 使用 `AURSMITH_NTFY_URL=https://<服务器>/<主题>` 配置。第一版不把 ntfy token 放入 URL，也不支持 URL 内嵌用户名和密码；需要私有认证时应在受信任反向代理处为固定来源配置，或只使用 HMAC Webhook。通知失败不会改变构建、发布或归档状态，可在 `alert_notifications` 表和 Controller 结构化日志中查看三次尝试后的最后错误。

Doctor 页面显示每个 Worker 的在线状态、数据卷可用百分比和时钟偏差。部署前应确保 Worker 主机启用时间同步；偏差超过 60 秒会告警。Publisher 可用空间低于 10% 时新任务和新 Release 被背压，恢复到 10% 以上后下次心跳自动解除。不要通过修改 SQLite 设置绕过容量保护。

## 控制面备份与恢复

Controller 默认把每日一致性备份写入同一持久卷的 `/var/lib/aursmith/backups/<Backup ID>/`。不要只复制 `controller.db` 而丢失 `backup-envelope.json`；`controller_signing_key`、GPG 私钥和管理员恢复材料仍必须另行离线备份。第一版默认不启用外部 Archiver，因此这些备份与 Controller 同故障域，只用于误操作恢复，不能冒充独立灾备。

旧的远端备份导出 sidecar 位于 `external-archiver` Compose profile，默认不会启动。只有显式启用外部 Archiver 协议时才同时启用该 profile 和 `AURSMITH_EXTERNAL_ARCHIVER_ENABLED=true`。

恢复时先停止 Controller 服务，确认没有其他容器打开数据库卷，然后以只挂载 Controller 数据卷和必要 secret 的一次性容器执行：

```bash
docker compose -f deploy/controller/compose.yaml stop controller
docker compose -f deploy/controller/compose.yaml run --rm --no-deps controller restore-control-plane --backup /var/lib/aursmith/backups/<Backup ID>
docker compose -f deploy/controller/compose.yaml start controller
```

命令会验证 Controller 签名、SHA-256 和 SQLite `integrity_check`。被替换的数据库、WAL 与 SHM 移入 `/var/lib/aursmith/recovery/<UTC 时间>-<Backup ID>/`，不会自动删除。恢复后必须运行 Doctor，并核对 Worker、当前 Release 和管理员登录；确认无误前不要清理 recovery 目录。

## 按包构建策略

软件包详情页默认显示“执行 `check()`”。只有确认上游测试在隔离构建环境中不可用时，才使用“禁用 `check()`”；该设置只影响之后创建的新 Build Job，不会修改已经签名或运行中的 Job。禁用状态会进入 JobSpec、provenance 和事件日志。恢复启用后同样只影响后续任务。

无论用户关注 split package 中的哪些输出，Builder 都会构建并核对该 pkgbase 声明的完整 outputs。缺少或多出产物会使 Job 失败，不能通过只发布成功的子包绕过批次原子性。

## Git 与发布

- 日常开发直接进入 `main`，每个独立且验证通过的改动形成一个英文提交。
- 发布前工作树必须干净，`just test` 必须通过。
- Release Manifest 必须记录 `git rev-parse HEAD` 的完整 commit。
- 发布标签使用 `vMAJOR.MINOR.PATCH` annotated tag；仓库 GPG 可用后对标签签名。
- 镜像发布使用相同版本标签和源码 commit label，不能使用漂移的 `latest` 作为可恢复版本依据。
