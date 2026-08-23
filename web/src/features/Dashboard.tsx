import { ReactNode, useMemo } from "react";
import { api } from "../lib/api";
import { usePolling, useStable } from "../lib/hooks";
import { relativeTime, shortHash, formatCount } from "../lib/format";
import { Badge, Button, Card, CardBody, CardHead, CardTitle, PageHead, Stat, StatusBadge } from "../components/ui";
import { Pipeline } from "../components/Pipeline";
import { useToast } from "../components/Toast";
import { IconActivity, IconAlert, IconCheck, IconClock, IconHammer, IconPackage, IconRefresh, IconRocket, IconShield } from "../components/icons";

export function Dashboard() {
  const toast = useToast();
  const loadDoctor = useStable(() => api.doctor());
  const loadSubs = useStable(() => api.subscriptions());
  const loadJobs = useStable(() => api.jobs());
  const loadAudits = useStable(() => api.audits());
  const loadReleases = useStable(() => api.releases());

  const doctor = usePolling(loadDoctor, 15000);
  const subs = usePolling(loadSubs, 15000);
  const jobs = usePolling(loadJobs, 15000);
  const audits = usePolling(loadAudits, 15000);
  const releases = usePolling(loadReleases, 15000);

  const pkgs = subs.data?.items ?? [];
  const jobsList = jobs.data?.items ?? [];
  const auditsList = audits.data?.items ?? [];
  const releasesList = releases.data?.items ?? [];

  const directCount = pkgs.filter((p) => p.kind === "direct").length;
  const implicitCount = pkgs.filter((p) => p.kind === "implicit").length;
  const activeJobs = jobsList.filter((j) => ["queued", "no_eligible_worker", "dispatched", "running", "uncertain"].includes(j.status)).length;
  const queued = jobsList.filter((j) => j.status === "queued").length;
  const attention = auditsList.filter((a) => a.state === "manual_review").length;
  const activeAudits = auditsList.filter((a) => ["agent_pending", "agent_running"].includes(a.state)).length;
  const currentRelease = releasesList.find((r) => r.position === "current");
  const failedRelease = releasesList.find((r) => r.position === "failed");

  const stages = useMemo(
    () => [
      { name: "同步", detail: "固定 AUR commit", state: "done" as const },
      {
        name: "审查",
        detail: attention ? `${attention} 需人工处置` : activeAudits ? `${activeAudits} 正在运行` : "3+1 全部通过",
        state: attention || activeAudits ? ("active" as const) : ("done" as const)
      },
      {
        name: "构建",
        detail: activeJobs ? `${activeJobs} 个活动任务` : "队列空闲",
        state: activeJobs ? ("active" as const) : ("idle" as const)
      },
      {
        name: "发布",
        detail: currentRelease ? `current · ${currentRelease.artifact_count} 个包` : "等待首个发布",
        state: currentRelease ? ("done" as const) : ("idle" as const)
      }
    ],
    [attention, activeAudits, activeJobs, currentRelease]
  );

  const checks = doctor.data?.checks ?? [];
  const attentionBad = checks.some((c) => !c.ok);

  const activity = useMemo(() => {
    const items: Array<{ icon: ReactNode; title: string; sub: string; when: string }> = [];
    for (const a of auditsList) {
      items.push({
        icon: <IconShield size={15} />,
        title: `${a.package_base} · 包装层审查`,
        sub: a.state === "manual_review" ? "需人工处置" : a.state,
        when: a.created_at
      });
    }
    for (const j of jobsList.slice(0, 2)) {
      items.push({
        icon: <IconHammer size={15} />,
        title: `构建 ${shortHash(j.id, 6)}`,
        sub: j.status,
        when: j.updated_at
      });
    }
    items.sort((a, b) => new Date(b.when).getTime() - new Date(a.when).getTime());
    return items.slice(0, 5);
  }, [auditsList, jobsList]);

  const agentConfigs = useMemo(() => {
    const seen = new Set<string>();
    return auditsList
      .flatMap((audit) => audit.runs)
      .filter((run) => run.provider !== "unconfigured" && run.model !== "unconfigured")
      .filter((run) => {
        const key = `${run.tier}:${run.slot}:${run.provider}:${run.model}`;
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      })
      .slice(0, 4);
  }, [auditsList]);

  const loadError = doctor.error || subs.error || jobs.error || audits.error || releases.error;

  const refreshAll = () => {
    doctor.reload();
    subs.reload();
    jobs.reload();
    audits.reload();
    releases.reload();
    toast.success("已刷新", "所有数据源已重新读取权威 JSON。");
  };

  return (
    <>
      <PageHead
        eyebrow="真实运行状态"
        title="审查后再构建，签名后再发布"
        lede="固定一台公网服务和一台家庭 Builder，不维护实时事件副本，只按固定节奏读取权威状态。"
        actions={
          <Button variant="ghost" onClick={refreshAll} icon={<IconRefresh size={15} />}>
            刷新
          </Button>
        }
      />

      <div className="stack">
        {loadError && (
          <div className="notice notice--danger" role="alert">
            <IconAlert size={16} />
            <div><b>状态读取失败。</b> {loadError}</div>
          </div>
        )}

        <Card>
          <Pipeline stages={stages} />
        </Card>

        <div className="grid grid--stats">
          <Stat label="订阅 pkgbase" value={directCount} unit="显式" icon={<IconPackage size={17} />} foot={<span>含必要依赖 {implicitCount} 个</span>} />
          <Stat label="活动构建" value={activeJobs} unit="任务" icon={<IconHammer size={17} />} foot={<span>排队 {queued} · 见构建队列</span>} />
          <Stat
            label="待处置审查"
            value={attention}
            unit="项"
            icon={<IconShield size={17} />}
            foot={<span>{attention ? "需要人工裁决" : activeAudits ? "Agent 正在运行" : "审查流程健康"}</span>}
          />
          <Stat
            label="当前发布"
            value={currentRelease ? formatCount(currentRelease.artifact_count) : "—"}
            unit="包"
            icon={<IconRocket size={17} />}
            foot={<span>{currentRelease ? `manifest ${shortHash(currentRelease.manifest_sha256, 10)}` : failedRelease ? "发布失败" : "尚无发布"}</span>}
          />
        </div>

        <div className="dash-hero">
          <Card>
            <CardHead>
              <CardTitle eyebrow="Doctor" title={doctor.data?.ready ? "系统已具备运行条件" : "仍有检查未通过"} />
              <StatusBadge meta={{ tone: doctor.data?.ready ? "success" : "warning", label: doctor.data?.ready ? "ready" : "attention" }} />
            </CardHead>
            <CardBody>
              <div className="doctor-list">
                {doctor.loading && checks.length === 0 && Array.from({ length: 5 }).map((_, i) => <div className="doctor-row" key={i}><span className="skeleton" style={{ width: 180 }} /></div>)}
                {!doctor.loading && checks.length === 0 && <div className="doctor-row"><span className="u-muted">没有 Doctor 检查项</span></div>}
                {checks.map((check) => (
                  <div className="doctor-row" key={check.id}>
                    <div className="doctor-row__name">
                      <span className={`doctor-row__check ${check.ok ? "ok" : "bad"}`}>
                        {check.ok ? <IconCheck size={13} /> : <IconAlert size={13} />}
                      </span>
                      <span>{check.message}</span>
                    </div>
                    <span className="doctor-row__msg">{check.ok ? "OK" : "FAIL"}</span>
                  </div>
                ))}
              </div>
            </CardBody>
          </Card>

          <div className="stack">
            <Card>
              <CardHead>
                <CardTitle eyebrow="Agent 配置" title="实际扫描组合" />
              </CardHead>
              <CardBody>
                <div className="dash-status__ai">
                  {agentConfigs.map((run) => (
                    <span className="ai-chip" key={`${run.tier}-${run.slot}-${run.provider}-${run.model}`}>
                      <b>{run.tier === "high" ? "high" : `low ${run.slot}`}</b> {run.provider} · {run.model}
                    </span>
                  ))}
                  {agentConfigs.length === 0 && <span className="u-muted">尚无已执行的 Agent 配置</span>}
                </div>
                <p className="u-muted" style={{ marginTop: 12, fontSize: 12.5, lineHeight: 1.6 }}>
                  三个 low 独立配置，仅 diff-first 覆盖 AUR 包装层；上游下载内容不纳入审查范围。
                </p>
              </CardBody>
            </Card>

            <Card>
              <CardHead>
                <CardTitle eyebrow="最近动态" title="状态变更" />
              </CardHead>
              <CardBody>
                <div className="dash-recent">
                  {activity.map((a, i) => (
                    <div className="recent-row" key={i}>
                      <span className="stat__icon" style={{ width: 28, height: 28 }}>{a.icon}</span>
                      <div className="recent-row__main">
                        <strong>{a.title}</strong>
                        <span>{a.sub}</span>
                      </div>
                      <Badge tone="neutral">
                        <span className="row" style={{ gap: 4 }}><IconClock size={12} />{relativeTime(a.when)}</span>
                      </Badge>
                    </div>
                  ))}
                </div>
              </CardBody>
            </Card>
          </div>
        </div>

        {attentionBad && (
          <div className="notice notice--warning">
            <IconActivity size={16} />
            <div>存在失败的 Doctor 检查项，修复后可重试对应阶段；不影响 current 仓库。</div>
          </div>
        )}
      </div>
    </>
  );
}
