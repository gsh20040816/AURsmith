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

低成本与高成本层分别配置以下变量：

- `AURSMITH_LOW_AGENT_ADAPTER`、`AURSMITH_HIGH_AGENT_ADAPTER`：只能是 `codex` 或 `claude_code`；
- `AURSMITH_*_AGENT_PROVIDER`：写入报告的 provider 标识，只允许字母、数字、连字符和下划线；
- `AURSMITH_*_AGENT_MODEL`：传给对应 CLI 的模型 ID；
- `AURSMITH_*_AGENT_BASE_URL`：凭据网关访问的上游 HTTPS Base URL；
- `AURSMITH_*_AGENT_AUTH_STYLE`：`bearer` 或 `x-api-key`；Codex 兼容 provider 通常使用 `bearer`，Claude 原生 API 使用 `x-api-key`；
- `AURSMITH_*_AGENT_API_KEY_FILE`：宿主机上的 Docker secret 文件路径。

默认低成本层使用 Codex 和 `https://api.openai.com/v1/`，高成本层使用 Claude Code 和 `https://api.anthropic.com/`。自建兼容网关也必须使用 HTTPS；第一版不允许明文 HTTP upstream。自定义 Base URL 只配置在凭据网关，Runner 实际看到的是 `http://agent-credential-gateway:8091/low/` 或 `/high/`，且只处于 Compose 内部网络。

将低成本 key 写入 `deploy/controller/secrets/low_agent_api_key`，高成本 key 写入 `deploy/controller/secrets/high_agent_api_key`，权限设为仅部署账户可读。不要把 key 写进 `.env`、Compose environment、Controller 设置、Agent prompt 或日志。凭据网关读取 secret 后删除 Runner 发送的 `Authorization`、`x-api-key`、Host 和 hop-by-hop 头，再按配置注入真实凭据；Runner 子进程中只有无权限占位令牌。

Codex 的自定义 provider 走 Responses API 兼容接口；Claude Code 的自定义 Base URL 需要提供 Anthropic Messages API 兼容接口。provider 名称只是可追踪标签，不会自动转换 API 协议。部署 Doctor 后续阶段会对两层分别执行不含软件包内容的结构化输出探测。

## 启动顺序

1. 启动 Publisher 和 Archiver Stack。
2. 启动至少一个 Builder Stack。
3. 启动 Controller Stack。
4. 通过 `docker compose exec controller aursmith-controller setup-token` 读取初始化令牌。
5. 在 Web 设置页创建管理员。
6. 注册三个角色的 Worker，并执行“探测”。
7. 运行 Doctor，确认 KVM、SSH、协议、仓库和归档存储状态。
8. 在订阅真实软件包前，确认四个 Agent Runner 都能返回符合 Schema 的测试报告，且报告与容器日志中不含 API key。

不同宿主机部署时只需要复制对应 Stack 的 Compose、镜像和该角色的 secret，不要复制其他角色的数据卷或私钥。

## Git 与发布

- 日常开发直接进入 `main`，每个独立且验证通过的改动形成一个英文提交。
- 发布前工作树必须干净，`just test` 必须通过。
- Release Manifest 必须记录 `git rev-parse HEAD` 的完整 commit。
- 发布标签使用 `vMAJOR.MINOR.PATCH` annotated tag；仓库 GPG 可用后对标签签名。
- 镜像发布使用相同版本标签和源码 commit label，不能使用漂移的 `latest` 作为可恢复版本依据。
