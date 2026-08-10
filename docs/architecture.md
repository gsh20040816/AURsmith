# 系统架构

## 信任边界

- Controller 是策略权威，负责签发内部授权。
- 第一版信任 Worker 宿主机和 Worker daemon。
- AUR 文件、下载源码、构建 Guest、构建产物和软件包元数据均不可信。
- Publisher 负责校验产物，但不能访问仓库签名私钥。
- Signer 完全断网，只接受已签名的 `ReleaseAuthorization`。
- Archiver 独立保存不可变 Release 和回执，归档状态不影响 Release 已发布状态。

## 部署

系统由 Controller、Builder、Publisher 和 Archiver 四套 Docker Compose Stack 组成，每个 Worker 实例只承担一个角色。同一物理设备可以运行多套 Stack，但不能共享可写服务状态。

Builder daemon 在容器中通过 `/dev/kvm` 直接启动 QEMU，不获得 Docker Socket、libvirt Socket、TUN 或 privileged 权限。受限联网的 Fetch Guest 通过 Publisher 代理获取源码；全新的 Build Guest 不带网卡，只接收不可变且已经审计的输入。

控制流使用固定 host key 和 forced command 的 OpenSSH。大文件使用 rsync 在 Builder 与 Publisher、Publisher 与 Archiver 之间直接传输，并由短期有效的 Controller 签名 `TransferCapability` 授权。

## 状态模型

`Revision`、`Job`、`Attempt`、`Artifact`、`Release` 和 `ArchiveCopy` 是相互独立的聚合。已提交的 Release 不会因为 ArchiveCopy 等待或失败而退回未发布状态。任务采用至少一次投递，Attempt token 用于保证结果接收幂等并拒绝迟到结果。

## 发布安全

受影响的依赖闭包组成一个 `ReleaseBatch`。系统完整暂存该批次，根据完整 Manifest 签名并验证，然后最后切换仓库数据库。失败批次不能修改当前 Release。
