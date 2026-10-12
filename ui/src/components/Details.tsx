import { MouseEvent, useEffect, useRef, useState } from "react";
import { api, FileEntry, FilePriority, PeerEntry, TorrentDetails } from "../api";
import { useT } from "../i18n";
import * as fmt from "../format";
import { FileTree } from "./FileTree";
import { useToast } from "./ui";

type Tab = "files" | "general" | "peers" | "trackers" | "speed";

function usePoll<T>(fn: (() => Promise<T>) | null, ms: number, deps: unknown[]) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const fnRef = useRef(fn);
  fnRef.current = fn;
  useEffect(() => {
    setData(null);
    setError(null);
    if (!fn) return;
    let alive = true;
    const run = async () => {
      try {
        const d = await fnRef.current!();
        if (alive) {
          setData(d);
          setError(null);
        }
      } catch (e) {
        if (alive) setError(String((e as Error).message ?? e));
      }
    };
    run();
    const h = setInterval(run, ms);
    return () => {
      alive = false;
      clearInterval(h);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return { data, error, refresh: () => fnRef.current?.().then(setData).catch(() => {}) };
}

export function Details({
  id,
  onPlay,
  onFileContext,
}: {
  id: string | null;
  onPlay: (torrentId: string, file: FileEntry) => void;
  onFileContext: (torrentId: string, file: FileEntry, e: MouseEvent) => void;
}) {
  const { t } = useT();
  const toast = useToast();
  const [tab, setTab] = useState<Tab>("files");
  const details = usePoll(id ? () => api.details(id) : null, 1500, [id]);

  const setPriority = async (indices: number[], p: FilePriority) => {
    if (!id) return;
    try {
      await api.setPriorities(id, Object.fromEntries(indices.map((i) => [i, p])));
      details.refresh();
    } catch (e) {
      toast(String((e as Error).message), true);
    }
  };

  const tabs: Tab[] = ["files", "general", "peers", "trackers", "speed"];
  return (
    <>
      <div className="tabs">
        {tabs.map((k) => (
          <button key={k} className={`tab ${tab === k ? "active" : ""}`} onClick={() => setTab(k)}>
            {t(k)}
          </button>
        ))}
      </div>
      <div className="tab-body">
        {tab === "speed" ? (
          <SpeedGraph />
        ) : !id ? (
          <div className="placeholder">{t("noSelection")}</div>
        ) : !details.data ? (
          <div className="placeholder">{details.error ?? t("st_metadata")}</div>
        ) : tab === "files" ? (
          <FileTree
            files={details.data.files}
            onPriority={setPriority}
            onPlay={(f) => onPlay(id, f)}
            onContext={(f, e) => onFileContext(id, f, e)}
          />
        ) : tab === "general" ? (
          <General d={details.data} />
        ) : tab === "peers" ? (
          <Peers id={id} />
        ) : (
          <Trackers d={details.data} />
        )}
      </div>
    </>
  );
}

function PieceBar({ id }: { id: string }) {
  const ref = useRef<HTMLCanvasElement>(null);
  const pieces = usePoll(() => api.pieces(id), 3000, [id]);
  useEffect(() => {
    const c = ref.current;
    const p = pieces.data;
    if (!c || !p) return;
    const w = (c.width = c.clientWidth * devicePixelRatio);
    const h = (c.height = c.clientHeight * devicePixelRatio);
    const ctx = c.getContext("2d")!;
    ctx.clearRect(0, 0, w, h);
    const bytes = Uint8Array.from(atob(p.bitfield), (ch) => ch.charCodeAt(0));
    const has = (i: number) => (bytes[i >> 3] >> (7 - (i & 7))) & 1;
    ctx.fillStyle = getComputedStyle(c).getPropertyValue("--blue") || "#4f9dff";
    // One column per pixel: fill if most pieces in that bucket are present.
    for (let x = 0; x < w; x++) {
      const from = Math.floor((x / w) * p.total);
      const to = Math.max(from + 1, Math.floor(((x + 1) / w) * p.total));
      let n = 0;
      for (let i = from; i < to; i++) n += has(i);
      if (n) {
        ctx.globalAlpha = Math.max(0.35, n / (to - from));
        ctx.fillRect(x, 0, 1, h);
      }
    }
  }, [pieces.data]);
  return (
    <div className="piecebar">
      <canvas ref={ref} />
    </div>
  );
}

function General({ d }: { d: TorrentDetails }) {
  const { t, lang } = useT();
  const s = d.summary;
  const rows: [string, string][] = [
    [t("savePath"), s.save_path],
    [t("totalSize"), fmt.bytes(d.total_size, lang)],
    [t("downloaded"), fmt.bytes(s.downloaded, lang)],
    [t("uploaded"), fmt.bytes(s.uploaded, lang)],
    [t("ratio"), fmt.ratio(s.ratio)],
    [t("pieces"), `${d.pieces} × ${fmt.bytes(d.piece_length, lang)}`],
    [t("addedAt"), fmt.date(s.added_at, lang)],
    [t("completedAt"), fmt.date(s.completed_at, lang)],
    [t("infoHash"), d.info_hash],
    [t("category"), s.category ?? "—"],
  ];
  return (
    <div>
      <PieceBar id={d.info_hash} />
      <div className="kv">
        {rows.map(([k, v]) => (
          <div key={k} style={{ display: "contents" }}>
            <div className="k">{k}</div>
            <div className="v" title={v}>
              {v}
            </div>
          </div>
        ))}
        {s.error && (
          <>
            <div className="k">{t("error")}</div>
            <div className="v error-text" style={{ gridColumn: "span 3", whiteSpace: "normal" }}>
              {s.error}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function Peers({ id }: { id: string }) {
  const { t, lang } = useT();
  const peers = usePoll<PeerEntry[]>(() => api.peers(id), 2000, [id]);
  if (!peers.data?.length) return <div className="placeholder">{t("noPeers")}</div>;
  return (
    <table className="grid">
      <thead>
        <tr>
          <th>{t("address")}</th>
          <th>{t("client")}</th>
          <th style={{ width: 90 }}>{t("connection")}</th>
          <th className="num" style={{ width: 110 }}>{t("down")}</th>
          <th className="num" style={{ width: 110 }}>{t("up")}</th>
          <th className="num" style={{ width: 110 }}>{t("downloaded")}</th>
          <th className="num" style={{ width: 110 }}>{t("uploaded")}</th>
        </tr>
      </thead>
      <tbody>
        {peers.data.map((p) => (
          <tr key={p.addr}>
            <td style={{ userSelect: "text" }}>{p.addr}</td>
            <td className="dim">{p.client ?? ""}</td>
            <td className="dim">{p.connection ?? ""}</td>
            <td className="num">{fmt.speed(p.down_speed, lang)}</td>
            <td className="num dim">{fmt.speed(p.up_speed, lang)}</td>
            <td className="num dim">{fmt.bytes(p.downloaded, lang)}</td>
            <td className="num dim">{fmt.bytes(p.uploaded, lang)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function Trackers({ d }: { d: TorrentDetails }) {
  const { t } = useT();
  return (
    <table className="grid">
      <tbody>
        {d.trackers.map((tr) => (
          <tr key={tr}>
            <td style={{ userSelect: "text" }}>{tr}</td>
          </tr>
        ))}
        <tr>
          <td className="dim">{t("dhtLsd")}</td>
        </tr>
      </tbody>
    </table>
  );
}

function SpeedGraph() {
  const { t, lang } = useT();
  const hist = usePoll(() => api.speed(), 2000, []);
  const data = hist.data ?? [];
  const W = 600;
  const H = 150;
  const max = Math.max(64 * 1024, ...data.map(([, d, u]) => Math.max(d, u)));
  const line = (idx: 1 | 2) =>
    data
      .map((p, i) => `${(i / Math.max(1, data.length - 1)) * W},${H - (p[idx] / max) * (H - 8)}`)
      .join(" ");
  const last = data[data.length - 1];
  return (
    <div className="graph">
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none">
        {[0.25, 0.5, 0.75].map((f) => (
          <line key={f} x1={0} x2={W} y1={H * f} y2={H * f} stroke="var(--line)" strokeWidth={1} vectorEffect="non-scaling-stroke" />
        ))}
        <polyline points={line(1)} fill="none" stroke="var(--blue)" strokeWidth={2} vectorEffect="non-scaling-stroke" />
        <polyline points={line(2)} fill="none" stroke="var(--green)" strokeWidth={2} vectorEffect="non-scaling-stroke" />
      </svg>
      <div className="legend">
        <span>
          <i style={{ background: "var(--blue)" }} />
          {t("down")}: {fmt.speed(last?.[1] ?? 0, lang) || "0"}
        </span>
        <span>
          <i style={{ background: "var(--green)" }} />
          {t("up")}: {fmt.speed(last?.[2] ?? 0, lang) || "0"}
        </span>
        <span className="faint" style={{ marginLeft: "auto" }}>
          max {fmt.speed(max, lang)}
        </span>
      </div>
    </div>
  );
}
