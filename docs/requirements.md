# AURsmith v1 需求总账

本文件是规范性文档，需求 ID 永久稳定。需求只有在 `decisions.md` 中记录原因后才能被替换或延期，不能因为计划缩短而静默消失。

| ID | 需求 | 第一阶段验证方式 |
|---|---|---|
| P01 | 搜索、订阅、暂停、退订、清除和手工重建 AUR 软件包 | API 与生命周期测试 |
| P02 | 跟踪 AUR commit 和 Git VCS 上游 commit；历史重写必须阻断并人工确认 | 同步器、祖先关系与人工审批测试 |
| P03 | 以 pkgbase 为单位构建全部 split outputs | 软件包基础模型测试 |
| P04 | 解析依赖 DAG、隐式引用、Provider 和循环依赖 | 依赖图测试 |
| P05 | 把受影响依赖闭包作为一个 ReleaseBatch 构建和发布 | 发布批次测试 |
| P06 | 同一上游版本重建时派生本地 pkgrel | 版本测试 |
| P07 | 显示官方仓库晋升、ABI、删除、合并和维护者事件 | 事件测试 |
| P08 | 新 Revision 任意阶段失败后保留当前稳定 Release | 故障注入测试 |
| A01 | 构建前执行确定性扫描和 Agent 审计 | 审计流水线测试 |
| A02 | 精确实施三个低成本 Agent 的 3/2/不超过 1 票规则 | 投票测试 |
| A03 | 仅适配 Codex CLI 与 Claude Code；三个低成本 Runner 的 provider、模型、Base URL、API key 和思考强度必须独立配置；隔离调用并保存完整溯源 | 容器、凭据网关及适配器测试 |
| A04 | 如实记录源码审计覆盖范围 | 报告 Schema 测试 |
| B01 | 所有不可信构建都运行在 KVM Guest 中 | KVM 集成测试 |
| B02 | Fetch Guest 使用源码代理；Build Guest 可由 Builder 配置为无网或直接访问公网，实际模式必须写入 provenance | KVM 网络模式测试 |
| B03 | 记录输入、依赖、Profile、工具、日志和产物 | provenance 测试 |
| B04 | 统计依赖使用情况并优化不可变 Guest Profile | 优化器测试 |
| B05 | 允许为 Profile 构建和 Fetch Guest 的官方依赖下载配置 Arch HTTPS 镜像源，并在 Profile 与 provenance 中固定实际镜像 | Profile 构建与协议测试 |
| W01 | 通过 Docker Compose 部署所有 AURsmith 服务 | Compose 策略测试 |
| W02 | Builder、Publisher 和 Archiver 可部署在不同主机 | 分布式冒烟测试 |
| W03 | 使用固定 OpenSSH host key 和受限 rsync | 传输测试 |
| W04 | 使用 Attempt、Journal 和迟到结果拒绝保证任务幂等 | Worker 测试 |
| W05 | 支持多 Builder、单活动 Publisher 和单主 Archiver | 调度测试 |
| R01 | 把产物视为不可信内容，仅允许离线 Signer 签名 | Signer 测试 |
| R02 | 原子发布完整、不可变的 pacman Release | 崩溃测试 |
| R03 | 归档 Release、审计证据、provenance 和 Controller 备份 | 恢复测试 |
| R04 | 恢复服务端 Release 并生成显式客户端降级命令 | 回滚测试 |
| U01 | 提供完整 Web UI | 浏览器验收测试 |
| U02 | 提供单用户局域网认证和客户端接入引导 | 认证与接入测试 |
| U03 | 提供告警、存储、Worker、Doctor 和恢复状态 | 运维测试 |
| O01 | 包装成熟 Arch 工具，不重新实现基础能力或 Fork AURCache | 架构审查 |
| O02 | 使用 Git 管理实现、版本、Release Manifest 和发布全过程 | Git 与发布测试 |

## 延期但未遗忘

- 多用户 RBAC 和自动高可用。
- 多个同时写入的 Publisher。
- Kubernetes、自研服务发现或 mTLS 控制面。
- 多架构构建和交叉编译。
- 非 Git VCS 的精细化跟踪。
- 双 Builder 可复现性验证和硬件签名。
- 客户端安装 Agent、自动客户端降级，以及任意 PKGBUILD、Git 或压缩包上传。
- 面向公网的零信任部署和自动 Archive GC。
