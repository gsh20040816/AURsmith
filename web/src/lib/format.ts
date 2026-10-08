export function shortHash(value: string | null | undefined, len = 8): string {
  if (!value) return "—";
  return value.slice(0, len);
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
