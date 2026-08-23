export function shortHash(value: string | null | undefined, len = 8): string {
  if (!value) return "—";
  return value.slice(0, len);
}

export function fullHash(value: string | null | undefined): string {
  if (!value) return "—";
  return value.slice(0, 16);
}

export function relativeTime(input: string | number | null | undefined): string {
  if (!input) return "—";
  const then = typeof input === "number" ? input : new Date(input).getTime();
  const diff = Date.now() - then;
  if (Number.isNaN(diff)) return "—";
  const future = diff < 0;
  const abs = Math.abs(diff);
  const seconds = Math.round(abs / 1000);
  const minutes = Math.round(seconds / 60);
  const hours = Math.round(minutes / 60);
  const days = Math.round(hours / 24);
  if (seconds < 45) return future ? "即将" : "刚刚";
  if (minutes < 60) return `${minutes} 分钟${future ? "后" : "前"}`;
  if (hours < 24) return `${hours} 小时${future ? "后" : "前"}`;
  if (days < 30) return `${days} 天${future ? "后" : "前"}`;
  return new Date(then).toLocaleDateString("zh-CN");
}

export function dateTime(input: string | null | undefined): string {
  if (!input) return "—";
  const d = new Date(input);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit"
  });
}

export function formatCount(v: number | null | undefined): string {
  if (v == null) return "—";
  return v.toLocaleString("zh-CN");
}

const MONTHS = ["01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12"];

export function isoLocal(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${MONTHS[d.getMonth()]}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

const HOUR = 3600_000;
const MIN = 60_000;
export function nowMinus(ms: number): string {
  return isoLocal(new Date(Date.now() - ms));
}
export const AGES = {
  min5: 5 * MIN,
  min15: 15 * MIN,
  min30: 30 * MIN,
  hour1: HOUR,
  hour2: 2 * HOUR,
  hour3: 3 * HOUR,
  hour6: 6 * HOUR,
  hour12: 12 * HOUR,
  day1: 24 * HOUR,
  day2: 48 * HOUR,
  day4: 4 * 24 * HOUR
} as const;
