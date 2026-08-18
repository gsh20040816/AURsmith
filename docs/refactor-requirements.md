# AURsmith 精简重构需求

本文是本轮重构的权威需求，取代此前围绕 KVM、多 Worker、独立 Publisher 设备、Archiver、完整证据链和通用运维平台形成的设计。实现、文档、测试和部署配置都必须以本文为准；旧文档与本文冲突时，应修正或删除旧文档，不能同时保留两套现状描述。

## 0. 执行原则

重构必须在现有代码和生产数据上原位完成。优先复用已经验证的 AUR 获取、3+1 Agent、Builder 轮询、`rrsync`、pacman 仓库和认证实现，按功能整簇删除旧抽象；不得另起 v2、重建一套平行核心、维护新旧双轨或先添加兼容层再等待未来清理。

允许对 API、环境变量和 Compose 做一次明确记录的破坏性迁移，但必须保留现有管理员、订阅关系、最后批准 baseline 和当前仓库状态。删除旧能力时应同时删除对应代码、路由、配置、页面、测试和文档，不能只加禁用开关。

每个重构阶段都必须满足：

- 改动边界清楚，相关测试通过；
- 当前稳定仓库继续可用，失败不会破坏 `current`；
- 不把未验证的设计写成已实现或已验收；
- 不通过吞错、fallback、弱化断言或跳过测试维持表面兼容；
- 不因“以后可能扩容”保留当前没有用途的平台抽象。

## 1. 产品目标与边界

AURsmith 是单个管理员为自己的少量 Arch Linux 设备维护的私有 AUR 二进制仓库。它只解决以下问题：

1. 跟踪管理员明确加入的 AUR `pkgbase` 及其必要 AUR 依赖；
2. 把 AUR 包装层固定到精确 commit，并通过 3+1 Agent 审查后自动批准或进入人工处置；
3. 在固定家庭 Builder 上构建已经批准的输入；
4. 把产物发送到固定公网设备，生成签名 pacman 仓库；
5. 让管理员查看审查、构建和仓库状态，并在失败时保留当前稳定仓库。

本项目不是通用供应链安全平台，不承诺：

- 审计全部上游源码、第三方依赖或预编译二进制；
- 证明 Agent 已理解所有文件，或提供模型行为取证；
- 在批准后继续把 PKGBUILD 当作必然恶意代码进行 KVM 级隔离；
- 多管理员、RBAC、组织、租户、OAuth/OIDC、PAT、MFA 或设备管理；
- 多 Builder 调度、Publisher 选主、自动故障转移或高可用；
- Archiver、跨故障域备份、完整 Release 取证链或灾备编排；
- 自动判断 Arch 官方依赖升级的 ABI 影响；
- 自动控制或降级客户端软件包版本。

## 2. 威胁模型与已接受风险

审查批准前，AUR Git tree 中的所有内容都是不可信输入。系统只能把它们当作数据读取、比较和展示，不能执行、`source` 或服从其中的 `AGENTS.md`、注释、提示词和脚本指令。

审查目标只覆盖 AUR 包装层，包括 `PKGBUILD`、`.SRCINFO`、install 脚本、hook、补丁和仓库内其他包装文件。系统固定并记录声明的上游来源和 Git VCS commit，但不因此声称上游源码安全。

同一 AUR commit 通过 3+1 或后续人工决定后，进入 Build 时视为可信。构建使用联网的一次性 Docker 容器；Docker 只提供干净、可删除的环境，不提供 KVM 级安全边界。

本项目明确接受以下风险，不得再用这些风险反向引入已删除的平台：

- 公网设备和家庭 Builder 都是单点；
- Docker 与 Builder 宿主共享内核；
- diff-first 会继承上一批准 baseline 中未发现的问题；
- Agent 可能产生相关误判，三个 low 的独立配置不等于形式化独立性证明；
- Publisher 进程直接持有仓库 GPG 私钥，公网设备失陷即可伪造仓库签名；
- 同一上游版本的手工重建保持原版本，客户端不会自动升级该重建；同名不同内容制品切换时，接受客户端持有旧数据库再请求新同名文件可能暂时不一致。

## 3. 固定两台拓扑

```text
浏览器 ──HTTPS──> 公网设备
                  ├── Controller / React Web / 调度
                  ├── Publisher / GPG / repo-add / 静态仓库
                  ├── 3 个固定 low Runner
                  ├── 1 个按需 high Runner
                  └── credential gateway

家庭 Builder ──HTTPS 轮询与上报──> 公网设备
      │
      ├── 一次性联网 Docker Build
      └── write-only rrsync 推送──> 公网 incoming
```

公网设备使用一套 Compose 部署。Controller 与 Publisher 可以继续作为独立进程以便原位复用，但它们是固定本机角色，不得继续通过通用 Worker 注册、标签、角色协商、选主或跨主机 SSH 模拟动态集群。宿主已有 Caddy 负责公网 TLS；AURsmith 容器不读取 TLS 私钥，也不部署第二层 Caddy 或内部 CA。

家庭设备只运行一个固定 Builder daemon 和它创建的临时 Build 容器。Builder 不开放 AURsmith 公网入站端口，不注册、不声明标签、不参与能力协商，也没有 online/degraded/draining/incompatible 状态机。Controller 只保存 `last_poll_at`、当前槽位使用量和最后错误。

## 4. 软件包主流程

1. 管理员在 Web 中加入一个 `pkgbase`。
2. 公网服务从 AUR Git 获取完整 tree，固定精确 AUR commit，并静态读取 `.SRCINFO`；不得执行 PKGBUILD。
3. 系统解析必要 AUR 依赖闭包。唯一 Provider 自动选择；Provider 歧义或依赖环阻断，并等待管理员处理。
4. 首次加入执行完整包装层审查；后续 AUR commit 相对最后批准 baseline 生成完整 tree-to-tree diff，再执行 diff-first 审查。只有 VCS 上游 commit 变化且包装 tree 不变时复用该 tree 的既有批准。
5. 3+1、后续人工决定或合法的 VCS-only 审查复用确认当前输入可构建后，Controller 创建 Build job 和首个 attempt。
6. Builder 使用固定 Bearer secret 轮询，下载批准 tree 和任务元数据，复验摘要后进入本地队列。
7. Builder 根据全局并发配置，在一次性联网 Docker 容器中运行 `makepkg`，生成全部预期 split outputs、Manifest 和有界日志。
8. Builder 使用固定 SSH key，通过官方 `rrsync` 的 write-only 模式把产物推送到 attempt 专属 partial 目录，再经 HTTPS 通知 Controller 完成上传。
9. Publisher 将 partial 原子移出 SSH 可写目录，核对 attempt、Manifest、普通文件、SHA-256、大小、`.PKGINFO` 名称、版本、架构和 split outputs。
10. 同一受影响依赖闭包全部成功后，Publisher 在 staging 中签名变化包，运行官方 `repo-add`/`repo-remove`，生成仓库数据库和小型 Manifest，最后原子切换 `current`。
11. 任一审查、构建、传输、校验、签名或发布步骤失败，都保持原 `current` 不变。

第一版只支持 `x86_64`。同一 `pkgbase` 的全部 split outputs 必须一起构建和校验，不能只发布用户关注的部分输出。

## 5. 订阅、依赖与更新语义

Web 只提供“加入”和“删除”两种订阅生命周期操作，不保留暂停、恢复、退订、清除和保留期等多套状态。

删除一个显式订阅时，系统必须在下一原子 Release 中移出该 pkgbase 的全部 split outputs，并重新计算从所有剩余显式订阅可达的 AUR 依赖闭包。不再被任何显式订阅引用的孤儿 AUR 依赖必须同时停止跟踪并移出仓库；共享依赖仍被引用时必须保留。

AUR 依赖解析只服务当前订阅闭包：

- 官方仓库已提供的依赖不创建 AUR 订阅；
- 同一 pkgbase 的 split output 依赖视为内部关系；
- 精确同名或唯一 Provider 的 AUR 依赖自动加入；
- 多 Provider 必须由管理员选择；
- 依赖环明确阻断，不猜测构建顺序；
- 不建设通用 Provider 平台、依赖优化器或官方依赖 ABI 模型。

系统使用一个全局可配置检查周期，并按 `pkgbase` 做确定性错峰，避免同一时刻并发查询全部包。管理员可以手工刷新单个包。

普通包在 AUR commit 变化时进入新一轮审查。Git VCS 包必须按 `.SRCINFO` 中解析后的 `git+https://` source 识别，不依赖包名是否带 `-git` 后缀，并跟踪该上游的精确 commit；只有上游 commit 变化而 AUR 包装 tree 不变时，复用该包装 tree 的既有批准，记录新的精确 VCS commit 并直接创建 Build，不重复调用 3+1，也不把复用描述成审计了新上游源码。AUR 包装 tree 同时变化时仍按正常 full/diff 规则重新审查。非 fast-forward 历史重写视为普通 VCS commit 变化，不保留祖先关系门禁或专用人工审批。

Arch 官方依赖版本变化不触发检测、提示或自动重建。管理员可以手工重建当前已批准 commit；手工重建仍使用上游原版本和原 `pkgrel`，不派生本地版本。系统必须在 UI 中明确说明该制品不会通过常规版本比较自动升级客户端，并显示同名制品替换风险。

新 commit 到达时，尚未开始审查的旧候选可以合并到最新状态；已经批准并创建 Build 的旧 commit 继续构建和发布，完成后再处理新 commit。不同 commit 的审查结果、attempt 和产物不得混用。

## 6. diff-first 3+1 Agent 审查

首次加入包或无法恢复批准 baseline 时执行 full 审查。更新包必须以同一 `pkgbase` 最后一次自动或人工批准的 commit 为 baseline，生成不截断的完整 tree-to-tree diff；不能使用最近抓取、失败、拒绝或未批准的 commit 作为 baseline。diff 无法完整生成时只能退回 full，不能在信息不全时自动批准。

Controller 固定调度三个 low Runner，必要时调度一个 high Runner。每个 low 的 provider、model、Base URL、API key 和思考强度由部署者分别配置；不在代码中强制不同供应商或模型，但 Web 和审查记录必须如实显示每次实际使用的 provider、model、CLI 版本和配置身份。当前生产配置是两个供应商、三个 low 模型，重构不得把三票合并成同一 Runner 的内部循环。

投票规则固定为：

- 等待三个 low 全部进入终态；
- 3/3 `approve` 自动批准；
- 恰好 2/3 `approve` 调用一次 high，high 只有明确 `approve` 才批准；
- 0–1/3 `approve`、high 非批准或技术错误耗尽重试后进入人工队列；
- 合法 `reject` 不重试；明确瞬时技术错误最多重试两次；
- 人工决定只能在上述自动流程终止后发生，必须绑定当前 commit 并填写理由；不能在审查开始前绕过首次 3+1。

Runner 必须满足：

- 工作区只包含当前包装 tree、可选 baseline/diff 和输出 Schema；
- AUR 文件只读，Runner 不得执行包内程序或访问任意外部网络、Docker Socket、数据库、SSH/GPG key；网络只允许到固定 credential gateway；
- Provider API key 只进入 credential gateway，不进入 Runner 环境、提示、结果或日志；
- Codex/Claude 等 CLI 使用固定绝对路径和结构化 argv，不接受用户提供的命令或适配器插件；
- 唯一权威结果是符合固定 Schema 的结果文件；自然语言、Markdown、stdout、last-message 或从文本中抽取 JSON 都不得作为 fallback；
- `files_read` 只用于排错，不构成模型理解或覆盖证明；
- 报告保存结构化结论、发现、实际 provider/model、CLI 版本、起止时间和有界错误，不保存隐藏推理或凭据。

确定性预检查只阻断由输入直接证明的违规，例如不安全路径、快照/摘要不一致和结构损坏。脚本中出现网络、权限、私网地址或可疑 Shell 片段只能作为上下文发现交给 Agent，不能用字符串匹配冒充恶意代码判定。

## 7. 固定 Builder 与 Docker

Builder 使用一份部署级 Bearer secret 调用轮询、输入下载、状态和完成通知接口。该 secret 不是可创建、枚举或分配权限的通用 API Token，不能用于浏览器管理 API。Builder 重启后必须从本地持久状态恢复未结束 attempt；重复轮询和重复上报必须幂等。

Builder 只使用以下全局部署配置：

- 最大并发 Build 数；
- 每个 Build 的 CPU；
- 每个 Build 的内存；
- 每个 Build 的总超时；
- 构建日志和输出大小上限；
- 固定 Build 镜像与 Arch HTTPS 镜像。

不得增加按包资源覆盖、自动资源探测、标签、亲和性、优先级平台、配额或 cgroup 合规状态机。Controller 只按固定并发槽位签发任务；同一 attempt 不得因重复轮询占用多个槽位。

每次 Build 必须使用全新容器和 attempt 专属目录：

- 使用普通 Docker bridge 网络直接访问互联网；
- 使用 Docker `--init` 处理信号与孤儿进程；
- 不使用 `privileged`，不增加设备或额外 capability；
- 不挂载 Docker Socket、Controller 数据、真实密钥或无关宿主目录；
- 输入只读，输出只写入 attempt 目录；
- `makepkg` 以普通用户运行，依赖安装仅发生在临时容器内；
- 默认执行 `check()`；管理员按包关闭时必须记录理由，并只影响之后创建的 Job；
- 不使用 `env -i` 清空标准构建环境，不注入语言或具体软件包专用变量、补丁和后台服务控制；
- Docker 失败绝不回退到宿主执行 PKGBUILD。

Build 完成后，Builder 必须重新核对结果身份、预期 split outputs、普通文件、大小和 SHA-256，才能标记 attempt 成功。

## 8. 跨机传输

Builder 只主动连接公网设备。控制面使用 HTTPS Bearer secret；产物使用一把固定 SSH key 和官方 `rrsync` write-only 模式。SSH 账户必须禁止 Shell、PTY、端口转发、agent 转发和任意目标路径，只能写固定 incoming 根目录下由服务预先创建的 attempt partial。

跨机协议只包含：

- attempt ID；
- 预期 Manifest；
- partial/finalized 状态；
- 完成通知和幂等结果。

不得保留或重新实现 Worker Ed25519 身份、nonce、防重放 Envelope、协议协商、writer epoch、TransferCapability、双向 pull/push 或跨角色路由。HTTPS 与 SSH 各自使用固定部署凭据；两份凭据都不得进入 Build 容器。

rsync 中断必须能继续同一 partial。完成通知后，Publisher 必须先把 partial 原子移出 SSH 可写目录，再校验文件。相同 attempt 和 Manifest 的重复完成必须幂等成功；同一 attempt 出现不同 Manifest 或文件内容必须失败关闭。

## 9. Publisher、仓库与 keyring

Publisher 是公网设备上的固定进程，直接持有仓库 GPG 私钥并承担产物接收、复验、签名、`repo-add`/`repo-remove`、静态仓库和 `current/previous` 切换。删除独立 Signer 服务、ReleaseAuthorization、writer epoch、Signer inbox/outbox 和双重授权协议。

Publisher 必须显式使用配置的 GPG 主指纹，不能从 keyring 中选择“第一个私钥”。私钥不得进入 Controller、Runner、gateway、Builder、Build 容器、SSH incoming 账户、日志或数据库。这里的隔离目标只是缩小误传范围，不声称抵御公网宿主失陷。

每次发布在同一文件系统 staging 中完成：

1. 核对受影响闭包的全部预期产物；
2. 复验包元数据和摘要；
3. 对变化包签名；
4. 从当前仓库复制或链接未变化包；
5. 使用官方 `repo-add`/`repo-remove` 生成 db/files；
6. 生成包含包名、版本、文件名、大小和 SHA-256 的小型 `repository-manifest.json`；
7. 复验 staging 完整性；
8. 将原 `current` 变为 `previous`，再原子切换新 `current`；
9. 切换成功后清理更旧 Release、incoming 和 staging。

系统只保留 `current` 与 `previous` 两个完整仓库，不维护 30 天、每包三版本、任意历史回滚、ArchiveCopy 或长期证据目录。服务端回退只把可用仓库恢复为 `previous`，不生成客户端 `pacman -U` 命令，也不声称已经安装的新版本会自动降级。

同版本重建允许产生同名不同内容的包。替换必须通过 staging 和原子切换完成，不能原地截断公开文件；Web 必须展示该包不会自动升级以及旧数据库/新同名文件竞态这一已接受风险，不建设额外版本或多 URL 兼容层掩盖它。

`aursmith-keyring` 是保留包名，AUR 产物不能覆盖。它由 Publisher 使用固定可信模板生成，不进入 Agent 审查或远程 Builder。包内包含 `aursmith.gpg`、`aursmith-trusted` 和 `aursmith-revoked`，安装或升级时执行 `pacman-key --populate aursmith`。

keyring 使用持久、单调递增 generation：

- 首次发布生成第一代；
- 配置刷新周期到期时，即使信任内容未变化也生成新 generation，并允许 keyring-only Release；
- 信任集合变化时立即生成新 generation；
- 同一次失败重试复用原 generation，只有 `current` 成功切换后才推进已发布 generation；
- Web 显示活动指纹、当前 generation、上次发布时间和下次到期时间。

首次客户端接入必须通过独立可信渠道人工核对完整 GPG 指纹，再导入公钥并安装 `aursmith-keyring`。正常换钥由管理员安排旧/新钥重叠期；旧钥泄露时重新进行带外信任。回退 `previous` 时不得静默恢复已经不再受当前信任集合接受的仓库；若 keyring 或签名指纹不兼容，回退必须失败并给出明确错误。

## 10. Web、认证与人工操作

管理 Web 通过公网 HTTPS 提供，只支持一个管理员。保留现有 React/Vite 界面并原位删减，不另写第二套前端。

认证要求：

- 管理员初始化、改密和吊销全部 session 只通过公网设备本地 CLI；
- 密码使用 Argon2id；数据库只保存服务端不透明 session token 的摘要；
- Cookie 使用 `__Host-` 前缀、`Secure`、`HttpOnly`、`SameSite=Strict`、`Path=/`，同时实施空闲和绝对过期；
- 登录校验固定公开 Origin；已认证写请求同时校验 Origin 和自定义 CSRF header；
- 关闭 CORS；GET/HEAD 不得改变状态；
- 登录只做简单有界节流，不增加注册、找回、永久锁号、CAPTCHA、MFA 或设备管理。

Web 只保留：

- 登录与退出；
- 加入和删除显式 pkgbase；
- 包详情、依赖闭包和 Provider 选择；
- 当前 full/diff、三个 low 与 high 结果、人工决定；
- 手工刷新、手工重建和按包 `check()` 设置；
- Build 队列、attempt、有限重试、Builder 最近轮询时间、最后错误和有界日志；
- `current/previous`、服务端回退、仓库 Manifest；
- keyring generation、活动指纹和客户端首次接入说明。

活动页面使用简单定时轮询重新读取权威 JSON。删除 SSE、事件序号和增量快照协议。删除 Worker/Profile/Archive/Backup/Settings/成本预算/通用指标/需求总账页面，以及 alerts、Webhook、ntfy 和通知 outbox。

管理员只能在自动审查进入人工队列后批准或拒绝当前 commit；可以选择 Provider、手工刷新、手工重建、修正配置后重试、关闭或恢复 `check()`、删除订阅以及回退 `previous`。不能跳过首次 3+1、不能发布未审查 commit，也不能通过 Web 获得 Builder secret、SSH key、Agent API key 或 GPG 私钥。

## 11. 状态、失败与清理

状态模型只保留闭合主流程所需身份：package、revision、audit、build job、attempt、artifact、publication 和 keyring generation。不能继续保留通用 WorkerRole、WorkerState、ArchiveState、Capability、Profile、Operation、Alert 或 Notification 状态机。

失败处理固定为：

| 类别 | 例子 | 处理 |
| --- | --- | --- |
| 明确瞬时错误 | AUR/Agent/镜像站 408、429、5xx，DNS/连接超时，Docker daemon/image pull、HTTPS 或 rsync 暂时失败 | 当前阶段最多自动重试 2 次；耗尽后显示最后错误，修复后可人工重试 |
| 合法审查结论 | Agent `reject`、high 非批准、人工拒绝 | 不自动重试；等待人工决定或新 commit |
| 确定性输入/构建失败 | 路径或摘要错误、checksum/PGP 不匹配、编译、测试、打包、产物集合或元数据错误 | 不自动重试；修正环境或上游后由管理员显式重建或等待新 commit |
| 配置与权限错误 | credential、磁盘、Docker 权限、GPG、Provider、Provider 选择、仓库环境错误 | 不自动循环；修复后显式重试同一 commit |

重试只重做失败阶段：上传失败不重新 Build；签名或 `repo-add` 失败复用已验证产物。attempt ID、重试次数、Manifest 和完成通知必须持久化且幂等；进程重启不能重置预算或把失败伪装成新任务。无法可靠分类的错误按配置错误处理，不自动重试。

Builder 根据全局并发槽位领取任务。任务完成并且 Controller 已保存必要结果、摘要和有界日志后，Builder 立即删除容器、input、output 和临时工作目录，只保留最小本地终态以拒绝重复 attempt。Publisher 成功切换后立即删除 incoming、staging 和早于 `previous` 的仓库；失败 staging 只保留到错误被 Controller 接收，之后清理，诊断信息进入有界日志。

项目不实现磁盘水位调度、年龄保留、自动归档、库存巡检或内置备份。数据库、`current/previous` 和 GPG 私钥由宿主备份工具负责；项目文档必须给出一致性备份、恢复顺序、权限和恢复后校验方法，但不得把同机副本描述成灾备。

## 12. 数据与部署迁移

重构必须使用现有 migration 链向前迁移生产 SQLite，不能从空数据库重建，也不能维护新旧双 Schema。迁移前必须由宿主完成数据库、仓库目录、Builder 本地状态和 GPG 私钥备份，并在副本上演练。

迁移必须保留：

- 唯一管理员的密码哈希；活动 session 可以统一吊销；
- 当前显式订阅及其仍可达的 AUR 依赖；
- 每个包最后批准 baseline 的 AUR/VCS commit 和生成后续 diff 所需的 tree；
- 支持该 baseline 的最终审查决定和必要 Agent 结构化结果；
- 当前仓库内容、Manifest、GPG 指纹和 keyring generation；
- 能映射为 `previous` 的最近一个完整有效仓库；
- 正在运行且能安全恢复的 Build attempt，或明确终止并在 Web 显示迁移错误。

旧的 Worker 注册、角色、标签、Profile、KVM、Fetch、TransferCapability、ReleaseAuthorization、Archiver、库存、备份、Alert、Notification、ABI 建议和完整 Evidence 数据不进入新主路径。确需保留以完成一次迁移读取的旧表只能作为有明确删除条件的临时 migration 输入，不能继续被运行时代码查询。

允许一次性更改 API、环境变量、secret 名称和 Compose。迁移说明必须列出旧值到新值的映射以及已经删除的配置；未知或冲突值失败关闭，不能静默采用默认值。保留当前真实 netcup 域名、宿主 Caddy 入口、实际三个 low/high Provider 配置、Builder jobs 路径、SSH host key 和仓库 GPG key，不为未使用云厂商保留分支。

## 13. 必须整簇删除的旧能力

| 删除对象 | 新边界 |
| --- | --- |
| KVM、QEMU、Guest Profile、overlay、Profile builder/optimizer、Fetch/Build 双阶段、source proxy | 一个联网的一次性 Docker Build |
| Guest Agent 作为 VM 协议角色 | 仅保留 Build 容器内最小固定入口；可改名但不得保留 VM/Profile 抽象 |
| 多 Worker 注册、Builder/Publisher/Archiver 角色、标签、drain、probe、选主、心跳状态机 | 固定公网服务和固定 Builder；只记录 Builder 最近轮询与槽位 |
| Ed25519 Worker 身份、nonce、防重放 Envelope、协议协商、签名 JobSpec | 固定 HTTPS Bearer secret、attempt ID、摘要和幂等状态 |
| TransferCapability、双向拉取、跨角色路由、writer epoch | 固定 Builder 通过 write-only rrsync 单向推送 |
| 独立 Signer、ReleaseAuthorization、Signer inbox/outbox、双重授权 | Publisher 直接持钥并在 staging 中签名发布 |
| pacoloco、缓存指标、官方依赖观察、ABI/重建建议 | Builder 直接使用配置镜像；官方依赖变化不处理 |
| 完整 ReleaseEvidence、日志摘要链、30 天/三版本历史、Archive receipt/inventory | 关键审查记录、有界日志、`current/previous` |
| Archiver、控制面备份 API、远端备份传输、恢复协议 | 宿主备份与文档化恢复检查 |
| alerts/events/outbox、Webhook、ntfy、通用 metrics、SSE | 对应页面直接显示状态、最后错误和定时轮询 |
| Worker/Profile/Archive/Backup/成本/需求总账等 Web 页面 | 本文第 10 节规定的核心页面 |
| 暂停、恢复、退订、清除、依赖保留期 | 只有加入与删除；删除同时移仓并清理孤儿依赖 |
| VCS 历史重写专用门禁 | AUR tree 变化走普通 3+1；VCS-only 变化复用未变化包装 tree 的批准 |
| 本地 `pkgrel` 派生 | 同上游版本重建保持原版本并明确风险 |
| 多管理员、公开 setup、远程控制 CLI、PAT/RBAC/OAuth | 本地管理员 CLI 和服务端 Cookie |
| 无调用的旧表、字段、fixture、Dockerfile、Compose stack 和云厂商分支 | 前向 migration 与真实两台部署 |

## 14. 最低验收

重构只有同时满足以下条件才算完成：

1. 当前仓库全部相关自动测试通过；旧测试只有在对应能力确实删除或被新闭环测试替代后才能删除，不能 skip、xfail 或弱化断言。
2. 生产数据库副本能原位迁移，管理员、显式订阅、可达 AUR 依赖、批准 baseline、必要 Agent 结果、`current/previous` 和 keyring generation 均与迁移前事实一致；活动 session 按说明吊销。
3. 三个实际 low 配置和 high 配置分别以真实 Provider 完成 full 与 diff 审查；结果文件缺失、stdout-only、Markdown、last-message、非法 Schema 和包内提示注入全部失败关闭。
4. 实际验证 3/3 自动批准、2/3 加 high、0–1/3 人工队列、high 非批准、合法 reject 和技术错误重试耗尽。
5. 在真实两台设备上完成“加入真实 AUR 包 → 自动依赖闭包 → full/diff 3+1 → 家庭 Builder 联网 Docker → rrsync 推送 → 公网 Publisher 校验/签名/repo-add → 原子 current → 独立 Arch 客户端安装与正常版本更新”。
6. 构建至少覆盖一个联网 Build、一个 split package、一组 AUR 依赖、一次 `check()` 通过和一次管理员关闭 `check()` 的后续 Build。
7. 全局检查按包错峰；Git VCS 上游 commit 变化能固定新 commit、复用未变化包装 tree 的批准并触发 Build；AUR tree 同时变化时仍重新审查；官方依赖版本变化不会创建建议或 Job；已批准旧 commit 能先发布，再处理新 commit。
8. 删除显式订阅会在一个原子 Release 中移出其全部 outputs 和不再可达的孤儿依赖，同时保留共享依赖。
9. Builder 按配置并发运行且不超发槽位；重启恢复、重复轮询、重复上报和迟到结果均幂等；Build 容器不含 Builder/SSH/GPG/Agent secret 或 Docker Socket。
10. rsync 中断能继续；SSH key 不能执行 Shell、PTY、转发或写出 incoming；完成后文件先脱离 SSH 可写目录再校验；冲突 Manifest 失败关闭。
11. 上传失败不重新 Build；签名或 repo-add 失败不破坏 current；瞬时错误最多自动重试两次，确定性和配置错误不循环。
12. `current/previous` 原子切换和服务端回退通过真实 GPG/pacman 验证；回退不声称客户端降级；keyring 不兼容的 previous 必须拒绝回退。
13. 首次公钥指纹带外核对、keyring 安装、周期 keyring-only generation 和一次计划换钥重叠流程通过真实 pacman/GPG 验证。
14. 同版本手工重建保持原版本，Web 明确展示不会自动升级和同名制品切换风险；验收只验证系统按该已接受语义工作，不伪造无竞态保证。
15. 未登录管理请求失败；Cookie、过期、logout、Origin/CSRF 生效；浏览器 session 不能调用 Builder API，Builder secret 不能调用管理 API；公共仓库、公钥和签名只能匿名读取。
16. Builder 与 Publisher 终态工作区按要求清理；文件系统只保留 `current/previous` 和必要运行状态；旧 Archiver/Profile/Capability/Alert/Evidence 等运行路径不再存在。
17. README、架构、部署、验收、验证和发布文档不再把 KVM、独立 Publisher 设备、Signer、Archiver、完整证据链或其他已删除能力描述为当前实现。

## 15. 完成定义与扩展门槛

完成重构意味着：本文删除矩阵中的机制在代码、配置、数据库运行路径、页面、文档和测试中都不再存在；保留的两台主路径已经通过真实 Provider、真实跨机传输、真实 GPG/pacman 和生产数据迁移验收。仅默认关闭旧功能、保留未来入口、用新包装层转调旧状态机或声称“以后再删”都不算完成。

重构完成后，新能力只有在以下条件全部满足时才能进入项目：

1. 解决当前单管理员、两台固定设备和实际订阅中已经发生的问题；
2. 不能由宿主工具、现有 Arch 工具或简单人工操作可靠解决；
3. 明确新增的信任边界、状态、失败模式、删除条件和验收方式；
4. 不重新引入多节点、通用协议、取证平台、IAM 或高可用抽象；
5. 相关真实验证已经具备，而不是只存在设计理由。
