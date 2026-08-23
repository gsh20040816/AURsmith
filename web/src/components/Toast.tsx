import { createContext, ReactNode, useCallback, useContext, useMemo, useRef, useState } from "react";
import { IconAlert, IconCheck, IconClose, IconInfo } from "./icons";

type ToastKind = "success" | "error" | "info" | "warning";
type ToastItem = { id: number; kind: ToastKind; title: string; message?: string };

type ToastCtx = {
  push: (kind: ToastKind, title: string, message?: string) => void;
  success: (title: string, message?: string) => void;
  error: (title: string, message?: string) => void;
  info: (title: string, message?: string) => void;
  warning: (title: string, message?: string) => void;
};

const Ctx = createContext<ToastCtx | null>(null);
let idc = 0;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastItem[]>([]);
  const timers = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map());

  const dismiss = useCallback((id: number) => {
    setItems((cur) => cur.filter((t) => t.id !== id));
    const t = timers.current.get(id);
    if (t) {
      clearTimeout(t);
      timers.current.delete(id);
    }
  }, []);

  const push = useCallback(
    (kind: ToastKind, title: string, message?: string) => {
      const id = ++idc;
      setItems((cur) => [...cur.slice(-3), { id, kind, title, message }]);
      timers.current.set(id, setTimeout(() => dismiss(id), 5200));
    },
    [dismiss]
  );

  const value = useMemo<ToastCtx>(
    () => ({
      push,
      success: (title, message) => push("success", title, message),
      error: (title, message) => push("error", title, message),
      info: (title, message) => push("info", title, message),
      warning: (title, message) => push("warning", title, message)
    }),
    [push]
  );

  return (
    <Ctx.Provider value={value}>
      {children}
      <div className="toast-region" role="status">
        {items.map((t) => (
          <div key={t.id} className={`toast toast--${t.kind}`} role={t.kind === "error" ? "alert" : undefined}>
            <div className="toast__icon">
              {t.kind === "success" ? (
                <IconCheck size={16} />
              ) : t.kind === "error" ? (
                <IconAlert size={16} />
              ) : t.kind === "warning" ? (
                <IconInfo size={16} />
              ) : (
                <IconInfo size={16} />
              )}
            </div>
            <div className="toast__body">
              <div className="toast__title">{t.title}</div>
              {t.message && <div className="toast__msg">{t.message}</div>}
            </div>
            <button className="toast__close" onClick={() => dismiss(t.id)} aria-label="关闭">
              <IconClose size={15} />
            </button>
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}

export function useToast(): ToastCtx {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useToast must be used inside <ToastProvider>");
  return ctx;
}
