# AURsmith

AURsmith 是面向少量 Arch Linux 客户端的私有、可审计 AUR 二进制仓库。它持续跟踪用户订阅的 AUR 软件包，审计不可变修订，在普通联网 Docker 容器中构建，发布经过签名的 pacman 仓库，并独立归档历史 Release。

系统第一版拆分为 Controller、Builder 和 Publisher 三套 Docker Compose Stack；外部 Archiver 保留为可选扩展。任何 AURsmith 服务都不会直接部署到宿主机。

规范性需求位于 `docs/requirements.md`。只有当一个需求 ID 具备实现、自动化测试或明确的人工验证记录时，才能标记为完成。

开发和发布全过程使用 Git 管理：验证通过的改动按阶段形成小型提交并直接进入 `main`，每个发布版本记录产生它的准确源码 commit。

## 公网入口

Controller 镜像同时提供 API 和 React 静态页面，Publisher Worker 同时提供 pacman 仓库文件。Compose 默认仅把二者映射到宿主 `127.0.0.1`，由已有的宿主 Caddy 统一完成公网 TLS、压缩和缓存策略；Stack 内不再重复部署 Caddy，也不把宿主证书或私钥挂入 AURsmith 容器。

netcup 的反代示例位于 `deploy/netcup/Caddyfile.snippet`。它只是待合并配置，部署脚本不会修改或 reload 宿主 Caddy。
