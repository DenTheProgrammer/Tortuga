const UNITS_RU = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
const UNITS_EN = ["B", "KB", "MB", "GB", "TB"];

export function bytes(n: number, lang = "en"): string {
  const u = lang === "ru" ? UNITS_RU : UNITS_EN;
  if (!Number.isFinite(n) || n <= 0) return `0 ${u[0]}`;
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  const digits = v >= 100 || i === 0 ? 0 : v >= 10 ? 1 : 2;
  return `${v.toFixed(digits).replace(".", lang === "ru" ? "," : ".")} ${u[i]}`;
}

export function speed(n: number, lang = "en"): string {
  if (!n) return "";
  return `${bytes(n, lang)}/${lang === "ru" ? "с" : "s"}`;
}

export function duration(secs: number | null | undefined, lang = "en"): string {
  if (secs == null || !Number.isFinite(secs)) return "";
  const ru = lang === "ru";
  if (secs < 60) return ru ? `${Math.max(1, Math.round(secs))} с` : `${Math.max(1, Math.round(secs))}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return ru ? `${m} мин` : `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return ru ? `${h} ч ${m % 60} мин` : `${h}h ${m % 60}m`;
  const d = Math.floor(h / 24);
  return ru ? `${d} д ${h % 24} ч` : `${d}d ${h % 24}h`;
}

export function percent(p: number): string {
  const v = Math.max(0, Math.min(1, p)) * 100;
  return `${v >= 100 ? 100 : v.toFixed(1)}%`;
}

export function date(ts: number | null | undefined, lang = "en"): string {
  if (!ts) return "";
  return new Date(ts * 1000).toLocaleString(lang === "ru" ? "ru-RU" : "en-US", {
    day: "2-digit",
    month: "2-digit",
    year: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function ratio(r: number): string {
  if (r >= 999) return "∞";
  return r.toFixed(2);
}
