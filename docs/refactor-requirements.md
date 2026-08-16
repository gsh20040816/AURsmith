# AURsmith 精简重构需求（草案）

## 0. 重构执行原则

本轮重构必须**原位简化并复用已经验证的实现**，不得借精简之名另起新核心、重建 Schema、重写 Web，或自制 Provider/Agent 协议。每一项都先删除已确认无用的旧抽象，再在现有调用链中做闭合需求所需的最小修改；禁止先造 v2、兼容双轨、通用适配平台或占位实现，再等待未来迁移。

以下现有主路径是重构基线，不得替换：Controller 调度 3 个 low、按需 1 个 high Runner，Runner 经 credential gateway 调用 Codex CLI；现有 netcup 环境变量、secret、Codex argv、Dockerfile 和真实 Provider 验证方式继续复用。Agent 只允许在既有代码中补齐 diff-first 提示、严格结果、失败分类和重试，不得改成 Controller 内联 Codex、手写 Responses 客户端、新 profile 系统或新的 Runner/gateway 抽象。

## 1. 已确认的产品边界

AURsmith 是单个管理员自用的 AUR 私有仓库打包器，不是通用供应链安全平台。

本草案采用以下固定部署前提：

- 公网 Controller 设备运行现有 Web、调度、3 个 low Runner、按需 high Runner 和 credential gateway；
- 固定 Builder 设备使用普通联网 Docker 构建；
- 公网 Publisher 分机运行 GPG/`repo-add` 和静态 pacman 仓库；
- Builder 只主动连接公网设备，不开放 AURsmith 入站端口；
- 第一版只有一个 Builder 和一个 Publisher，不考虑扩容、选主、故障转移或高可用。

项目只负责跟踪明确加入的 AUR 包、审查 AUR 包装层变化、构建获批包、远程传输产物、签名和发布仓库，以及有限失败重试。

**信任边界到审查批准为止。** 批准前，AUR 文件是不可信输入；批准后，同一 commit 的包进入 Build 时视为可信。Docker 只提供干净、可删除的构建环境，不承担 KVM 级强隔离，也不继续围绕“恶意 Build”扩建设施。

项目明确接受：Controller、单 Builder 和单 Publisher 是单点；Docker 与宿主共享内核；Publisher 失陷可能导致仓库签名能力失陷；diff-first 审查会继承上一批准版本中未发现的问题。不得用这些已接受风险反向引入 HA、KVM、自动密钥管理或取证平台。

审查目标只覆盖 AUR 包装层及其变更，不承诺完整审计所有上游源码、预编译二进制或安装后的行为。

## 2. 最小架构

```text
浏览器 ──HTTPS──> 公网 Controller（React Web / 调度）
                              │
                              └──> 3 low + 按需 high Runner
                                       │
                                credential gateway
                                       │
                                   Codex CLI

固定 Builder ──现有认证通道──> Controller ──现有发布通道──> 公网 Publisher
      │                                                     │
      └──> 一次性联网 Docker Build             GPG / repo-add / 公共只读仓库
```

部署继续保留现有 Controller、4 个固定 Runner、credential gateway、Builder 和 Publisher 服务边界；不得把它们折叠成一个新核心，也不得新建第二套并行拓扑。精简目标是删除这些服务内部及其协议中已经无用的通用 Worker、KVM、归档、能力协商和平台化逻辑，而不是重写已经通过真实环境验证的 Agent、认证、传输和发布路径。

## 3. 必须整簇删除的过度开发

| 现有机制 | 精简后的替代 |
| --- | --- |
| KVM、Guest Agent、Profile、overlay、Fetch/Build 双 Guest | 一个普通的一次性 Docker Build |
| 多 Builder/Publisher/Archiver 注册、角色、标签、能力、drain、probe、选主 | 配置中固定一个 Builder 和当前公网 Publisher |
| lease、heartbeat 状态机、协议协商、nonce、防重放签名、SignedEnvelope、签名 JobSpec | 一个 Builder secret、HTTPS、attempt ID 和幂等状态 |
| 通用 TransferCapability、双向 pull/push、跨角色路由 | Builder 单向 push 到固定 incoming 目录 |
| SSH 通用命令网关、Controller 与同机 Publisher 间 SSH、内部 backbone | SSH 只保留官方 `rrsync` 的 write-only 上传命令 |
| Fetch/Build 强制分离、Source Manifest、Dependency Snapshot、source proxy | 审查 AUR tree；获批后 Build 直接联网下载 |
| 离线 Build、出口代理、域名白名单、DNS rebinding、网络等级和流量 provenance | Docker 使用普通网络，不建设出口控制面 |
| pacoloco、缓存指标和 Profile 依赖优化器 | Builder 直接使用配置的 Arch 镜像；缓存由部署环境决定 |
| Signer 周边的通用 epoch、双重授权和无用 inbox/outbox 状态机 | 保留 Publisher 分机和现有签名隔离，在既有发布事务中收缩 |
| 完整 ReleaseEvidence、日志摘要链、Archive receipt/inventory、长期历史 | 保存必要审查结果、日志、当前和上一个完整仓库 |
| Runner 动态注册、Agent Doctor、成本预算、随机 high 抽查和通用适配器注册表 | 保留现有 4 Runner 与 credential gateway，只删除这些外围抽象；固定 3 low + 按需 high |
| React/Vite 中的 Worker/Profile/Archive/Settings 等无用页面、字段和 SSE 路径 | 保留现有 React 认证和管理界面，在原页面内删减 |
| 多管理员、RBAC、OAuth/OIDC、JWT、PAT、注册、邮件找回 | 一个本地初始化的管理员和服务端 Cookie 会话 |
| `aursmithctl` 的 Worker/Profile/Release/Archive/远程控制命令 | 只保留管理员初始化、改密和吊销 session 的本地命令 |
| ABI/官方依赖重建建议、alerts/events/outbox、Webhook/ntfy、通用指标 | 在包、任务或 keyring 状态上直接显示最后错误 |
| Archiver、控制面备份、恢复 API 和灾备协议 | SQLite、仓库、GPG key 交给普通宿主备份 |
| 已废弃的部署栈、重复 Caddy 拓扑和不再使用的云厂商分支 | 保留当前真实 netcup 部署、环境变量、secret 和已验证入口，删除其余重复实现 |
| 无调用的旧表、字段和迁移辅助抽象 | 保留现有迁移链和生产 Schema，在后续迁移中原位收缩 |

删除必须覆盖对应代码、配置、页面和测试中的旧假设，不能改成默认关闭的 feature、兼容层或“以后可能用”的抽象。不得删除仍被真实部署使用的 Controller、Runner、gateway、Builder、Publisher、迁移链、React 认证界面或其测试。

## 4. 软件包主流程

1. 管理员通过 Web 添加、暂停、恢复或删除 `pkgbase`。服务定期检查，也允许手工刷新。
2. 公网 AURsmith 拉取 AUR Git 并固定到精确 commit。宿主只读取 Git 文件，绝不执行或 `source` AUR 的 `PKGBUILD`。
3. 系统生成相对最后批准 commit 的完整 tree-to-tree diff，并执行少量确定性输入检查。
4. 三个 low 全部进入终态后，当前 commit 按固定 3+1 规则自动批准或进入人工审查；批准后生成一个 Build job 及其首个 attempt。
5. Builder 使用专用 HTTPS credential 轮询任务，下载获批 tree 归档并核对摘要。
6. Builder 在一次性 Docker 中直接联网构建，把预期 packages、Manifest 和日志留在任务目录。
7. Builder 沿用现有跨机认证和传输路径，把产物上传到 Publisher 分机的受限 incoming，并向 Controller 提交完成通知；精简时只删除通用能力协商和无用角色分支。
8. Publisher 按 Controller 已批准的 attempt 接管 partial，在私有 received 目录中核对 Manifest、文件 SHA-256 和 `.PKGINFO` 的名称、版本、架构及 split outputs。校验失败的内容隔离且不得发布。
9. Publisher 加入当前 `aursmith-keyring`，在 staging 中运行 GPG 和官方 `repo-add`，成功后原子切换 `current`；任意失败保持当前仓库不变。

第一版只支持 `x86_64`。同一 `pkgbase` 的全部 split outputs 一起构建。AUR 依赖只解析当前显式加入的包集合，使用 `.SRCINFO` 和简单拓扑排序；缺失依赖、provider 歧义或环由管理员调整配置，不建设通用依赖平台。

批准只绑定 `pkgbase + AUR commit`；tree hash 只用于发现 checkout 或传输错误。Provider、Model 和提示词记录在审计结果中，但不制造 Candidate、Bundle、Source Manifest 等多层身份。新 AUR commit 到达时，尚未获批的旧审查失效；已经获批并创建的 Build attempt 允许完成，新 commit 排队进入下一轮审查，不取消或混用结果。

## 5. diff-first 3+1 Agent 审查

更新包的 Agent 必须先看相对最后批准版本的 diff，再按需要读取当前完整文件。第一次加入包，或无法生成完整 diff 时，执行全量审查。

Agent 执行链完整保留重构前实现：Controller 创建 3 个 low 任务，必要时再创建 high 任务；固定 Runner 领取任务，经现有 credential gateway 获得对应 Provider 能力，并使用现有 Codex CLI argv、容器和结果文件约定完成审查。继续沿用旧 netcup 环境变量与四份 secret。不得改成 Controller 内联调用、手写 Provider HTTP、另一套工具协议、新 profile 或 adapter registry。

- baseline 只能是同一 `pkgbase` 最后一次 3+1 或人工批准的 commit，不能使用最近抓取、失败或拒绝的版本。
- diff 模式在既有任务工作区和 Codex 提示中强制先读取完整 diff，再按需查看当前文件；full 模式直接读取当前 tree。不得把整个包压成巨型纯文本请求，也不得改变既有工作区身份与结果协议。
- baseline 不存在、无法恢复或 diff 不能完整生成时自动改为 full；不得截断 diff 后自动批准。Git 是否 fast-forward 不影响 tree-to-tree diff。
- Agent 可以读、搜索、比较文件并写结果，但不能执行包内程序。包中的 `AGENTS.md`、注释和脚本都是待审数据，不是指令。
- Runner/Codex 工作区不得包含数据库、Docker Socket、GPG/SSH key；Provider credential 只经现有 credential gateway 注入既有调用边界，不得进入提示、结果或日志。
- 唯一权威输出是符合固定 Schema 的结果文件；自然语言、Markdown 和 stdout 不作 fallback。
- 不建设文件访问取证或“模型理解了全部文件”的 coverage 证明；`files_read` 自报只能帮助排错。
- 每个实际启用的既有 Runner/Provider 配置必须分别通过 diff 和 full 真实端到端测试。CLI 能启动或 Provider 可达不等于审查可用；修复兼容问题只能改现有提示、argv、结果校验和失败分类，不得另造协议。

投票规则保持简单：必须等待三个 low 全部终态；3/3 直接批准；恰好 2/3 调用一个 high；0–1/3 转人工。high 只有 `approve` 才批准，其余转人工。合法 `reject` 不重试但仍参与聚合；技术错误按重试规则处理。人工入口只在 low 聚合、high 非批准或技术错误耗尽后开放，不能在审查开始前直接绕过 3+1；人工决定绑定当前 commit 并填写理由。

## 6. 固定 Builder 与 Docker

Builder 不注册、不协商能力、不领取租约。重复轮询应返回同一个未结束 attempt；同一时间最多运行一个 Build。服务只记录 `last_poll_at` 供 Web 判断“最近是否有联系”，不扩展 online/degraded/draining 状态机。

Builder 重启后必须从本地状态恢复同一 attempt。若原容器已经不存在，应明确报告一次瞬时失败；服务按持久化预算为同一 job 显式创建下一 attempt，不能静默领取无关联的新任务。

Builder HTTPS credential 是一份部署 secret，只能调用轮询、输入下载、状态和完成通知接口；它不是可在 Web 创建或枚举的通用 API Token。上传 SSH key 只能写固定 incoming 目录，禁止 shell、PTY 和转发。两份凭据均不得进入 Build 容器。

获批 Build 直接使用普通 Docker 网络访问互联网，不增加代理、白名单、DNS 策略、联网开关、连接日志或第二套下载阶段。

Docker 模板只固定：每次使用全新容器和可写任务目录；`makepkg` 使用普通用户；不使用 privileged 或宿主集成；不挂 Docker Socket、真实密钥和无关目录；设置可配置总超时与日志上限；终态后清理容器；Docker 失败时绝不回退到宿主执行 AUR `PKGBUILD`。

CPU、内存和磁盘限制可以作为部署默认值，但不建设资源探测、配额、cgroup 合规或调度平台。

## 7. 远程接收、签名与 keyring

跨机接收只需要 `attempt ID + Manifest + 当前任务状态`。rsync 中断可继续同一 partial；完成通知后先把 partial 原子移出 SSH 可写目录，再执行校验。重复完成相同 Manifest 必须幂等成功，内容冲突必须停止。无需 TransferCapability、Controller Ed25519、writer epoch 或远程 ReleaseAuthorization。

Publisher 分机串行执行接收和发布：只接管预期普通包文件，重新计算 SHA-256 并核对包元数据；随后在同一文件系统 staging 中生成包签名、仓库数据库、数据库签名和一个小型 `repository-manifest.json`，最后只切换一个权威 `current` 指针。保留 `previous` 用于人工恢复，不建设完整 Release 历史或 Web 回滚平台。切换后，`current` 和 `previous` 数据库引用的包文件都必须继续通过原有公开 URL 读取，避免客户端先取得旧数据库、后下载旧包时失败。

仓库 GPG 私钥只存在于 Publisher 分机的既有签名隔离边界，不进入 Controller、Runner、gateway、Builder、Build 容器或 incoming 账户。发布必须显式指定配置的签名主指纹，不能从 keyring 中任取“第一个 key”。本项目接受 Publisher 失陷后攻击者可能取得签名能力；不得为了精简把签名合并进 Controller，也不恢复无用的双重授权平台。

`aursmith-keyring` 是必须长期保留的系统包：

- 包名保留，AUR 包不能覆盖；即使没有其他软件包，它也必须存在于仓库最终集合；
- 由 Publisher 使用固定可信模板生成，不进入 3+1 或远程 Builder；“宿主不得执行 PKGBUILD”只针对 AUR 输入；
- 包含 `aursmith.gpg`、`aursmith-trusted`、`aursmith-revoked`，安装/升级时执行 `pacman-key --populate aursmith`；
- 使用持久、单调递增的 generation 作为包版本来源；配置的刷新周期到期或信任集合变化时生成新 generation；同一次发布重试复用原 generation，只有 `current` 成功切换后才把它视为已发布；
- 允许没有普通 AUR 包变化的 keyring-only 发布，不再使用 `include_repository_keyring` 开关；
- 公网 Web 显示当前 generation、活动指纹、上次更新时间和下次到期时间。

首次客户端引导仍需从 Publisher 下载公钥，并通过独立可信渠道核对完整指纹，再执行 `pacman-key --add`、`--lsign-key` 和安装 `aursmith-keyring`。keyring 包不能自证首次信任。

正常换钥由管理员显式执行：先用旧钥签发同时包含旧/新钥的 keyring，确认经过客户端重叠期后再切换活动签名指纹，最后再移除或撤销旧钥。刷新周期和重叠期是部署配置，不建设自动轮换状态机。旧钥泄露时必须重新走带外人工信任，不能依赖旧钥签出的过渡包。

人工恢复 `previous` 不得降低已发布的 keyring generation；若要恢复旧软件集合，应继续带上当前 keyring 重新发布，而不是直接恢复已经过时的信任集合。

## 8. 公网 Web 与认证

管理 Web 必须通过 HTTPS 公网访问。保留当前 netcup Controller 入口、反向代理和已验证安全头边界；删除其他未使用或重复的部署拓扑，不为了“通用化”重写真实入口。

只保留一个管理员：

- 管理员首次创建、密码重置和 session 全部吊销只通过公网设备上的本地 CLI 完成；删除公网 setup/status、setup token 以及其余远程控制 CLI；
- 密码使用 Argon2id；登录后创建服务端不透明 session，数据库只保存 token 摘要；
- Cookie 使用 `__Host-` 前缀，不设置 `Domain`，并固定 `Secure`、`HttpOnly`、`SameSite=Strict`、`Path=/`，同时有服务端空闲和绝对过期；
- 已认证且有副作用的浏览器请求必须校验管理员 session、固定公开 Origin 和自定义 CSRF header；关闭 CORS，GET/HEAD 不得改变状态；
- 登录不要求已有 session，但必须校验固定公开 Origin 并做简单的有界节流；不增加永久锁号、CAPTCHA、邮件找回、MFA 或设备管理；
- 认证中间件默认保护全部管理 API，只有登录和明确列出的公共读取端点例外。

Builder 机器接口只接受独立的固定 Bearer secret，不接受浏览器 Cookie。公共 pacman 仓库、仓库公钥和签名只允许匿名 GET/HEAD；管理 API、Builder API 和仓库文件必须使用明确分离的路由边界。health、诊断和本机初始化不经公网代理。

Web 保留现有 React/Vite 认证与管理界面，在原组件、API 和测试中删减。最终只提供：登录/退出；包管理；完整 diff 与 3+1/人工决定；任务、Builder 最近联系、错误和日志；仓库与 keyring 状态。删除 Worker/Profile/Archive/成本/备份/需求总账等无用页面、字段和 SSE 路径；不得另写 SSR、原生 HTML/JS 前端或第二套 UI。

## 9. 失败分类与重试

| 类别 | 例子 | 处理 |
| --- | --- | --- |
| 瞬时外部失败 | AUR/Agent/镜像站暂时不可用，DNS/连接超时，HTTP 408/429/5xx，Docker daemon/image pull、HTTPS 或 rsync 暂时失败 | 当前阶段最多自动重试 2 次；耗尽后显示失败，允许人工重试同一 commit |
| 审查结论 | 确定性输入检查阻止、最终人工拒绝 | 不自动重试；管理员明确处理当前 commit 或等待新 commit |
| 确定性 Build 失败 | checksum/上游 PGP 不匹配，`prepare/build/check/package` 的编译、测试或打包错误，产物集合/元数据不符 | 不自动重试，也不允许手工重建同一 commit；仅新 AUR commit 解除 |
| 需管理员处理 | Web/Builder credential、Provider、权限、磁盘、镜像、依赖选择、仓库 GPG/keyring 或发布环境错误，以及无法可靠分类的错误 | 不自动重试；修复后可显式重试同一 commit |

构建器能明确识别的 DNS、HTTP 或下载器瞬时错误可以使用小型错误码/日志模式表分类，因为 Build 已可信且次数有上限；不为分类建设出口监控。

重试只重做失败阶段：上传失败不重新 Build；签名、keyring 或 `repo-add` 失败保留已验证产物，只重试发布。Builder 暂时没有轮询只是“等待 Builder”，不自动创建第二 attempt。attempt、Manifest 和完成通知必须幂等；服务重启不重置次数。禁止无限重试。

## 10. 状态与迁移

保留现有生产 SQLite Schema、迁移链和已经验证的打开/升级流程；不得新建 fresh-only 数据库、另起 core Schema 或只导出少数字段后重建。包、AUR commit 与批准 baseline、3+1 结果、Build attempt、重试、管理员认证、Builder/Publisher 状态和 keyring 查询继续在现有表中原位演进。文件系统 `current` 及其中的仓库 Manifest 继续作为已发布集合的权威，Builder 本地状态继续按现有恢复路径持久化。

删除某项功能时，先删除其代码调用、API、页面和测试依赖，再用正常前向 migration 收缩已经确定无用的字段或表。不得为了让新代码看起来干净而跳过旧 migration、拒绝现有生产库或维护新旧双 Schema。暂时仍被现有 Controller/Runner/Builder/Publisher 路径引用的 Worker、Job、attempt 或发布数据必须保留，直到调用方完成原位简化。

日志、AUR tree、partial、verified packages 和仓库仍放文件系统，数据库不新增大内容副本或新的 alerts/events/evidence 平台。现有数据升级必须保留显式包列表、暂停状态、批准 baseline、活动审查/任务、管理员与 session、当前仓库、GPG/keyring 以及恢复正在进行流程所需的状态；确实无法继续的状态必须通过明确 migration 或管理员可见错误处理，不能静默丢弃。

现有仓库保持可用，所有收缩修改必须在 staging 或真实部署副本验证迁移、跨机 Build、传输、keyring、签名和客户端安装后再进入生产。

## 11. 最低验收

1. 在真实 Controller、Builder 和 Publisher 部署上完成“新 commit → diff-first 3+1 → Builder 领取 → 联网 Docker Build → 既有传输路径 → Publisher 校验 → keyring/GPG/repo-add → pacman 安装/升级”。
2. 覆盖首次 full、正常 diff、baseline 丢失回退 full，以及 3/3、2/3+high、0–1/3/人工结果。
3. 每个启用 Agent 用真实 Provider 证明：diff 模式先读 diff 再读当前文件，full 模式直接读完整 tree，二者都写结果文件；纯文本、stdout-only、禁用文件读取和包内指令劫持不得成为有效审查。
4. 未登录管理请求失败；Cookie、过期、logout、Origin/CSRF 和登录节流生效；公共仓库/公钥可匿名读取，管理与 Builder API 不能匿名访问。
5. Builder 不能访问管理 API；浏览器 session 不能调用 Builder 接口；rsync key 不能执行 shell 或写出 incoming；两类凭据都不进入 Build。
6. rsync 中断可续传；完成后先脱离 SSH 可写目录再校验；相同完成通知幂等；冲突 Manifest 失败；上传或发布失败不重新 Build，也不破坏 `current`。
7. 构建一个 Build 阶段需要联网的真实包、一个 split package 和一组已选择的 AUR 依赖，不需要代理或域名策略。
8. 瞬时错误按预算重试；配置错误修复后可重试同一 commit；确定性 Build 失败只有新 AUR commit 才解除。
9. 首次指纹引导、正常 keyring 安装、定期 keyring-only generation 和一次计划换钥流程通过真实 pacman/GPG 验证。
10. `current/previous`、GPG 签名和客户端过渡读取验证通过，包括“读取旧数据库 → 切换 current → 下载旧包”的交错场景；相关单元、集成、真实 Docker 及两机部署测试全部通过。
11. 旧功能只有在被删除或由新闭环测试替代后才能删除旧测试，不得 skip 或弱化断言制造通过。

## 12. 防止再次过度开发

- 固定一套 Controller（含 4 Runner 与 gateway）、一台 Builder 和一台 Publisher；在现有配置和表中收缩动态注册、角色、能力和调度抽象，不新建平行的单值平台。
- 跨机只解决认证、续传、内容完整性和幂等；不要把 attempt ID 重新包装成 Capability、证据链或签名控制协议。
- 公网认证只服务一个管理员；不要因为 Web 公网化加入 IAM、IdP、PAT、MFA 生命周期或多前端 CORS。
- keyring 只解决 pacman 信任分发、周期刷新和显式换钥；不要扩展成自动 KMS、客户端升级追踪或紧急恢复平台。
- diff-first 依赖批准 baseline；不要为“证明 Agent 读过并理解全部文件”建设系统调用审计或 coverage 平台。
- 部署选择不得升级为产品协议；性能优化不得成为正确性前提；日志和摘要不得升级为取证平台。
- 新能力必须解决这个单管理员、固定 Controller/Builder/Publisher 部署已经出现的实际问题；“以后也许扩容”不是理由。
- 优先删除已确认无用的旧抽象；同时保留真实部署仍在使用的 Migration、API、Compose 服务和数据库状态，调用方原位迁移完成后再删除，不加 v2 或双轨兼容层。

完成重构意味着已确认删除的机制在代码、配置、页面、文档和测试中都不再存在，保留的 Controller→Runner→gateway→Codex、React Web、迁移链、Builder 和 Publisher 主路径已经通过真实验收；仅加禁用开关或用新实现包住旧实现都不算完成。
