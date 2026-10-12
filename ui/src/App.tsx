import { MouseEvent as ReactMouseEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertCircle,
  ArrowDown,
  ArrowUp,
  CheckCircle2,
  Copy,
  Download,
  ExternalLink,
  FileDown,
  FolderOpen,
  ListOrdered,
  Moon,
  Network,
  PackagePlus,
  Pause,
  Play,
  Plus,
  Rss,
  Search,
  Settings as SettingsIcon,
  Shield,
  Sparkles,
  X,
  ShieldAlert,
  ShieldOff,
  Sun,
  Tag,
  Trash2,
  Turtle,
  Upload,
  Layers,
} from "lucide-react";
import { api, configure, FileEntry, Settings, StateResponse, TorrentSummary } from "./api";
import { I18nContext, Key, makeT, systemLang } from "./i18n";
import * as fmt from "./format";
import { canPause, canStart } from "./torrentActions";
import {
  backendInfo,
  copyText,
  detectPlayers,
  fileToBase64,
  isTauri,
  localStorageGet,
  localStorageSet,
  onFileDrop,
  onOpenSource,
  onUpdateAvailable,
  installUpdate,
  UpdateInfo,
  openInPlayer,
  openPath,
  PlayerInfo,
  revealPath,
} from "./native";
import { Logo } from "./components/Logo";
import { SortKey, TorrentTable } from "./components/TorrentTable";
import { Details } from "./components/Details";
import { AddDialog, AddSource } from "./components/AddDialog";
import { SettingsDialog } from "./components/SettingsDialog";
import { CreateDialog } from "./components/CreateDialog";
import { RssView } from "./components/RssView";
import { PlayerDialog } from "./components/PlayerDialog";
import { ContextMenu, Dialog, MenuItem, useToast } from "./components/ui";

type Filter = "all" | "downloading" | "seeding" | "completed" | "paused" | "errors" | `cat:${string}` | "rss";

const filterMatch: Record<string, (t: TorrentSummary) => boolean> = {
  all: () => true,
  downloading: (t) => ["downloading", "stalled", "metadata", "checking"].includes(t.state),
  seeding: (t) => t.state === "seeding",
  completed: (t) => t.progress >= 1,
  paused: (t) => t.state === "paused" || t.state === "queued" || t.state === "completed",
  errors: (t) => t.state === "error",
};

function looksLikeTorrentLink(s: string) {
  const v = s.trim();
  return v.startsWith("magnet:?") || /^[0-9a-f]{40}$/i.test(v) || /^https?:\/\/\S+\.torrent(\?|$)/i.test(v);
}

function applyTheme(theme: string) {
  const dark = theme === "system" ? matchMedia("(prefers-color-scheme: dark)").matches : theme !== "light";
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}

export default function App() {
  const [ready, setReady] = useState<"connecting" | "ok" | "error">("connecting");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [state, setState] = useState<StateResponse | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<{ key: SortKey; desc: boolean }>({ key: "added_at", desc: true });
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const [addSource, setAddSource] = useState<AddSource | null | undefined>(undefined);
  const [showSettings, setShowSettings] = useState(false);
  const [showCreate, setShowCreate] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState<{ ids: string[]; files: boolean } | null>(null);
  const [categoryFor, setCategoryFor] = useState<string[] | null>(null);
  const [player, setPlayer] = useState<{ url: string; name: string } | null>(null);
  const [players, setPlayers] = useState<PlayerInfo[]>([]);
  const [dragging, setDragging] = useState(false);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [updating, setUpdating] = useState(false);
  const [actionBusy, setActionBusy] = useState(false);
  const [detailsHeight, setDetailsHeight] = useState(() => Number(localStorageGet("tortuga.details")) || 300);
  const toast = useToast();

  const lang = settings?.language ?? systemLang();
  const t = useMemo(() => makeT(lang), [lang]);
  const i18n = useMemo(() => ({ t, lang }), [t, lang]);
  const err = useCallback((e: unknown) => toast(String((e as Error)?.message ?? e), true), [toast]);

  // ---- connect & poll ----
  useEffect(() => {
    let alive = true;
    (async () => {
      for (let attempt = 0; attempt < 40 && alive; attempt++) {
        try {
          const info = await backendInfo();
          configure(info.base, info.token);
          if (info.token) localStorageSet("tortuga.token", info.token);
          const s = await api.settings();
          if (!alive) return;
          setSettings(s);
          applyTheme(s.theme);
          setReady("ok");
          return;
        } catch {
          await new Promise((r) => setTimeout(r, 500));
        }
      }
      if (alive) setReady("error");
    })();
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    if (ready !== "ok") return;
    let alive = true;
    const tick = () =>
      api
        .state()
        .then((s) => alive && setState(s))
        .catch(() => {});
    tick();
    const h = setInterval(tick, 1000);
    detectPlayers().then(setPlayers).catch(() => {});
    return () => {
      alive = false;
      clearInterval(h);
    };
  }, [ready]);

  useEffect(() => {
    if (!settings) return;
    applyTheme(settings.theme);
    document.documentElement.lang = settings.language;
    if (settings.theme !== "system") return;
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const on = () => applyTheme("system");
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, [settings]);

  // ---- OS integration: magnet links, file association, drag & drop ----
  useEffect(() => {
    if (ready !== "ok") return;
    const unsubs: Promise<() => void>[] = [
      onOpenSource((source) => setAddSource({ source })),
      onFileDrop((paths) => paths[0] && setAddSource({ source: paths[0] })),
      onUpdateAvailable(setUpdate),
    ];
    return () => unsubs.forEach((u) => u.then((f) => f()));
  }, [ready]);

  useEffect(() => {
    if (isTauri) return;
    const over = (e: DragEvent) => {
      if (e.dataTransfer?.types.includes("Files")) {
        e.preventDefault();
        setDragging(true);
      }
    };
    const leave = (e: DragEvent) => !e.relatedTarget && setDragging(false);
    const drop = async (e: DragEvent) => {
      e.preventDefault();
      setDragging(false);
      const f = [...(e.dataTransfer?.files ?? [])].find((f) => f.name.endsWith(".torrent"));
      if (f) setAddSource({ torrent_base64: await fileToBase64(f), label: f.name, source: undefined });
    };
    window.addEventListener("dragover", over);
    window.addEventListener("dragleave", leave);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragover", over);
      window.removeEventListener("dragleave", leave);
      window.removeEventListener("drop", drop);
    };
  }, []);

  // ---- derived ----
  const torrents = state?.torrents ?? [];
  const visible = useMemo(() => {
    const q = search.trim().toLowerCase();
    let list = torrents;
    if (filter.startsWith("cat:")) {
      const c = filter.slice(4);
      list = list.filter((t) => (t.category ?? "") === c);
    } else if (filter !== "rss") {
      list = list.filter(filterMatch[filter]);
    }
    if (q) list = list.filter((t) => t.name.toLowerCase().includes(q));
    const k = sort.key;
    return [...list].sort((a, b) => {
      let r: number;
      if (k === "name" || k === "state") r = String(a[k]).localeCompare(String(b[k]));
      else if (k === "eta") r = (a.eta ?? Infinity) - (b.eta ?? Infinity);
      else r = (a[k] as number) - (b[k] as number);
      return sort.desc ? -r : r;
    });
  }, [torrents, filter, search, sort]);

  const counts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const k of Object.keys(filterMatch)) c[k] = torrents.filter(filterMatch[k]).length;
    return c;
  }, [torrents]);

  const selectedList = torrents.filter((t) => selected.has(t.id));
  const startIds = selectedList.filter(canStart).map((t) => t.id);
  const pauseIds = selectedList.filter(canPause).map((t) => t.id);
  const focused = selectedList.length === 1 ? selectedList[0] : null;

  // drop selection of torrents that disappeared
  useEffect(() => {
    if (!state) return;
    const ids = new Set(state.torrents.map((t) => t.id));
    setSelected((s) => {
      const n = new Set([...s].filter((id) => ids.has(id)));
      return n.size === s.size ? s : n;
    });
  }, [state]);

  // ---- actions ----
  const act = async (fn: (id: string) => Promise<unknown>, ids = [...selected]) => {
    if (!ids.length || actionBusy) return;
    setActionBusy(true);
    try {
      await Promise.all(ids.map(fn));
    } catch (e) {
      err(e);
    } finally {
      await api.state().then(setState).catch(() => {});
      setActionBusy(false);
    }
  };

  const doRemove = async (ids: string[], files: boolean) => {
    await act((id) => api.remove(id, files), ids);
    setSelected(new Set());
  };

  const saveSettings = async (patch: Partial<Settings>) => {
    if (!settings) return;
    try {
      setSettings(await api.saveSettings({ ...settings, ...patch }));
    } catch (e) {
      err(e);
    }
  };

  const play = async (torrentId: string, file: FileEntry, playerOverride?: string) => {
    try {
      const r = await api.play(torrentId, file.index);
      const url = new URL(r.path, (await backendInfo()).base).toString();
      const p = playerOverride ?? settings?.player ?? "builtin";
      if (p === "builtin" || !isTauri) setPlayer({ url, name: r.name });
      else await openInPlayer(url, r.name, p, settings?.player_path);
    } catch (e) {
      err(e);
    }
  };

  const streamUrl = async (torrentId: string, file: FileEntry) => {
    const r = await api.play(torrentId, file.index);
    return new URL(r.path, (await backendInfo()).base).toString();
  };

  // ---- selection ----
  const onRowMouseDown = (id: string, e: ReactMouseEvent) => {
    if (e.button === 2 && selected.has(id)) return;
    if (e.shiftKey && anchor) {
      const ids = visible.map((t) => t.id);
      const [a, b] = [ids.indexOf(anchor), ids.indexOf(id)].sort((x, y) => x - y);
      if (a >= 0) {
        setSelected(new Set(ids.slice(a, b + 1)));
        return;
      }
    }
    if (e.ctrlKey || e.metaKey) {
      setSelected((s) => {
        const n = new Set(s);
        if (n.has(id)) n.delete(id);
        else n.add(id);
        return n;
      });
    } else {
      setSelected(new Set([id]));
    }
    setAnchor(id);
  };

  const torrentMenu = (id: string, e: ReactMouseEvent) => {
    e.preventDefault();
    const ids = selected.has(id) ? [...selected] : [id];
    if (!selected.has(id)) setSelected(new Set([id]));
    const list = torrents.filter((t) => ids.includes(t.id));
    const one = list.length === 1 ? list[0] : null;
    const isPending = one && (one.state === "metadata" || (one.state === "error" && !one.size));
    const items: MenuItem[] = [
      { label: t("start"), icon: <Play />, onClick: () => act(api.resume, list.filter(canStart).map((t) => t.id)), disabled: actionBusy || !list.some(canStart) },
      { label: t("pause"), icon: <Pause />, onClick: () => act(api.pause, list.filter(canPause).map((t) => t.id)), disabled: actionBusy || !list.some(canPause) },
      { separator: true },
      {
        label: t("openFolder"),
        icon: <FolderOpen />,
        disabled: !one || !isTauri || !!isPending,
        onClick: () => one && revealPath(one.save_path).catch(err),
      },
      {
        label: t("copyMagnet"),
        icon: <Copy />,
        disabled: !one || !!isPending,
        onClick: async () => {
          if (!one) return;
          try {
            const d = await api.details(one.id);
            await copyText(d.magnet);
            toast(t("copied"));
          } catch (e) {
            err(e);
          }
        },
      },
      {
        label: t("saveTorrentFile"),
        icon: <FileDown />,
        disabled: !one || !!isPending,
        onClick: () => one && window.open(api.torrentFileUrl(one.id), "_blank"),
      },
      { separator: true },
      {
        label: `${one?.sequential ? "✓ " : ""}${t("sequential")}`,
        icon: <ListOrdered />,
        disabled: !one || !!isPending,
        onClick: () => one && api.setOptions(one.id, { sequential: !one.sequential }).catch(err),
      },
      { label: t("setCategory"), icon: <Tag />, disabled: !!isPending, onClick: () => setCategoryFor(ids) },
      { separator: true },
      { label: t("remove"), icon: <Trash2 />, danger: true, onClick: () => setConfirmRemove({ ids, files: false }) },
      {
        label: t("removeWithFiles"),
        icon: <Trash2 />,
        danger: true,
        disabled: !!isPending,
        onClick: () => setConfirmRemove({ ids, files: true }),
      },
    ];
    setMenu({ x: e.clientX, y: e.clientY, items });
  };

  const fileMenu = (torrentId: string, files: FileEntry[], e: ReactMouseEvent) => {
    e.preventDefault();
    if (!files.length) return;
    const file = files[0];
    const single = files.length === 1;
    const installed = players.filter((p) => p.path);
    const indices = files.map((f) => f.index);
    const items: MenuItem[] = [
      ...(single && file.streamable
        ? [
            { label: t("stream"), icon: <Play />, onClick: () => play(torrentId, file) },
            ...(isTauri
              ? [
                  ...(settings?.player !== "builtin"
                    ? [{ label: `${t("openIn")} ${t("pl_builtin")}`, icon: <ExternalLink />, onClick: () => play(torrentId, file, "builtin") }]
                    : []),
                  ...installed
                    .filter((p) => p.id !== settings?.player)
                    .map((p) => ({ label: `${t("openIn")} ${p.name}`, icon: <ExternalLink />, onClick: () => play(torrentId, file, p.id) })),
                ]
              : []),
            {
              label: t("copyStreamLink"),
              icon: <Copy />,
              onClick: async () => {
                try {
                  await copyText(await streamUrl(torrentId, file));
                  toast(t("copied"));
                } catch (e) {
                  err(e);
                }
              },
            },
            { separator: true },
          ]
        : []),
      {
        label: t("downloadInOrder"),
        icon: <ListOrdered />,
        onClick: () =>
          api
            .downloadInOrder(torrentId, indices)
            .then(() => toast(t("inOrderOk")))
            .catch(err),
      },
      { separator: true },
      ...(single
        ? [
            {
              label: t("openFile"),
              icon: <ExternalLink />,
              disabled: !isTauri || file.done < file.length,
              onClick: () => api.filePath(torrentId, file.index).then((p) => openPath(p.path)).catch(err),
            },
            {
              label: t("openFolder"),
              icon: <FolderOpen />,
              disabled: !isTauri,
              onClick: () =>
                api
                  .filePath(torrentId, file.index)
                  .then((p) => revealPath(file.done > 0 ? p.path : p.folder))
                  .catch(err),
            },
            { separator: true },
          ]
        : []),
      ...(["high", "normal", "low", "skip"] as const).map((p) => ({
        label: `${files.every((f) => f.priority === p) ? "✓ " : ""}${t(`pr_${p}` as Key)}`,
        onClick: () =>
          api.setPriorities(torrentId, Object.fromEntries(indices.map((i) => [i, p]))).catch(err),
      })),
    ];
    setMenu({ x: e.clientX, y: e.clientY, items });
  };

  // ---- keyboard ----
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      if (document.querySelector(".backdrop")) return;
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
        e.preventDefault();
        setSelected(new Set(visible.map((t) => t.id)));
      } else if (e.key === "Delete" && selected.size) {
        setConfirmRemove({ ids: [...selected], files: e.shiftKey });
      }
    };
    const onPaste = (e: ClipboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || document.querySelector(".backdrop")) return;
      const text = e.clipboardData?.getData("text") ?? "";
      if (looksLikeTorrentLink(text)) setAddSource({ source: text.trim() });
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("paste", onPaste);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("paste", onPaste);
    };
  }, [visible, selected]);

  // ---- splitter ----
  const dragSplit = (e: ReactMouseEvent) => {
    const startY = e.clientY;
    const startH = detailsHeight;
    const move = (ev: MouseEvent) => {
      const h = Math.max(120, Math.min(window.innerHeight - 220, startH - (ev.clientY - startY)));
      setDetailsHeight(h);
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      setDetailsHeight((h) => {
        localStorageSet("tortuga.details", String(h));
        return h;
      });
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  if (ready !== "ok" || !settings) {
    return (
      <I18nContext.Provider value={i18n}>
        <div className="empty">
          <Logo className="logo" />
          {ready === "error" ? <div className="error-text">{t("connectError")}</div> : <div className="spinner" />}
        </div>
      </I18nContext.Provider>
    );
  }

  const session = state?.session;
  const net = session?.network;
  const isDark = document.documentElement.dataset.theme !== "light";

  const sideItem = (f: Filter, label: string, icon: React.ReactNode, count?: number) => (
    <div className={`side-item ${filter === f ? "active" : ""}`} onClick={() => setFilter(f)}>
      {icon}
      {label}
      {count !== undefined && count > 0 && <span className="count">{count}</span>}
    </div>
  );

  return (
    <I18nContext.Provider value={i18n}>
      <div className="app" onContextMenu={(e) => e.preventDefault()}>
        <div className="topbar">
          <div className="brand">
            <Logo />
            Tortuga
          </div>
          <button className="btn primary" onClick={() => setAddSource(null)}>
            <Plus />
            {t("add")}
          </button>
          <button className="icon-btn" title={t("create")} onClick={() => setShowCreate(true)}>
            <PackagePlus />
          </button>
          <div className="sep" />
          <button className="icon-btn" title={t("start")} disabled={actionBusy || !startIds.length} onClick={() => act(api.resume, startIds)}>
            <Play />
          </button>
          <button className="icon-btn" title={t("pause")} disabled={actionBusy || !pauseIds.length} onClick={() => act(api.pause, pauseIds)}>
            <Pause />
          </button>
          <button
            className="icon-btn"
            title={t("remove")}
            disabled={actionBusy || !selectedList.length}
            onClick={() => setConfirmRemove({ ids: [...selected], files: false })}
          >
            <Trash2 />
          </button>
          <div className="spacer" />
          <div className="search">
            <Search />
            <input className="input" placeholder={t("search")} value={search} onChange={(e) => setSearch(e.target.value)} />
          </div>
          <button
            className={`icon-btn ${settings.alt_speed_enabled ? "on" : ""}`}
            title={t("altSpeed")}
            onClick={() => api.altSpeed(!settings.alt_speed_enabled).then(() => setSettings({ ...settings, alt_speed_enabled: !settings.alt_speed_enabled })).catch(err)}
          >
            <Turtle />
          </button>
          <button className="icon-btn" title={t("theme")} onClick={() => saveSettings({ theme: isDark ? "light" : "dark" })}>
            {isDark ? <Sun /> : <Moon />}
          </button>
          <button className="icon-btn" title={t("settings")} onClick={() => setShowSettings(true)}>
            <SettingsIcon />
          </button>
        </div>

        <div className="body">
          <div className="sidebar">
            {sideItem("all", t("all"), <Layers />, counts.all)}
            {sideItem("downloading", t("downloading"), <Download />, counts.downloading)}
            {sideItem("seeding", t("seeding"), <Upload />, counts.seeding)}
            {sideItem("completed", t("completed"), <CheckCircle2 />, counts.completed)}
            {sideItem("paused", t("paused"), <Pause />, counts.paused)}
            {counts.errors > 0 && sideItem("errors", t("errors"), <AlertCircle />, counts.errors)}
            {!!session?.categories.length && (
              <>
                <div className="side-title">{t("categories")}</div>
                {session.categories.map((c) =>
                  sideItem(`cat:${c}`, c, <Tag />, torrents.filter((t) => t.category === c).length),
                )}
              </>
            )}
            <div className="side-title" />
            {sideItem("rss", t("rss"), <Rss />)}
          </div>

          <div className="main">
            {update && (
              <div className="banner">
                <Sparkles />
                <span>
                  {t("updateAvailable")} <b>{update.version}</b>
                </span>
                <button
                  className="btn primary"
                  disabled={updating}
                  onClick={() => {
                    setUpdating(true);
                    installUpdate().catch((e) => {
                      setUpdating(false);
                      err(e);
                    });
                  }}
                >
                  {updating ? t("updating") : t("updateNow")}
                </button>
                <button className="icon-btn" onClick={() => setUpdate(null)} aria-label="close">
                  <X />
                </button>
              </div>
            )}
            {filter === "rss" ? (
              <RssView />
            ) : (
              <>
                <TorrentTable
                  torrents={visible}
                  selected={selected}
                  sort={sort}
                  onSort={(key) => setSort((s) => ({ key, desc: s.key === key ? !s.desc : key !== "name" }))}
                  onRowMouseDown={onRowMouseDown}
                  onRowContext={torrentMenu}
                  onRowDoubleClick={(id) => {
                    const tr = torrents.find((x) => x.id === id);
                    if (tr && isTauri) revealPath(tr.save_path).catch(err);
                  }}
                  onBackgroundClick={() => setSelected(new Set())}
                />
                <div className="splitter" onMouseDown={dragSplit} />
                <div className="details" style={{ height: detailsHeight }}>
                  <Details
                    id={focused && focused.state !== "metadata" && !(focused.state === "error" && !focused.size) ? focused.id : null}
                    waiting={
                      focused && (focused.state === "metadata" || (focused.state === "error" && !focused.size))
                        ? { error: focused.error }
                        : null
                    }
                    onPlay={play}
                    onFileContext={fileMenu}
                  />
                </div>
              </>
            )}
          </div>
        </div>

        <div className="statusbar">
          <span className="item" title={t("down")}>
            <ArrowDown style={{ color: "var(--blue)" }} />
            {fmt.speed(session?.down_speed ?? 0, lang) || "0"}
            {!!session?.download_limit && <span className="faint">/ {fmt.speed(session.download_limit * 1024, lang)}</span>}
          </span>
          <span className="item" title={t("up")}>
            <ArrowUp style={{ color: "var(--green)" }} />
            {fmt.speed(session?.up_speed ?? 0, lang) || "0"}
            {!!session?.upload_limit && <span className="faint">/ {fmt.speed(session.upload_limit * 1024, lang)}</span>}
          </span>
          {session?.alt_speed_active && (
            <span className="item" style={{ color: "var(--accent)" }}>
              <Turtle /> {t("slot_alt")}
            </span>
          )}
          <div className="spacer" />
          {net && (
            <span
              className="item"
              title={[net.vpn_name && `VPN: ${net.vpn_name}`, net.interface_name, t(`reason_${net.reason}` as Key)]
                .filter((x) => x && !String(x).startsWith("reason_"))
                .join(" · ")}
              style={{ color: net.route === "blocked" ? "var(--red)" : net.route === "direct" && net.vpn_active ? "var(--accent)" : undefined }}
            >
              {net.route === "blocked" ? <ShieldOff /> : net.route === "vpn" ? <Shield /> : net.vpn_active ? <ShieldAlert /> : <Network />}
              {t(`net_${net.route}` as Key)}
              {net.vpn_active && net.route === "system" && " (VPN)"}
            </span>
          )}
          {session?.restarting && <span className="item">{t("restarting")}</span>}
          <span className="item faint">
            {t("dhtNodes")}: {session?.dht_nodes ?? 0}
          </span>
          {session?.listen_port && (
            <span className="item faint">
              {t("port")}: {session.listen_port}
            </span>
          )}
        </div>
      </div>

      {menu && <ContextMenu {...menu} onClose={() => setMenu(null)} />}

      {addSource !== undefined && (
        <AddDialog
          initial={addSource}
          settings={settings}
          categories={session?.categories ?? []}
          onClose={() => setAddSource(undefined)}
          onAdded={(id) => setSelected(new Set([id]))}
        />
      )}
      {showSettings && <SettingsDialog settings={settings} onClose={() => setShowSettings(false)} onSaved={setSettings} onUpdate={setUpdate} />}
      {showCreate && <CreateDialog onClose={() => setShowCreate(false)} />}
      {player && (
        <PlayerDialog
          url={player.url}
          name={player.name}
          onClose={() => setPlayer(null)}
          onExternal={() => {
            const p = settings.player === "builtin" ? players.find((x) => x.path)?.id ?? "system" : settings.player;
            openInPlayer(player.url, player.name, p, settings.player_path).catch(err);
            setPlayer(null);
          }}
        />
      )}
      {confirmRemove && (
        <Dialog
          title={confirmRemove.files ? t("removeWithFiles") : t("remove")}
          onClose={() => setConfirmRemove(null)}
          footer={
            <>
              <button className="btn" onClick={() => setConfirmRemove(null)}>
                {t("cancel")}
              </button>
              <button
                className="btn primary"
                autoFocus
                onClick={() => {
                  doRemove(confirmRemove.ids, confirmRemove.files);
                  setConfirmRemove(null);
                }}
              >
                {t("remove")}
              </button>
            </>
          }
        >
          <div>{confirmRemove.files ? t("confirmRemoveFiles") : t("confirmRemove")}</div>
          <div className="hint">
            {torrents
              .filter((x) => confirmRemove.ids.includes(x.id))
              .map((x) => x.name)
              .slice(0, 5)
              .join(", ")}
          </div>
        </Dialog>
      )}
      {categoryFor && (
        <CategoryDialog
          categories={session?.categories ?? []}
          initial={torrents.find((x) => x.id === categoryFor[0])?.category ?? ""}
          onClose={() => setCategoryFor(null)}
          onSave={(c) => {
            act((id) => api.setOptions(id, { category: c || null }), categoryFor);
            setCategoryFor(null);
          }}
        />
      )}
      {dragging && (
        <div className="dropzone">
          <div>{t("dropHere")}</div>
        </div>
      )}
    </I18nContext.Provider>
  );
}

function CategoryDialog({
  categories,
  initial,
  onClose,
  onSave,
}: {
  categories: string[];
  initial: string;
  onClose: () => void;
  onSave: (c: string) => void;
}) {
  const t = useRef(makeT(document.documentElement.lang || systemLang())).current;
  const [value, setValue] = useState(initial);
  return (
    <Dialog
      title={t("category")}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {t("cancel")}
          </button>
          <button className="btn primary" onClick={() => onSave(value.trim())}>
            {t("save")}
          </button>
        </>
      }
    >
      <input
        className="input"
        autoFocus
        list="cat-list"
        value={value}
        placeholder={t("noCategory")}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && onSave(value.trim())}
      />
      <datalist id="cat-list">
        {categories.map((c) => (
          <option key={c} value={c} />
        ))}
      </datalist>
    </Dialog>
  );
}
