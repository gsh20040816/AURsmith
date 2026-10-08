import { ReactNode, useMemo } from "react";
import { api } from "../lib/api";
import { usePolling, useStable } from "../lib/hooks";
import { relativeTime, shortHash, formatCount } from "../lib/format";
import { statusMeta } from "../lib/status";
import { Badge, Button, Card, CardBody, CardHead, CardTitle, PageHead, Stat, StatusBadge } from "../components/ui";
import { Pipeline } from "../components/Pipeline";
import { useToast } from "../components/Toast";
import { IconAlert, IconCheck, IconClock, IconHammer, IconPackage, IconRefresh, IconRocket, IconShield } from "../components/icons";

export function Dashboard() {
  const toast = useToast();
  const loadStatus = useStable(() => api.status());
  const loadReviews = useStable(() => api.reviews());
  const loadBuilds = useStable(() => api.builds());
  const loadPublications = useStable(() => api.publications());

  const status = usePolling(loadStatus, 15000);
  const reviews = usePolling(loadReviews, 15000);
  const builds = usePolling(loadBuilds, 15000);
  const publications = usePolling(loadPublications, 15000);

  const counts = status.data?.counts;
  const reviewList = reviews.data?.items ?? [];
  const buildList = builds.data?.items ?? [];
  const publicationList = publications.data?.items ?? [];
  const desired = publications.data?.desired;

  const attention = counts?.manual_review ?? 0;
  const pendingReview = counts?.pending_review ?? 0;
  const activeBuilds = (counts?.queued ?? 0) + (counts?.running ?? 0);
  const current = publicationList.find((p) => p.current);
  const latest = publicationList[0];
  const converged = !!current && !!desired && current.plan_sha256 === desired.plan_sha256;

  const stages = useMemo(
    () => [
      { name: "同步", detail: "固定 AUR commit", state: "done" as const },
      {
        name: "审查",
        detail: attention ? `${attention} 需人工审批` : pendingReview ? `${pendingReview} 正在 2+1 审查` : "无待审 revision",
        state: attention || pendingReview ? ("active" as const) : ("done" as const)
      },
      {
        name: "构建",
        detail: activeBuilds ? `${activeBuilds} 个活动构建` : "队列空闲",
        state: activeBuilds ? ("active" as const) : ("idle" as const)
      },
      {
        name: "发布",
        detail: converged ? `已收敛 · ${current?.artifacts.length ?? 0} 个包` : current ? "等待收敛到期望状态" : "等待首个发布",
        state: converged ? ("done" as const) : current ? ("active" as const) : ("idle" as const)
      }
    ],
    [attention, pendingReview, activeBuilds, converged, current]
  );

  const checks = status.data?.checks ?? [];
  const orderedChecks = [...checks.filter((c) => !c.ok), ...checks.filter((c) => c.ok)];
  const failedChecks = checks.filter((c) => !c.ok);

  const alerts = useMemo(() => {
    const items: Array<{ tone: "warning" | "danger"; text: string }> = [];
    if (attention > 0) items.push({ tone: "warning", text: `${attention} 个 revision 等待人工审批` });
    if (latest?.state === "failed") items.push({ tone: "danger", text: `发布失败：${latest.error ?? latest.plan_sha256}` });
    if ((desired?.withheld.length ?? 0) > 0) items.push({ tone: "warning", text: `暂缓发布：${desired?.withheld.join(", ")}` });
    return items;
  }, [attention, latest, desired]);

  const activity = useMemo(() => {
    const items: Array<{ icon: ReactNode; title: string; sub: string; when: string }> = [];
    for (const r of reviewList.slice(0, 5)) {
      items.push({ icon: <IconShield size={15} />, title: `${r.package_base} ${r.version}`, sub: statusMeta(r.state).label, when: r.decided_at ?? r.created_at });
    }
    for (const b of buildList.slice(0, 5)) {
      items.push({ icon: <IconHammer size={15} />, title: `${b.package_base} 构建`, sub: statusMeta(b.state).label, when: b.finished_at ?? b.started_at ?? b.created_at });
    }
    for (const p of publicationList.slice(0, 3)) {
      items.push({ icon: <IconRocket size={15} />, title: `发布 ${shortHash(p.plan_sha256, 8)}`, sub: statusMeta(p.state).label, when: p.finished_at ?? p.created_at });
    }
    items.sort((a, b) => new Date(b.when).getTime() - new Date(a.when).getTime());
    return items.slice(0, 6);
  }, [reviewList, buildList, publicationList]);

  const loadError = status.error || reviews.error || builds.error || publications.error;

  const refreshAll = () => {
    status.reload();
    reviews.reload();
    builds.reload();
    publications.reload();
    toast.success("已刷新");
  };

  return (
    <>
      <PageHead
        title="总览"
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
            <div>
              <b>状态读取失败。</b> {loadError}
            </div>
          </div>
        )}

        {alerts.length > 0 && (
          <div className="dash-attention">
            {alerts.map((a) => (
              <div className={`notice notice--${a.tone}`} key={a.text}>
                <IconAlert size={16} />
                <div>{a.text}</div>
              </div>
            ))}
          </div>
        )}

        <Card>
          <Pipeline stages={stages} />
        </Card>

        <div className="grid grid--stats">
          <Stat
            label="待人工审批"
            value={attention}
            unit="项"
            icon={<IconShield size={17} />}
            foot={<span>{attention ? "首次添加或 Agent 未通过" : pendingReview ? "Agent 正在审查" : "无待办"}</span>}
          />
          <Stat
            label="活动构建"
            value={activeBuilds}
            unit="个"
            icon={<IconHammer size={17} />}
            foot={<span>近 24 小时失败 {counts?.failed ?? 0}</span>}
          />
          <Stat
            label="仓库包数"
            value={current ? formatCount(current.artifacts.length) : counts?.subscriptions || "—"}
            unit="包"
            icon={current ? <IconRocket size={17} /> : <IconPackage size={17} />}
            foot={<span>{current ? `plan ${shortHash(current.plan_sha256, 10)}` : "尚无发布"}</span>}
          />
        </div>

        <div className="dash-hero">
          <Card>
            <CardHead>
              <CardTitle title={status.data?.ready ? "系统状态：就绪" : `系统状态：${failedChecks.length || "—"} 项异常`} />
              <StatusBadge meta={status.data?.ready ? statusMeta("ready") : { tone: "warning", label: "未就绪" }} />
            </CardHead>
            <CardBody>
              <div className="check-list">
                {status.loading && checks.length === 0 &&
                  Array.from({ length: 4 }).map((_, i) => (
                    <div className="check-row" key={i}>
                      <span className="skeleton" style={{ width: 180 }} />
                    </div>
                  ))}
                {orderedChecks.map((check) => (
                  <div className={`check-row ${check.ok ? "is-ok" : "is-bad"}`} key={check.id}>
                    <div className="check-row__name">
                      <span className={`check-row__check ${check.ok ? "ok" : "bad"}`}>
                        {check.ok ? <IconCheck size={13} /> : <IconAlert size={13} />}
                      </span>
                      <span>{check.message}</span>
                    </div>
                    <span className="check-row__msg">{check.ok ? "OK" : "FAIL"}</span>
                  </div>
                ))}
              </div>
            </CardBody>
          </Card>

          <Card>
            <CardHead>
              <CardTitle title="最近动态" />
            </CardHead>
            <CardBody>
              <div className="dash-recent">
                {activity.length === 0 && <div className="u-muted" style={{ fontSize: 13 }}>暂无动态</div>}
                {activity.map((a, i) => (
                  <div className="recent-row" key={i}>
                    <span className="stat__icon" style={{ width: 28, height: 28 }}>
                      {a.icon}
                    </span>
                    <div className="recent-row__main">
                      <strong>{a.title}</strong>
                      <span>{a.sub}</span>
                    </div>
                    <Badge tone="neutral">
                      <span className="row" style={{ gap: 4 }}>
                        <IconClock size={12} />
                        {relativeTime(a.when)}
                      </span>
                    </Badge>
                  </div>
                ))}
              </div>
            </CardBody>
          </Card>
        </div>

        <details className="dash-agent-details">
          <summary>审查规则（2+1）</summary>
          <div className="dash-agent-body">
            <p className="u-muted" style={{ fontSize: 12.5, lineHeight: 1.7 }}>
              两个低档 Agent 独立审查固定 AUR 包装层（非首次时附带与上一已批准 revision 的 diff）。两者都批准即通过；任一拒绝、意见不一致或输出无效时交给高档 Agent；高档批准即通过，拒绝或出错进入人工审批。首次添加的软件包始终需要人工审批，Agent 结论仅作参考。上游下载内容不纳入审查范围。
            </p>
          </div>
        </details>
      </div>
    </>
  );
}
