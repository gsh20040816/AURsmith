import { ReactNode, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { IconBox, IconGrid, IconHammer, IconPackage, IconRocket, IconShield, IconTerminal } from "./icons";

const ITEMS: Array<{ to: string; label: string; hint: string; icon: ReactNode }> = [
  { to: "/dashboard", label: "总览", hint: "系统状态与流程", icon: <IconGrid size={16} /> },
  { to: "/packages", label: "软件包", hint: "订阅与依赖闭包", icon: <IconPackage size={16} /> },
  { to: "/audits", label: "审查", hint: "diff-first 3+1", icon: <IconShield size={16} /> },
  { to: "/builds", label: "构建", hint: "Builder 队列与日志", icon: <IconHammer size={16} /> },
  { to: "/releases", label: "发布", hint: "current / previous", icon: <IconRocket size={16} /> },
  { to: "/client", label: "客户端", hint: "接入与 keyring", icon: <IconTerminal size={16} /> }
];

export function CommandPalette({ open, onClose }: { open: boolean; onClose: () => void }) {
  const navigate = useNavigate();
  const [q, setQ] = useState("");
  const [idx, setIdx] = useState(0);

  const filtered = ITEMS.filter((i) => !q || i.label.includes(q) || i.hint.includes(q));

  useEffect(() => setIdx(0), [q, open]);

  useEffect(() => {
    if (!open) return;
    const fn = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setIdx((i) => Math.min(i + 1, filtered.length - 1));
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setIdx((i) => Math.max(i - 1, 0));
      }
      if (e.key === "Enter" && filtered[idx]) {
        navigate(filtered[idx].to);
        onClose();
      }
    };
    window.addEventListener("keydown", fn);
    return () => window.removeEventListener("keydown", fn);
  }, [open, filtered, idx, onClose, navigate]);

  if (!open) return null;
  return (
    <div className="modal-scrim" style={{ alignItems: "flex-start", paddingTop: "12vh" }} onClick={onClose}>
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label="跳转页面"
        style={{ width: "min(560px, 100%)", padding: 0, overflow: "hidden" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="searchbox" style={{ border: "none", borderBottom: "1px solid var(--border)", height: 52, borderRadius: 0, fontSize: 15 }}>
          <IconBox size={16} />
          <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="跳转页面…" autoFocus />
        </div>
        <div style={{ padding: 8 }}>
          {filtered.length === 0 && (
            <div className="empty" style={{ padding: 28 }}>
              <h3>没有匹配的页面</h3>
              <p>尝试输入「软件包」「审查」「构建」等。</p>
            </div>
          )}
          {filtered.map((item, i) => (
            <button
              key={item.to}
              className="nav-item"
              style={{
                height: 44,
                justifyContent: "flex-start",
                background: i === idx ? "var(--surface-2)" : "transparent",
                color: i === idx ? "var(--text)" : "var(--text-secondary)",
                width: "100%",
                borderRadius: 8,
                padding: "0 12px"
              }}
              onMouseEnter={() => setIdx(i)}
              onClick={() => {
                navigate(item.to);
                onClose();
              }}
            >
              {item.icon}
              <span style={{ flex: 1, opacity: 1 }}>{item.label}</span>
              <span className="u-muted" style={{ fontSize: 12, fontFamily: "var(--font-mono)" }}>
                {item.hint}
              </span>
            </button>
          ))}
        </div>
        <div style={{ borderTop: "1px solid var(--border)", padding: "8px 12px", display: "flex", gap: 12, color: "var(--text-muted)", fontSize: 11.5 }}>
          <span>↑↓ 选择</span>
          <span>Enter 前往</span>
          <span>Esc 关闭</span>
        </div>
      </div>
    </div>
  );
}
