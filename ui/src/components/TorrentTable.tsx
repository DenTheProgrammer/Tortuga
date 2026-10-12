import { MouseEvent, useLayoutEffect, useRef, useState } from "react";
import { ArrowDown, ArrowUp } from "lucide-react";
import type { TorrentState, TorrentSummary } from "../api";
import { useT } from "../i18n";
import * as fmt from "../format";
import { Logo } from "./Logo";
import { localStorageGet, localStorageSet } from "../native";

export type SortKey = "name" | "size" | "progress" | "state" | "down_speed" | "up_speed" | "eta" | "peers" | "ratio" | "added_at";

export const stateColor: Record<TorrentState, string> = {
  metadata: "var(--blue)",
  checking: "var(--yellow)",
  downloading: "var(--blue)",
  stalled: "var(--blue)",
  seeding: "var(--green)",
  completed: "var(--green)",
  paused: "var(--gray)",
  queued: "var(--gray)",
  error: "var(--red)",
};

const columns: { key: SortKey; label: Parameters<ReturnType<typeof useT>["t"]>[0]; width: number; min: number; num?: boolean }[] = [
  { key: "name", label: "name", width: 380, min: 120 },
  { key: "size", label: "size", width: 90, min: 65, num: true },
  { key: "progress", label: "progress", width: 140, min: 80 },
  { key: "state", label: "status", width: 190, min: 140 },
  { key: "down_speed", label: "down", width: 100, min: 74, num: true },
  { key: "up_speed", label: "up", width: 100, min: 70, num: true },
  { key: "eta", label: "eta", width: 90, min: 70, num: true },
  { key: "peers", label: "peers", width: 70, min: 50, num: true },
];

const WIDTHS_KEY = "tortuga.columnWidths";
const MIN_TABLE_WIDTH = columns.reduce((sum, c) => sum + c.min, 0);

/** Give spare space to the name; shrink it first, then the other columns. */
function fitWidths(preferred: number[], available: number) {
  const widths = [...preferred];
  let excess = widths.reduce((sum, w) => sum + w, 0) - Math.max(available, MIN_TABLE_WIDTH);
  const fromName = Math.min(Math.max(excess, 0), widths[0] - columns[0].min);
  widths[0] -= fromName;
  excess -= fromName;
  if (excess > 0) {
    const slack = widths.slice(1).reduce((sum, w, i) => sum + w - columns[i + 1].min, 0);
    for (let i = 1; i < widths.length; i++) {
      widths[i] -= excess * (widths[i] - columns[i].min) / slack;
    }
  } else {
    widths[0] -= excess;
  }
  return widths;
}

function loadWidths(): Record<string, number> {
  try {
    return JSON.parse(localStorageGet(WIDTHS_KEY) ?? "{}") ?? {};
  } catch {
    return {};
  }
}

/** Column widths the user dragged, remembered across restarts. */
function useColumnWidths() {
  const [custom, setCustom] = useState<Record<string, number>>(loadWidths);
  const save = (next: Record<string, number>) => {
    setCustom(next);
    localStorageSet(WIDTHS_KEY, JSON.stringify(next));
  };
  const width = (key: string) => {
    const col = columns.find((c) => c.key === key)!;
    return Math.max(col.min, Number.isFinite(custom[key]) ? custom[key] : col.width);
  };
  const startResize = (key: string, e: MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    // The name column may be wider than its setting (it takes the spare width).
    const th = (e.currentTarget as HTMLElement).parentElement;
    const start = th?.offsetWidth ?? width(key);
    let latest = custom;
    const move = (ev: globalThis.MouseEvent) => {
      latest = { ...latest, [key]: Math.max(columns.find((c) => c.key === key)!.min, Math.round(start + ev.clientX - startX)) };
      setCustom(latest);
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("col-resizing");
      save(latest);
    };
    document.body.classList.add("col-resizing");
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };
  const reset = (key: string) => {
    const next = { ...custom };
    delete next[key];
    save(next);
  };
  return { width, startResize, reset };
}

/** Renders torrents in the given (already filtered and sorted) order. */
export function TorrentTable({
  torrents,
  selected,
  sort,
  onSort,
  onRowMouseDown,
  onRowContext,
  onRowDoubleClick,
  onBackgroundClick,
}: {
  torrents: TorrentSummary[];
  selected: Set<string>;
  sort: { key: SortKey; desc: boolean };
  onSort: (key: SortKey) => void;
  onRowMouseDown: (id: string, e: MouseEvent) => void;
  onRowContext: (id: string, e: MouseEvent) => void;
  onRowDoubleClick: (id: string) => void;
  onBackgroundClick: () => void;
}) {
  const { t, lang } = useT();
  const cols = useColumnWidths();
  const wrap = useRef<HTMLDivElement>(null);
  const [available, setAvailable] = useState(0);
  useLayoutEffect(() => {
    const element = wrap.current!;
    const measure = () => setAvailable(element.clientWidth);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const widths = fitWidths(columns.map((c) => cols.width(c.key)), available);

  return (
    <div ref={wrap} className="table-wrap" onMouseDown={(e) => e.target === e.currentTarget && onBackgroundClick()}>
      <table className="grid" style={{ minWidth: MIN_TABLE_WIDTH }}>
        <colgroup>
          {columns.map((c, i) => (
            <col key={c.key} style={{ width: widths[i] }} />
          ))}
        </colgroup>
        <thead>
          <tr>
            {columns.map((c) => (
              <th key={c.key} title={t(c.label)} className={c.num ? "num" : ""} onClick={() => onSort(c.key)}>
                {t(c.label)}
                {sort.key === c.key &&
                  (sort.desc ? <ArrowDown size={11} style={{ marginLeft: 4 }} /> : <ArrowUp size={11} style={{ marginLeft: 4 }} />)}
                <span
                  className="col-resize"
                  onMouseDown={(e) => cols.startResize(c.key, e)}
                  onClick={(e) => e.stopPropagation()}
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    cols.reset(c.key);
                  }}
                />
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {torrents.map((tr) => (
            <tr
              key={tr.id}
              className={selected.has(tr.id) ? "selected" : ""}
              onMouseDown={(e) => onRowMouseDown(tr.id, e)}
              onContextMenu={(e) => onRowContext(tr.id, e)}
              onDoubleClick={() => onRowDoubleClick(tr.id)}
              title={tr.error ?? undefined}
            >
              <td className="name">{tr.name}</td>
              <td className="num dim">{tr.size ? fmt.bytes(tr.size, lang) : ""}</td>
              <td>
                <div className="pbar">
                  <div className={`c-${tr.state}`} style={{ width: `${tr.progress * 100}%` }} />
                  <span>{fmt.percent(tr.progress)}</span>
                </div>
              </td>
              <td>
                <span className="state" title={t(`st_${tr.state}` as never)}>
                  <span className="dot" style={{ background: stateColor[tr.state] }} />
                  <span className="state-label">{t(`st_${tr.state}` as never)}</span>
                </span>
              </td>
              <td className="num">{fmt.speed(tr.down_speed, lang)}</td>
              <td className="num dim">{fmt.speed(tr.up_speed, lang)}</td>
              <td className="num dim">{fmt.duration(tr.eta, lang)}</td>
              <td className="num dim">{tr.peers || tr.peers_seen ? `${tr.peers} (${tr.peers_seen})` : ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {torrents.length === 0 && (
        <div className="empty">
          <Logo className="logo" />
          <h3>{t("emptyTitle")}</h3>
          <div>{t("emptyText")}</div>
        </div>
      )}
    </div>
  );
}
