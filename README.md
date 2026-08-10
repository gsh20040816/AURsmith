# AURsmith

AURsmith 是面向少量 Arch Linux 客户端的私有、可审计 AUR 二进制仓库。它持续跟踪用户订阅的 AUR 软件包，审计不可变修订，在 KVM 虚拟机中构建，发布经过签名的 pacman 仓库，并独立归档历史 Release。

系统拆分为 Controller、Builder、Publisher 和 Archiver 四套 Docker Compose Stack。任何 AURsmith 服务都不会直接部署到宿主机。

规范性需求位于 `docs/requirements.md`。只有当一个需求 ID 具备实现、自动化测试或明确的人工验证记录时，才能标记为完成。

开发和发布全过程使用 Git 管理：验证通过的改动按阶段形成小型提交并直接进入 `main`，每个发布版本记录产生它的准确源码 commit。

## Controller HTTPS

Controller Stack 默认在 `https://aursmith.lan:8443` 使用 Caddy 内部 CA。部署前应把 `aursmith.lan` 解析到 Controller 主机；`caddy-data` 命名卷持久保存 CA 和站点证书，必须连同其他密钥材料离线备份。管理员登录后可在设置页下载根证书，Doctor 会检查证书格式及未来 30 天有效期。首次访问尚未信任内部 CA 时，也可以在 Controller 主机执行：

```bash
docker compose -f deploy/controller/compose.yaml cp web:/data/caddy/pki/authorities/local/root.crt ./aursmith-root-ca.crt
```

如果已有受信任证书，使用用户证书覆盖文件启动；私钥只挂载到 Web Caddy，不进入 Controller：

```bash
AURSMITH_WEB_TLS_CERTIFICATE_FILE=/安全路径/fullchain.pem \
AURSMITH_WEB_TLS_PRIVATE_KEY_FILE=/安全路径/private-key.pem \
docker compose -f deploy/controller/compose.yaml \
  -f deploy/controller/compose.user-tls.yaml up -d
```

轮换内部 CA 会使所有已安装的旧根证书失效，因此必须先备份旧 `caddy-data`、安排客户端重新导入，再在维护窗口更换该卷；系统不会静默自动轮换根 CA。
