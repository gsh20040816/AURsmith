import { useMemo, useState } from "react";
import { api } from "../lib/api";
import { usePolling, useStable } from "../lib/hooks";
import { relativeTime, shortHash } from "../lib/format";
import { statusMeta } from "../lib/status";
import type { AgentFinding, ReviewItem, ReviewRecord, ScanFinding } from "../lib/types";
import { Badge, Button, Card, CardBody, CardHead, Empty, PageHead, Segmented, StatusBadge } from "../components/ui";
import { useToast } from "../components/Toast";
import { IconAlert, IconCheck, IconRefresh, IconShield } from "../components/icons";

type Filter = "all" | "attention" | "running" | "resolved";

const RESOLVED = ["approved", "rejected"];

export function Reviews() {
  const toast = useToast();
  const [filter, setFilter] = useState<Filter>("all");
  const [rationale, setRationale] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState("");

  const load = useStable(() => api.reviews());
  const reviews = usePolling(load, 10000);
  const list = reviews.data?.items ?? [];

  const counts = useMemo(
    () => ({
      attention: list.filter((r) => r.state === "manual_review").length,
      running: list.filter((r) => r.state === "pending_review").length
    }),
    [list]
  );

  const filtered = list.filter((r) => {
    if (filter === "attention") return r.state === "manual_review";
    if (filter === "running") return r.state === "pending_review";
    if (filter === "resolved") return RESOLVED.includes(r.state);
    return true;
  });

  const decide = async (item: ReviewItem, approve: boolean) => {
    const text = (rationale[item.revision_id] ?? "").trim();
    if (text.length < 8) {
      toast.warning("理由过短", "人工决定需要至少 8 个字符的理由。");
      return;
    }
    setBusy(item.revision_id);
    try {
      await api.decideRevision(item.revision_id, approve, text);
      reviews.reload();
      toast.success(approve ? "已批准该 revision" : "已拒绝该 revision", "决定只对当前固定 commit 生效。");
    } catch (reason) {
      toast.error("操作失败", reason instanceof Error ? reason.message : undefined);
    } finally {
      setBusy("");
    }
  };

  const retry = async (item: ReviewItem) => {
    setBusy(item.revision_id);
    try {
      await api.retryReview(item.revision_id);
      reviews.reload();
      toast.success("已重新排队", "将重新运行 2+1 Agent 审查。");
    } catch (reason) {
      toast.error("重跑失败", reason instanceof Error ? reason.message : undefined);
    } finally {
      setBusy("");
    }
  };

  return (
    <>
      <PageHead
        title="审查"
        lede="2+1：两个低档 Agent 一致批准即通过，否则交给高档 Agent；首次添加始终人工审批。"
        actions={
          <>
            <Segmented
              value={filter}
              onChange={(v) => setFilter(v as Filter)}
              options={[
                { value: "all", label: "全部" },
                { value: "attention", label: `待审批 ${counts.attention}` },
                { value: "running", label: `审查中 ${counts.running}` },
                { value: "resolved", label: "已定案" }
              ]}
            />
            <Button variant="ghost" onClick={reviews.reload} icon={<IconRefresh size={15} />}>刷新</Button>
          </>
        }
      />

      {reviews.error && (
        <div className="notice notice--danger" role="alert" style={{ marginBottom: 16 }}>
          <IconAlert size={16} />
          <div><b>审查列表读取失败。</b> {reviews.error}</div>
        </div>
      )}

      {filtered.length === 0 ? (
        <Card><CardBody><Empty title="没有审查记录" detail="订阅的软件包出现新的 AUR commit 后会自动进入审查。" glyph={<IconShield size={22} />} /></CardBody></Card>
      ) : (
        <div className="stack">
          {filtered.map((item) => (
            <ReviewCard
              key={item.revision_id}
              item={item}
              busy={busy === item.revision_id}
              rationale={rationale[item.revision_id] ?? ""}
              onRationale={(v) => setRationale((cur) => ({ ...cur, [item.revision_id]: v }))}
              onApprove={() => void decide(item, true)}
              onReject={() => void decide(item, false)}
              onRetry={() => void retry(item)}
            />
          ))}
        </div>
      )}
    </>
  );
}

function scanFindings(records: ReviewRecord[]): ScanFinding[] {
  const scan = records.find((r) => r.kind === "scan");
  return Array.isArray(scan?.findings) ? scan.findings : [];
}

function agentFindings(record: ReviewRecord): AgentFinding[] {
  return record.findings && !Array.isArray(record.findings) ? record.findings.findings : [];
}

function ReviewCard({
  item,
  busy,
  rationale,
  onRationale,
  onApprove,
  onReject,
  onRetry
}: {
  item: ReviewItem;
  busy: boolean;
  rationale: string;
  onRationale: (v: string) => void;
  onApprove: () => void;
  onReject: () => void;
  onRetry: () => void;
}) {
  const agents = item.reviews.filter((r) => r.kind === "agent");
  const findings = scanFindings(item.reviews);
  const others = item.reviews.filter((r) => r.kind === "reuse" || r.kind === "human");
  const manual = item.state === "manual_review";

  return (
    <Card className={`review-card${manual ? " review-card--attention" : ""}`}>
      <CardHead>
        <div className="review-card__title">
          <div className="row" style={{ gap: 8 }}>
            {item.first_time ? <Badge tone="warning">首次添加</Badge> : <Badge tone="accent">增量（附 diff）</Badge>}
            <span className="u-muted u-mono">
              commit {shortHash(item.aur_commit)}
              {item.vcs_commit && ` · vcs ${shortHash(item.vcs_commit)}`} · {relativeTime(item.created_at)}
            </span>
          </div>
          <strong>
            {item.package_base} <span className="u-muted">{item.version}</span>
          </strong>
        </div>
        <StatusBadge meta={statusMeta(item.state)} />
      </CardHead>

      <CardBody>
        <div>
          <h3 style={{ marginBottom: 8 }}>Agent 审查</h3>
          {agents.length === 0 ? (
            <p className="u-muted" style={{ fontSize: 12.5 }}>
              {item.state === "pending_review" ? "等待 Agent 审查。" : "没有 Agent 记录（复用或扫描阻断）。"}
            </p>
          ) : (
            <div className="agent-runs">
              {agents.map((record, i) => (
                <div className="agent-run" key={`${record.role}-${i}`}>
                  <div className="agent-run__tier">
                    <Badge tone={record.role === "high" ? "accent" : "info"}>{record.role === "high" ? "高档" : "低档"}</Badge>
                    <span className="agent-run__verdict">{relativeTime(record.created_at)}</span>
                  </div>
                  <div className="agent-run__model">{record.model ?? "—"}</div>
                  <div className="agent-run__verdict">
                    <StatusBadge meta={statusMeta(record.verdict)} />
                  </div>
                  <div className="u-muted" style={{ fontSize: 12 }}>{record.summary}</div>
                  {agentFindings(record).map((f, j) => (
                    <div className="u-muted" style={{ fontSize: 11.5 }} key={j}>
                      [{f.severity}] {f.file ? `${f.file}${f.line ? `:${f.line}` : ""} ` : ""}{f.message}
                    </div>
                  ))}
                </div>
              ))}
            </div>
          )}
        </div>

        {others.map((record, i) => (
          <div className="coverage-box" key={`${record.kind}-${i}`}>
            <b>{record.kind === "human" ? "人工决定" : "复用审查"}：</b>
            {record.summary}
          </div>
        ))}

        <div>
          <h3 style={{ marginBottom: 8 }}>确定性扫描</h3>
          {findings.length === 0 ? (
            <p className="u-muted" style={{ fontSize: 12.5 }}>确定性扫描未发现阻断或可疑项。</p>
          ) : (
            <div className="tbl-wrap">
              <table className="tbl finding-table">
                <thead>
                  <tr>
                    <th>规则</th>
                    <th>级别</th>
                    <th>路径</th>
                    <th>说明</th>
                  </tr>
                </thead>
                <tbody>
                  {findings.map((f) => (
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
          )}
        </div>

        {manual && (
          <div className="stack" style={{ gap: 10 }}>
            <div className="notice notice--warning" style={{ margin: 0 }}>
              <IconAlert size={16} />
              <div>
                <b>{item.first_time ? "首次添加的软件包必须人工审批。" : "Agent 未能自动批准，需要人工审批。"}</b>
                批准或拒绝只对当前固定 commit 有效，并记录理由。
              </div>
            </div>
            <div className="field">
              <label htmlFor={`rationale-${item.revision_id.slice(0, 8)}`}>人工判断理由</label>
              <textarea
                id={`rationale-${item.revision_id.slice(0, 8)}`}
                className="textarea"
                value={rationale}
                onChange={(e) => onRationale(e.target.value)}
                placeholder="至少 8 个字符，只对当前 commit 有效"
              />
              <span className="field__hint">{rationale.length}/8 字符</span>
            </div>
            <div className="row">
              <Button size="sm" variant="ghost" icon={<IconRefresh size={14} />} disabled={busy} onClick={onRetry}>
                重新运行 2+1 审查
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
