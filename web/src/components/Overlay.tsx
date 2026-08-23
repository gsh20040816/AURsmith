import { ReactNode, useEffect } from "react";
import { Button } from "./ui";
import { IconClose } from "./icons";

export function useEscape(onClose: () => void, active = true) {
  useEffect(() => {
    if (!active) return;
    const fn = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", fn);
    return () => window.removeEventListener("keydown", fn);
  }, [onClose, active]);
}

export function useLockScroll(active = true) {
  useEffect(() => {
    if (!active) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [active]);
}

export function Drawer({
  open,
  onClose,
  title,
  eyebrow,
  children,
  footer
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  eyebrow?: string;
  children: ReactNode;
  footer?: ReactNode;
}) {
  useEscape(onClose, open);
  useLockScroll(open);
  if (!open) return null;
  return (
    <>
      <div className="drawer-scrim" onClick={onClose} />
      <div className="drawer" role="dialog" aria-modal="true">
        <div className="drawer__head">
          <div>
            {eyebrow && <span className="page-head__eyebrow">{eyebrow}</span>}
            <h2 style={{ fontSize: 16 }}>{title}</h2>
          </div>
          <button className="btn btn--ghost btn--icon" onClick={onClose} aria-label="关闭">
            <IconClose size={16} />
          </button>
        </div>
        <div className="drawer__body">{children}</div>
        {footer && <div className="drawer__head" style={{ borderTop: "1px solid var(--border)", borderBottom: "none" }}>{footer}</div>}
      </div>
    </>
  );
}

export function Confirm({
  open,
  onClose,
  onConfirm,
  title,
  body,
  confirmLabel = "确认",
  danger,
  busy
}: {
  open: boolean;
  onClose: () => void;
  onConfirm: () => void;
  title: string;
  body: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
  busy?: boolean;
}) {
  useEscape(onClose, open);
  useLockScroll(open);
  if (!open) return null;
  return (
    <div className="modal-scrim" onClick={onClose}>
      <div className="modal" role="dialog" aria-modal="true" onClick={(e) => e.stopPropagation()}>
        <div className="modal__title">{title}</div>
        <div className="modal__body">{body}</div>
        <div className="modal__actions">
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            取消
          </Button>
          <Button variant={danger ? "danger" : "primary"} onClick={onConfirm} loading={busy}>
            {confirmLabel}
          </Button>
        </div>
      </div>
    </div>
  );
}
