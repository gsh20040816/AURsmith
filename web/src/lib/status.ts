export type Tone = "neutral" | "success" | "warning" | "danger" | "info" | "accent";

export type StatusMeta = { tone: Tone; label: string; pulse?: boolean };

const MAP: Record<string, StatusMeta> = {
  ready: { tone: "success", label: "就绪" },

  /* revision */
  pending_review: { tone: "info", label: "Agent 审查中", pulse: true },
  manual_review: { tone: "warning", label: "人工审查" },
  approved: { tone: "success", label: "已批准" },
  rejected: { tone: "danger", label: "已拒绝" },
  superseded: { tone: "neutral", label: "已被取代" },

  /* build */
  queued: { tone: "neutral", label: "排队中" },
  running: { tone: "info", label: "构建中", pulse: true },
  uploading: { tone: "info", label: "上传中", pulse: true },
  succeeded: { tone: "success", label: "成功" },
  failed: { tone: "danger", label: "失败" },

  /* publication */
  pending: { tone: "info", label: "等待签名器", pulse: true },
  published: { tone: "success", label: "已发布" },

  /* review verdict */
  approve: { tone: "success", label: "批准" },
  reject: { tone: "danger", label: "拒绝" },
  error: { tone: "danger", label: "出错" }
};

export function statusMeta(status: string | null | undefined): StatusMeta {
  if (!status) return { tone: "neutral", label: "—" };
  return MAP[status] ?? { tone: "neutral", label: status };
}

export const ACTIVE_BUILD_STATES = ["queued", "running", "uploading"];

export function toneClass(tone: Tone): string {
  return `badge--${tone}`;
}
