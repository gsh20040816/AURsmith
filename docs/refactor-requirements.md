# AURsmith 全面简化需求（权威）

本文件是本轮重构的权威需求，取代此前的“精简重构需求”。它描述的是**目标架构本身**，而不是对旧架构的增量修改；旧架构的细节只在“删除清单”和迁移步骤中出现。

## 0. 原则

1. **一个服务做一件事，能合并就合并。** 公网节点只保留一个主服务 `aursmithd` 和一个无网络签名器；家用节点只保留一个 Builder。
2. **状态单一来源。** 只有一个 SQLite 数据库，由 `aursmithd` 独占；签名器和 Builder 都不持有数据库。
3. **期望状态驱动，而不是批次驱动。** 仓库应包含什么由当前订阅闭包和已批准、已构建的产物直接推导；发布只是把期望状态交给签名器收敛。
4. **安全边界保留，旁路组件删除。** 7 条安全基线（第 2 节）全部保留；为了实现它们而堆叠的凭据网关、runner 容器、SSH 中继、多套网络全部删除。
5. **结构化输入输出。** Agent 结果、Builder 协议、签名计划都是 `deny_unknown_fields` 的严格 JSON；无效输出一律视为错误并失败关闭。

## 1. 产品边界

- 单管理员、少量 Arch Linux 客户端的私有 AUR 二进制仓库。
- 跟踪显式订阅的 AUR pkgbase 及其 AUR 依赖闭包；官方仓库已提供的包不订阅。
- 固定 AUR commit（以及 `git+https://` VCS source 的上游 commit），经确定性扫描与 2+1 Agent 审查（首次添加必须人工审批）后，在家用 Builder 的一次性 Docker 容器内构建，由无网络签名器签名发布。
- 不提供：多 Builder 调度策略、告警平台、内置备份、长期证据归档、KVM、Profile、独立 Archiver。

## 2. 安全基线（必须全部保留）

| # | 基线 | 新架构中的落点 |
|---|------|----------------|
| 1 | 签名私钥只在无网络签名器中，签名器自行校验暂存产物 | `signer` 容器 `network_mode: none`，唯一挂载 `repository_gpg_key`；读取 `plan.json` 后按 SHA-256/大小逐个校验 inbox 产物，再 `repo-add` 与签名 |
| 2 | 固定 commit 并校验 | revision 记录 `aur_commit`、`tree_sha256`、`vcs_commit`；Builder 收到内联文件后逐个校验大小与 SHA-256；依赖产物下载后校验 |
| 3 | 只构建已批准 revision；首次添加必须人工审批 | 调度只读取 `state = 'approved'` 的 revision；2+1 判定函数对首次添加始终返回 `manual_review` |
| 4 | 不在公网节点执行不可信 AUR 内容 | `git clone --no-checkout --template=`、`core.hooksPath=/dev/null`，只用 `git show` 读取对象并静态解析 `.SRCINFO`，绝不执行 PKGBUILD |
| 5 | 构建容器中没有任何秘密 | Builder 只把 AUR 文件、依赖包和 `guest.json` 只读挂载进 `aursmith-build`；Builder token 不进入构建容器 |
| 6 | 原子切换 current / previous | 签名器把每次发布写入 `releases/<plan_sha256>/`，用符号链接原子切换 `current`，保留 `previous`；`aursmithd signer --rollback` 交换两者 |
| 7 | 审查结论只经严格 JSON schema 进入系统 | OpenAI `json_schema strict` / Anthropic 强制 `submit_review` 工具；解析使用 `deny_unknown_fields`、长度上限和一致性检查（如 approve + critical 视为无效）；无效 → `error` |

## 3. 拓扑

```
公网节点（netcup）                                   家用节点
┌──────────────────────────────────────────┐        ┌──────────────────────────┐
│ Caddy（宿主）                              │        │ aursmith-builder run      │
│  ├─ aursmith.* → aursmithd:8080            │◀─HTTPS─│  ├─ 领取构建（租约）        │
│  └─ repo.*     → 静态文件 /srv/aursmith/repo│        │  ├─ docker run aursmith-build│
│ aursmithd（Web/API/同步/审查/调度/计划）     │        │  └─ 32 MiB 分片上传         │
│   └─ exchange 卷 ⇄ signer（network: none）  │        └──────────────────────────┘
└──────────────────────────────────────────┘
```

- 公网：2 个容器、1 个 Docker 网络（固定网段，供可选的 Arch IPv6 CONNECT 通道使用）。
- 家用：1 个常驻容器（builder）+ 按需的一次性构建容器。
- Builder 只主动发起 HTTPS 请求；公网不需要 SSH、rrsync 或任何入站到家用网络的连接。

## 4. 主流程

1. **同步**：`aursmithd` 定期（默认 30 分钟，按包错开）检查订阅闭包内每个 pkgbase 的 AUR commit 与 VCS commit。
2. **revision**：新 commit 生成 revision，确定性扫描 Block → `rejected`；AUR 树与已批准 revision 逐字节相同 → 复用审查，直接 `approved`；否则 `pending_review`，并 supersede 同包更早的待审 revision。
3. **审查**：后台循环逐个处理 `pending_review`（第 5 节）。
4. **调度**：已批准 revision 若没有可用构建，则插入一行 `builds(state = 'queued')`；依赖未就绪时保持排队并在 UI 显示等待原因。
5. **构建**：Builder 领取 → 准备输入 → 一次性容器 → 回报 → 分片上传 → 完成（第 6 节）。
6. **发布**：期望状态与最新发布不同、且闭包内没有活动构建时，写入发布计划交给签名器（第 7 节）。

## 5. 2+1 Agent 审查规则

审查输入：固定 AUR 包装层的全部文本文件（带 SHA-256），非首次时附加与上一已批准 revision 的统一 diff；总量上限 1.5 MiB。上游下载内容不在审查范围内。

| 情况 | 结果 |
|------|------|
| 两个低档 Agent 都返回有效 `approve` | 通过（非首次 → `approved`） |
| 任一低档 `reject`，或两者意见不一致，或任一输出无效/出错 | 交给高档 Agent |
| 高档返回有效 `approve` | 通过（非首次 → `approved`） |
| 高档 `reject` 或出错/无效 | `manual_review`（人工门禁） |
| **首次添加**（没有已批准 baseline） | 无论 Agent 结论如何都进入 `manual_review`；Agent 结论只作参考 |
| 未配置 Agent（无 `AURSMITH_REVIEW_CONFIG`） | 全部进入 `manual_review` |

- 两个低档可以是不同厂商，也可以是同一模型的两次独立调用；高档只在需要时调用。
- 模型直接由 `aursmithd` 通过 HTTPS 调用（OpenAI Chat Completions 或 Anthropic Messages），API key 以文件 secret 提供；没有 runner 容器、凭据网关或专用网络。
- Agent 永远不能直接让 revision 变成 `rejected`：Agent 的拒绝最坏只会导致人工审批。只有确定性扫描的 Block 会自动拒绝。
- 人工审批只能作用于 `manual_review` 的 revision，需要 8–4000 字符理由；也可以“重新运行 2+1 审查”。

## 6. Builder 协议（HTTPS）

所有请求带 `Authorization: Bearer <builder token>`，服务端只保存 token 的 SHA-256 并常量时间比较。

| 请求 | 说明 |
|------|------|
| `POST /api/v1/builder/lease` | `{builder_id, lease_seconds}` → `BuildSpec`（内联 AUR 文件、依赖产物清单、期望输出、check 策略）或 `null`；租约裁剪到 600–172800 秒 |
| `POST /api/v1/builder/builds/{id}/report` | `BuildReport`：成功时附产物清单（服务端校验包名=期望输出、文件名与元数据一致、同一版本、架构 x86_64/any）；失败时附失败码与日志尾部（≤128 KiB） |
| `GET/PUT /api/v1/builder/builds/{id}/files/{file}?offset=N` | 断点续传：偏移必须等于已接收字节；每片 ≤ 32 MiB；最后一片后校验 SHA-256 |
| `POST /api/v1/builder/builds/{id}/complete` | 全部产物上传完成后标记成功 |
| `GET /api/v1/builder/artifacts/{build_id}/{file}` | 下载同一期望状态中已构建的 AUR 依赖 |

- **Cloudflare 限制**：免费/Pro 套餐单个请求体上限 100 MB，因此分片固定为 32 MiB，Caddy 管理站点请求体上限 40 MB。
- 失败分类：`transient`（网络、Docker 守护进程、租约过期、Builder 重启、上传失败等）在同一构建行上自动重试，最多 2 次；`deterministic`（校验和、PGP、check()、打包、构建超时等）与 `config`（镜像缺失、输入无效、Docker 权限/磁盘）不自动重试。
- Builder 重启后：已生成回报的继续回报/续传；构建中途被打断的以 `BUILDER_RESTARTED` 回报。

## 7. 期望状态发布与签名器

- **期望状态** = 订阅闭包内每个 pkgbase 的最新已批准 revision 的成功构建产物（按文件名排序）。依赖不完整的包被“暂缓”（withheld），不会出现在计划中。
- **计划** `PublishPlan {format, repository_name, artifacts}` 不含时间戳，其规范 JSON 的 SHA-256 即计划身份。
- 计划与最新发布不同、且闭包内没有活动构建（running/uploading，或依赖已就绪的 queued）时写入 `exchange/inbox/<plan_sha256>/`（`plan.json` + 上一发布中没有的产物）。相同计划失败后 15 分钟重试；签名器 60 分钟无结果视为超时。
- 签名器：校验计划与产物 → 每次全量 `repo-add` → 签名包、数据库与 `manifest.json` → 写入 `releases/<plan_sha256>/` → 原子切换 `current`/`previous` → 清理只被更旧发布引用的文件 → 写 `outbox/<plan_sha256>.json`。
- `aursmith-keyring` 版本为 `1:<密钥创建时间戳>-1`，只在签名密钥变化时重建。
- 不提供 Web 回滚按钮：紧急回滚由运维在签名器上运行 `aursmithd signer --rollback`；下一次期望状态变化会重新收敛。

## 8. 数据

单一 SQLite（`/var/lib/aursmith/aursmith.db`），9 张表：

`admin`、`sessions`、`subscriptions`、`provider_choices`、`packages`（AUR 缓存 + 同步状态 + check 策略）、`revisions`、`reviews`（scan / reuse / agent / human）、`builds`（合并后的构建：一行一个 revision 构建，重试复用同一行）、`publications`。

迁移工具（`aursmithd` 子命令）：

- `export --output` / `import --input` / `verify --input`：新库的状态导出、导入到空库（含外键检查与导出复核）、比对；
- `legacy-export --legacy-database-url`：从旧 Controller 数据库导出管理员、订阅、Provider 选择、包缓存/同步状态与每个包最新的已批准 revision（作为新的审查 baseline）。旧构建与发布记录不迁移，迁移后进行一次全量重建。

## 9. 配置

- `aursmithd`：15 个 `AURSMITH_*` 环境变量（其中多数有默认值，必填仅 `PUBLIC_ORIGIN`、`REPOSITORY_BASE_URL`、`BUILDER_TOKEN_SHA256`）+ 一个审查配置 JSON（`deploy/server/review.example.json`）。
- `signer`：3 个（`EXCHANGE_DIR`、`REPOSITORY_DIR`、`SIGNER_GPG_KEY_FILE`）。
- `aursmith-builder`：9 个（`SERVER_URL`、`BUILDER_TOKEN_FILE`、`BUILDER_ID`、`JOBS_DIR`、`BUILD_CONCURRENCY`、`BUILD_CPUS`、`BUILD_MEMORY_MIB`、`BUILD_TIMEOUT_SECONDS`、`BUILD_IMAGE`）。

## 10. 删除清单

- crate：`aursmith-controller`、`aursmith-worker`、`aursmithctl`、`aursmith-agent-runner`、`aursmith-agent-gateway`、`aursmith-guest-agent`、`aursmith-repository`、`aursmith-protocol`、`aursmith-domain`（共 9 个，合并为 `aursmith-core` / `aursmithd` / `aursmith-builder`）。
- 容器：agent-low-1/2/3、agent-high、agent-credential-gateway、publisher、publisher-ssh、独立仓库 HTTP 服务。
- 机制：3+1 投票、ReleaseBatch、反向推送（reverse publisher endpoint）、签名信封（envelope）、授权阶段（authorizing）、`no_eligible_worker` / `uncertain` 等状态、rrsync/SSH 中继与 SSH 凭据、三个 SQLite 数据库、35 个历史迁移、Doctor 接口（合并为 `/api/v1/status`）。

## 11. 阶段与提交映射

| 阶段 | 内容 | 提交 |
|------|------|------|
| 0 | 文档与命名清理、状态导出工具 | `docs`、`aursmith-core`、`aursmithd`（state 模块） |
| 1 | 进程内 2+1 审查 | `aursmithd`（review 模块） |
| 2 | 密钥隔离：签名器无网络/无数据库/不解析 AUR；AUR 抓取移入主服务 | `aursmithd`（signer、aur 模块） |
| 3 | 家用 Builder HTTPS 分片上传，删除 SSH 中继 | `aursmith-builder`、删除提交、`deploy` |
| 4 | 期望状态发布、合并构建表、单一 SQLite、3 crate / 2 公网容器 | `aursmithd`、删除提交、`deploy`、`web` |

影子运行阶段（新旧并行比对）按决定跳过，以迁移前备份 + 迁移后全量重建 + 人工核对代替。

## 12. 完成定义

- `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 通过；
- Web `npm ci && npm test && npm run build` 通过；
- `python3 scripts/check-compose.py` 通过；
- 部署后按 `docs/deployment.md` 完成迁移、全量重建与独立 pacman 客户端验证，并把结果写入 `docs/verification.md`。
