# 部署

两台设备：公网节点运行 `deploy/server/compose.yaml`（`aursmithd` + `signer` 两个容器）与宿主 Caddy；家用节点运行 `deploy/home/compose.yaml`（`builder`）。所有命令在仓库根目录执行。

## 0. 部署前必须处理

- **固定基础镜像摘要**：`deploy/Dockerfile` 的全部基础镜像（含 Docker CLI）已固定 digest；升级时先核对新摘要，再构建并验证。
- **Cloudflare 请求体上限**：免费/Pro 套餐单个请求体上限 100 MB。Builder 以 32 MiB 分片上传，Caddy 片段限制 `request_body max_size 40MB`；不要把分片调大到超过 Cloudflare 上限。
- 秘密文件以宿主权限挂载进容器，容器内进程是 UID 10001：秘密文件需属主 10001（`chown 10001:10001 file && chmod 0400 file`）或属组可读。

## 1. 公网节点（全新部署）

```sh
cd deploy/server
cp env.example .env
cp review.example.json review.json            # 两个 low + 一个 high，见下文
install -d -m 0700 secrets
# 三个 LLM API key，各一个文件
printf '%s' "$LOW1_KEY"  > secrets/low_agent_1_api_key
printf '%s' "$LOW2_KEY"  > secrets/low_agent_2_api_key
printf '%s' "$HIGH_KEY"  > secrets/high_agent_api_key
# 仓库签名私钥（ASCII armor）。沿用旧部署的同一把钥匙，客户端无需重新信任。
cp /path/to/repository_gpg_private_key.asc secrets/repository_gpg_private_key.asc
sudo chown 10001:10001 secrets/* && sudo chmod 0400 secrets/*
# 宿主仓库目录：signer 写入，Caddy 只读提供
sudo install -d -o 10001 -g 10001 -m 0755 /srv/aursmith/repo
```

Builder 令牌在家用节点生成（见第 3 节），把它的 SHA-256 填进 `.env`：

```sh
printf '%s' "$(cat builder_token)" | sha256sum | cut -d' ' -f1
```

`review.json`：

- `low` 必须恰好两个，`high` 一个；`api` 为 `openai`（`<base_url>/chat/completions`，`response_format: json_schema` 严格模式）或 `anthropic`（`<base_url>/messages`，强制单一工具调用输出）。
- `api_key_file` 指向 `/run/secrets/<secret 名>`；`timeout_seconds` 默认 600。
- 两个 low 最好来自不同供应商，降低同源误判。

启动并创建管理员：

```sh
docker compose up -d --build
docker compose run --rm -T aursmithd admin init --username admin < admin-password.txt
docker compose ps        # aursmithd healthy；signer running
```

宿主 Caddy 片段见 `deploy/netcup/Caddyfile.snippet`：管理站点反代 `127.0.0.1:18443`；`repo.*` 直接 `file_server` 提供 `/srv/aursmith/repo`。

如需 Arch 官方仓库 IPv6 代理，宿主 `arch-ipv6-proxy` 保持监听 `192.168.64.1:19443`（compose 网络固定为 `192.168.64.0/20`），并设置 `AURSMITH_ARCH_HTTPS_PROXY=http://192.168.64.1:19443`。

## 2. 从旧部署迁移（3+1 / Controller / Publisher 栈）

旧栈与新栈的 compose 项目名都是 `aursmith`，但卷名不同，旧数据卷不会被新栈触碰，可随时回退。

1. **排空**：在旧 Web 确认没有进行中的构建与发布；停止家用旧 Builder。
2. **停止旧公网栈**：`docker compose -f <旧仓库>/deploy/controller/compose.yaml down`（不要加 `-v`）。
3. **一致性备份**（宿主上）：
   ```sh
   install -d -o 10001 -g 10001 /root/aursmith-migration
   docker run --rm -v aursmith-controller_controller-data:/legacy:ro -v /root/aursmith-migration:/out \
     keinos/sqlite3 sqlite3 /legacy/controller.db ".backup /out/controller.db"
   chown 10001:10001 /root/aursmith-migration/controller.db
   tar -C /var/lib/docker/volumes -czf /root/aursmith-migration/old-volumes.tgz \
     aursmith-controller_controller-data aursmith-publisher_publisher-state aursmith-publisher_publisher-hot
   ```
   （任何可用的 sqlite3 都可以，只要用 `.backup` 得到一致副本。）
4. **构建新镜像**：`cd deploy/server && docker compose build`。
5. **导出旧状态**（只读打开备份副本）：
   ```sh
   docker run --rm --read-only -v /root/aursmith-migration:/m aursmith/aursmithd:development \
     legacy-export --legacy-database-url 'sqlite:///m/controller.db?mode=ro' --output /m/legacy-state.json
   ```
   导出内容：管理员（含口令哈希）、订阅、Provider 选择、包同步状态、每个包最新已批准 revision（作为新审查 baseline）。旧构建与发布记录不迁移。
6. **导入新库并校验**（新库必须为空，导入在单事务内逐表校验后才提交）：
   ```sh
   docker compose run --rm -v /root/aursmith-migration/legacy-state.json:/mnt/state.json:ro aursmithd import --input /mnt/state.json
   docker compose run --rm -v /root/aursmith-migration/legacy-state.json:/mnt/state.json:ro aursmithd verify --input /mnt/state.json
   ```
7. **迁移仓库文件**（保持 current 可用，客户端在新栈第一次发布前不中断）：
   ```sh
   rsync -a /var/lib/docker/volumes/aursmith-publisher_publisher-hot/_data/ /srv/aursmith/repo/
   chown -R 10001:10001 /srv/aursmith/repo
   ```
8. **切换 Caddy**：用新片段替换旧的 `repo.*` 与管理站点配置，`caddy reload`。
9. **启动新栈**：`docker compose up -d`；登录 Web（旧管理员口令），确认订阅数、待审数与旧库一致。
10. **全量重建**：在“订阅”页对需要的包点“重建”，或等下一次同步周期自动排队；首次发布会生成新的 keyring 包版本并原子切换 `current`。
11. **独立客户端验证**：在另一台 Arch 上 `pacman -Sy`，核对 `pacman-key --finger` 与旧指纹一致，安装一个包并确认签名校验通过。

回退：`docker compose down`（新栈）→ 恢复旧 Caddy 配置 → 用旧仓库 `deploy/controller/compose.yaml` 启动旧栈。旧卷未被修改。

## 3. 家用节点（Builder）

```sh
cd deploy/home
cp env.example .env
install -d -m 0700 secrets
openssl rand -hex 32 > secrets/builder_token
sudo chown 10001:10001 secrets/builder_token && sudo chmod 0400 secrets/builder_token
# Docker socket 的 GID，填进 .env 的 DOCKER_GID
stat -c %g /var/run/docker.sock
# jobs 目录：宿主与容器内必须是同一绝对路径
sudo install -d -o 10001 -g 10001 -m 0750 /var/lib/aursmith-builder/jobs
docker compose --profile build-image build build-image     # 一次性构建容器镜像 aursmith-build:latest
docker compose up -d --build builder
docker compose logs -f builder
```

- Builder 只主动访问 `AURSMITH_SERVER_URL`（HTTPS），不需要 SSH，不开放端口。
- 构建容器每次 `docker run --rm`，有网络（下载源码），无任何秘密；资源由 `AURSMITH_BUILD_CPUS / MEMORY_MIB / TIMEOUT_SECONDS` 限制。
- Builder 重启后从 jobs 目录恢复：未完成的构建报告 `BUILDER_RESTARTED`（瞬态，自动重试）；已完成未送达的结果继续上传。
- 构建镜像默认启用 Arch 官方 `core/extra/multilib` 与 `archlinuxcn`（可用 `AURSMITH_ARCH_MIRROR`、`AURSMITH_ARCHLINUXCN_MIRROR` 覆盖，必须 HTTPS）。更新镜像后重新执行 build-image。

## 4. 日常运维

| 操作 | 命令 |
|------|------|
| 健康 | `curl -fsS http://127.0.0.1:18443/healthz`；Web 首页 `/api/v1/status` |
| 备份 | `docker compose run --rm -v /backup:/out aursmithd export --output /out/state-$(date +%F).json`（`/backup` 属主 10001）；另备份 `/srv/aursmith/repo` 与 GPG 私钥 |
| 重置口令 | `docker compose run --rm -T aursmithd admin reset-password < new-password.txt` |
| 注销全部会话 | `docker compose run --rm aursmithd admin revoke-sessions` |
| 紧急回滚仓库 | `docker compose run --rm signer --rollback`（切回 `previous`；下一次期望状态变化会重新收敛。Web“发布”页显示的是数据库最近一次成功发布，回滚后可能与磁盘 `current` 不同） |

## 5. 恢复顺序

1. 停止两边所有容器；
2. 恢复 GPG 私钥（属主 10001，0400）；
3. 恢复 `/srv/aursmith/repo`；
4. 用最近的状态 JSON 执行 `import` + `verify`（或恢复数据卷）；
5. 启动公网栈，确认 `/healthz`、Web 状态；
6. 启动 Builder；
7. 独立 Arch 客户端执行 `pacman -Sy` 与一次安装验证。

同机副本不是灾备；异机存储与密钥托管由宿主工具负责。
