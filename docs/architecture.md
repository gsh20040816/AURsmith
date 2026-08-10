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

## AUR 同步与依赖闭包

Controller 不直接访问 AUR。浏览器请求由 Controller 认证后，经固定 argv 的 OpenSSH forced command 发给在线 Publisher；Publisher Worker 才能调用 AUR RPC 和 AUR Git。Builder 或 Archiver 收到同类命令会以角色错误拒绝。

搜索使用 AUR RPC v5。订阅时，Publisher 先执行有界浅克隆，以 40 位 AUR Git commit 固定 Revision，并通过 `git show HEAD:.SRCINFO` 读取静态元数据；该过程不执行或 `source` PKGBUILD。`.SRCINFO` 被折叠为 pkgbase、全部 split outputs、依赖类型、架构、Provider 和 source 清单。

Controller 在写数据库前遍历最多 64 个 AUR pkgbase 的依赖闭包。精确同名 AUR 依赖成为隐式订阅；虚拟依赖查询 `provides`，唯一候选可以解析，多个候选进入 `awaiting_provider_selection`。全部上游输入获取成功后，直接订阅、隐式引用、不可变 Revision、依赖边和 ReleaseBatch 才进入同一个控制面事务。循环依赖进入 `blocked_cycle`，不会猜测顺序。

普通包的 AUR commit 变化就产生新 Revision。`-git` 包还从 `.SRCINFO` 的 `git+https` source 查询上游 commit；查询前拒绝私网、回环、链路本地和保留地址，并禁用 Git 重定向及 file/ext 协议。AUR commit、VCS commit 或固定 Provider 变化都会产生新 Revision，未开始发布的旧 Revision 标记为 `superseded`。split outputs 始终整体固定和构建，用户选择只表示客户端关注项。

Publisher 同时包装 Arch 官方仓库 JSON 接口。新订阅若与官方包同名会被拒绝；周期检查发现已有订阅进入官方仓库时，会暂停后续 AUR 更新、保留当前私有版本，并生成迁移告警和独立事件。

## 发布安全

受影响的依赖闭包组成一个 `ReleaseBatch`。系统完整暂存该批次，根据完整 Manifest 签名并验证，然后最后切换仓库数据库。失败批次不能修改当前 Release。
