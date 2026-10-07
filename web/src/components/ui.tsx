import {
  ButtonHTMLAttributes,
  HTMLAttributes,
  ReactNode,
  useRef,
  useState
} from "react";
import { IconInfo, IconSearch } from "./icons";
import type { StatusMeta } from "../lib/status";
import { toneClass } from "../lib/status";

/* ---- Button ------------------------------------------------------------- */
type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "default" | "primary" | "accent" | "ghost" | "danger";
  size?: "default" | "sm";
  loading?: boolean;
  icon?: ReactNode;
};

export function Button({
  variant = "default",
  size = "default",
  loading,
  icon,
  className,
  children,
  disabled,
  ...rest
}: ButtonProps) {
  const cls = [
    "btn",
    variant === "default" ? "" : variant === "primary" || variant === "accent" ? "btn--primary" : variant === "ghost" ? "btn--ghost" : "btn--danger",
    size === "sm" ? "btn--sm" : "",
    className ?? ""
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button className={cls} disabled={disabled || loading} {...rest}>
      {loading ? <span className="spinner" /> : icon}
      {children}
    </button>
  );
}

/* ---- Badge -------------------------------------------------------------- */
export function Badge({ tone, children, dot }: { tone: string; children: ReactNode; dot?: boolean }) {
  return (
    <span className={`badge ${toneClass(tone as never)}`}>
      {dot && <span className="dot" />}
      {children}
    </span>
  );
}

export function StatusBadge({ meta }: { meta: StatusMeta }) {
  return (
    <Badge tone={meta.tone} dot>
      {meta.pulse && <span className="badge__spinner" />}
      {meta.label}
    </Badge>
  );
}

/* ---- Card --------------------------------------------------------------- */
export function Card({ className, children, hover, ...rest }: HTMLAttributes<HTMLDivElement> & { hover?: boolean }) {
  return (
    <div className={`card ${hover ? "is-hover" : ""} ${className ?? ""}`} {...rest}>
      {children}
    </div>
  );
}
export function CardHead({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={`card__head ${className ?? ""}`}>{children}</div>;
}
export function CardTitle({ eyebrow, title, sub }: { eyebrow?: string; title: ReactNode; sub?: string }) {
  return (
    <div className="card__title">
      {eyebrow && <span className="eyebrow">{eyebrow}</span>}
      <h2>{title}</h2>
      {sub && <span className="u-secondary" style={{ fontSize: 12.5 }}>{sub}</span>}
    </div>
  );
}
export function CardBody({ children, className, flush }: { children: ReactNode; className?: string; flush?: boolean }) {
  return <div className={`card__body ${flush ? "card__body--flush" : ""} ${className ?? ""}`}>{children}</div>;
}

/* ---- Stat --------------------------------------------------------------- */
export function Stat({ label, value, unit, icon, foot }: { label: string; value: ReactNode; unit?: string; icon?: ReactNode; foot?: ReactNode }) {
  return (
    <Card className="stat" hover>
      <div>
        <div className="stat__label">{label}</div>
        <div className="stat__value">
          {value}
          {unit && <small>{unit}</small>}
        </div>
        {foot && <div className="stat__foot">{foot}</div>}
      </div>
      {icon && <div className="stat__icon">{icon}</div>}
    </Card>
  );
}

/* ---- SearchBox ---------------------------------------------------------- */
export function SearchBox({
  value,
  onChange,
  placeholder,
  onKeyDown,
  autoFocus
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  onKeyDown?: (e: React.KeyboardEvent<HTMLInputElement>) => void;
  autoFocus?: boolean;
}) {
  return (
    <div className="searchbox">
      <IconSearch size={16} />
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder ?? "搜索…"}
        onKeyDown={onKeyDown}
        autoFocus={autoFocus}
        spellCheck={false}
      />
    </div>
  );
}

/* ---- Segmented control -------------------------------------------------- */
type SegmentedProps = Omit<HTMLAttributes<HTMLDivElement>, "value" | "onChange"> & {
  value: string;
  onChange: (v: string) => void;
  options: Array<{ value: string; label: ReactNode }>;
};
export function Segmented({ value, onChange, options, className }: SegmentedProps) {
  return (
    <div className={`seg ${className ?? ""}`} role="tablist">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="tab"
          aria-selected={value === o.value}
          className={`seg__item ${value === o.value ? "is-on" : ""}`}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/* ---- Empty -------------------------------------------------------------- */
export function Empty({ title, detail, glyph }: { title: string; detail?: string; glyph?: ReactNode }) {
  return (
    <div className="empty">
      <div className="empty__glyph">{glyph ?? <IconInfo size={22} />}</div>
      <h3>{title}</h3>
      {detail && <p>{detail}</p>}
    </div>
  );
}

/* ---- Spinner ------------------------------------------------------------ */
export function Spinner({ size = 16 }: { size?: number }) {
  return <span className="spinner" style={{ width: size, height: size }} />;
}

/* ---- Skeleton ----------------------------------------------------------- */
export function Skeleton({ height = 12, width }: { height?: number; width?: number | string }) {
  return <span className="skeleton" style={{ height, width: width ?? "100%" }} />;
}

/* ---- Kbd ---------------------------------------------------------------- */
export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="key">{children}</kbd>;
}

/* ---- Notice ------------------------------------------------------------- */
export function Notice({ kind = "info", children }: { kind?: "info" | "warning" | "danger"; children: ReactNode }) {
  const cls = kind === "info" ? "" : kind === "warning" ? "notice--warning" : "notice--danger";
  return (
    <div className={`notice ${cls}`}>
      <IconInfo size={16} />
      <div>{children}</div>
    </div>
  );
}

/* ---- Tooltip ------------------------------------------------------------ */
export function Tooltip({ content, children }: { content: ReactNode; children: ReactNode }) {
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const ref = useRef<HTMLSpanElement>(null);
  const show = () => {
    const rect = ref.current?.getBoundingClientRect();
    if (rect) setPos({ x: rect.left + rect.width / 2, y: rect.bottom + 6 });
  };
  return (
    <span
      ref={ref}
      onMouseEnter={show}
      onMouseLeave={() => setPos(null)}
      style={{ display: "inline-flex" }}
    >
      {children}
      {pos && (
        <span
          className="tooltip"
          style={{ left: pos.x, top: pos.y, transform: "translateX(-50%)" }}
        >
          {content}
        </span>
      )}
    </span>
  );
}

/* ---- Page head ---------------------------------------------------------- */
export function PageHead({ eyebrow, title, lede, actions }: { eyebrow?: string; title: ReactNode; lede?: string; actions?: ReactNode }) {
  return (
    <div className="page-head">
      <div>
        {eyebrow && <span className="page-head__eyebrow">{eyebrow}</span>}
        <h1>{title}</h1>
        {lede && <p className="page-head__lede">{lede}</p>}
      </div>
      {actions && <div className="toolbar">{actions}</div>}
    </div>
  );
}
