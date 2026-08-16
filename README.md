# AURsmith

AURsmith 是单个管理员自用的 AUR 私有仓库打包器。本仓库正在按
[`docs/refactor-requirements.md`](docs/refactor-requirements.md) 从过度扩张的旧实现重建为固定两机、单 Builder、单 Publisher 的精简系统。

当前 `0.2.0-dev` 是删除型核心里程碑，只完成：

- 一个 `aursmith` 二进制；
- 本地管理员初始化、改密和 session 吊销；
- 固定安全 Cookie、Origin/CSRF、双过期和有界登录节流；
- 服务端 HTML 中的显式 pkgbase 添加、暂停、恢复、物理删除和手工刷新；
- 从固定 `https://aur.archlinux.org/<pkgbase>.git` 获取精确 commit，以离线 Git tree
  物化包输入并保存完整 full/diff 证据；
- 对 PKGBUILD 与 `.SRCINFO` 执行有限、确定性的输入检查，结果只停在
  `prepared` 或 `input_blocked`；
- fresh-only SQLite，且只包含 `administrators`、`sessions`、`tracked_packages`、
  `aur_reviews`。

它尚不包含 Agent 审查、批准、远程构建、产物接收、GPG/keyring 或 pacman
仓库发布，不能替换生产，也不应被描述为“审查闭环完成”。旧生产仓库应继续从旧
commit 或镜像只读运行，直到新系统通过权威需求中的完整两机验收。

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

## 当前 AUR 输入边界

生产远端不可配置，只能是经过校验的 pkgbase 对应的 AUR HTTPS Git URL。Git 使用固定
argv、隔离配置、禁用 credential prompt、hook、submodule、ext protocol、外部 diff 和
textconv；PKGBUILD、包脚本、包内 `AGENTS.md` 与 `.gitattributes` 都只作为数据读取。

每次刷新共享 30 秒 Git 总时限，并固定限制文件数、路径、单文件、完整 tree、
`.SRCINFO` 和 diff 大小。超过完整 diff 的 4 MiB 边界会诚实回退为 full，不保存部分
diff。Git 使用 `--depth=1`，但远端 pack 在完成 fetch 并读取 tree 前无法由这些物化边界
精确限制；固定 AUR 服务的传输量和 named volume 的宿主磁盘容量仍由部署者负责。

自动化测试使用离线本地 bare Git fixture。本里程碑未执行真实公网 AUR 端到端验证。
