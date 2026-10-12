// Client for the local Tortuga HTTP API (see crates/tortuga-core/src/server.rs).

export type TorrentState =
  | "metadata"
  | "checking"
  | "downloading"
  | "stalled"
  | "seeding"
  | "paused"
  | "completed"
  | "queued"
  | "error";

export type FilePriority = "skip" | "low" | "normal" | "high";

export interface TorrentSummary {
  id: string;
  name: string;
  state: TorrentState;
  progress: number;
  size: number;
  done: number;
  down_speed: number;
  up_speed: number;
  eta: number | null;
  peers: number;
  peers_seen: number;
  ratio: number;
  downloaded: number;
  uploaded: number;
  added_at: number;
  completed_at: number | null;
  category: string | null;
  save_path: string;
  error: string | null;
  sequential: boolean;
  user_paused: boolean;
}

export type Route = "system" | "direct" | "vpn" | "blocked";
export type NetworkMode = "auto" | "system" | "direct" | "vpn_only";
export type ScheduleSlot = "normal" | "alt" | "off";

export interface NetStatus {
  mode: NetworkMode;
  route: Route;
  vpn_active: boolean;
  vpn_name: string | null;
  interface_name: string | null;
  reason: string;
}

export interface SessionInfo {
  down_speed: number;
  up_speed: number;
  dht_nodes: number;
  listen_port: number | null;
  download_limit: number;
  upload_limit: number;
  alt_speed_active: boolean;
  scheduler_slot: ScheduleSlot;
  network: NetStatus;
  categories: string[];
  restarting: boolean;
}

export interface StateResponse {
  torrents: TorrentSummary[];
  session: SessionInfo;
}

export interface FileEntry {
  index: number;
  path: string;
  length: number;
  done: number;
  priority: FilePriority;
  mime: string | null;
  streamable: boolean;
}

export interface TorrentDetails {
  summary: TorrentSummary;
  info_hash: string;
  magnet: string;
  piece_length: number;
  pieces: number;
  total_size: number;
  files: FileEntry[];
  trackers: string[];
  comment: string | null;
  private: boolean;
}

export interface PeerEntry {
  addr: string;
  client: string | null;
  connection: string | null;
  downloaded: number;
  uploaded: number;
  down_speed: number;
  up_speed: number;
  state: string;
}

export interface ResolveResponse {
  id: string;
  name: string;
  total_size: number;
  files: { index: number; path: string; length: number }[];
  already_added: boolean;
}

export interface AddRequest {
  source?: string;
  torrent_base64?: string;
  save_path?: string;
  paused?: boolean;
  priorities?: FilePriority[];
  sequential?: boolean;
  category?: string;
}

export interface Settings {
  language: string;
  theme: string;
  close_to_tray: boolean;
  start_minimized: boolean;
  notify_on_complete: boolean;
  player: string;
  player_path: string | null;
  download_dir: string;
  default_sequential: boolean;
  start_paused: boolean;
  seed_ratio_limit: number | null;
  listen_port: number;
  upnp: boolean;
  dht: boolean;
  lsd: boolean;
  utp: boolean;
  peer_limit: number;
  proxy_url: string | null;
  network: {
    mode: NetworkMode;
    direct_interface: string | null;
    vpn_interface: string | null;
  };
  download_limit: number;
  upload_limit: number;
  alt_download_limit: number;
  alt_upload_limit: number;
  alt_speed_enabled: boolean;
  scheduler: { enabled: boolean; grid: ScheduleSlot[][] };
}

export interface NetInterface {
  bind_name: string;
  display_name: string;
  description: string | null;
  ipv4: string[];
  is_vpn: boolean;
  is_default_route: boolean;
  kind: string;
}

export interface RssFeed {
  id: string;
  name: string;
  url: string;
  enabled: boolean;
  interval_min: number;
  last_checked: number | null;
  last_error: string | null;
}

export interface RssRule {
  id: string;
  name: string;
  enabled: boolean;
  feed_ids: string[];
  must_contain: string;
  must_not_contain: string;
  use_regex: boolean;
  category: string | null;
  save_path: string | null;
  start_paused: boolean;
}

export interface RssItem {
  feed_id: string;
  guid: string;
  title: string;
  link: string | null;
  published: number | null;
  size: number | null;
  downloaded: boolean;
}

export interface RssData {
  feeds: RssFeed[];
  rules: RssRule[];
  items: RssItem[];
}

export interface CreateRequest {
  path: string;
  name?: string;
  trackers: string[];
  piece_length?: number;
  comment?: string;
  seed: boolean;
  save_to?: string;
}

export interface CreateResponse {
  id: string;
  name: string;
  magnet: string;
  torrent_base64: string;
  saved_to: string | null;
}

let base = "";
let token = "";

export function configure(b: string, t: string) {
  base = b.replace(/\/$/, "");
  token = t;
}

export function apiUrl(path: string) {
  return base + path;
}

export class ApiError extends Error {}

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(base + path, {
    method,
    headers: {
      "x-tortuga-token": token,
      ...(body !== undefined ? { "content-type": "application/json" } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) throw new ApiError(data?.error ?? res.statusText);
  return data as T;
}

const enc = encodeURIComponent;

export const api = {
  state: () => req<StateResponse>("GET", "/api/state"),
  speed: () => req<[number, number, number][]>("GET", "/api/speed"),
  settings: () => req<Settings>("GET", "/api/settings"),
  saveSettings: (s: Settings) => req<Settings>("PUT", "/api/settings", s),
  altSpeed: (enabled: boolean) => req("POST", "/api/alt-speed", { enabled }),
  resolve: (r: AddRequest) => req<ResolveResponse>("POST", "/api/resolve", r),
  add: (r: AddRequest) => req<{ id: string; already_added: boolean }>("POST", "/api/torrents", r),
  details: (id: string) => req<TorrentDetails>("GET", `/api/torrents/${enc(id)}`),
  peers: (id: string) => req<PeerEntry[]>("GET", `/api/torrents/${enc(id)}/peers`),
  pieces: (id: string) =>
    req<{ bitfield: string; total: number }>("GET", `/api/torrents/${enc(id)}/pieces`),
  pause: (id: string) => req("POST", `/api/torrents/${enc(id)}/pause`),
  resume: (id: string) => req("POST", `/api/torrents/${enc(id)}/resume`),
  pauseAll: () => req("POST", "/api/torrents/pause-all"),
  resumeAll: () => req("POST", "/api/torrents/resume-all"),
  remove: (id: string, deleteFiles: boolean) =>
    req("DELETE", `/api/torrents/${enc(id)}?delete_files=${deleteFiles}`),
  setPriorities: (id: string, priorities: Record<number, FilePriority>) =>
    req("POST", `/api/torrents/${enc(id)}/files`, { priorities }),
  setOptions: (id: string, o: { sequential?: boolean; category?: string | null }) =>
    req("POST", `/api/torrents/${enc(id)}/options`, o),
  play: (id: string, file: number) =>
    req<{ path: string; local_path: string; name: string }>(
      "POST",
      `/api/torrents/${enc(id)}/files/${file}/play`,
    ),
  filePath: (id: string, file: number) =>
    req<{ path: string; folder: string }>("GET", `/api/torrents/${enc(id)}/files/${file}/path`),
  torrentFileUrl: (id: string) => `${base}/api/torrents/${enc(id)}/torrent-file?token=${token}`,
  network: () =>
    req<{
      status: NetStatus;
      snapshot: { interfaces: NetInterface[]; vpn_active: boolean } | null;
    }>("GET", "/api/network"),
  create: (r: CreateRequest) => req<CreateResponse>("POST", "/api/create", r),
  rss: () => req<RssData>("GET", "/api/rss"),
  saveFeed: (f: Partial<RssFeed>) => req<RssFeed>("POST", "/api/rss/feeds", f),
  deleteFeed: (id: string) => req("DELETE", `/api/rss/feeds/${enc(id)}`),
  refreshFeed: (id: string) => req("POST", `/api/rss/feeds/${enc(id)}/refresh`),
  saveRule: (r: Partial<RssRule>) => req<RssRule>("POST", "/api/rss/rules", r),
  deleteRule: (id: string) => req("DELETE", `/api/rss/rules/${enc(id)}`),
  downloadItem: (feed_id: string, guid: string) =>
    req("POST", "/api/rss/download", { feed_id, guid }),
};
