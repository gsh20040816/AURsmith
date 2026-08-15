import { FormEvent, useEffect, useMemo, useState } from "react";
import { Alert, ApiError, ArchiveCopy, ArchiveInventory, Audit, AurPackage, AuthorizedProfile, BuildProfile, ClientBootstrap, ControlPlaneBackup, Doctor, Job, PackageDetail, ProfileRecommendation, RebuildRecommendation, Release, ReleaseEvidence, Requirement, Session, Settings, Subscription, Worker, api } from "./api";

type View =
  | "dashboard"
  | "packages"
  | "audits"
  | "builds"
  | "workers"
  | "profiles"
  | "releases"
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
  { id: "alerts", label: "告警", requirement: "U03" },
  { id: "settings", label: "设置", requirement: "U02" }
];

export function App() {
  const [boot, setBoot] = useState<"loading" | "setup" | "login" | "ready">("loading");
  const [session, setSession] = useState<Session | null>(null);
  const [view, setView] = useState<View>("dashboard");
  const [error, setError] = useState("");
  const [liveVersion, setLiveVersion] = useState(0);
  const [liveState, setLiveState] = useState("等待实时连接");
  const [alerts, setAlerts] = useState<Alert[]>([]);

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

  useEffect(() => {
    if (boot !== "ready" || typeof EventSource === "undefined") return;
    const source = new EventSource("/api/v1/events");
    source.onopen = () => setLiveState("实时连接正常");
    source.onmessage = () => setLiveVersion((current) => current + 1);
    source.onerror = () => setLiveState("实时连接重试中");
    return () => source.close();
  }, [boot]);

  useEffect(() => {
    if (boot !== "ready") return;
    const refreshAlerts = () => void api.alerts()
      .then((response) => setAlerts(response.items))
      .catch(() => undefined);
    refreshAlerts();
    const interval = window.setInterval(refreshAlerts, 30_000);
    return () => window.clearInterval(interval);
  }, [boot]);

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

  const activeAlerts = alerts.filter((alert) => alert.state === "open");
  const leadingAlert = activeAlerts.find((alert) => alert.severity === "warning") ?? activeAlerts[0];
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
              <span className="nav-meta">{item.id === "alerts" && activeAlerts.length > 0 && <strong className="alert-count">{activeAlerts.length}</strong>}<code>{item.requirement}</code></span>
            </button>
          ))}
        </nav>
        <div className="operator">
          <span className="status-dot" aria-hidden="true" />
          <div>
            <strong>{session?.username}</strong>
            <small>{liveState}</small>
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
        {leadingAlert && view !== "alerts" && <section className={`global-alert ${leadingAlert.severity}`} role="alert"><div><strong>{leadingAlert.title}</strong><span>{lifecycleAlertSummary(leadingAlert)}</span></div><button className="secondary-button" onClick={() => setView("alerts")}>查看 {activeAlerts.length} 条待处理告警</button></section>}
        {view === "dashboard" && <Dashboard alerts={activeAlerts} onShowAlerts={() => setView("alerts")} />}
        {view === "workers" && <WorkersView />}
        {view === "builds" && <BuildsView liveVersion={liveVersion} />}
        {view === "packages" && <PackagesView />}
        {view === "audits" && <AuditsView />}
        {view === "profiles" && <ProfilesView />}
        {view === "releases" && <ReleasesView />}
        {view === "settings" && <SettingsView />}
        {view === "alerts" && <AlertsView />}
        {view !== "dashboard" && view !== "workers" && view !== "builds" && view !== "packages" && view !== "audits" && view !== "profiles" && view !== "releases" && view !== "settings" && view !== "alerts" && <PlannedView view={view} />}
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

function Dashboard({ alerts, onShowAlerts }: { alerts: Alert[]; onShowAlerts: () => void }) {
  const [requirements, setRequirements] = useState<Requirement[]>([]);
  const [workers, setWorkers] = useState<Worker[]>([]);
  const [error, setError] = useState("");
  const [doctor, setDoctor] = useState<Doctor | null>(null);

  useEffect(() => {
    void Promise.all([api.requirements(), api.workers()])
      .then(([requirementResponse, workerResponse]) => {
        setRequirements(requirementResponse.items);
        setWorkers(workerResponse.items);
      })
      .catch((reason) => setError(messageOf(reason)));
    void api.doctor()
      .then((response) => {
        if (Array.isArray(response.checks)) setDoctor(response);
      })
      .catch(() => setDoctor(null));
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
      {doctor && <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">Doctor</p><h2>{doctor.ready ? "系统已具备运行条件" : "仍有检查未通过"}</h2></div><span className={`state ${doctor.ready ? "online" : "degraded"}`}>{doctor.ready ? "ready" : "attention"}</span></div><div className="finding-list">{doctor.checks.map((check) => <div key={check.id}><code>{check.ok ? "通过" : "失败"}</code><strong>{check.message}</strong></div>)}</div></section>}
      <section className="dashboard-grid">
        <div className="work-panel">
          <div className="section-heading">
            <div><p className="eyebrow">待处理</p><h2>现在需要你的决定</h2></div>
            {alerts.length > 0 && <button className="text-button" onClick={onShowAlerts}>查看全部</button>}
          </div>
          {alerts.length === 0 ? <div className="empty-state">
            <span className="empty-symbol">✓</span>
            <div><strong>没有待处理项目</strong><p>出现 Provider 冲突或审计分歧时，会在这里说明原因和下一步。</p></div>
          </div> : <div className="finding-list">{alerts.slice(0, 4).map((alert) => <div key={alert.id}><code>{alert.severity}</code><strong>{alert.title}</strong><span>{lifecycleAlertSummary(alert)}</span></div>)}</div>}
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
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState({ name: "", role: "builder" as Worker["role"], mode: "reverse" as "direct" | "reverse", endpoint: "", hostKey: "", workerId: "", identityKey: "", labels: "" });
  const refresh = () => void api.workers().then((response) => setWorkers(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  const register = async (event: FormEvent) => {
    event.preventDefault(); setBusy(true); setError("");
    try {
      await api.registerWorker({
        name: draft.name.trim(), role: draft.role, endpoint: draft.endpoint.trim(),
        ssh_host_key_sha256: draft.hostKey.trim(), protocol_version: 1,
        connection_mode: draft.mode,
        worker_id: draft.mode === "reverse" ? draft.workerId.trim() : undefined,
        identity_signing_key_hex: draft.mode === "reverse" ? draft.identityKey.trim() : undefined,
        labels: draft.labels.split(",").map((label) => label.trim()).filter(Boolean)
      });
      setDraft({ name: "", role: "builder", mode: "reverse", endpoint: "", hostKey: "", workerId: "", identityKey: "", labels: "" });
      refresh();
    } catch (reason) { setError(messageOf(reason)); } finally { setBusy(false); }
  };
  return (
    <>
      <header className="page-header compact"><div><p className="eyebrow">W02 / W04</p><h1>Worker</h1><p className="lede">角色分离部署，任务在本地 Journal 中保持幂等。</p></div></header>
      {error && <Notice kind="error">{error}</Notice>}
      <section className="work-panel">
        <div className="section-heading"><div><p className="eyebrow">固定 Worker 身份</p><h2>注册 Worker</h2></div></div>
        <form className="worker-form" onSubmit={(event) => void register(event)}>
          <label>实例名称<input value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} placeholder="compute-01" required /></label>
          <label>角色<select value={draft.role} onChange={(event) => { const role = event.target.value as Worker["role"]; setDraft({ ...draft, role, mode: role === "builder" ? draft.mode : "direct" }); }}><option value="builder">Builder</option><option value="publisher">Publisher</option></select></label>
          <label>连接模式<select value={draft.mode} onChange={(event) => setDraft({ ...draft, mode: event.target.value as "direct" | "reverse" })}><option value="reverse" disabled={draft.role !== "builder"}>Builder 主动轮询</option><option value="direct">Controller 直连 SSH</option></select></label>
          {draft.mode === "direct" ? <>
            <label>SSH 端点<input value={draft.endpoint} onChange={(event) => setDraft({ ...draft, endpoint: event.target.value })} placeholder="ssh://aursmith@192.0.2.10:2222" required /></label>
            <label>SSH host key 指纹<input value={draft.hostKey} onChange={(event) => setDraft({ ...draft, hostKey: event.target.value })} placeholder="SHA256:…" required /></label>
          </> : <>
            <label>Builder 实例 UUID<input value={draft.workerId} onChange={(event) => setDraft({ ...draft, workerId: event.target.value })} placeholder="从本地 Worker status 复制" required /></label>
            <label>Builder 身份公钥<input value={draft.identityKey} onChange={(event) => setDraft({ ...draft, identityKey: event.target.value })} placeholder="64 位十六进制 Ed25519 公钥" required /></label>
          </>}
          <label>标签（逗号分隔）<input value={draft.labels} onChange={(event) => setDraft({ ...draft, labels: event.target.value })} placeholder="nvme,large-memory" /></label>
          <button className="secondary-button" disabled={busy}>{busy ? "正在探测…" : "探测并注册"}</button>
        </form>
        <p className="panel-note">家庭 Builder 使用主动轮询，不开放公网端口；实例 UUID 和身份公钥从本地 Worker status 复制。Publisher/Archiver 直连模式仍要求端点已写入 Controller known_hosts。</p>
      </section>
      <section className="table-panel">
        <div className="section-heading"><h2>已注册节点</h2><button className="secondary-button" onClick={refresh}>刷新</button></div>
        {workers.length === 0 ? (
          <div className="empty-state"><span className="empty-symbol">＋</span><div><strong>尚未注册 Worker</strong><p>先部署对应 Compose Stack，再固定 SSH host key 并注册端点。</p></div></div>
        ) : (
          <div className="table-scroll"><table><thead><tr><th>名称</th><th>角色</th><th>状态</th><th>端点</th><th>资源</th><th>标签</th><th /></tr></thead><tbody>
            {workers.map((worker) => <tr key={worker.id}><td><strong>{worker.name}</strong></td><td>{roleLabel(worker.role)}<small className="cell-note">{worker.connection_mode === "reverse" ? "主动轮询" : "SSH 直连"}</small></td><td><span className={`state ${worker.state}`}>{worker.state}</span></td><td>{worker.connection_mode === "reverse" ? "仅出站" : <code>{worker.endpoint}</code>}</td><td>{worker.storage ? `${worker.storage.available_percent}% 可用` : "等待上报"}<small className="cell-note">时钟 {worker.clock_skew_seconds ?? "?"} 秒</small></td><td>{worker.labels.join(" · ") || "—"}</td><td><div className="row-actions">{worker.connection_mode === "direct" && <button className="text-button" onClick={() => void api.probeWorker(worker.id).then(refresh).catch((reason) => setError(messageOf(reason)))}>探测</button>}{worker.state === "online" && <button className="text-button" onClick={() => void api.drainWorker(worker.id).then(refresh).catch((reason) => setError(messageOf(reason)))}>排空</button>}</div></td></tr>)}
          </tbody></table></div>
        )}
      </section>
    </>
  );
}

function BuildsView({ liveVersion }: { liveVersion: number }) {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [error, setError] = useState("");
  const [evidence, setEvidence] = useState<{ job_id: string; kind: string; sha256: string; document: unknown } | null>(null);
  const refresh = () => void api.jobs().then((response) => setJobs(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, [liveVersion]);
  const showEvidence = async (job: Job) => {
    setError("");
    try { setEvidence(await api.jobEvidence(job.id)); }
    catch (reason) { setError(messageOf(reason)); }
  };
  return <>
    <header className="page-header compact"><div><p className="eyebrow">W04 / B03</p><h1>构建任务</h1><p className="lede">Controller 签发 JobSpec；Worker Journal 拒绝冲突和迟到 Attempt。只有基础设施失败会自动重试两次。</p></div></header>
    {error && <Notice kind="error">{error}</Notice>}
    {evidence && <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">有界日志与 provenance</p><h2>{evidence.kind} · {evidence.job_id.slice(0, 12)}</h2></div><button className="text-button" onClick={() => setEvidence(null)}>关闭</button></div><p className="panel-note">证据摘要 {evidence.sha256}</p><pre><code>{JSON.stringify(evidence.document, null, 2)}</code></pre></section>}
    <section className="table-panel"><div className="section-heading"><h2>任务队列</h2><button className="secondary-button" onClick={refresh}>刷新</button></div>{jobs.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>没有可执行任务</strong><p>如果 Revision 显示“等待构建环境”，请先在 Profile 页面授权、验证并激活 Builder 已安装的 Profile。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>任务</th><th>阶段</th><th>状态 / Attempt</th><th>Worker</th><th>Revision</th><th>更新时间 / 证据</th></tr></thead><tbody>{jobs.map((job) => <tr key={job.id}><td><code>{job.id.slice(0, 8)}</code></td><td>{job.kind}</td><td><span className={`state ${job.status}`}>{job.failure_code ?? job.status}</span><small className="cell-note">{job.attempt_count} 次{job.next_attempt_at ? ` · ${new Date(job.next_attempt_at).toLocaleTimeString("zh-CN")} 重试` : ""}</small></td><td>{job.worker_name ?? "—"}</td><td><code>{job.revision_sha256?.slice(0, 12) ?? "—"}</code></td><td>{new Date(job.updated_at).toLocaleString("zh-CN")}{job.has_evidence && <small className="cell-note"><button className="text-button" onClick={() => void showEvidence(job)}>查看日志与证据</button></small>}</td></tr>)}</tbody></table></div>}</section>
  </>;
}

function PackagesView() {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<AurPackage[]>([]);
  const [subscriptions, setSubscriptions] = useState<Subscription[]>([]);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [detail, setDetail] = useState<PackageDetail | null>(null);
  const [rewriteRationale, setRewriteRationale] = useState("");
  const [rebuilds, setRebuilds] = useState<RebuildRecommendation[]>([]);
  const refresh = () => void Promise.all([api.subscriptions(), api.rebuildRecommendations()]).then(([subscriptionResponse, rebuildResponse]) => { setSubscriptions(subscriptionResponse.items); setRebuilds(rebuildResponse.items); }).catch((reason) => setError(messageOf(reason)));
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
  const showDetail = async (packageBase: string) => {
    setBusy(`detail-${packageBase}`); setError("");
    try { setDetail(await api.packageDetail(packageBase)); } catch (reason) { setError(messageOf(reason)); }
    finally { setBusy(""); }
  };
  const selectProvider = async (dependencyName: string, selectedPackageBase: string) => {
    if (!detail) return;
    const packageBase = detail.package_base;
    setBusy(`provider-${dependencyName}-${selectedPackageBase}`); setError("");
    try {
      await api.selectProvider(packageBase, dependencyName, selectedPackageBase);
      setDetail(await api.packageDetail(packageBase));
      refresh();
    } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  const setCheckPolicy = async (allowCheck: boolean) => {
    if (!detail) return;
    const packageBase = detail.package_base;
    setBusy(`check-policy-${packageBase}`); setError("");
    try {
      await api.setBuildPolicy(packageBase, allowCheck);
      setDetail(await api.packageDetail(packageBase));
    } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  const decideVcsRewrite = async (approve: boolean) => {
    if (!detail) return;
    const packageBase = detail.package_base;
    setBusy(`vcs-rewrite-${packageBase}`); setError("");
    try {
      await api.decideVcsRewrite(packageBase, approve, rewriteRationale);
      setDetail(await api.packageDetail(packageBase));
      setRewriteRationale("");
    } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  return <>
    <header className="page-header compact"><div><p className="eyebrow">P01 / P02 / P03 / P04</p><h1>AUR 软件包</h1><p className="lede">搜索在 Publisher 上执行；订阅会固定完整 pkgbase Git commit，并展开隐式 AUR 依赖。</p></div></header>
    {error && <Notice kind="error">{error}</Notice>}
    {detail?.vcs_rewrite_review?.state === "pending" && <section className="work-panel"><div className="manual-decision"><h2>Git VCS 历史重写待确认</h2><p className="panel-note">{detail.package_base} 的上一 commit {detail.vcs_rewrite_review.previous_commit.slice(0, 12)} 不在当前 {detail.vcs_rewrite_review.current_commit.slice(0, 12)} 的祖先链中。批准只对这次 commit 对有效，拒绝会继续阻止更新。</p><label>人工判断理由<input value={rewriteRationale} onChange={(event) => setRewriteRationale(event.target.value)} placeholder="至少 8 个字符" /></label><div><button className="secondary-button" disabled={busy === `vcs-rewrite-${detail.package_base}`} onClick={() => void decideVcsRewrite(true)}>批准本次重写</button><button className="secondary-button danger" disabled={busy === `vcs-rewrite-${detail.package_base}`} onClick={() => void decideVcsRewrite(false)}>拒绝本次重写</button></div></div></section>}
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
      {subscriptions.length === 0 ? <div className="empty-state"><span className="empty-symbol">＋</span><div><strong>尚未订阅软件包</strong><p>先部署并注册在线 Publisher，然后从上方搜索 AUR。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>pkgbase</th><th>来源</th><th>版本 / outputs</th><th>状态</th><th>引用</th><th /></tr></thead><tbody>{subscriptions.map((subscription) => <tr key={subscription.id}><td><strong>{subscription.package_base}</strong><small className="cell-note">{subscription.description}</small></td><td>{subscription.kind === "direct" ? "用户订阅" : "隐式依赖"}</td><td><code>{subscription.version ?? "等待同步"}</code><small className="cell-note">{subscription.outputs.join(" · ") || "—"}</small></td><td><span className={`state ${subscription.state}`}>{subscription.state}</span></td><td>{subscription.reference_count}</td><td><div className="row-actions"><button className="text-button" onClick={() => void showDetail(subscription.package_base)}>详情</button>{subscription.kind === "direct" && <button className="text-button" onClick={() => void operate(`refresh-${subscription.id}`, () => api.refreshPackage(subscription.package_base))}>检查</button>}{subscription.kind === "direct" && subscription.state === "active" && <button className="text-button" onClick={() => void operate(subscription.id, () => api.pauseSubscription(subscription.package_base))}>暂停</button>}{subscription.kind === "direct" && subscription.state === "paused" && <button className="text-button" onClick={() => void operate(subscription.id, () => api.resumeSubscription(subscription.package_base))}>恢复</button>}{subscription.kind === "direct" && <button className="text-button danger" onClick={() => void operate(subscription.id, () => api.unsubscribe(subscription.package_base))}>退订</button>}{subscription.kind === "direct" && <button className="text-button danger" onClick={() => void operate(`purge-${subscription.id}`, () => api.purgeSubscription(subscription.package_base))}>清除</button>}</div></td></tr>)}</tbody></table></div>}
    </section>
    {rebuilds.some((recommendation) => recommendation.state === "suggested") && <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">P07 · 保守 ABI 检测</p><h2>官方依赖变化，建议重建</h2></div></div><div className="finding-list">{rebuilds.filter((recommendation) => recommendation.state === "suggested").map((recommendation) => <div key={recommendation.package_base}><code>{recommendation.package_base}</code><strong>{recommendation.changes.map((change) => `${change.dependency} ${change.built_with} → ${change.current}`).join("；")}</strong><span><button className="text-button" onClick={() => void operate(`schedule-${recommendation.package_base}`, () => api.scheduleRebuildRecommendation(recommendation.package_base))}>立即重建</button><button className="text-button" onClick={() => void operate(`disable-${recommendation.package_base}`, () => api.disableRebuildRecommendation(recommendation.package_base))}>关闭该包建议</button></span></div>)}</div><p className="panel-note">版本变化只是一种保守信号，不能证明 ABI 已经不兼容；未处理建议在七天后合并为一个重建批次。</p></section>}
    {detail && <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">pkgbase 详情</p><h2>{detail.package_base} · {detail.version}</h2></div><div className="row-actions"><button className="text-button" disabled={busy === `rebuild-${detail.package_base}`} onClick={() => void operate(`rebuild-${detail.package_base}`, () => api.rebuildPackage(detail.package_base))}>手工重建</button><button className="text-button" onClick={() => setDetail(null)}>关闭</button></div></div><p>{detail.description ?? "没有描述"} · {detail.maintainer ? `维护者 ${detail.maintainer}` : "孤儿包"}</p><h3>构建策略</h3><div className="finding-list"><div><code>check()</code><strong>{detail.build_policy.allow_check ? "默认执行" : "已显式禁用"}</strong><span><button className="text-button" disabled={busy === `check-policy-${detail.package_base}`} onClick={() => void setCheckPolicy(!detail.build_policy.allow_check)}>{detail.build_policy.allow_check ? "禁用 check()" : "重新启用 check()"}</button></span></div></div>{!detail.build_policy.allow_check && <p className="panel-note">禁用 check() 会降低验证覆盖，且只影响后续新建 Job；该决定会写入 JobSpec、provenance 和事件日志。</p>}<h3>Revision 与 split outputs</h3><div className="finding-list">{detail.revisions.map((revision) => <div key={revision.id}><code>{revision.release_state ?? revision.state}</code><strong>{revision.upstream_version} · {revision.aur_commit.slice(0, 12)}</strong><span>{revision.release_state === "published" ? `仓库已发布 ${revision.published_version}` : revision.published_version ? `构建产物 ${revision.published_version}；尚未进入 pacman 数据库` : "尚未构建"}</span></div>)}</div><h3>依赖解析</h3><div className="finding-list">{detail.dependency_resolution.map((dependency) => <div key={`${dependency.kind}-${dependency.name}`}><code>{dependency.kind}</code><strong>{dependency.name}</strong><span>{dependency.state === "needs_selection" ? dependency.candidates.map((candidate) => <button key={candidate} className="text-button" disabled={busy.startsWith(`provider-${dependency.name}-`)} onClick={() => void selectProvider(dependency.name, candidate)}>选择 {candidate}</button>) : dependency.target_package_base ?? dependency.state}</span></div>)}</div><h3>上游与人工事件</h3><div className="finding-list">{detail.events.length === 0 ? <p>尚无事件。</p> : detail.events.map((event, index) => <div key={`${event.type}-${index}`}><code>{event.type}</code><strong>{new Date(event.created_at).toLocaleString("zh-CN")}</strong><span>{JSON.stringify(event.payload)}</span></div>)}</div></section>}
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
  const [recommendations, setRecommendations] = useState<ProfileRecommendation[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [authorized, setAuthorized] = useState<AuthorizedProfile | null>(null);
  const refresh = () => void Promise.all([api.profiles(), api.profileRecommendations()]).then(([profileResponse, recommendationResponse]) => { setProfiles(profileResponse.items); setRecommendations(recommendationResponse.items); }).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  const activate = async (profile: BuildProfile) => {
    setBusy(profile.id); setError("");
    try { await api.activateProfile(profile.id); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  const authorize = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault(); setError(""); setBusy("authorize"); setAuthorized(null);
    const input = event.currentTarget.elements.namedItem("candidate") as HTMLInputElement | null;
    const file = input?.files?.[0];
    if (!file) { setError("请选择 profile-candidate.json"); setBusy(""); return; }
    try {
      const candidate = JSON.parse(await file.text()) as unknown;
      setAuthorized(await api.authorizeProfile(candidate));
      refresh();
    } catch (reason) { setError(reason instanceof SyntaxError ? "Profile candidate 不是有效 JSON" : messageOf(reason)); }
    finally { setBusy(""); }
  };
  const downloadEnvelope = () => {
    if (!authorized) return;
    const url = URL.createObjectURL(new Blob([JSON.stringify(authorized.envelope, null, 2)], { type: "application/json" }));
    const link = document.createElement("a"); link.href = url; link.download = "profile-envelope.json"; link.click(); URL.revokeObjectURL(url);
  };
  return <><header className="page-header compact"><div><p className="eyebrow">B04</p><h1>构建 Profile</h1><p className="lede">Profile 是签名且不可变的 KVM 根文件系统；候选通过 fixture 验证后才能参与任务选择。</p></div></header>{error && <Notice kind="error">{error}</Notice>}<section className="work-panel"><div className="section-heading"><div><p className="eyebrow">一次性 profile-builder 输出</p><h2>授权 Profile candidate</h2></div></div><form className="worker-form" onSubmit={(event) => void authorize(event)}><label>profile-candidate.json<input name="candidate" type="file" accept="application/json,.json" required /></label><button className="secondary-button" disabled={busy === "authorize"}>{busy === "authorize" ? "正在授权…" : "提交并创建 fixture"}</button></form>{authorized && <div className="notice"><strong>Profile {authorized.profile_sha256.slice(0, 16)} 已授权。</strong><p>下载 Envelope，与三个构建文件一起放入 Builder 的同摘要 Profile 目录；fixture 成功后再激活。</p><button className="text-button" onClick={downloadEnvelope}>下载 profile-envelope.json</button></div>}</section><section className="table-panel"><div className="section-heading"><div><p className="eyebrow">最多四个活跃版本</p><h2>Profile 清单</h2></div><button className="secondary-button" onClick={refresh}>刷新</button></div>{profiles.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>尚无已授权 Profile</strong><p>运行一次性 profile-builder，随后在上方提交 candidate。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>名称</th><th>状态</th><th>摘要</th><th>包数量</th><th>验证</th><th /></tr></thead><tbody>{profiles.map((profile) => <tr key={profile.id}><td><strong>{profile.name}</strong><small className="cell-note">{profile.architecture}</small></td><td><span className={`state ${profile.state}`}>{profile.state}</span></td><td><code>{profile.profile_sha256.slice(0, 16)}</code></td><td>{profile.packages.length}</td><td>{profile.failure_reason ?? (profile.last_verified_at ? new Date(profile.last_verified_at).toLocaleString("zh-CN") : "等待 fixture")}</td><td>{profile.state !== "active" && <button className="text-button" disabled={busy === profile.id} onClick={() => void activate(profile)}>激活</button>}</td></tr>)}</tbody></table></div>}</section><section className="table-panel"><div className="section-heading"><div><p className="eyebrow">每七天评估 · 两周期加入 · 三周期移除</p><h2>官方构建依赖建议</h2></div></div>{recommendations.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>仍在观察</strong><p>前 20 次真实构建只统计，不会提前固化依赖。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>依赖</th><th>建议</th><th>最近 20 次</th><th>月使用</th><th>预计节省</th><th>连续周期</th></tr></thead><tbody>{recommendations.map((item) => <tr key={item.package_name}><td><strong>{item.package_name}</strong></td><td><span className={`state ${item.action}`}>{item.action}</span></td><td>{item.stats.uses_recent}</td><td>{item.stats.uses_this_month}</td><td>{item.stats.average_saved_seconds} 秒</td><td>热 {item.consecutive_hot_periods} / 冷 {item.consecutive_low_periods}</td></tr>)}</tbody></table></div>}</section></>;
}

function ReleasesView() {
  const [releases, setReleases] = useState<Release[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [downgrade, setDowngrade] = useState<{ release: string; commands: string[] } | null>(null);
  const [evidence, setEvidence] = useState<ReleaseEvidence | null>(null);
  const [evidenceRecord, setEvidenceRecord] = useState<unknown | null>(null);
  const refresh = () => void api.releases().then((response) => setReleases(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  const rollback = async (release: Release) => { if (!window.confirm(`确认把服务端仓库切换到 Release ${release.id}？客户端不会自动降级。`)) return; setBusy(release.id); setError(""); try { const result = await api.rollbackRelease(release.id); setDowngrade({ release: result.release_id, commands: result.pacman_commands }); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); } };
  const showEvidence = async (release: Release) => { setBusy(`evidence-${release.id}`); setError(""); setEvidenceRecord(null); try { setEvidence(await api.releaseEvidence(release.id)); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); } };
  return <>
    <header className="page-header compact"><div><p className="eyebrow">R01 / R02 / R03 / R04</p><h1>Release</h1><p className="lede">每条记录对应一个完整不可变仓库；Publisher 只在全部签名复验后切换当前数据库。</p></div></header>
    {error && <Notice kind="error">{error}</Notice>}
    {downgrade && <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">客户端不会自动降级</p><h2>Release {downgrade.release.slice(0, 12)} 已在服务端生效</h2></div></div><p>在每台已经安装较新版本的客户端显式执行：</p><div className="finding-list">{downgrade.commands.map((command) => <div key={command}><code>{command}</code></div>)}</div></section>}
    {evidence && <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">Controller + GPG 签名证据</p><h2>{evidence.evidence.records.length} 条证据记录</h2></div><button className="text-button" onClick={() => { setEvidence(null); setEvidenceRecord(null); }}>关闭</button></div><p className="panel-note">Authorization {evidence.authorization_sha256}</p><div className="finding-list">{evidence.evidence.records.map((record) => <div key={`${record.kind}-${record.identity}`}><code>{record.kind}</code><button className="text-button" onClick={() => setEvidenceRecord(record.document)}>{record.identity}</button><span>{record.sha256.slice(0, 16)}</span></div>)}</div>{evidenceRecord !== null && <div className="manual-decision"><div className="section-heading"><h3>证据文档</h3><button className="text-button" onClick={() => setEvidenceRecord(null)}>收起</button></div><pre><code>{JSON.stringify(evidenceRecord, null, 2)}</code></pre></div>}</section>}
    <section className="table-panel"><div className="section-heading"><div><p className="eyebrow">完整 Manifest</p><h2>发布历史</h2></div><button className="secondary-button" onClick={refresh}>刷新</button></div>{releases.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>尚无 Release</strong><p>ReleaseBatch 的全部 Artifact 传输并验证后会在这里进入签名与发布状态。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>Release</th><th>状态</th><th>包</th><th>Manifest</th><th>Writer</th><th>时间 / 操作</th></tr></thead><tbody>{releases.map((release) => <tr key={release.id}><td><strong>{release.id.slice(0, 12)}</strong><small className="cell-note">批次 {release.batch_id.slice(0, 12)}</small></td><td><span className={`state ${release.state}`}>{release.state}</span><small className="cell-note">{release.authorization_state ?? "等待授权"}</small></td><td>{release.artifact_count}</td><td><code>{release.manifest_sha256.startsWith("pending:") ? "等待 Signer" : release.manifest_sha256.slice(0, 16)}</code><small className="cell-note">源码 {release.source_git_commit.slice(0, 12)}</small></td><td>epoch {release.writer_epoch}</td><td>{release.last_error ?? new Date(release.committed_at ?? release.created_at).toLocaleString("zh-CN")}<small className="cell-note"><button className="text-button" disabled={busy === `evidence-${release.id}`} onClick={() => void showEvidence(release)}>查看证据</button>{release.state === "committed" && <button className="text-button" disabled={busy === release.id} onClick={() => void rollback(release)}>切换到此 Release</button>}</small></td></tr>)}</tbody></table></div>}</section>
  </>;
}

function ArchivesView() {
  const [archives, setArchives] = useState<ArchiveCopy[]>([]);
  const [inventories, setInventories] = useState<ArchiveInventory[]>([]);
  const [error, setError] = useState("");
  const refresh = () => void Promise.all([api.archives(), api.archiveInventories()]).then(([archiveResponse, inventoryResponse]) => { setArchives(archiveResponse.items); setInventories(inventoryResponse.items); }).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  return <><header className="page-header compact"><div><p className="eyebrow">R03</p><h1>归档</h1><p className="lede">ArchiveCopy 与 Release 独立推进；归档离线不会撤销已发布仓库。</p></div></header>{error && <Notice kind="error">{error}</Notice>}<section className="table-panel"><div className="section-heading"><div><p className="eyebrow">签名 Receipt</p><h2>归档副本</h2></div><button className="secondary-button" onClick={refresh}>刷新</button></div>{archives.length === 0 ? <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>尚无 ArchiveCopy</strong><p>Release 提交后，Archiver 会直接从 Publisher 拉取并核对完整文件集合。</p></div></div> : <div className="table-scroll"><table><thead><tr><th>Release</th><th>状态</th><th>Archiver</th><th>Receipt</th><th>更新时间 / 错误</th></tr></thead><tbody>{archives.map((archive) => <tr key={archive.id}><td><strong>{archive.release_id.slice(0, 12)}</strong><small className="cell-note">Manifest {archive.release_manifest_sha256.slice(0, 12)}</small></td><td><span className={`state ${archive.state}`}>{archive.state}</span></td><td>{archive.archiver_name ?? "等待调度"}</td><td><code>{archive.receipt_sha256?.slice(0, 16) ?? "等待验证"}</code></td><td>{archive.last_error ?? new Date(archive.updated_at).toLocaleString("zh-CN")}</td></tr>)}</tbody></table></div>}</section><section className="work-panel"><div className="section-heading"><div><p className="eyebrow">库存巡检</p><h2>每周集合 / 每季度完整摘要</h2></div></div>{inventories.length === 0 ? <p className="panel-note">尚无库存报告。</p> : <div className="finding-list">{inventories.map((inventory) => <div key={inventory.id}><code>{inventory.full_digest ? "完整摘要" : "集合与大小"}</code><strong>{inventory.archiver_name} · {inventory.release_count} Releases · {inventory.backup_count} 控制面备份 · {inventory.file_count} 文件</strong><span className={`state ${inventory.failure_count === 0 ? "online" : "degraded"}`}>{inventory.failure_count === 0 ? "通过" : `${inventory.failure_count} 失败`}</span></div>)}</div>}</section></>;
}

function SettingsView() {
  const [bootstrap, setBootstrap] = useState<ClientBootstrap | null>(null);
  const [backups, setBackups] = useState<ControlPlaneBackup[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const refreshBackups = () => void api.backups().then((response) => setBackups(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(() => { void Promise.all([api.clientBootstrap(), api.settings()]).then(([client, configuration]) => { setBootstrap(client); setSettings(configuration); }).catch((reason) => setError(messageOf(reason))); refreshBackups(); }, []);
  const createBackup = async () => { setError(""); try { await api.createBackup(); refreshBackups(); } catch (reason) { setError(messageOf(reason)); } };
  const verifyBackup = async (id: string) => { setError(""); try { await api.verifyBackup(id); refreshBackups(); } catch (reason) { setError(messageOf(reason)); } };
  const saveBudget = async (event: FormEvent<HTMLFormElement>) => { event.preventDefault(); if (!settings) return; setSaving(true); setError(""); try { setSettings(await api.updateSettings(settings.budget)); } catch (reason) { setError(messageOf(reason)); } finally { setSaving(false); } };
  const setBudget = (key: "agent_daily_call_limit" | "agent_monthly_call_limit" | "agent_monthly_cost_limit_microusd" | "agent_random_high_cost_review_basis_points", value: number) => setSettings((current) => current ? { ...current, budget: { ...current.budget, [key]: value } } : current);
  return <><header className="page-header compact"><div><p className="eyebrow">U02 · A03 · R03</p><h1>设置、客户端与备份</h1><p className="lede">运行时预算可在此修改；provider、Base URL 与 API key 通过 Compose 环境和 Docker secret 配置，密钥不会回显到控制面。</p></div></header>{error && <Notice kind="error">{error}</Notice>}{settings && <><section className="work-panel"><div className="section-heading"><div><p className="eyebrow">Codex / Claude Code</p><h2>Agent 与预算</h2></div><span className="state online">API key 不可见</span></div><p>{settings.agents.low_runner_count} 个低成本 Runner · 高成本 Runner {settings.agents.high_runner_configured ? "已配置" : "未配置"} · 支持 {settings.agents.supported_adapters.join(" / ")}</p><form className="package-search" onSubmit={(event) => void saveBudget(event)}><label htmlFor="daily-agent-limit">每日调用上限</label><input id="daily-agent-limit" type="number" min="0" value={settings.budget.agent_daily_call_limit} onChange={(event) => setBudget("agent_daily_call_limit", Number(event.target.value))} /><label htmlFor="monthly-agent-limit">每月调用上限</label><input id="monthly-agent-limit" type="number" min="0" value={settings.budget.agent_monthly_call_limit} onChange={(event) => setBudget("agent_monthly_call_limit", Number(event.target.value))} /><label htmlFor="monthly-agent-cost">每月成本上限（微美元）</label><input id="monthly-agent-cost" type="number" min="0" value={settings.budget.agent_monthly_cost_limit_microusd} onChange={(event) => setBudget("agent_monthly_cost_limit_microusd", Number(event.target.value))} /><label htmlFor="random-high-review">三票通过后的随机高成本复查（基点，100=1%）</label><input id="random-high-review" type="number" min="0" max="10000" value={settings.budget.agent_random_high_cost_review_basis_points} onChange={(event) => setBudget("agent_random_high_cost_review_basis_points", Number(event.target.value))} /><button className="primary-button" disabled={saving}>{saving ? "保存中…" : "保存预算"}</button></form><p className="panel-note">默认 0，不追加复查；启用后按 AuditBundle 摘要确定性抽样，命中项只有高成本 Agent 明确通过才放行。今日已调用 {settings.budget.daily_used}；本月 {settings.budget.monthly_used} 次、{settings.budget.monthly_cost_microusd} 微美元。provider/Base URL 修改后需重建或重启 Agent Stack；API key 只更新对应 secret。</p></section><section className="work-panel"><div className="section-heading"><div><p className="eyebrow">通知与保留</p><h2>部署状态</h2></div></div><div className="finding-list"><div><code>Webhook</code><strong>{settings.notifications.webhook_configured ? "已配置" : "未配置"}</strong></div><div><code>ntfy</code><strong>{settings.notifications.ntfy_configured ? "已配置" : "未配置"}</strong></div><div><code>Publisher</code><strong>30 天 / 每包至少 3 个版本</strong><span>{settings.repository.base_url}</span></div></div></section></>}{bootstrap && <><section className="work-panel"><div className="section-heading"><div><p className="eyebrow">完整指纹</p><h2><code>{bootstrap.gpg_fingerprint}</code></h2></div>{bootstrap.client_ca_url && <a className="secondary-button" href={bootstrap.client_ca_url} download>下载内部 CA</a>}</div>{bootstrap.warnings.map((warning) => <p key={warning}>{warning}</p>)}</section><section className="work-panel"><div className="section-heading"><div><p className="eyebrow">pacman.conf</p><h2>仓库配置</h2></div></div><pre><code>{bootstrap.repository_config}</code></pre><div className="finding-list">{bootstrap.commands.map((command) => <div key={command}><code>{command}</code></div>)}</div></section></>}<section className="work-panel"><div className="section-heading"><div><p className="eyebrow">控制面</p><h2>签名一致性备份</h2></div><button className="secondary-button" onClick={() => void createBackup()}>立即备份</button></div>{backups.length === 0 ? <p className="panel-note">尚无控制面备份。</p> : <div className="finding-list">{backups.map((backup) => <div key={backup.id}><code>{backup.state}</code><strong>{new Date(backup.created_at).toLocaleString("zh-CN")} · {backup.database_size ?? 0} 字节</strong><span className="state online">本机签名备份</span><button className="text-button" onClick={() => void verifyBackup(backup.id)}>复验</button></div>)}</div>}<p className="panel-note">恢复必须停止 Controller 后，在容器中执行 restore-control-plane；系统会保留被替换数据库。Controller 签名密钥和管理员恢复材料仍需离线备份。</p></section></>;
}

function AlertsView() {
  const [alerts, setAlerts] = useState<Alert[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const refresh = () => void api.alerts().then((response) => setAlerts(response.items)).catch((reason) => setError(messageOf(reason)));
  useEffect(refresh, []);
  const acknowledge = async (alert: Alert) => { setBusy(alert.id); setError(""); try { await api.acknowledgeAlert(alert.id); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); } };
  return <><header className="page-header compact"><div><p className="eyebrow">U03</p><h1>告警</h1><p className="lede">相同故障按稳定 fingerprint 去重；恢复检测会保留历史并把状态改为 resolved。</p></div></header>{error && <Notice kind="error">{error}</Notice>}<section className="audit-list">{alerts.length === 0 ? <div className="empty-state"><span className="empty-symbol">✓</span><div><strong>没有告警记录</strong><p>Worker、磁盘、时钟、传输、发布和归档异常会显示在这里。</p></div></div> : alerts.map((alert) => <article className="audit-card" key={alert.id}><div className="audit-title"><div><p className="eyebrow">{alert.severity} · {alert.fingerprint}</p><h2>{alert.title}</h2></div><span className={`state ${alert.state}`}>{alert.state}</span></div>{alert.fingerprint.startsWith("aur-lifecycle-missing:") && <Notice kind="error">当前仓库中的稳定版本会继续保留。请确认该包是否被删除、重命名或合并，并在订阅新包后退订旧包。</Notice>}<pre><code>{JSON.stringify(alert.details, null, 2)}</code></pre><p className="panel-note">首次发现：{new Date(alert.opened_at).toLocaleString("zh-CN")}</p>{alert.state === "open" && <button className="secondary-button" disabled={busy === alert.id} onClick={() => void acknowledge(alert)}>确认已知晓</button>}</article>)}</section></>;
}

function lifecycleAlertSummary(alert: Alert): string {
  if (alert.fingerprint.startsWith("aur-lifecycle-missing:")) {
    return "AUR 上游已不可见；当前已发布版本继续保留，请检查替代包并迁移订阅。";
  }
  return alert.fingerprint;
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
