import { MouseEvent } from "react";
import { ArrowDown, ArrowUp } from "lucide-react";
import type { TorrentState, TorrentSummary } from "../api";
import { useT } from "../i18n";
import * as fmt from "../format";
import { Logo } from "./Logo";

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

const columns: { key: SortKey; label: Parameters<ReturnType<typeof useT>["t"]>[0]; width?: number; num?: boolean }[] = [
  { key: "name", label: "name" },
  { key: "size", label: "size", width: 90, num: true },
  { key: "progress", label: "progress", width: 140 },
  { key: "state", label: "status", width: 150 },
  { key: "down_speed", label: "down", width: 100, num: true },
  { key: "up_speed", label: "up", width: 100, num: true },
  { key: "eta", label: "eta", width: 90, num: true },
  { key: "peers", label: "peers", width: 70, num: true },
];

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

  return (
    <div className="table-wrap" onMouseDown={(e) => e.target === e.currentTarget && onBackgroundClick()}>
      <table className="grid">
        <colgroup>
          {columns.map((c) => (
            <col key={c.key} style={c.width ? { width: c.width } : undefined} />
          ))}
        </colgroup>
        <thead>
          <tr>
            {columns.map((c) => (
              <th key={c.key} className={c.num ? "num" : ""} onClick={() => onSort(c.key)}>
                {t(c.label)}
                {sort.key === c.key &&
                  (sort.desc ? <ArrowDown size={11} style={{ marginLeft: 4 }} /> : <ArrowUp size={11} style={{ marginLeft: 4 }} />)}
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
                <span className="state">
                  <span className="dot" style={{ background: stateColor[tr.state] }} />
                  {t(`st_${tr.state}` as never)}
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
