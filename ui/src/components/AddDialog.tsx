import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, FileUp, FolderOpen } from "lucide-react";
import { AddRequest, api, FilePriority, ResolveResponse, Settings } from "../api";
import { useT } from "../i18n";
import * as fmt from "../format";
import { isTauri, localStorageGet, localStorageSet, pickFolder, pickTorrentFile } from "../native";
import { Check, Dialog, Field, useToast } from "./ui";

const VIDEO = /\.(mp4|m4v|mkv|webm|mov|avi|ts|m2ts|mpg|mpeg|wmv|flv)$/i;

/** Same rule as the engine: 2+ videos making up most of the size = a series. */
function looksLikeSeries(files: { path: string; length: number }[]) {
  const videos = files.filter((f) => VIDEO.test(f.path));
  const total = files.reduce((s, f) => s + f.length, 0);
  const vbytes = videos.reduce((s, f) => s + f.length, 0);
  return videos.length >= 2 && vbytes * 10 >= total * 7;
}

const RECENT_KEY = "tortuga.recentFolders";
const RECENT_MAX = 5;

function folderKey(dir: string) {
  const path = dir.trim().replace(/\\/g, "/").replace(/\/$/, "");
  return /^(?:[a-z]:\/|\/\/)/i.test(path) ? path.toLowerCase() : path;
}

function recentFolders(): string[] {
  try {
    const v = JSON.parse(localStorageGet(RECENT_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string" && x.trim()).slice(0, RECENT_MAX) : [];
  } catch {
    return [];
  }
}

function rememberFolder(dir: string) {
  const d = dir.trim();
  if (!d) return;
  const next = [d, ...recentFolders().filter((x) => folderKey(x) !== folderKey(d))].slice(0, RECENT_MAX);
  localStorageSet(RECENT_KEY, JSON.stringify(next));
  return next;
}

/** Save folder: free text, a folder picker, and the last used folders. */
function FolderField({ value, defaultDir, onChange }: { value: string; defaultDir: string; onChange: (v: string) => void }) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const [folders, setFolders] = useState(recentFolders);
  const options = useMemo(() => [...folders, defaultDir].filter((p, i, all) => p && all.findIndex((x) => folderKey(x) === folderKey(p)) === i).slice(0, RECENT_MAX), [defaultDir, folders]);
  const choose = (path: string) => {
    onChange(path);
    setFolders(rememberFolder(path) ?? []);
    setOpen(false);
  };
  return (
    <div className="folder-field">
      <div className="row-flex">
        <input className="input" value={value} onChange={(e) => onChange(e.target.value)} />
        {options.length > 0 && (
          <button className="icon-btn" title={t("recentFolders")} aria-expanded={open} onClick={() => setOpen((o) => !o)}>
            <ChevronDown />
          </button>
        )}
        {isTauri && (
          <button className="icon-btn" title={t("browse")} onClick={async () => { const path = await pickFolder(value); if (path) choose(path); }}>
            <FolderOpen />
          </button>
        )}
      </div>
      {open && (
        <div className="folder-menu" onMouseLeave={() => setOpen(false)}>
          {options.map((o) => (
            <button
              key={o}
              className={o === value ? "active" : ""}
              title={o}
              onClick={() => {
                choose(o);
              }}
            >
              {o}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export interface AddSource {
  source?: string;
  torrent_base64?: string;
  label?: string;
}

export function AddDialog({
  initial,
  settings,
  categories,
  onClose,
  onAdded,
}: {
  initial: AddSource | null;
  settings: Settings;
  categories: string[];
  onClose: () => void;
  onAdded: (id: string) => void;
}) {
  const { t, lang } = useT();
  const toast = useToast();
  const [text, setText] = useState(initial?.source ?? "");
  const [src, setSrc] = useState<AddSource | null>(initial);
  const [resolving, setResolving] = useState(false);
  const [resolved, setResolved] = useState<ResolveResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [wanted, setWanted] = useState<Set<number>>(new Set());
  const [savePath, setSavePath] = useState(() => recentFolders()[0] ?? settings.download_dir);
  const [category, setCategory] = useState("");
  const [sequential, setSequential] = useState(settings.default_sequential);
  const [start, setStart] = useState(!settings.start_paused);
  const [busy, setBusy] = useState(false);
  const reqId = useRef(0);

  const request = (s: AddSource): AddRequest => ({ source: s.source, torrent_base64: s.torrent_base64 });

  useEffect(() => {
    if (!src || (!src.source?.trim() && !src.torrent_base64)) return;
    const my = ++reqId.current;
    setResolving(true);
    setResolved(null);
    setError(null);
    api
      .resolve(request(src))
      .then((r) => {
        if (my !== reqId.current) return;
        setResolved(r);
        setWanted(new Set(r.files.map((f) => f.index)));
        if (looksLikeSeries(r.files)) setSequential(true);
      })
      .catch((e) => my === reqId.current && setError(String(e.message ?? e)))
      .finally(() => my === reqId.current && setResolving(false));
  }, [src]);

  const submitText = () => {
    const v = text.trim();
    if (v) setSrc({ source: v });
  };

  const chooseFile = async () => {
    const f = await pickTorrentFile();
    if (!f) return;
    setText(f.path ?? f.name);
    setSrc({ source: f.path, torrent_base64: f.base64, label: f.name });
  };

  // One button: with the file list loaded it adds the chosen files, before
  // that it adds the whole torrent right away (files can be skipped later).
  const add = async () => {
    const s = src ?? (text.trim() ? { source: text.trim() } : null);
    if (!s) return;
    setBusy(true);
    try {
      const priorities: FilePriority[] | undefined = resolved
        ? resolved.files.map((f) => (wanted.has(f.index) ? "normal" : "skip"))
        : undefined;
      const r = await api.add({
        ...request(s),
        save_path: savePath !== settings.download_dir ? savePath : undefined,
        paused: !start,
        // Before the file list is known, let the backend detect series itself.
        sequential: resolved ? sequential : sequential || undefined,
        category: category || undefined,
        priorities,
      });
      rememberFolder(savePath);
      toast(r.already_added ? t("alreadyAdded") : t("added_ok"));
      onAdded(r.id);
      onClose();
    } catch (e) {
      setError(String((e as Error).message ?? e));
    } finally {
      setBusy(false);
    }
  };

  const selectedSize = useMemo(
    () => resolved?.files.filter((f) => wanted.has(f.index)).reduce((s, f) => s + f.length, 0) ?? 0,
    [resolved, wanted],
  );

  const toggleAll = (on: boolean) => setWanted(on ? new Set(resolved?.files.map((f) => f.index)) : new Set());

  return (
    <Dialog
      title={t("addTorrent")}
      onClose={onClose}
      wide={!!resolved}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {t("cancel")}
          </button>
          <button
            className="btn primary"
            disabled={busy || (!src && !text.trim()) || (!!resolved && (resolved.already_added || wanted.size === 0))}
            onClick={add}
          >
            {t("add")}
          </button>
        </>
      }
    >
      <div className="row-flex">
        <input
          className="input"
          autoFocus={!initial}
          placeholder={t("addPlaceholder")}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && submitText()}
          onBlur={() => text.trim() && text.trim() !== (src?.source ?? src?.label) && submitText()}
          onPaste={(e) => {
            const v = e.clipboardData.getData("text").trim();
            if (v) {
              e.preventDefault();
              setText(v);
              setSrc({ source: v });
            }
          }}
        />
        <button className="btn" onClick={chooseFile}>
          <FileUp />
          {t("chooseFile")}
        </button>
      </div>

      {resolving && (
        <div className="note row-flex">
          <div className="spinner" />
          <div>
            <div style={{ color: "var(--text)" }}>{t("fetchingMeta")}</div>
            <div>{t("fetchingMetaHint")}</div>
          </div>
        </div>
      )}
      {error && <div className="error-text">{error}</div>}

      {resolved && (
        <>
          <div>
            <b style={{ userSelect: "text" }}>{resolved.name}</b>
            <div className="hint">
              {t("selected")}: {fmt.bytes(selectedSize, lang)} {t("of")} {fmt.bytes(resolved.total_size, lang)}
            </div>
          </div>
          {resolved.already_added && <div className="note">{t("alreadyAdded")}</div>}
          {resolved.files.length > 1 && (
            <div className="box">
              <div className="ftree">
                <div className="row head" style={{ gridTemplateColumns: "1fr 100px" }}>
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={wanted.size === resolved.files.length}
                      onChange={(e) => toggleAll(e.target.checked)}
                    />
                    {t("name")}
                  </label>
                  <div style={{ textAlign: "right" }}>{t("size")}</div>
                </div>
                {[...resolved.files].sort((a, b) => a.path.localeCompare(b.path, undefined, { numeric: true })).map((f) => (
                  <div key={f.index} className="row" style={{ gridTemplateColumns: "1fr 100px" }}>
                    <label className="check" style={{ minWidth: 0 }}>
                      <input
                        type="checkbox"
                        checked={wanted.has(f.index)}
                        onChange={(e) =>
                          setWanted((w) => {
                            const n = new Set(w);
                            if (e.target.checked) n.add(f.index);
                            else n.delete(f.index);
                            return n;
                          })
                        }
                      />
                      <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={f.path}>
                        {f.path}
                      </span>
                    </label>
                    <div className="dim" style={{ textAlign: "right" }}>
                      {fmt.bytes(f.length, lang)}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </>
      )}

      <div className="grid2">
        <Field label={t("savePath")}>
          <FolderField value={savePath} defaultDir={settings.download_dir} onChange={setSavePath} />
        </Field>
        <Field label={t("category")}>
          <input className="input" list="categories" value={category} onChange={(e) => setCategory(e.target.value)} />
          <datalist id="categories">
            {categories.map((c) => (
              <option key={c} value={c} />
            ))}
          </datalist>
        </Field>
      </div>
      <div className="row-flex" style={{ gap: 20 }}>
        <Check checked={start} onChange={setStart}>
          {t("startImmediately")}
        </Check>
        <Check checked={sequential} onChange={setSequential}>
          <span title={t("sequentialHint")}>{t("sequential")}</span>
        </Check>
      </div>
    </Dialog>
  );
}
