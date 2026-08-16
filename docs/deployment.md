# 部署当前核心里程碑

当前版本只部署单个 AURsmith 核心服务，用于管理员登录、显式 pkgbase 目录管理，以及手工获取固定 AUR Git 输入。它只生成 `prepared` / `input_blocked` 的确定性输入证据，不执行 Agent 审查、批准、构建、签名或仓库发布，不能替换旧生产系统。旧生产仓库应继续通过旧 commit 或镜像只读运行，且不得把旧数据库挂给当前版本。

## 前置条件

- Docker Engine 与 Compose plugin；
- 一个由 Docker 管理的专用 named volume；
- 一个终止公网 HTTPS 的通用反向代理；
- 固定、无路径的公网 Origin，例如 `https://aursmith.example.com`。
- 容器可通过 HTTPS 访问固定的 `aur.archlinux.org` Git 服务。

服务只接受 fresh Schema。数据库文件不存在时，`serve` 使用原子文件创建语义创建它；初始化或 Schema 验证失败会清理本次新建的半成品文件。文件已经存在时只验证 `application_id`、`user_version`、精确四表/列集合和单 current review partial unique index，不运行迁移。空库、旧库以及上一中间版本的三表数据库都会明确失败。

## Compose

```bash
export AURSMITH_PUBLIC_ORIGIN=https://aursmith.example.com
docker compose -f deploy/compose.yaml up -d --build --wait
```

Compose 只有一个 `aursmith` 服务和一个数据卷。容器以非 root 用户运行，根文件系统只读、移除全部 capability、不挂载 Docker socket，只把端口发布到宿主回环地址。运行镜像固定包含 Git 和 CA 证书；数据库、shallow Git 对象与审查 artifact 都位于同一个 `/var/lib/aursmith` named volume。容器内 healthcheck 请求 `/healthz`，反向代理不得公开该路径。

`AURSMITH_PUBLIC_ORIGIN` 是 Compose 的必填插值变量。后续每次执行 `docker compose run` 管理员命令时，它仍须保留在当前 shell；不能在 `up` 后立即 unset，否则 Compose 会在启动管理员容器前按设计拒绝渲染。

可以覆盖：

| 变量 | 默认值 | 约束 |
| --- | --- | --- |
| `AURSMITH_WEB_BIND` | `127.0.0.1:18443` | 必须保持回环或受限内部地址 |
| `AURSMITH_PUBLIC_ORIGIN` | 无 | 必填、完整 HTTPS Origin |
| `AURSMITH_SESSION_IDLE_MINUTES` | `60` | 1 至 10080 |
| `AURSMITH_SESSION_ABSOLUTE_HOURS` | `168` | 1 至 8760，且不得短于 idle |
| `RUST_LOG` | `aursmith=info` | Rust 日志过滤器 |

## 唯一管理员

数据库必须先由 `serve` 创建。管理员命令只连接既有且验证通过的新数据库，不会创建数据库或迁移旧 Schema。

用安全管道初始化：

```bash
read -rsp '管理员密码：' AURSMITH_ADMIN_PASSWORD_INPUT
printf '\n'
printf '%s\n' "$AURSMITH_ADMIN_PASSWORD_INPUT" | \
  docker compose -f deploy/compose.yaml run -T --rm --no-deps aursmith \
  admin --database-path /var/lib/aursmith/aursmith.db init
unset AURSMITH_ADMIN_PASSWORD_INPUT
```

重置密码并同时吊销全部现有 session：

```bash
read -rsp '新管理员密码：' AURSMITH_ADMIN_PASSWORD_INPUT
printf '\n'
printf '%s\n' "$AURSMITH_ADMIN_PASSWORD_INPUT" | \
  docker compose -f deploy/compose.yaml run -T --rm --no-deps aursmith \
  admin --database-path /var/lib/aursmith/aursmith.db reset-password
unset AURSMITH_ADMIN_PASSWORD_INPUT
```

直接在公网设备本地运行 `aursmith` 二进制时，仍可使用 `--password-file`；输入文件必须是非空、不超过 64 KiB、group/other 权限均为零的普通文件。CLI 先移除尾随换行，再执行共享密码规则：至少 12 个字符且最多 512 个 UTF-8 字节。因此 64 KiB 只是输入文件的拒绝上限，不是有效密码长度。Compose 默认不额外挂载密码文件。未提供文件时只接受非 TTY stdin；为避免终端回显，TTY 会直接拒绝。密码从不接受 argv。

吊销全部 session：

```bash
docker compose -f deploy/compose.yaml run -T --rm --no-deps aursmith \
  admin --database-path /var/lib/aursmith/aursmith.db revoke-sessions
```

管理员固定为数据库 `id=1`，没有多用户、角色或身份注册抽象。改密会在同一事务中吊销所有 session。

## 反向代理

`deploy/Caddyfile.example` 是通用示例。代理必须：

1. 终止 HTTPS；
2. 把 `/healthz` 固定返回 404，不转发；
3. 删除客户端提供的 `X-AURsmith-Client-IP`，并覆盖为当前 TCP 客户端 IP；
4. 把其余路径转发到宿主回环端口。

不得透传来自公网的同名头，也不得使用未经闭合信任边界的 forwarded-for 链。AURsmith 只信任代理覆盖后的单个合法 IP；缺失或非法值归入 `direct` 限流桶。

`/healthz` 只是进程 HTTP liveness。`serve` 只有在数据库完成创建或精确 Schema 验证后才会启动 listener；运行期间若数据库失效，管理请求会明确返回 500，但 `/healthz` 不声称数据库健康或 readiness。

## 浏览器安全边界

- Cookie 固定为 `__Host-aursmith_session; Secure; HttpOnly; SameSite=Strict; Path=/`，无 `Domain`。
- session 同时执行服务端 idle 与 absolute 过期。
- 登录校验固定 Origin，并使用每来源小桶和全局硬上限。
- 管理写请求统一要求 session、固定 Origin 和 `X-AURsmith-CSRF: 1`。
- GET/HEAD 不刷新 session，也不改变数据库。
- 直接访问 `/` 会进入 `/manage`；无有效 session 的管理页会跳转 `/login`，管理写接口仍返回 401。

## pkgbase 规则

pkgbase 长度为 1 至 128 字节，只接受小写 ASCII 字母、数字和 Arch 允许的 `@._+-`。点和连字符不能作为首字符；`@`、`_`、`+` 不被任意排除。输入必须是明确 pkgbase，不提供搜索、模糊匹配或自动依赖加入。

## AUR 输入与证据

管理员可在包目录中手工选择“刷新 AUR”。生产 URL 不可配置，固定为
`https://aur.archlinux.org/<validated-pkgbase>.git`。同一进程一次只执行一个包变更；
添加、暂停、恢复、删除和刷新共享该边界，忙时返回 409。HTTP 客户端断开不会取消
已经开始的包变更。

Git 使用固定 argv、独立进程组和 30 秒总时限，隔离 system/global 配置与 HOME，禁止
credential prompt、hook、submodule、ext protocol、外部 diff 和 textconv。fetch 使用
`--depth=1`；固定 commit 后只读取 Git tree，不 checkout、source 或执行 PKGBUILD、包脚本、
包内 `AGENTS.md` 或 `.gitattributes` driver。

完整 tree 只接受 `100644` / `100755` 普通 blob，并使用以下固定输入边界：

- 最多 2048 个文件、路径最多 1024 UTF-8 字节和 32 层；
- 单文件最多 8 MiB，完整 tree 最多 64 MiB；
- `.SRCINFO` 最多 1 MiB，且 pkgbase/pkgname/arch 的数量和值长度另有限制；
- 完整 binary/full-index diff 最多 4 MiB，超限或 baseline 对象/摘要不可用时整体回退 full，
  不保存部分 diff。

证据位于数据库同级派生的 `aur/<pkgbase>/<commit>/`，包含安全物化成功时的 `package/`、
`review.json`、`findings.json` 和可选 `changes.diff`。数据库只保存派生身份、状态与 artifact
SHA-256，不保存任意绝对路径。详情读取会拒绝 symlink、非普通文件、超限或摘要不符；
非 UTF-8 diff 以完整可逆十六进制显示。

named volume 是服务独占的可信状态目录，不应由其他容器或宿主任务并发修改。物理删除会
先删除该 pkgbase 的 Git/证据目录，成功后才删除数据库行；磁盘删除失败时数据库保持不变。

这些限制约束固定 commit 解包后的输入，并不声称能在 fetch 完成前精确限制远端 pack。
固定 AUR 服务的传输量以及 named volume 的宿主磁盘容量仍是部署边界。本里程碑没有
后台 GC、磁盘配额或任意远端代理平台。

## 验证

```bash
bash scripts/test-all.sh
```

该命令执行 Rust 格式、Clippy、测试、构建、旧实现残留扫描以及 Compose/Caddy 静态安全检查；它不会启动容器，也不把 liveness 当作数据库 readiness。AUR 测试使用离线本地 bare Git fixture，不是公网 AUR E2E。真实公网 AUR、HTTPS 浏览器、容器运行冒烟和完整两机打包发布验收属于独立验证，当前结果不能替代它们，也不能据此宣称 Agent 审查或发布闭环已经完成。
