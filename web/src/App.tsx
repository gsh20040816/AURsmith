import { FormEvent, useEffect, useMemo, useState } from "react";
import { ApiError, Audit, AurPackage, BuildProfile, Job, Release, Requirement, Session, Subscription, Worker, api } from "./api";

type View =
  | "dashboard"
  | "packages"
  | "audits"
  | "builds"
  | "workers"
  | "profiles"
  | "releases"
  | "archives"
  | "alerts"
  | "settings";

const navigation: Array<{ id: View; label: string; requirement: string }> = [
  { id: "dashboard", label: "总览", requirement: "U01" },
  { id: "packages", label: "软件包", requirement: "P01" },
  { id: "audits", label: "审计", requirement: "A01" },
  { id: "builds", label: "构建", requirement: "B01" },
  { id: "workers", label: "Worker", requirement: "W02" },
  { id: "profiles", label: "Profile", requirement: "B04" },
  { id: "releases", label: "Release", requirement: "R02" },
  { id: "archives", label: "归档", requirement: "R03" },
  { id: "alerts", label: "告警", requirement: "U03" },
  { id: "settings", label: "设置", requirement: "U02" }
];

export function App() {
  const [boot, setBoot] = useState<"loading" | "setup" | "login" | "ready">("loading");
  const [session, setSession] = useState<Session | null>(null);
  const [view, setView] = useState<View>("dashboard");
  const [error, setError] = useState("");

  useEffect(() => {
    void api.setupStatus()
      .then(async ({ initialized }) => {
        if (!initialized) {
          setBoot("setup");
          return;
        }
        try {
          const current = await api.me();
          setSession(current);
          setBoot("ready");
        } catch {
          setBoot("login");
        }
      })
      .catch((reason) => {
        setError(messageOf(reason));
        setBoot("login");
      });
  }, []);

  if (boot === "loading") {
    return <LoadingScreen />;
  }
  if (boot === "setup") {
    return <SetupScreen onComplete={() => setBoot("login")} />;
  }
  if (boot === "login") {
    return (
      <LoginScreen
        initialError={error}
        onLogin={async () => {
          const current = await api.me();
          setSession(current);
          setBoot("ready");
        }}
      />
    );
  }

  return (
    <div className="shell">
      <aside className="sidebar">
        <Brand />
        <nav aria-label="主导航">
          {navigation.map((item) => (
            <button
              className={view === item.id ? "nav-item active" : "nav-item"}
              key={item.id}
              onClick={() => setView(item.id)}
            >
              <span>{item.label}</span>
              <code>{item.requirement}</code>
            </button>
          ))}
        </nav>
        <div className="operator">
          <span className="status-dot" aria-hidden="true" />
          <div>
            <strong>{session?.username}</strong>
            <small>本地管理员</small>
          </div>
          <button
            aria-label="退出登录"
            className="text-button"
            onClick={() => void api.logout().finally(() => setBoot("login"))}
          >
            退出
          </button>
        </div>
      </aside>
      <main className="workspace">
        {view === "dashboard" && <Dashboard />}
        {view === "workers" && <WorkersView />}
        {view === "builds" && <BuildsView />}
        {view === "packages" && <PackagesView />}
        {view === "audits" && <AuditsView />}
        {view === "profiles" && <ProfilesView />}
        {view === "releases" && <ReleasesView />}
        {view !== "dashboard" && view !== "workers" && view !== "builds" && view !== "packages" && view !== "audits" && view !== "profiles" && view !== "releases" && <PlannedView view={view} />}
      </main>
    </div>
  );
}

function Brand() {
  return (
    <div className="brand">
      <div className="brand-mark" aria-hidden="true"><span /></div>
      <div>
        <strong>AURsmith</strong>
        <small>锻造控制台</small>
      </div>
    </div>
  );
}

function Dashboard() {
  const [requirements, setRequirements] = useState<Requirement[]>([]);
  const [workers, setWorkers] = useState<Worker[]>([]);
  const [error, setError] = useState("");

  useEffect(() => {
    void Promise.all([api.requirements(), api.workers()])
      .then(([requirementResponse, workerResponse]) => {
        setRequirements(requirementResponse.items);
        setWorkers(workerResponse.items);
      })
      .catch((reason) => setError(messageOf(reason)));
  }, []);

  const onlineWorkers = workers.filter((worker) => worker.state === "online").length;
  return (
    <>
      <header className="page-header">
        <div>
          <p className="eyebrow">当前锻造状态</p>
          <h1>从上游变化到可安装软件包</h1>
          <p className="lede">每一步都保留输入、决策和产物，失败不会覆盖当前稳定仓库。</p>
        </div>
        <div className="header-facts">
          <span><strong>{onlineWorkers}</strong> 在线 Worker</span>
          <span><strong>{requirements.length}</strong> 条需求受总账约束</span>
        </div>
      </header>
      {error && <Notice kind="error">{error}</Notice>}
      <ForgeRail />
      <section className="dashboard-grid">
        <div className="work-panel">
          <div className="section-heading">
            <div><p className="eyebrow">待处理</p><h2>现在需要你的决定</h2></div>
            <button className="text-button">查看全部</button>
          </div>
          <div className="empty-state">
            <span className="empty-symbol">✓</span>
            <div><strong>没有待处理项目</strong><p>出现 Provider 冲突或审计分歧时，会在这里说明原因和下一步。</p></div>
          </div>
        </div>
        <div className="ledger-panel">
          <p className="eyebrow">需求总账</p>
          <h2>实现不能悄悄丢项</h2>
          <div className="ledger-list">
            {requirements.slice(0, 8).map((requirement) => (
              <div key={requirement.id}><code>{requirement.id}</code><span>{requirement.title}</span></div>
            ))}
          </div>
          <p className="panel-note">API 直接读取代码中的规范列表；文档和测试将逐项核对。</p>
        </div>
      </section>
    </>
  );
}

function ForgeRail() {
  const stages = [
    ["同步", "等待订阅"],
    ["审计", "三 Agent 决策"],
    ["构建", "KVM 无网"],
    ["发布", "原子切换"],
    ["归档", "独立回执"]
  ];
  return (
    <section className="forge-rail" aria-label="软件包锻造流程">
      {stages.map(([name, detail], index) => (
        <div className="forge-stage" key={name}>
          <span className={index === 0 ? "rail-node hot" : "rail-node"} />
          <div><strong>{name}</strong><small>{detail}</small></div>
        </div>
      ))}
    </section>
  );
}

function WorkersView() {
  const [workers, setWorkers] = useState<Worker[]>([]);
  const [error, setError] = useState("");
  const refresh = () => void api.workers().then((response) => setWorkers(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  return (
    <>
      <header className="page-header compact"><div><p className="eyebrow">W02 / W04</p><h1>Worker</h1><p className="lede">角色分离部署，任务在本地 Journal 中保持幂等。</p></div></header>
      {error && <Notice kind="error">{error}</Notice>}
      <section className="table-panel">
        <div className="section-heading"><h2>已注册节点</h2><button className="secondary-button" onClick={refresh}>刷新</button></div>
        {workers.length === 0 ? (
          <div className="empty-state"><span className="empty-symbol">＋</span><div><strong>尚未注册 Worker</strong><p>先部署对应 Compose Stack，再固定 SSH host key 并注册端点。</p></div></div>
        ) : (
          <div className="table-scroll"><table><thead><tr><th>名称</th><th>角色</th><th>状态</th><th>端点</th><th>标签</th><th /></tr></thead><tbody>
            {workers.map((worker) => <tr key={worker.id}><td><strong>{worker.name}</strong></td><td>{roleLabel(worker.role)}</td><td><span className={`state ${worker.state}`}>{worker.state}</span></td><td><code>{worker.endpoint}</code></td><td>{worker.labels.join(" · ") || "—"}</td><td><div className="row-actions"><button className="text-button" onClick={() => void api.probeWorker(worker.id).then(refresh).catch((reason) => setError(messageOf(reason)))}>探测</button>{worker.state === "online" && <button className="text-button" onClick={() => void api.drainWorker(worker.id).then(refresh).catch((reason) => setError(messageOf(reason)))}>排空</button>}</div></td></tr>)}
          </tbody></table></div>
        )}
      </section>
    </>
  );
}

function BuildsView() {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [error, setError] = useState("");
  const refresh = () => void api.jobs().then((response) => setJobs(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  return <><header className="page-header compact"><div><p className="eyebrow">W04 / B03</p><h1>构建任务</h1><p className="lede">Controller 签发 JobSpec；Worker Journal 拒绝冲突和迟到 Attempt。</p></div></header>{error && <Notice kind="error">{error}</Notice>}<section className="table-panel"><div className="section-heading"><h2>任务队列</h2><button className="secondary-button" onClick={refresh}>刷新</button></div>{jobs.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>没有构建任务</strong><p>订阅产生通过审计的 Revision 后，完整依赖闭包会显示在这里。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>任务</th><th>角色</th><th>状态</th><th>Worker</th><th>Revision</th><th>更新时间</th></tr></thead><tbody>{jobs.map((job) => <tr key={job.id}><td><code>{job.id.slice(0, 8)}</code></td><td>{roleLabel(job.required_role)}</td><td><span className={`state ${job.status}`}>{job.failure_code ?? job.status}</span></td><td>{job.worker_name ?? "—"}</td><td><code>{job.revision_sha256?.slice(0, 12) ?? "—"}</code></td><td>{new Date(job.updated_at).toLocaleString("zh-CN")}</td></tr>)}</tbody></table></div>}</section></>;
}

function PackagesView() {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<AurPackage[]>([]);
  const [subscriptions, setSubscriptions] = useState<Subscription[]>([]);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const refresh = () => void api.subscriptions().then((response) => setSubscriptions(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  const search = async (event: FormEvent) => {
    event.preventDefault();
    setError("");
    if (query.trim().length < 2) {
      setError("搜索词至少需要 2 个字符");
      return;
    }
    setBusy("search");
    try {
      setResults((await api.searchAur(query.trim())).items);
    } catch (reason) {
      setError(messageOf(reason));
    } finally {
      setBusy("");
    }
  };
  const operate = async (key: string, action: () => Promise<unknown>) => {
    setBusy(key);
    setError("");
    try {
      await action();
      refresh();
    } catch (reason) {
      setError(messageOf(reason));
    } finally {
      setBusy("");
    }
  };
  return <>
    <header className="page-header compact"><div><p className="eyebrow">P01 / P02 / P03 / P04</p><h1>AUR 软件包</h1><p className="lede">搜索在 Publisher 上执行；订阅会固定完整 pkgbase Git commit，并展开隐式 AUR 依赖。</p></div></header>
    {error && <Notice kind="error">{error}</Notice>}
    <section className="search-panel">
      <form className="package-search" onSubmit={(event) => void search(event)}>
        <label htmlFor="aur-query">搜索 AUR</label>
        <div><input id="aur-query" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="例如 visual-studio-code-bin" /><button className="primary-button" disabled={busy === "search"}>{busy === "search" ? "查询中…" : "查询"}</button></div>
      </form>
      {results.length > 0 && <div className="search-results">{results.map((item) => {
        const subscribed = subscriptions.some((subscription) => subscription.package_base === item.package_base && subscription.kind === "direct");
        return <article key={item.name} className="search-result"><div><div className="package-title"><strong>{item.name}</strong><code>{item.version}</code>{item.name !== item.package_base && <span>pkgbase {item.package_base}</span>}</div><p>{item.description ?? "没有描述"}</p><small>{item.maintainer ? `维护者 ${item.maintainer}` : "孤儿包"}{item.out_of_date ? " · 已标记过期" : ""}</small></div><button className="secondary-button" disabled={subscribed || busy === item.name} onClick={() => void operate(item.name, () => api.subscribe(item.name))}>{subscribed ? "已订阅" : busy === item.name ? "解析依赖…" : "加入构建"}</button></article>;
      })}</div>}
    </section>
    <section className="table-panel"><div className="section-heading"><div><p className="eyebrow">订阅账本</p><h2>直接与隐式订阅</h2></div><button className="secondary-button" onClick={refresh}>刷新</button></div>
      {subscriptions.length === 0 ? <div className="empty-state"><span className="empty-symbol">＋</span><div><strong>尚未订阅软件包</strong><p>先部署并注册在线 Publisher，然后从上方搜索 AUR。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>pkgbase</th><th>来源</th><th>版本 / outputs</th><th>状态</th><th>引用</th><th /></tr></thead><tbody>{subscriptions.map((subscription) => <tr key={subscription.id}><td><strong>{subscription.package_base}</strong><small className="cell-note">{subscription.description}</small></td><td>{subscription.kind === "direct" ? "用户订阅" : "隐式依赖"}</td><td><code>{subscription.version ?? "等待同步"}</code><small className="cell-note">{subscription.outputs.join(" · ") || "—"}</small></td><td><span className={`state ${subscription.state}`}>{subscription.state}</span></td><td>{subscription.reference_count}</td><td><div className="row-actions">{subscription.kind === "direct" && <button className="text-button" onClick={() => void operate(`refresh-${subscription.id}`, () => api.refreshPackage(subscription.package_base))}>检查</button>}{subscription.kind === "direct" && subscription.state === "active" && <button className="text-button" onClick={() => void operate(subscription.id, () => api.pauseSubscription(subscription.package_base))}>暂停</button>}{subscription.kind === "direct" && subscription.state === "paused" && <button className="text-button" onClick={() => void operate(subscription.id, () => api.resumeSubscription(subscription.package_base))}>恢复</button>}{subscription.kind === "direct" && <button className="text-button danger" onClick={() => void operate(subscription.id, () => api.unsubscribe(subscription.package_base))}>退订</button>}</div></td></tr>)}</tbody></table></div>}
    </section>
  </>;
}

function AuditsView() {
  const [audits, setAudits] = useState<Audit[]>([]);
  const [error, setError] = useState("");
  const [rationale, setRationale] = useState<Record<string, string>>({});
  const refresh = () => void api.audits().then((response) => setAudits(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  const decide = async (audit: Audit, approve: boolean) => {
    setError("");
    try {
      await api.decideAudit(audit.sha256, approve, rationale[audit.sha256] ?? "");
      refresh();
    } catch (reason) {
      setError(messageOf(reason));
    }
  };
  return <><header className="page-header compact"><div><p className="eyebrow">A01 / A02 / A04</p><h1>审计</h1><p className="lede">确定性阻断、三低成本 Agent 投票和高成本复核均绑定不可变 AuditBundle。</p></div></header>{error && <Notice kind="error">{error}</Notice>}<section className="audit-list">{audits.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>没有审计任务</strong><p>订阅固定 Revision 后会自动生成覆盖范围明确的 AuditBundle。</p></div></div> : audits.map((audit) => <article className="audit-card" key={audit.sha256}><div className="audit-title"><div><p className="eyebrow">{audit.policy_version} · {audit.aur_commit.slice(0, 12)}</p><h2>{audit.package_base}</h2></div><span className={`state ${audit.state}`}>{audit.state}</span></div><p className="coverage-note">{audit.coverage.upstream_source?.statement}</p><div className="finding-list">{audit.findings.length === 0 ? <p>确定性扫描未发现阻断或可疑项。</p> : audit.findings.map((finding, index) => <div key={`${finding.rule_id}-${index}`}><code>{finding.rule_id}</code><span>{finding.path}</span><strong>{finding.summary}</strong></div>)}</div>{audit.state === "manual_review" && <div className="manual-decision"><label>人工判断理由<input value={rationale[audit.sha256] ?? ""} onChange={(event) => setRationale((current) => ({ ...current, [audit.sha256]: event.target.value }))} placeholder="至少 8 个字符，只对当前 Revision 有效" /></label><div><button className="secondary-button" onClick={() => void decide(audit, true)}>批准当前 Revision</button><button className="secondary-button danger" onClick={() => void decide(audit, false)}>拒绝当前 Revision</button></div></div>}</article>)}</section></>;
}

function ProfilesView() {
  const [profiles, setProfiles] = useState<BuildProfile[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const refresh = () => void api.profiles().then((response) => setProfiles(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  const activate = async (profile: BuildProfile) => {
    setBusy(profile.id); setError("");
    try { await api.activateProfile(profile.id); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  return <><header className="page-header compact"><div><p className="eyebrow">B04</p><h1>构建 Profile</h1><p className="lede">Profile 是签名且不可变的 KVM 根文件系统；候选通过 fixture 验证后才能参与任务选择。</p></div></header>{error && <Notice kind="error">{error}</Notice>}<section className="table-panel"><div className="section-heading"><div><p className="eyebrow">最多四个活跃版本</p><h2>Profile 清单</h2></div><button className="secondary-button" onClick={refresh}>刷新</button></div>{profiles.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>尚无已授权 Profile</strong><p>运行一次性 profile-builder，随后把 candidate 提交给 Controller 授权。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>名称</th><th>状态</th><th>摘要</th><th>包数量</th><th>验证</th><th /></tr></thead><tbody>{profiles.map((profile) => <tr key={profile.id}><td><strong>{profile.name}</strong><small className="cell-note">{profile.architecture}</small></td><td><span className={`state ${profile.state}`}>{profile.state}</span></td><td><code>{profile.profile_sha256.slice(0, 16)}</code></td><td>{profile.packages.length}</td><td>{profile.failure_reason ?? (profile.last_verified_at ? new Date(profile.last_verified_at).toLocaleString("zh-CN") : "等待 fixture")}</td><td>{profile.state !== "active" && <button className="text-button" disabled={busy === profile.id} onClick={() => void activate(profile)}>激活</button>}</td></tr>)}</tbody></table></div>}</section></>;
}

function ReleasesView() {
  const [releases, setReleases] = useState<Release[]>([]);
  const [error, setError] = useState("");
  const refresh = () => void api.releases().then((response) => setReleases(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  return <><header className="page-header compact"><div><p className="eyebrow">R01 / R02</p><h1>Release</h1><p className="lede">每条记录对应一个完整不可变仓库；Publisher 只在全部签名复验后切换当前数据库。</p></div></header>{error && <Notice kind="error">{error}</Notice>}<section className="table-panel"><div className="section-heading"><div><p className="eyebrow">完整 Manifest</p><h2>发布历史</h2></div><button className="secondary-button" onClick={refresh}>刷新</button></div>{releases.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>尚无 Release</strong><p>ReleaseBatch 的全部 Artifact 传输并验证后会在这里进入签名与发布状态。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>Release</th><th>状态</th><th>包</th><th>Manifest</th><th>Writer</th><th>时间 / 错误</th></tr></thead><tbody>{releases.map((release) => <tr key={release.id}><td><strong>{release.id.slice(0, 12)}</strong><small className="cell-note">批次 {release.batch_id.slice(0, 12)}</small></td><td><span className={`state ${release.state}`}>{release.state}</span><small className="cell-note">{release.authorization_state ?? "等待授权"}</small></td><td>{release.artifact_count}</td><td><code>{release.manifest_sha256.startsWith("pending:") ? "等待 Signer" : release.manifest_sha256.slice(0, 16)}</code><small className="cell-note">源码 {release.source_git_commit.slice(0, 12)}</small></td><td>epoch {release.writer_epoch}</td><td>{release.last_error ?? new Date(release.committed_at ?? release.created_at).toLocaleString("zh-CN")}</td></tr>)}</tbody></table></div>}</section></>;
}

function PlannedView({ view }: { view: View }) {
  const item = navigation.find((candidate) => candidate.id === view)!;
  const explanations: Record<View, string> = {
    dashboard: "",
    packages: "搜索 AUR、管理直接与隐式订阅，并解释依赖阻塞链。",
    audits: "查看确定性扫描、三个低成本 Agent 投票和人工处置记录。",
    builds: "跟踪 ReleaseBatch DAG、KVM 阶段日志和构建 provenance。",
    workers: "",
    profiles: "依据真实构建统计管理不可变 Guest Profile。",
    releases: "检查完整 Manifest、签名和当前仓库的原子切换记录。",
    archives: "查看 ArchiveCopy、空间背压、巡检和恢复入口。",
    alerts: "确认、追踪并解决去重后的系统告警。",
    settings: "管理 Agent、保留期、预算、通知和客户端接入。"
  };
  return (
    <><header className="page-header compact"><div><p className="eyebrow">{item.requirement}</p><h1>{item.label}</h1><p className="lede">{explanations[view]}</p></div></header><section className="work-panel"><div className="empty-state"><span className="empty-symbol">◇</span><div><strong>该纵向切片正在实现</strong><p>当前不会用静态假数据伪装功能完成；对应 API 和状态机落地后再开放操作。</p></div></div></section></>
  );
}

function SetupScreen({ onComplete }: { onComplete: () => void }) {
  const [token, setToken] = useState("");
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setError("");
    try { await api.setup({ token, username, password }); onComplete(); } catch (reason) { setError(messageOf(reason)); }
  };
  return <AuthFrame title="初始化锻造控制台" note="令牌只能通过 Controller 容器内命令读取。完成后它将失效。"><form onSubmit={(event) => void submit(event)}><Field label="初始化令牌" value={token} onChange={setToken} /><Field label="管理员名称" value={username} onChange={setUsername} /><Field label="密码" type="password" value={password} onChange={setPassword} hint="至少 12 个字符" />{error && <Notice kind="error">{error}</Notice>}<button className="primary-button" type="submit">创建管理员</button></form></AuthFrame>;
}

function LoginScreen({ initialError, onLogin }: { initialError: string; onLogin: () => Promise<void> }) {
  const [username, setUsername] = useState("admin"); const [password, setPassword] = useState(""); const [error, setError] = useState(initialError);
  const submit = async (event: FormEvent) => { event.preventDefault(); setError(""); try { await api.login({ username, password }); await onLogin(); } catch (reason) { setError(messageOf(reason)); } };
  return <AuthFrame title="回到锻造控制台" note="登录只管理仓库控制面，不会远程操作你的 Arch 客户端。"><form onSubmit={(event) => void submit(event)}><Field label="管理员名称" value={username} onChange={setUsername} /><Field label="密码" type="password" value={password} onChange={setPassword} />{error && <Notice kind="error">{error}</Notice>}<button className="primary-button" type="submit">登录</button></form></AuthFrame>;
}

function AuthFrame({ title, note, children }: { title: string; note: string; children: React.ReactNode }) {
  return <main className="auth-page"><section className="auth-intro"><Brand /><div><p className="eyebrow">私有 AUR 二进制仓库</p><h1>每一个包，<br />都有来路。</h1><p>{note}</p></div><ForgeRail /></section><section className="auth-form"><div><h2>{title}</h2><p>所有操作都会写入不可变事件记录。</p>{children}</div></section></main>;
}

function Field({ label, value, onChange, type = "text", hint }: { label: string; value: string; onChange: (value: string) => void; type?: string; hint?: string }) {
  const id = useMemo(() => `field-${label}`, [label]);
  return <label className="field" htmlFor={id}><span>{label}</span><input id={id} type={type} value={value} onChange={(event) => onChange(event.target.value)} required />{hint && <small>{hint}</small>}</label>;
}

function Notice({ children, kind }: { children: React.ReactNode; kind: "error" | "info" }) { return <div className={`notice ${kind}`} role={kind === "error" ? "alert" : "status"}>{children}</div>; }
function LoadingScreen() { return <main className="loading"><Brand /><span className="loading-line" /><p>正在读取控制面状态…</p></main>; }
function messageOf(reason: unknown) { return reason instanceof ApiError || reason instanceof Error ? reason.message : "发生未知错误"; }
function roleLabel(role: Worker["role"]) { return { builder: "Builder", publisher: "Publisher", archiver: "Archiver" }[role]; }
