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

Compose file-backed secret 以 UID/GID `10001`、模式 `0400` 挂载。部署前使用 `docker compose config` 检查实际渲染结果，不要把 `deploy/*/secrets` 加入 Git。

## 启动顺序

1. 启动 Publisher 和 Archiver Stack。
2. 启动至少一个 Builder Stack。
3. 启动 Controller Stack。
4. 通过 `docker compose exec controller aursmith-controller setup-token` 读取初始化令牌。
5. 在 Web 设置页创建管理员。
6. 注册三个角色的 Worker，并执行“探测”。
7. 运行 Doctor，确认 KVM、SSH、协议、仓库和归档存储状态。

不同宿主机部署时只需要复制对应 Stack 的 Compose、镜像和该角色的 secret，不要复制其他角色的数据卷或私钥。

## Git 与发布

- 日常开发直接进入 `main`，每个独立且验证通过的改动形成一个英文提交。
- 发布前工作树必须干净，`just test` 必须通过。
- Release Manifest 必须记录 `git rev-parse HEAD` 的完整 commit。
- 发布标签使用 `vMAJOR.MINOR.PATCH` annotated tag；仓库 GPG 可用后对标签签名。
- 镜像发布使用相同版本标签和源码 commit label，不能使用漂移的 `latest` 作为可恢复版本依据。
