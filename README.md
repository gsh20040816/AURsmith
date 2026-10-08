# AURsmith

AURsmith 是供一个管理员和少量 Arch Linux 客户端使用的私有 AUR 二进制仓库。它跟踪明确订阅的 AUR pkgbase，固定包装层 commit，经 **2+1 Agent 审查**（两个低成本 Agent，分歧或拒绝时升级一个高成本 Agent；首次添加的包始终需要人工批准）后，在家庭 Builder 的一次性 Docker 容器中构建，并由公网节点上**无网络的签名器**签名、原子发布为 pacman 仓库。

拓扑固定为两台设备：

| 设备 | 容器 | 职责 |
|------|------|------|
| 公网 | `aursmithd` | Web/API、AUR 同步、2+1 审查（直接调用 OpenAI/Anthropic API）、构建调度、发布计划 |
| 公网 | `signer` | `network_mode: none`，唯一持有 GPG 私钥；校验产物、`repo-add`、签名、原子切换 `current`/`previous` |
| 家庭 | `builder` | 通过 HTTPS 领取构建、在一次性容器中执行 `makepkg`、分片上传产物 |

仓库目录由宿主 Caddy 直接以静态文件提供；`deploy/netcup/Caddyfile.snippet` 是待合并配置，部署不会修改宿主 Caddy。

## 代码

| 路径 | 内容 |
|------|------|
| `crates/aursmith-core` | 纯逻辑：校验、`.SRCINFO`、扫描、依赖图、2+1 判定、线协议 |
| `crates/aursmithd` | 公网主服务 + 签名器 + 管理/迁移 CLI |
| `crates/aursmith-builder` | 家庭 Builder + 构建容器入口 |
| `web` | React 管理界面 |
| `deploy` | 单一多 target `Dockerfile`、`server/` 与 `home/` 两个 compose |

## 文档

- `docs/refactor-requirements.md`：权威需求与安全基线
- `docs/architecture.md`：结构、数据流、前后对比
- `docs/deployment.md`：全新部署、从旧栈迁移、运维
- `docs/verification.md` / `docs/acceptance.md`：已验证项与验收清单

## 开发

```sh
./scripts/test-all.sh   # fmt、clippy -D warnings、cargo test、前端 test/build、compose 静态检查
```

签名器测试需要宿主有 `repo-add`、`makepkg`、`fakeroot`、`gpg`、`bsdtar`、`zstd`。生产镜像以准确源码 commit 写入 OCI revision label。
