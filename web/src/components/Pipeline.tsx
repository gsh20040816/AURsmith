import { IconCheck, IconDots } from "./icons";

export type StageState = "done" | "active" | "idle";
export type Stage = { name: string; detail: string; state: StageState };

export function Pipeline({ stages, className }: { stages: Stage[]; className?: string }) {
  return (
    <div className={`pipeline ${className ?? ""}`} aria-label="软件包锻造流程">
      {stages.map((s) => (
        <div className={`pipe-stage is-${s.state}`} key={s.name}>
          <div className="pipe-stage__node">
            {s.state === "done" ? <IconCheck size={14} /> : s.state === "active" ? <IconDots size={14} /> : null}
          </div>
          <div className="pipe-stage__name">{s.name}</div>
          <div className="pipe-stage__detail">{s.detail}</div>
        </div>
      ))}
    </div>
  );
}
