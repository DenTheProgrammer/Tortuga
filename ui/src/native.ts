// Desktop integration (Tauri) with plain-browser fallbacks for the web UI.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export interface BackendInfo {
  base: string;
  token: string;
}

export async function backendInfo(): Promise<BackendInfo> {
  if (isTauri) return invoke<BackendInfo>("backend_info");
  const hash = new URLSearchParams(location.hash.slice(1));
  const query = new URLSearchParams(location.search);
  return {
    base: query.get("api") ?? location.origin,
    token: hash.get("token") ?? query.get("token") ?? localStorageGet("tortuga.token") ?? "",
  };
}

export function localStorageGet(k: string): string | null {
  try {
    return localStorage.getItem(k);
  } catch {
    return null;
  }
}

export function localStorageSet(k: string, v: string) {
  try {
    localStorage.setItem(k, v);
  } catch {
    /* ignore */
  }
}

function readAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split(",")[1] ?? "");
    r.onerror = () => reject(r.error);
    r.readAsDataURL(file);
  });
}

/** Returns either a local path (desktop) or base64 file contents (browser). */
export async function pickTorrentFile(): Promise<{ path?: string; base64?: string; name: string } | null> {
  if (isTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const p = await open({ multiple: false, filters: [{ name: "Torrent", extensions: ["torrent"] }] });
    if (!p || Array.isArray(p)) return null;
    return { path: p, name: p.split(/[\\/]/).pop() ?? p };
  }
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".torrent,application/x-bittorrent";
    input.onchange = async () => {
      const f = input.files?.[0];
      resolve(f ? { base64: await readAsBase64(f), name: f.name } : null);
    };
    input.click();
  });
}

export async function fileToBase64(f: File) {
  return readAsBase64(f);
}

export async function pickFolder(defaultPath?: string): Promise<string | null> {
  if (!isTauri) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const p = await open({ directory: true, defaultPath });
  return typeof p === "string" ? p : null;
}

export async function pickAnyPath(directory: boolean): Promise<string | null> {
  if (!isTauri) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const p = await open({ directory, multiple: false });
  return typeof p === "string" ? p : null;
}

export async function pickSavePath(defaultName: string): Promise<string | null> {
  if (!isTauri) return null;
  const { save } = await import("@tauri-apps/plugin-dialog");
  return save({ defaultPath: defaultName, filters: [{ name: "Torrent", extensions: ["torrent"] }] });
}

export async function pickExecutable(): Promise<string | null> {
  if (!isTauri) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const p = await open({ multiple: false });
  return typeof p === "string" ? p : null;
}

export async function revealPath(path: string) {
  if (!isTauri) return;
  const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
  await revealItemInDir(path);
}

export async function openPath(path: string) {
  if (!isTauri) return;
  const { openPath } = await import("@tauri-apps/plugin-opener");
  await openPath(path);
}

export interface PlayerInfo {
  id: string;
  name: string;
  path: string | null;
}

export async function detectPlayers(): Promise<PlayerInfo[]> {
  if (!isTauri) return [];
  return invoke<PlayerInfo[]>("detect_players");
}

/** Open a stream URL in an external player. Returns false if not possible. */
export async function openInPlayer(url: string, title: string, player: string, playerPath?: string | null) {
  if (!isTauri) {
    window.open(url, "_blank");
    return true;
  }
  await invoke("open_in_player", { url, title, player, playerPath: playerPath ?? null });
  return true;
}

export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}

/** Magnet links / .torrent files passed by the OS (file association, second instance). */
export async function onOpenSource(cb: (source: string) => void) {
  if (!isTauri) return () => {};
  const pending = await invoke<string[]>("take_pending_sources");
  pending.forEach(cb);
  return listen<string>("open-source", (e) => cb(e.payload));
}

export async function onFileDrop(cb: (paths: string[]) => void) {
  if (!isTauri) return () => {};
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((e) => {
    if (e.payload.type === "drop") cb(e.payload.paths.filter((p) => p.toLowerCase().endsWith(".torrent")));
  });
}

export async function defaultHandlerStatus(): Promise<{ supported: boolean; is_default: boolean }> {
  if (!isTauri) return { supported: false, is_default: true };
  return invoke("default_handler_status");
}

export async function makeDefaultHandler() {
  if (!isTauri) return;
  await invoke("make_default_handler");
}

export interface UpdateInfo {
  version: string;
  current_version: string;
  notes: string | null;
}

/** Calls `cb` with an available update (now or when one is found later). */
export async function onUpdateAvailable(cb: (u: UpdateInfo) => void) {
  if (!isTauri) return () => {};
  const unlisten = await listen<UpdateInfo>("update-available", (e) => cb(e.payload));
  try {
    const now = await invoke<UpdateInfo | null>("pending_update");
    if (now) cb(now);
    return unlisten;
  } catch (e) {
    unlisten();
    throw e;
  }
}

export async function appVersion(): Promise<string> {
  return isTauri ? getVersion() : __APP_VERSION__;
}

export async function checkUpdate(): Promise<UpdateInfo | null> {
  return isTauri ? invoke("check_update") : null;
}

export async function installUpdate() {
  await invoke("install_update");
}
