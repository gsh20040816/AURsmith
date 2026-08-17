# 当前架构

AURsmith 只面向一个管理员、一个公网节点、一个家庭 Builder 和少量 Arch 客户端。

```text
浏览器 / Arch 客户端 ──HTTPS──> 宿主 Caddy
                                  ├── Controller + Web
                                  └── Publisher + pacman 仓库

Controller ──固定内部 HTTP──> 3 个 low Runner / 1 个 high Runner
家庭 Builder ──Bearer HTTPS 轮询──> Controller
家庭 Builder ──固定 SSH key + rrsync──> Publisher incoming
```

Controller 保存订阅、AUR revision、批准 baseline、Agent 结构化结果、Build job、attempt 和发布状态。Builder 不注册身份或能力，只用一份部署级 Bearer secret 轮询。Publisher 是固定进程，直接持有仓库 GPG 私钥，在同一 staging 文件系统中校验产物、签名、运行 `repo-add`/`repo-remove`、复验并原子切换仓库。

审查只覆盖 AUR 包装 tree。首次是 full；更新相对最后批准 baseline 生成完整 diff。三个 low 全部终态后，3/3 approve 自动通过，2/3 approve 调一次 high，其余进入人工队列。Runner 只接受固定 JSON Schema 结果，不从 stdout、Markdown 或自然语言提取结论。

家庭 Builder 使用一次性联网 Docker 容器执行 `makepkg`。Docker 是干净构建环境，不声称提供虚拟机安全边界。产物经普通文件、大小、SHA-256、`.PKGINFO` 和 split-output 集合校验后，使用 write-only rrsync 推送。

Publisher 成功发布后只保留当前仓库和一个 previous；失败不切换 current。项目不实现独立 Signer、长期 Release 归档、pacoloco、Profile/KVM、多 Worker 调度、告警/通知、内置备份或高可用。

完整边界以 [refactor-requirements.md](refactor-requirements.md) 为准。
