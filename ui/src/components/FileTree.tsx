import { MouseEvent, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, File, FileVideo, Folder, Play } from "lucide-react";
import type { FileEntry, FilePriority } from "../api";
import { useT } from "../i18n";
import * as fmt from "../format";

interface Node {
  name: string;
  path: string;
  file?: FileEntry;
  children: Node[];
  length: number;
  done: number;
  indices: number[];
}

function buildTree(files: FileEntry[]): Node {
  const root: Node = { name: "", path: "", children: [], length: 0, done: 0, indices: [] };
  for (const f of files) {
    const parts = f.path.split("/");
    let cur = root;
    parts.forEach((part, i) => {
      const isFile = i === parts.length - 1;
      const path = parts.slice(0, i + 1).join("/");
      let next = cur.children.find((c) => c.name === part && !!c.file === isFile);
      if (!next) {
        next = { name: part, path, children: [], length: 0, done: 0, indices: [], file: isFile ? f : undefined };
        cur.children.push(next);
      }
      cur = next;
    });
  }
  const sum = (n: Node) => {
    if (n.file) {
      n.length = n.file.length;
      n.done = n.file.done;
      n.indices = [n.file.index];
      return;
    }
    n.children.forEach(sum);
    n.children.sort((a, b) => Number(!!a.file) - Number(!!b.file) || a.name.localeCompare(b.name, undefined, { numeric: true }));
    n.length = n.children.reduce((s, c) => s + c.length, 0);
    n.done = n.children.reduce((s, c) => s + c.done, 0);
    n.indices = n.children.flatMap((c) => c.indices);
  };
  sum(root);
  return root;
}

const PRIORITIES: FilePriority[] = ["high", "normal", "low", "skip"];

export function FileTree({
  files,
  onPriority,
  onPlay,
  onContext,
}: {
  files: FileEntry[];
  onPriority: (indices: number[], p: FilePriority) => void;
  onPlay: (file: FileEntry) => void;
  onContext: (files: FileEntry[], e: MouseEvent) => void;
}) {
  const { t, lang } = useT();
  const tree = useMemo(() => buildTree(files), [files]);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [anchor, setAnchor] = useState<number | null>(null);
  const byIndex = useMemo(() => new Map(files.map((f) => [f.index, f])), [files]);

  const priorityOf = (n: Node): FilePriority | "" => {
    const ps = new Set(n.indices.map((i) => byIndex.get(i)?.priority));
    return ps.size === 1 ? ([...ps][0] as FilePriority) : "";
  };

  const rows: { node: Node; depth: number }[] = [];
  const walk = (n: Node, depth: number) => {
    for (const c of n.children) {
      rows.push({ node: c, depth });
      if (!c.file && !collapsed.has(c.path)) walk(c, depth + 1);
    }
  };
  // A single top-level folder is just the torrent name; skip it.
  const top = tree.children.length === 1 && !tree.children[0].file ? tree.children[0] : tree;
  walk(top, 0);

  const isSelected = (n: Node) => n.indices.length > 0 && n.indices.every((i) => selected.has(i));

  const select = (rowIdx: number, e: MouseEvent) => {
    const node = rows[rowIdx].node;
    if (e.shiftKey && anchor !== null) {
      const [a, b] = [anchor, rowIdx].sort((x, y) => x - y);
      setSelected(new Set(rows.slice(a, b + 1).flatMap((r) => r.node.indices)));
      return;
    }
    if (e.ctrlKey || e.metaKey) {
      setSelected((s) => {
        const n = new Set(s);
        const on = !isSelected(node);
        node.indices.forEach((i) => (on ? n.add(i) : n.delete(i)));
        return n;
      });
    } else {
      setSelected(new Set(node.indices));
    }
    setAnchor(rowIdx);
  };

  const toggle = (path: string) =>
    setCollapsed((s) => {
      const n = new Set(s);
      if (n.has(path)) n.delete(path);
      else n.add(path);
      return n;
    });

  return (
    <div className="ftree">
      <div className="row head">
        <div>{t("name")}</div>
        <div style={{ textAlign: "right" }}>{t("size")}</div>
        <div>{t("progress")}</div>
        <div>{t("priority")}</div>
        <div />
      </div>
      {rows.map(({ node, depth }, rowIdx) => {
        const p = priorityOf(node);
        const progress = node.length ? node.done / node.length : 0;
        const f = node.file;
        return (
          <div
            key={node.path + (f ? `#${f.index}` : "")}
            className={`row ${p === "skip" ? "skipped" : ""} ${isSelected(node) ? "selected" : ""}`}
            onMouseDown={(e) => {
              if ((e.target as HTMLElement).closest("select, button, .twisty")) return;
              if (e.button === 2 && isSelected(node)) return;
              select(rowIdx, e);
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              const sel = isSelected(node) ? selected : new Set(node.indices);
              onContext(
                [...sel].map((i) => byIndex.get(i)!).filter(Boolean),
                e,
              );
            }}
            onDoubleClick={() => f?.streamable && onPlay(f)}
          >
            <div className="fname" style={{ paddingLeft: depth * 18 }}>
              {f ? (
                <span className="twisty" />
              ) : (
                <span className="twisty" onClick={() => toggle(node.path)}>
                  {collapsed.has(node.path) ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
                </span>
              )}
              {f ? f.streamable ? <FileVideo /> : <File /> : <Folder />}
              <span title={node.path}>{node.name}</span>
            </div>
            <div className="dim" style={{ textAlign: "right" }}>
              {fmt.bytes(node.length, lang)}
            </div>
            <div className="pbar">
              <div className={progress >= 1 ? "c-seeding" : "c-downloading"} style={{ width: `${progress * 100}%` }} />
              <span>{fmt.percent(progress)}</span>
            </div>
            <select
              className="input prio"
              value={p}
              onChange={(e) => onPriority(node.indices, e.target.value as FilePriority)}
            >
              {p === "" && <option value="">—</option>}
              {PRIORITIES.map((x) => (
                <option key={x} value={x}>
                  {t(`pr_${x}` as never)}
                </option>
              ))}
            </select>
            <div>
              {f?.streamable && (
                <button className="icon-btn play-btn" title={t("stream")} onClick={() => onPlay(f)}>
                  <Play />
                </button>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
