# 决策记录

## 已接受

- ADR-001：控制面使用 Rust、Axum、SQLx 和 SQLite，UI 使用 React 与 TypeScript。
- ADR-002：部署拆分为 Controller、Builder、Publisher 和 Archiver 四套 Docker Compose Stack。
- ADR-003：由非 privileged Builder 容器直接启动 QEMU/KVM。
- ADR-004：远程控制和批量传输使用 OpenSSH 与 rsync。
- ADR-005：确定性 CBOR payload 由包含 SHA-256 和 Ed25519 签名的 Envelope 承载。
- ADR-006：三个低成本 Agent；三票通过、恰好两票升级一个高成本 Agent、不超过一票转人工。
- ADR-007：热点依赖进入不可变 KVM Guest Profile，不进入服务容器镜像。
- ADR-008：AURsmith 自有代码使用 Apache-2.0 许可证。
- ADR-009：交付全过程在 `main` 上使用 Git。每个验证通过的阶段形成独立提交，提交标题使用英文 `<type>: <message>`；Release Manifest 记录源码 commit，签名设施可用后为发布版本创建带签名的 annotated tag。

## 已拒绝

- Fork AURCache 或复制 aurto、lilac 核心代码。
- 裸机 AURsmith 服务、privileged Docker-in-Docker，以及 Docker/libvirt Socket 挂载。
- Kubernetes、Redis、Publisher 自动选主和自研 mTLS 集群协议。
- 使用 `latest` 作为角色名称；网络敏感角色命名为 Publisher。
- 三个低成本 Agent 全部通过后，仅因为软件包风险分类而强制调用高成本 Agent。
