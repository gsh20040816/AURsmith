# AURsmith

AURsmith 是单个管理员自用的 AUR 私有仓库打包器。本仓库正在按
[`docs/refactor-requirements.md`](docs/refactor-requirements.md) 从过度扩张的旧实现重建为固定两机、单 Builder、单 Publisher 的精简系统。

当前 `0.2.0-dev` 是删除型核心里程碑，只完成：

- 一个 `aursmith` 二进制；
- 本地管理员初始化、改密和 session 吊销；
- 固定安全 Cookie、Origin/CSRF、双过期和有界登录节流；
- 服务端 HTML 中的显式 pkgbase 添加、暂停、恢复和物理删除；
- fresh-only SQLite，且只包含 `administrators`、`sessions`、`tracked_packages`。

它尚不包含 AUR 拉取、3+1 审查、远程构建、产物接收、GPG/keyring 或 pacman 仓库发布，不能替换生产。旧生产仓库应继续从旧 commit 或镜像只读运行，直到新系统通过权威需求中的完整两机验收。

## 开发验证

```bash
just test
```

## 本地启动

数据库目录必须已经存在；`serve` 只会在显式指定且尚不存在的数据库文件上创建新 Schema：

```bash
cargo run -p aursmith -- serve \
  --database-path ./aursmith.db \
  --public-origin https://aursmith.example.com
```

随后用安全管道或权限为 `0600` 的文件初始化唯一管理员：

```bash
read -rsp '管理员密码：' AURSMITH_ADMIN_PASSWORD_INPUT
printf '\n'
printf '%s\n' "$AURSMITH_ADMIN_PASSWORD_INPUT" | \
  cargo run -p aursmith -- admin --database-path ./aursmith.db init
unset AURSMITH_ADMIN_PASSWORD_INPUT
```

生产反代与 Compose 说明见 [`docs/deployment.md`](docs/deployment.md)。
