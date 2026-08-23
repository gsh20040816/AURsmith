import type { ReactNode } from "react";

/* Minimal 24x24 stroke icon set, consistent 1.8 stroke width. */

type IconProps = { size?: number; className?: string };

function Svg({ size = 16, className, children }: IconProps & { children: ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

export function IconGrid(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="3" y="3" width="7" height="7" rx="1.5" />
      <rect x="14" y="3" width="7" height="7" rx="1.5" />
      <rect x="3" y="14" width="7" height="7" rx="1.5" />
      <rect x="14" y="14" width="7" height="7" rx="1.5" />
    </Svg>
  );
}
export function IconPackage(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M21 8 12 3 3 8v8l9 5 9-5V8Z" />
      <path d="m3 8 9 5 9-5M12 13v8" />
    </Svg>
  );
}
export function IconShield(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M12 3 5 6v6c0 4.5 3 7.5 7 9 4-1.5 7-4.5 7-9V6l-7-3Z" />
      <path d="m9 12 2 2 4-4" />
    </Svg>
  );
}
export function IconHammer(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="m15 12-8.5 8.5a2 2 0 0 1-2.8-2.8L12 9" />
      <path d="M17.6 15 22 10.6 19 7.6l3-3-2.6-2.6-3 3-3-3L9 6.4 13.4 10.8l4 4.2Z" />
    </Svg>
  );
}
export function IconRocket(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4.5 16.5c-1.5 1.2-2 5-2 5s3.8-.5 5-2" />
      <path d="M12 15 9 12c1-3 3.5-6.5 8-8 1.5-.5 3.5-.6 4 0 .6.5.5 2.5 0 4-1.5 4.5-5 7-9 7Z" />
      <path d="M9 12H5.5L7 9.5 9 9l2 2M12 15v3.5L14.5 17l.5-2 2-2" />
    </Svg>
  );
}
export function IconTerminal(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="2" y="4" width="20" height="16" rx="2" />
      <path d="m6 9 3 3-3 3M12 15h6" />
    </Svg>
  );
}
export function IconSearch(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="11" cy="11" r="7" />
      <path d="m21 21-4-4" />
    </Svg>
  );
}
export function IconBell(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M18 8a6 6 0 0 0-12 0c0 7-3 8-3 8h18s-3-1-3-8" />
      <path d="M13.7 21a2 2 0 0 1-3.4 0" />
    </Svg>
  );
}
export function IconChevronRight(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="m9 6 6 6-6 6" />
    </Svg>
  );
}
export function IconChevronDown(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="m6 9 6 6 6-6" />
    </Svg>
  );
}
export function IconClose(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M18 6 6 18M6 6l12 12" />
    </Svg>
  );
}
export function IconCheck(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="m5 12 5 5L20 7" />
    </Svg>
  );
}
export function IconClock(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </Svg>
  );
}
export function IconAlert(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M12 3 2 20h20L12 3Z" />
      <path d="M12 10v4M12 17h.01" />
    </Svg>
  );
}
export function IconInfo(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5M12 8h.01" />
    </Svg>
  );
}
export function IconPlus(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M12 5v14M5 12h14" />
    </Svg>
  );
}
export function IconRefresh(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M3 12a9 9 0 0 1 15.5-6.2L21 8M21 3v5h-5M21 12a9 9 0 0 1-15.5 6.2L3 16M3 21v-5h5" />
    </Svg>
  );
}
export function IconCopy(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M5 15V5a2 2 0 0 1 2-2h10" />
    </Svg>
  );
}
export function IconKey(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="7.5" cy="15.5" r="4.5" />
      <path d="m10.5 12.5 8-8M18 5l2 2M15 8l2 2" />
    </Svg>
  );
}
export function IconLog(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4 4h16v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V4Z" />
      <path d="M8 9h8M8 13h5" />
    </Svg>
  );
}
export function IconFingerprint(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M7 3a9 9 0 0 0-4 8v3M12 3a9 9 0 0 1 9 9c0 3-3 6-3 6M16 12a7 7 0 0 1-5 7M8 12a4 4 0 0 1 1-2.6" />
      <path d="M12 11v4c0 2-1 3-2 4M12 11c2 0 4 1 4 4" />
    </Svg>
  );
}
export function IconDots(p: IconProps) {
  return (
    <Svg {...p}>
      <circle cx="5" cy="12" r="1" />
      <circle cx="12" cy="12" r="1" />
      <circle cx="19" cy="12" r="1" />
    </Svg>
  );
}
export function IconFilter(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M3 5h18l-7 8v6l-4 2v-8L3 5Z" />
    </Svg>
  );
}
export function IconCmd(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M9 9V6a3 3 0 1 0-3 3h3Zm0 0v6m0-6h6m0 0V6a3 3 0 1 1 3 3h-3Zm0 0v6m-6 0h6m-6 0V18a3 3 0 1 1-3-3h3Zm6 0V18a3 3 0 1 0 3-3h-3Z" />
    </Svg>
  );
}
export function IconBox(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M3 9 12 3l9 6v6l-9 6-9-6V9Z" />
      <path d="M3 9l9 6 9-6M12 15v6" />
    </Svg>
  );
}
export function IconActivity(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M3 12h4l2-6 4 12 2-6h6" />
    </Svg>
  );
}
