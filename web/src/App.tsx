import { FormEvent, useEffect, useMemo, useState } from "react";
import { ApiError, Audit, AurPackage, ClientBootstrap, Doctor, Job, PackageDetail, Release, Session, Subscription, api } from "./api";

type View = "dashboard" | "packages" | "audits" | "builds" | "releases" | "client";

const navigation: Array<{ id: View; label: string }> = [
  { id: "dashboard", label: "总览" },
  { id: "packages", label: "软件包" },
  { id: "audits", label: "审查" },
  { id: "builds", label: "构建" },
  { id: "releases", label: "发布" },
  { id: "client", label: "客户端" }
];

export function App() {
  const [boot, setBoot] = useState<"loading" | "login" | "ready">("loading");
  const [session, setSession] = useState<Session | null>(null);
  const [view, setView] = useState<View>("dashboard");
  const [error, setError] = useState("");

  useEffect(() => {
    void api.me()
      .then((current) => { setSession(current); setBoot("ready"); })
      .catch((reason) => {
        if (!(reason instanceof ApiError && reason.status === 401)) setError(messageOf(reason));
        setBoot("login");
      });
  }, []);

  if (boot === "loading") return <LoadingScreen />;
  if (boot === "login") {
    return <LoginScreen initialError={error} onLogin={async () => {
      setSession(await api.me());
      setError("");
      setBoot("ready");
    }} />;
  }

  return <div className="shell">
    <aside className="sidebar">
      <Brand />
      <nav aria-label="主导航">{navigation.map((item) => <button className={view === item.id ? "nav-item active" : "nav-item"} key={item.id} onClick={() => setView(item.id)}><span>{item.label}</span></button>)}</nav>
      <div className="operator"><span className="status-dot" aria-hidden="true" /><div><strong>{session?.username}</strong><small>固定两机部署</small></div><button aria-label="退出登录" className="text-button" onClick={() => {
        setError("");
        void api.logout().then(() => { setSession(null); setBoot("login"); }).catch((reason) => {
          if (reason instanceof ApiError && reason.status === 401) { setSession(null); setBoot("login"); return; }
          setError(messageOf(reason));
        });
      }}>退出</button></div>
    </aside>
    <main className="workspace">
      {error && <Notice kind="error">{error}</Notice>}
      {view === "dashboard" && <Dashboard />}
      {view === "packages" && <PackagesView />}
      {view === "audits" && <AuditsView />}
      {view === "builds" && <BuildsView />}
      {view === "releases" && <ReleasesView />}
      {view === "client" && <ClientView />}
    </main>
  </div>;
}

function Brand() {
  return <div className="brand"><div className="brand-mark" aria-hidden="true"><span /></div><div><strong>AURsmith</strong><small>私有 AUR 构建</small></div></div>;
}

function usePolling(refresh: () => void) {
  useEffect(() => {
    refresh();
    const interval = window.setInterval(refresh, 15_000);
    return () => window.clearInterval(interval);
  }, [refresh]);
}

function Dashboard() {
  const [doctor, setDoctor] = useState<Doctor | null>(null);
  const [error, setError] = useState("");
  const refresh = useMemo(() => () => void api.doctor().then(setDoctor).catch((reason) => setError(messageOf(reason))), []);
  usePolling(refresh);
  return <>
    <header className="page-header"><div><p className="eyebrow">真实运行状态</p><h1>审查后再构建，签名后再发布</h1><p className="lede">固定公网服务和一台 Builder。页面每 15 秒读取一次权威状态，不维护实时事件副本。</p></div></header>
    {error && <Notice kind="error">{error}</Notice>}
    <ForgeRail />
    {doctor && <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">Doctor</p><h2>{doctor.ready ? "系统已具备运行条件" : "仍有检查未通过"}</h2></div><span className={`state ${doctor.ready ? "online" : "degraded"}`}>{doctor.ready ? "ready" : "attention"}</span></div><div className="finding-list">{doctor.checks.map((check) => <div key={check.id}><code>{check.ok ? "通过" : "失败"}</code><strong>{check.message}</strong></div>)}</div></section>}
  </>;
}

function ForgeRail() {
  const stages = [["同步", "固定 AUR commit"], ["审查", "3 low + 按需 high"], ["构建", "隔离 Docker"], ["发布", "GPG 与原子切换"]];
  return <section className="forge-rail" aria-label="软件包锻造流程">{stages.map(([name, detail], index) => <div className="forge-stage" key={name}><span className={index === 0 ? "rail-node hot" : "rail-node"} /><div><strong>{name}</strong><small>{detail}</small></div></div>)}</section>;
}

function BuildsView() {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [error, setError] = useState("");
  const [selectedLogs, setSelectedLogs] = useState<{ job_id: string; kind: string; sha256: string; document: unknown } | null>(null);
  const refresh = useMemo(() => () => void api.jobs().then((response) => setJobs(response.items)).catch((reason) => setError(messageOf(reason))), []);
  usePolling(refresh);
  const showLogs = async (job: Job) => {
    setError("");
    try { setSelectedLogs(await api.jobLogs(job.id)); } catch (reason) { setError(messageOf(reason)); }
  };
  return <>
    <header className="page-header compact"><div><p className="eyebrow">固定 Builder</p><h1>构建任务</h1><p className="lede">只显示 Build、attempt、有限重试、最后错误和有界日志。</p></div></header>
    {error && <Notice kind="error">{error}</Notice>}
    {selectedLogs && <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">有界构建日志</p><h2>{selectedLogs.job_id.slice(0, 12)}</h2></div><button className="text-button" onClick={() => setSelectedLogs(null)}>关闭</button></div><p className="panel-note">摘要 {selectedLogs.sha256}</p><pre><code>{JSON.stringify(selectedLogs.document, null, 2)}</code></pre></section>}
    <section className="table-panel"><div className="section-heading"><h2>任务队列</h2><button className="secondary-button" onClick={refresh}>刷新</button></div>{jobs.length === 0 ? <Empty title="没有构建任务" detail="加入软件包并批准审查后，Builder 会从这里领取任务。" /> : <div className="table-scroll"><table><thead><tr><th>任务</th><th>状态 / Attempt</th><th>Builder</th><th>Revision</th><th>更新时间</th></tr></thead><tbody>{jobs.map((job) => <tr key={job.id}><td><code>{job.id.slice(0, 8)}</code></td><td><span className={`state ${job.status}`}>{job.failure_code ?? job.status}</span><small className="cell-note">{job.attempt_count} 次{job.next_attempt_at ? ` · ${new Date(job.next_attempt_at).toLocaleTimeString("zh-CN")} 重试` : ""}</small></td><td>固定 Builder</td><td><code>{job.revision_sha256?.slice(0, 12) ?? "—"}</code></td><td>{new Date(job.updated_at).toLocaleString("zh-CN")}{job.has_logs && <small className="cell-note"><button className="text-button" onClick={() => void showLogs(job)}>查看日志</button></small>}</td></tr>)}</tbody></table></div>}</section>
  </>;
}

function PackagesView() {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<AurPackage[]>([]);
  const [subscriptions, setSubscriptions] = useState<Subscription[]>([]);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [detail, setDetail] = useState<PackageDetail | null>(null);
  const refresh = useMemo(() => () => void api.subscriptions().then((response) => setSubscriptions(response.items)).catch((reason) => setError(messageOf(reason))), []);
  usePolling(refresh);
  const search = async (event: FormEvent) => {
    event.preventDefault(); setError("");
    if (query.trim().length < 2) { setError("搜索词至少需要 2 个字符"); return; }
    setBusy("search");
    try { setResults((await api.searchAur(query.trim())).items); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  const operate = async (key: string, action: () => Promise<unknown>) => {
    setBusy(key); setError("");
    try { await action(); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  const showDetail = async (packageBase: string) => {
    setBusy(`detail-${packageBase}`); setError("");
    try { setDetail(await api.packageDetail(packageBase)); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  const selectProvider = async (dependencyName: string, selectedPackageBase: string) => {
    if (!detail) return;
    setBusy(`provider-${dependencyName}-${selectedPackageBase}`); setError("");
    try { await api.selectProvider(detail.package_base, dependencyName, selectedPackageBase); setDetail(await api.packageDetail(detail.package_base)); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  const setCheckPolicy = async (allowCheck: boolean) => {
    if (!detail) return;
    setBusy(`check-policy-${detail.package_base}`); setError("");
    try { await api.setBuildPolicy(detail.package_base, allowCheck); setDetail(await api.packageDetail(detail.package_base)); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  return <>
    <header className="page-header compact"><div><p className="eyebrow">pkgbase</p><h1>AUR 软件包</h1><p className="lede">只有加入与删除；依赖随根订阅自动加入，删除时同步清理不再可达的隐式依赖。</p></div></header>
    {error && <Notice kind="error">{error}</Notice>}
    <section className="search-panel"><form className="package-search" onSubmit={(event) => void search(event)}><label htmlFor="aur-query">搜索 AUR</label><div><input id="aur-query" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="例如 visual-studio-code-bin" /><button className="primary-button" disabled={busy === "search"}>{busy === "search" ? "查询中…" : "查询"}</button></div></form>{results.length > 0 && <div className="search-results">{results.map((item) => {
      const subscribed = subscriptions.some((subscription) => subscription.package_base === item.package_base && subscription.kind === "direct");
      return <article key={item.name} className="search-result"><div><div className="package-title"><strong>{item.name}</strong><code>{item.version}</code>{item.name !== item.package_base && <span>pkgbase {item.package_base}</span>}</div><p>{item.description ?? "没有描述"}</p><small>{item.maintainer ? `维护者 ${item.maintainer}` : "孤儿包"}{item.out_of_date ? " · 已标记过期" : ""}</small></div><button className="secondary-button" disabled={subscribed || busy === item.name} onClick={() => void operate(item.name, () => api.subscribe(item.name))}>{subscribed ? "已加入" : busy === item.name ? "解析依赖…" : "加入"}</button></article>;
    })}</div>}</section>
    <section className="table-panel"><div className="section-heading"><div><p className="eyebrow">订阅</p><h2>显式包与必要依赖</h2></div><button className="secondary-button" onClick={refresh}>刷新</button></div>{subscriptions.length === 0 ? <Empty title="尚未加入软件包" detail="从上方搜索 AUR，并加入需要的 pkgbase。" /> : <div className="table-scroll"><table><thead><tr><th>pkgbase</th><th>来源</th><th>版本 / outputs</th><th>引用</th><th /></tr></thead><tbody>{subscriptions.map((subscription) => <tr key={subscription.id}><td><strong>{subscription.package_base}</strong><small className="cell-note">{subscription.description}</small></td><td>{subscription.kind === "direct" ? "显式加入" : "必要依赖"}</td><td><code>{subscription.version ?? "等待同步"}</code><small className="cell-note">{subscription.outputs.join(" · ") || "—"}</small></td><td>{subscription.reference_count}</td><td><div className="row-actions"><button className="text-button" onClick={() => void showDetail(subscription.package_base)}>详情</button>{subscription.kind === "direct" && <button className="text-button" onClick={() => void operate(`refresh-${subscription.id}`, () => api.refreshPackage(subscription.package_base))}>检查更新</button>}{subscription.kind === "direct" && <button className="text-button danger" onClick={() => { if (window.confirm(`确认删除 ${subscription.package_base}？它及不再需要的依赖会在下一次发布中移出仓库。`)) void operate(`delete-${subscription.id}`, () => api.deleteSubscription(subscription.package_base)); }}>删除</button>}</div></td></tr>)}</tbody></table></div>}</section>
    {detail && <PackageDetailPanel detail={detail} busy={busy} close={() => setDetail(null)} operate={operate} selectProvider={selectProvider} setCheckPolicy={setCheckPolicy} />}
  </>;
}

function PackageDetailPanel({ detail, busy, close, operate, selectProvider, setCheckPolicy }: { detail: PackageDetail; busy: string; close: () => void; operate: (key: string, action: () => Promise<unknown>) => Promise<void>; selectProvider: (dependency: string, selected: string) => Promise<void>; setCheckPolicy: (allow: boolean) => Promise<void> }) {
  return <section className="work-panel"><div className="section-heading"><div><p className="eyebrow">pkgbase 详情</p><h2>{detail.package_base} · {detail.version}</h2></div><div className="row-actions"><button className="text-button" disabled={busy === `rebuild-${detail.package_base}`} onClick={() => void operate(`rebuild-${detail.package_base}`, () => api.rebuildPackage(detail.package_base))}>手工重建</button><button className="text-button" onClick={close}>关闭</button></div></div>
    <Notice kind="info">手工重建保持原版本和 pkgrel。客户端不会按版本比较自动升级；同名制品切换时，旧数据库与新文件可能短暂不一致。</Notice>
    <p>{detail.description ?? "没有描述"} · {detail.maintainer ? `维护者 ${detail.maintainer}` : "孤儿包"}</p>
    <h3>构建策略</h3><div className="finding-list"><div><code>check()</code><strong>{detail.build_policy.allow_check ? "默认执行" : "已显式禁用"}</strong><span><button className="text-button" disabled={busy === `check-policy-${detail.package_base}`} onClick={() => void setCheckPolicy(!detail.build_policy.allow_check)}>{detail.build_policy.allow_check ? "禁用 check()" : "恢复 check()"}</button></span></div></div>
    <h3>Revision</h3><div className="finding-list">{detail.revisions.map((revision) => <div key={revision.id}><code>{revision.release_state ?? revision.state}</code><strong>{revision.upstream_version} · {revision.aur_commit.slice(0, 12)}</strong><span>{revision.published_version ?? "尚未构建"}</span></div>)}</div>
    <h3>依赖解析</h3><div className="finding-list">{detail.dependency_resolution.map((dependency) => <div key={`${dependency.kind}-${dependency.name}`}><code>{dependency.kind}</code><strong>{dependency.name}</strong><span>{dependency.state === "needs_selection" ? dependency.candidates.map((candidate) => <button key={candidate} className="text-button" disabled={busy.startsWith(`provider-${dependency.name}-`)} onClick={() => void selectProvider(dependency.name, candidate)}>选择 {candidate}</button>) : dependency.target_package_base ?? dependency.state}</span></div>)}</div>
  </section>;
}

function AuditsView() {
  const [audits, setAudits] = useState<Audit[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [rationale, setRationale] = useState<Record<string, string>>({});
  const refresh = useMemo(() => () => void api.audits().then((response) => setAudits(response.items)).catch((reason) => setError(messageOf(reason))), []);
  usePolling(refresh);
  const decide = async (audit: Audit, approve: boolean) => {
    setError("");
    setBusy(audit.sha256);
    try { await api.decideAudit(audit.sha256, approve, rationale[audit.sha256] ?? ""); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  const retry = async (audit: Audit) => {
    setError("");
    setBusy(audit.sha256);
    try { await api.retryAudit(audit.sha256); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  return <><header className="page-header compact"><div><p className="eyebrow">diff-first 3+1</p><h1>审查</h1><p className="lede">报告只覆盖固定 AUR wrapper；上游下载内容未被审查时必须明确说明。</p></div></header>{error && <Notice kind="error">{error}</Notice>}<section className="audit-list">{audits.length === 0 ? <Empty title="没有审查任务" detail="新 AUR commit 固定后会自动生成审查输入。" /> : audits.map((audit) => <article className="audit-card" key={audit.sha256}><div className="audit-title"><div><p className="eyebrow">{audit.policy_version} · {audit.aur_commit.slice(0, 12)}</p><h2>{audit.package_base}</h2></div><span className={`state ${audit.state}`}>{audit.state}</span></div><p className="coverage-note">{audit.coverage.upstream_source?.statement}</p><h3>实际 Agent 运行</h3><div className="finding-list">{audit.runs.map((run) => <div key={`${run.tier}-${run.slot}-${run.attempt}`}><code>{run.tier} {run.slot} · attempt {run.attempt}</code><strong>{run.provider} / {run.model} · {run.verdict ?? run.status}</strong><span>{run.adapter} {run.adapter_version}{run.report?.summary ? ` · ${run.report.summary}` : ""}</span></div>)}</div><h3>确定性检查</h3><div className="finding-list">{audit.findings.length === 0 ? <p>确定性扫描未发现阻断或可疑项。</p> : audit.findings.map((finding, index) => <div key={`${finding.rule_id}-${index}`}><code>{finding.rule_id}</code><span>{finding.path}</span><strong>{finding.summary}</strong></div>)}</div>{audit.state === "manual_review" && <div className="manual-decision"><label>人工判断理由<input value={rationale[audit.sha256] ?? ""} onChange={(event) => setRationale((current) => ({ ...current, [audit.sha256]: event.target.value }))} placeholder="至少 8 个字符，只对当前 commit 有效" /></label><div><button className="secondary-button" disabled={busy === audit.sha256} onClick={() => void retry(audit)}>修复配置后重跑 3 个 low</button><button className="secondary-button" disabled={busy === audit.sha256} onClick={() => void decide(audit, true)}>批准当前 commit</button><button className="secondary-button danger" disabled={busy === audit.sha256} onClick={() => void decide(audit, false)}>拒绝当前 commit</button></div></div>}</article>)}</section></>;
}

function ReleasesView() {
  const [releases, setReleases] = useState<Release[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [rollbackMessage, setRollbackMessage] = useState("");
  const refresh = useMemo(() => () => void api.releases().then((response) => setReleases(response.items)).catch((reason) => setError(messageOf(reason))), []);
  usePolling(refresh);
  const rollback = async (release: Release) => {
    if (!window.confirm(`确认把服务端仓库恢复到 previous（${release.id.slice(0, 12)}）？已安装客户端不会自动降级。`)) return;
    setBusy(release.id); setError("");
    try { const result = await api.rollbackRelease(release.id); setRollbackMessage(`服务端已恢复到 ${result.release_id.slice(0, 12)}；客户端不会自动降级。`); refresh(); } catch (reason) { setError(messageOf(reason)); } finally { setBusy(""); }
  };
  return <>
    <header className="page-header compact"><div><p className="eyebrow">current / previous</p><h1>发布</h1><p className="lede">Publisher 在 staging 中签名并生成仓库数据库，校验完成后原子切换；只保留 current 和 previous。</p></div></header>
    {error && <Notice kind="error">{error}</Notice>}{rollbackMessage && <Notice kind="info">{rollbackMessage}</Notice>}
    <section className="table-panel"><div className="section-heading"><h2>仓库状态</h2><button className="secondary-button" onClick={refresh}>刷新</button></div>{releases.length === 0 ? <Empty title="尚无发布" detail="批准的构建产物完成签名和 repo-add 后会出现在这里。" /> : <div className="table-scroll"><table><thead><tr><th>Release</th><th>状态</th><th>包</th><th>Manifest</th><th>时间 / 操作</th></tr></thead><tbody>{releases.map((release, index) => <tr key={release.id}><td><strong>{index === 0 ? "current" : index === 1 ? "previous" : release.id.slice(0, 12)}</strong><small className="cell-note">{release.id.slice(0, 12)}</small></td><td><span className={`state ${release.state}`}>{release.state}</span></td><td>{release.artifact_count}</td><td><code>{release.manifest_sha256.slice(0, 16)}</code></td><td>{release.last_error ?? new Date(release.committed_at ?? release.created_at).toLocaleString("zh-CN")}{index === 1 && release.state === "committed" && <small className="cell-note"><button className="text-button" disabled={busy === release.id} onClick={() => void rollback(release)}>恢复 previous</button></small>}</td></tr>)}</tbody></table></div>}</section>
  </>;
}

function ClientView() {
  const [bootstrap, setBootstrap] = useState<ClientBootstrap | null>(null);
  const [error, setError] = useState("");
  useEffect(() => { void api.clientBootstrap().then(setBootstrap).catch((reason) => setError(messageOf(reason))); }, []);
  return <><header className="page-header compact"><div><p className="eyebrow">首次接入</p><h1>客户端</h1><p className="lede">先带外核对完整 GPG 指纹，再安装 keyring 和仓库配置。</p></div></header>{error && <Notice kind="error">{error}</Notice>}{bootstrap && <><section className="work-panel"><div className="section-heading"><div><p className="eyebrow">完整指纹</p><h2><code>{bootstrap.gpg_fingerprint}</code></h2></div></div><p>keyring generation：{bootstrap.keyring_generation ?? "等待首次发布"}</p>{bootstrap.keyring_published_at && <p>上次发布：{new Date(bootstrap.keyring_published_at).toLocaleString("zh-CN")}</p>}{bootstrap.keyring_next_due_at && <p>下次到期：{new Date(bootstrap.keyring_next_due_at).toLocaleString("zh-CN")}</p>}{bootstrap.warnings.map((warning) => <p key={warning}>{warning}</p>)}</section><section className="work-panel"><div className="section-heading"><div><p className="eyebrow">pacman.conf</p><h2>仓库配置</h2></div></div><pre><code>{bootstrap.repository_config}</code></pre><div className="finding-list">{bootstrap.commands.map((command) => <div key={command}><code>{command}</code></div>)}</div></section></>}</>;
}

function LoginScreen({ initialError, onLogin }: { initialError: string; onLogin: () => Promise<void> }) {
  const [username, setUsername] = useState("admin"); const [password, setPassword] = useState(""); const [error, setError] = useState(initialError);
  const submit = async (event: FormEvent) => { event.preventDefault(); setError(""); try { await api.login({ username, password }); await onLogin(); } catch (reason) { setError(messageOf(reason)); } };
  return <AuthFrame title="回到构建控制台" note="登录只管理私有仓库，不会远程操作 Arch 客户端。"><form onSubmit={(event) => void submit(event)}><Field label="管理员名称" value={username} onChange={setUsername} /><Field label="密码" type="password" value={password} onChange={setPassword} />{error && <Notice kind="error">{error}</Notice>}<button className="primary-button" type="submit">登录</button></form></AuthFrame>;
}

function AuthFrame({ title, note, children }: { title: string; note: string; children: React.ReactNode }) { return <main className="auth-page"><section className="auth-intro"><Brand /><div><p className="eyebrow">私有 AUR 二进制仓库</p><h1>每一个包，<br />先审查再安装。</h1><p>{note}</p></div><ForgeRail /></section><section className="auth-form"><div><h2>{title}</h2><p>固定两台设备，保持流程可理解。</p>{children}</div></section></main>; }
function Field({ label, value, onChange, type = "text" }: { label: string; value: string; onChange: (value: string) => void; type?: string }) { const id = useMemo(() => `field-${label}`, [label]); return <label className="field" htmlFor={id}><span>{label}</span><input id={id} type={type} value={value} onChange={(event) => onChange(event.target.value)} required /></label>; }
function Empty({ title, detail }: { title: string; detail: string }) { return <div className="empty-state"><span className="empty-symbol">◇</span><div><strong>{title}</strong><p>{detail}</p></div></div>; }
function Notice({ children, kind }: { children: React.ReactNode; kind: "error" | "info" }) { return <div className={`notice ${kind}`} role={kind === "error" ? "alert" : "status"}>{children}</div>; }
function LoadingScreen() { return <main className="loading"><Brand /><span className="loading-line" /><p>正在读取控制面状态…</p></main>; }
function messageOf(reason: unknown) { return reason instanceof ApiError || reason instanceof Error ? reason.message : "发生未知错误"; }
