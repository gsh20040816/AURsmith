import type { Audit, Job, Release } from "./types";

export type Tone = "neutral" | "success" | "warning" | "danger" | "info" | "accent";

export type StatusMeta = { tone: Tone; label: string; pulse?: boolean };

const MAP: Record<string, StatusMeta> = {
  /* shared */
  ready: { tone: "success", label: "就绪" },
  ok: { tone: "success", label: "正常" },
  degraded: { tone: "warning", label: "降级" },
  attention: { tone: "warning", label: "需关注" },
  error: { tone: "danger", label: "错误" },
  failed: { tone: "danger", label: "失败" },
  failed_retry: { tone: "danger", label: "失败" },

  /* audits */
  agent_pending: { tone: "neutral", label: "等待 Agent" },
  agent_running: { tone: "info", label: "Agent 运行中", pulse: true },
  manual_review: { tone: "warning", label: "人工审查" },
  approved: { tone: "success", label: "已批准" },
  rejected: { tone: "danger", label: "已拒绝" },
  blocked: { tone: "danger", label: "规则阻断" },
  full: { tone: "accent", label: "全量审查" },
  diff: { tone: "accent", label: "增量审查" },

  /* jobs */
  queued: { tone: "neutral", label: "排队中" },
  no_eligible_worker: { tone: "warning", label: "无可用 Builder" },
  dispatched: { tone: "info", label: "已派发", pulse: true },
  succeeded: { tone: "success", label: "成功" },
  cancelled: { tone: "neutral", label: "已取消" },
  uncertain: { tone: "warning", label: "状态待确认", pulse: true },
  running_job: { tone: "info", label: "构建中", pulse: true },

  /* releases */
  authorizing: { tone: "info", label: "等待发布器", pulse: true },
  committed: { tone: "success", label: "已提交" },

  /* positions */
  current: { tone: "success", label: "current" },
  previous: { tone: "neutral", label: "previous" }
};

export function statusMeta(status: string | null | undefined): StatusMeta {
  if (!status) return { tone: "neutral", label: "—" };
  return MAP[status] ?? { tone: "neutral", label: status };
}

export const auditState = (a: Audit): StatusMeta => {
  if (a.state === "manual_review") return { tone: "warning", label: "人工处置" };
  if (a.state === "approved") return { tone: "success", label: "已批准" };
  return statusMeta(a.state);
};

export const jobState = (j: Job): StatusMeta => {
  if (j.status === "running") return { tone: "info", label: "构建中", pulse: true };
  if (j.status === "failed") return { tone: "danger", label: "失败" };
  return statusMeta(j.status);
};

export const releaseState = (r: Release): StatusMeta => {
  if (r.position === "current") return { tone: "success", label: "当前仓库" };
  if (r.position === "previous") return { tone: "neutral", label: "上一版本" };
  return statusMeta(r.state);
};

export function toneClass(tone: Tone): string {
  return `badge--${tone}`;
}
