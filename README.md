# AURsmith

AURsmith 是供一个管理员和少量 Arch Linux 客户端使用的私有 AUR 二进制仓库。它跟踪明确订阅的 AUR pkgbase，固定包装层 commit，经三个 low Agent 和按需 high Agent 审查后，在家庭 Builder 的一次性联网 Docker 容器中构建，并由公网 Publisher 直接签名为 pacman 仓库。

真实拓扑固定为两台设备：公网设备运行 Controller、Web、Publisher、三个 low Runner、一个 high Runner和 credential gateway；家庭设备只运行一个主动轮询的 Builder。项目不提供多 Worker、独立 Signer、Archiver、告警平台或内置备份服务。

本轮权威需求位于 `docs/refactor-requirements.md`，当前架构、部署和验证边界分别见 `docs/architecture.md`、`docs/deployment.md` 和 `docs/verification.md`。

开发和部署使用 Git 管理；生产镜像以准确源码 commit 写入 OCI revision label。

## 公网入口

Controller 镜像同时提供 API 和 React 静态页面，Publisher Worker 同时提供 pacman 仓库文件。Compose 默认仅把二者映射到宿主 `127.0.0.1`，由已有的宿主 Caddy 统一完成公网 TLS、压缩和缓存策略；Stack 内不再重复部署 Caddy，也不把宿主证书或私钥挂入 AURsmith 容器。

netcup 的反代示例位于 `deploy/netcup/Caddyfile.snippet`。它只是待合并配置，部署脚本不会修改或 reload 宿主 Caddy。
