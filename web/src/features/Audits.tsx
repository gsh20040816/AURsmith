import { useMemo, useState } from "react";
import { api } from "../lib/api";
import { usePolling, useStable, useTicker } from "../lib/hooks";
import { shortHash } from "../lib/format";
import { auditState } from "../lib/status";
import { type Audit, type AuditRun } from "../lib/types";
import { Badge, Button, Card, CardBody, CardHead, Empty, PageHead, Segmented, StatusBadge } from "../components/ui";
import { useToast } from "../components/Toast";
import { IconCheck, IconRefresh, IconShield, IconAlert } from "../components/icons";

type Filter = "all" | "attention" | "running" | "resolved";

export function Audits() {
  const toast = useToast();
  const [filter, setFilter] = useState<Filter>("all");
  const [rationale, setRationale] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState("");

  useTicker(1000);
  const load = useStable(() => api.audits());
  const audits = usePolling(load, 10000);
  const list = audits.data?.items ?? [];

  const counts = useMemo(
    () => ({
      all: list.length,
      attention: list.filter((a) => a.state === "manual_review").length,
      running: list.filter((a) => ["agent_pending", "agent_running"].includes(a.state)).length,
      resolved: list.filter((a) => ["approved", "rejected", "blocked"].includes(a.state)).length
    }),
    [list]
  );

  const filtered = list.filter((a) => {
    if (filter === "attention") return a.state === "manual_review";
    if (filter === "running") return ["agent_pending", "agent_running"].includes(a.state);
    if (filter === "resolved") return ["approved", "rejected", "blocked"].includes(a.state);
    return true;
  });

  const decide = async (audit: Audit, approve: boolean) => {
    if ((rationale[audit.sha256] ?? "").trim().length < 8) {
      toast.warning("理由过短", "人工决定需要至少 8 个字符的理由。");
      return;
    }
    setBusy(audit.sha256);
    try {
      await api.decideAudit(audit.sha256, approve, rationale[audit.sha256] ?? "");
      audits.reload();
      toast.success(approve ? "已批准该 commit" : "已拒绝该 commit", "决定只对当前 commit 生效。");
    } catch (reason) {
      toast.error("操作失败", reason instanceof Error ? reason.message : undefined);
    } finally {
      setBusy("");
    }
  };

  const retry = async (audit: Audit) => {
    setBusy(audit.sha256);
    try {
      await api.retryAudit(audit.sha256);
      audits.reload();
      toast.success("已重跑", "重新调度三个 low。");
    } catch (reason) {
      toast.error("重跑失败", reason instanceof Error ? reason.message : undefined);
    } finally {
      setBusy("");
    }
  };

  return (
    <>
      <PageHead
        eyebrow="diff-first 3+1"
        title="审查"
        lede="报告只覆盖固定 AUR 包装层；上游下载内容未被审查时必须明确说明，不作为模型理解证明。"
        actions={
          <>
            <Segmented
              value={filter}
              onChange={(v) => setFilter(v as Filter)}
              options={[
                { value: "all", label: "全部" },
                { value: "attention", label: `待处置 ${counts.attention}` },
                { value: "running", label: `进行中 ${counts.running}` },
                { value: "resolved", label: "已定案" }
              ]}
            />
            <Button variant="ghost" onClick={audits.reload} icon={<IconRefresh size={15} />}>刷新</Button>
          </>
        }
      />

      {audits.error && (
        <div className="notice notice--danger" role="alert" style={{ marginBottom: 16 }}>
          <IconAlert size={16} />
          <div><b>审查列表读取失败。</b> {audits.error}</div>
        </div>
      )}

      {filtered.length === 0 ? (
        <Card><CardBody><Empty title="没有审查任务" detail="新 AUR commit 固定后会自动生成审查输入。" glyph={<IconShield size={22} />} /></CardBody></Card>
      ) : (
        <div className="stack">
          {filtered.map((audit) => (
            <AuditCard
              key={audit.sha256}
              audit={audit}
              busy={busy === audit.sha256}
              rationale={rationale[audit.sha256] ?? ""}
              onRationale={(v) => setRationale((cur) => ({ ...cur, [audit.sha256]: v }))}
              onApprove={() => void decide(audit, true)}
              onReject={() => void decide(audit, false)}
              onRetry={() => void retry(audit)}
            />
          ))}
        </div>
      )}
    </>
  );
}

function AuditCard({
  audit,
  busy,
  rationale,
  onRationale,
  onApprove,
  onReject,
  onRetry
}: {
  audit: Audit;
  busy: boolean;
  rationale: string;
  onRationale: (v: string) => void;
  onApprove: () => void;
  onReject: () => void;
  onRetry: () => void;
}) {
  const state = auditState(audit);
  const lowRuns = audit.runs.filter((r) => r.tier === "low");
  const highRun = audit.runs.find((r) => r.tier === "high");
  const blocking = audit.findings.filter((f) => f.severity === "block").length;
  const warning = audit.findings.filter((f) => f.severity === "suspicious").length;

  return (
    <Card className="audit-card">
      <CardHead>
        <div className="audit-card__title">
          <div className="row" style={{ gap: 8 }}>
            <Badge tone="accent">包装层 {audit.coverage.aur_wrapper?.mode ?? "未声明"}</Badge>
            <span className="u-muted u-mono">{audit.policy_version} · commit {shortHash(audit.aur_commit)}</span>
          </div>
          <strong>{audit.package_base}</strong>
        </div>
        <StatusBadge meta={state} />
      </CardHead>

      <CardBody>
        {audit.coverage.upstream_source && (
          <div className={`coverage-box ${audit.coverage.upstream_source.mode === "not_reviewed" ? "coverage-box--muted" : ""}`}>
            {audit.coverage.upstream_source.statement}
          </div>
        )}

        <div>
          <h3 style={{ marginBottom: 8 }}>实际 Agent 运行</h3>
          <div className="agent-runs">
            {lowRuns.map((run) => (
              <AgentRun key={`${run.tier}-${run.slot}-${run.attempt}`} run={run} />
            ))}
            {highRun && <AgentRun run={highRun} isHigh />}
          </div>
        </div>

        <div>
          <h3 style={{ marginBottom: 8 }}>确定性检查</h3>
          {audit.findings.length === 0 ? (
            <p className="u-muted" style={{ fontSize: 12.5 }}>确定性扫描未发现阻断或可疑项。</p>
          ) : (
            <div className="log-grid" style={{ gridTemplateColumns: "1fr" }}>
              <div className="tbl-wrap">
                <table className="tbl finding-table">
                  <thead>
                    <tr>
                      <th style={{ width: 100 }}>规则</th>
                      <th style={{ width: 90 }}>级别</th>
                      <th>路径</th>
                      <th>说明</th>
                    </tr>
                  </thead>
                  <tbody>
                    {audit.findings.map((f) => (
                      <tr key={`${f.rule_id}-${f.path}`}>
                        <td><code className="inline">{f.rule_id}</code></td>
                        <td>
                          <Badge tone={f.severity === "block" ? "danger" : f.severity === "suspicious" ? "warning" : "neutral"}>{f.severity}</Badge>
                        </td>
                        <td><code className="inline hash">{f.path}</code></td>
                        <td className="u-secondary" style={{ fontSize: 12.5 }}>{f.summary}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </div>

        {audit.state === "manual_review" && (
          <div className="notice notice--danger" style={{ margin: 0 }}>
            <IconAlert size={16} />
            <div>
              <b>自动流程已终止，进入人工处置。</b>
              {blocking > 0 && ` ${blocking} 个阻断项，`}
              {warning > 0 && `${warning} 个上下文发现。`}
              批准或拒绝只在当前 commit 有效；需记录人工理由。
            </div>
          </div>
        )}

        {audit.state === "manual_review" && (
          <div className="stack" style={{ gap: 10 }}>
            <div className="field">
              <label htmlFor={`rationale-${audit.sha256.slice(0, 8)}`}>人工判断理由</label>
              <textarea
                id={`rationale-${audit.sha256.slice(0, 8)}`}
                className="textarea"
                value={rationale}
                onChange={(e) => onRationale(e.target.value)}
                placeholder="至少 8 个字符，只对当前 commit 有效"
              />
              <span className="field__hint">{rationale.length}/8 字符</span>
            </div>
            <div className="row">
              <Button size="sm" variant="ghost" icon={<IconRefresh size={14} />} disabled={busy} onClick={onRetry}>
                修复配置后重跑 3 个 low
              </Button>
              <div className="toolbar__spacer" />
              <Button size="sm" variant="ghost" disabled={busy} onClick={onReject}>拒绝</Button>
              <Button size="sm" variant="primary" icon={<IconCheck size={14} />} loading={busy} onClick={onApprove}>批准当前 commit</Button>
            </div>
          </div>
        )}
      </CardBody>
    </Card>
  );
}

function AgentRun({ run, isHigh }: { run: AuditRun; isHigh?: boolean }) {
  const verdictColor = run.verdict === "approve" ? "success" : run.verdict === "reject" ? "danger" : run.verdict === "error" ? "danger" : "neutral";
  return (
    <div className="agent-run">
      <div className="agent-run__tier">
        <Badge tone={isHigh ? "accent" : "info"}>{isHigh ? "high" : `low ${run.slot}`}</Badge>
        <span className="agent-run__verdict">{run.status === "succeeded" ? "完成" : run.status === "failed" ? "失败" : run.status === "running" ? "运行中" : "等待"}</span>
      </div>
      <div className="agent-run__model">{run.provider} · {run.model}</div>
      <div className="agent-run__verdict">
        <Badge tone={verdictColor}>{run.verdict ?? run.status}</Badge>
      </div>
      {run.report?.summary && <div className="u-muted" style={{ fontSize: 12 }}>{run.report.summary}</div>}
    </div>
  );
}
