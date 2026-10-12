use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    num::NonZeroU32,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use base64::Engine as _;
use bytes::Bytes;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ConnectionOptions, DhtSessionConfig,
    ListenerMode, ListenerOptions, Magnet, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig, TorrentStatsState,
    api::TorrentIdOrHash,
    dht::{DhtPersistenceConfig, Id20},
    limits::LimitsConfig,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::{
    meta::{FilePriority, MetaStore, TorrentMeta},
    net::{Binding, NetStatus, NetworkMode, Route},
    settings::{ScheduleSlot, Settings},
    util::now_ts,
};

pub type TorrentHandle = Arc<ManagedTorrent>;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Completed {
        id: String,
        name: String,
    },
    Added {
        id: String,
        name: String,
    },
    Error {
        id: Option<String>,
        message: String,
    },
    NetworkChanged {
        status: NetStatus,
    },
    RssDownloaded {
        title: String,
        feed: String,
    },
    /// Files of a torrent disappeared from disk while Tortuga was not running.
    FilesMissing {
        id: String,
        name: String,
        files: usize,
        all: bool,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TorrentState {
    Metadata,
    Checking,
    Downloading,
    Stalled,
    Seeding,
    Paused,
    Completed,
    Queued,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct TorrentSummary {
    pub id: String,
    pub name: String,
    pub state: TorrentState,
    pub progress: f64,
    pub size: u64,
    pub done: u64,
    pub down_speed: u64,
    pub up_speed: u64,
    pub eta: Option<u64>,
    pub peers: u32,
    pub peers_seen: u32,
    pub ratio: f64,
    pub downloaded: u64,
    pub uploaded: u64,
    pub added_at: i64,
    pub completed_at: Option<i64>,
    pub category: Option<String>,
    pub save_path: String,
    pub error: Option<String>,
    pub sequential: bool,
    pub user_paused: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileEntry {
    pub index: usize,
    pub path: String,
    pub length: u64,
    pub done: u64,
    pub priority: FilePriority,
    pub mime: Option<String>,
    pub streamable: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TorrentDetails {
    pub summary: TorrentSummary,
    pub info_hash: String,
    pub magnet: String,
    pub piece_length: u32,
    pub pieces: u32,
    pub total_size: u64,
    pub files: Vec<FileEntry>,
    pub trackers: Vec<String>,
    pub comment: Option<String>,
    pub private: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PeerEntry {
    pub addr: String,
    pub client: Option<String>,
    pub connection: Option<String>,
    pub downloaded: u64,
    pub uploaded: u64,
    pub down_speed: u64,
    pub up_speed: u64,
    pub state: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolvedFile {
    pub index: usize,
    pub path: String,
    pub length: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolveResponse {
    pub id: String,
    pub name: String,
    pub total_size: u64,
    pub files: Vec<ResolvedFile>,
    pub already_added: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AddRequest {
    /// Magnet link, http(s) URL of a .torrent, or local .torrent path.
    pub source: Option<String>,
    /// Raw .torrent file contents, base64.
    pub torrent_base64: Option<String>,
    pub save_path: Option<String>,
    pub paused: Option<bool>,
    pub priorities: Option<Vec<FilePriority>>,
    pub sequential: Option<bool>,
    pub category: Option<String>,
    /// Extra peers to connect to ("ip:port").
    pub peers: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AddResponse {
    pub id: String,
    pub already_added: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionInfo {
    pub down_speed: u64,
    pub up_speed: u64,
    pub dht_nodes: usize,
    pub listen_port: Option<u16>,
    pub download_limit: u32,
    pub upload_limit: u32,
    pub alt_speed_active: bool,
    pub scheduler_slot: ScheduleSlot,
    pub network: NetStatus,
    pub categories: Vec<String>,
    pub restarting: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct StateResponse {
    pub torrents: Vec<TorrentSummary>,
    pub session: SessionInfo,
}

/// A magnet/URL being resolved before it becomes a real torrent.
struct Pending {
    name: String,
    error: Option<String>,
    added_at: i64,
    req: AddRequest,
    /// Info hash once the metadata is known (URL sources start without one).
    resolved_id: Option<String>,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// A magnet / URL still waiting for metadata, kept across restarts.
#[derive(Serialize, Deserialize)]
struct SavedPending {
    key: String,
    added_at: i64,
    req: AddRequest,
}

struct Resolved {
    bytes: Bytes,
    trackers: Vec<String>,
    peers: Vec<SocketAddr>,
}

#[derive(Default)]
struct TickState {
    last_fetched: u64,
    last_uploaded: u64,
    seen_unfinished: bool,
}

#[derive(Default)]
struct PeerPrev {
    at: Option<Instant>,
    values: HashMap<String, (u64, u64)>,
}

/// Reasons for pausing all torrents regardless of user choice.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct GlobalPause {
    scheduler: bool,
    network: bool,
}

impl GlobalPause {
    fn any(self) -> bool {
        self.scheduler || self.network
    }
}

pub struct Engine {
    pub data_dir: PathBuf,
    settings_path: PathBuf,
    settings: RwLock<Settings>,
    pub(crate) meta: MetaStore,
    session: RwLock<Option<Arc<Session>>>,
    restart_lock: tokio::sync::Mutex<()>,
    restarting: std::sync::atomic::AtomicBool,
    binding: RwLock<Binding>,
    pub(crate) net_status: RwLock<NetStatus>,
    pending: Mutex<HashMap<String, Pending>>,
    resolved: Mutex<HashMap<String, Resolved>>,
    ticks: Mutex<HashMap<String, TickState>>,
    /// Torrents with a file-selection update in flight.
    syncing_files: Mutex<HashSet<String>>,
    /// Torrents with a pause/unpause in flight.
    syncing_run: Mutex<HashSet<String>>,
    /// Lazily loaded RSS feeds and rules.
    pub(crate) rss: Mutex<Option<Arc<crate::rss::RssStore>>>,
    peer_prev: Mutex<HashMap<String, PeerPrev>>,
    global_pause: Mutex<GlobalPause>,
    scheduler_slot: Mutex<ScheduleSlot>,
    speed_history: Mutex<VecDeque<(i64, u64, u64)>>,
    events: broadcast::Sender<Event>,
    pub(crate) cancel: tokio_util::sync::CancellationToken,
    pub token: String,
}

fn hash_str(id: &Id20) -> String {
    id.as_string()
}

fn parse_hash(id: &str) -> anyhow::Result<Id20> {
    use std::str::FromStr;
    Id20::from_str(id.trim()).map_err(|_| anyhow::anyhow!("invalid torrent id"))
}

fn file_path_string(components: impl Iterator<Item = String>) -> String {
    components.collect::<Vec<_>>().join("/")
}

pub fn is_video(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "mp4"
            | "m4v"
            | "mkv"
            | "webm"
            | "mov"
            | "avi"
            | "ts"
            | "m2ts"
            | "mpg"
            | "mpeg"
            | "wmv"
            | "flv"
    )
}

/// Looks like a season of a show: several videos making up most of the size.
/// Such torrents default to downloading files in order.
pub fn looks_like_series<'a>(files: impl IntoIterator<Item = (&'a str, u64)>) -> bool {
    let (mut videos, mut video_bytes, mut total) = (0usize, 0u64, 0u64);
    for (path, len) in files {
        total += len;
        if is_video(path) {
            videos += 1;
            video_bytes += len;
        }
    }
    videos >= 2 && video_bytes * 10 >= total * 7
}

pub fn is_streamable(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "mp4"
            | "m4v"
            | "mkv"
            | "webm"
            | "mov"
            | "avi"
            | "ts"
            | "m2ts"
            | "mpg"
            | "mpeg"
            | "wmv"
            | "flv"
            | "ogv"
            | "3gp"
            | "mp3"
            | "flac"
            | "ogg"
            | "opus"
            | "m4a"
            | "aac"
            | "wav"
    )
}

fn ipv6_available() -> bool {
    std::net::TcpListener::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))).is_ok()
}

fn safe_subfolder(name: &str) -> Option<PathBuf> {
    let pb = PathBuf::from(name);
    pb.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(pb)
}

impl Engine {
    pub async fn new(data_dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        Self::new_with_token(data_dir, None).await
    }

    pub async fn new_with_token(
        data_dir: PathBuf,
        token: Option<String>,
    ) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&data_dir)?;
        let settings_path = data_dir.join("settings.json");
        let settings = Settings::load(&settings_path);
        settings.save(&settings_path)?;
        let (events, _) = broadcast::channel(64);
        let engine = Arc::new(Self {
            meta: MetaStore::load(data_dir.join("torrents.json")),
            data_dir,
            settings_path,
            net_status: RwLock::new(NetStatus {
                mode: settings.network.mode,
                route: Route::System,
                vpn_active: false,
                vpn_name: None,
                interface_name: None,
                reason: "starting",
            }),
            settings: RwLock::new(settings),
            session: RwLock::new(None),
            restart_lock: tokio::sync::Mutex::new(()),
            restarting: Default::default(),
            binding: RwLock::new(Binding::default()),
            pending: Default::default(),
            resolved: Default::default(),
            ticks: Default::default(),
            syncing_files: Default::default(),
            syncing_run: Default::default(),
            rss: Default::default(),
            peer_prev: Default::default(),
            global_pause: Default::default(),
            scheduler_slot: Mutex::new(ScheduleSlot::Normal),
            speed_history: Default::default(),
            events,
            cancel: Default::default(),
            token: token.unwrap_or_else(crate::util::random_token),
        });

        crate::dns::install_udp_tracker_resolver();
        let binding = crate::netguard::initial_binding(&engine);
        engine.start_session(binding).await?;
        engine.apply_scheduler();
        engine.apply_limits();
        engine.restore_pending();
        engine.clone().spawn_background();
        Ok(engine)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub(crate) fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    pub fn session(&self) -> anyhow::Result<Arc<Session>> {
        self.session
            .read()
            .clone()
            .context("session is restarting, try again")
    }

    pub(crate) fn current_binding(&self) -> Binding {
        self.binding.read().clone()
    }

    // ---------------------------------------------------------------------
    // Session lifecycle
    // ---------------------------------------------------------------------

    async fn build_session(&self, binding: &Binding) -> anyhow::Result<Arc<Session>> {
        let s = self.settings();
        std::fs::create_dir_all(&s.download_dir)
            .with_context(|| format!("can't create {}", s.download_dir.display()))?;

        // Resolved with a DNS-over-HTTPS fallback: filtering DNS (AdGuard etc.)
        // often blocks these names, leaving DHT with no way in.
        let bootstrap = if s.dht {
            Some(crate::dns::dht_bootstrap_addrs().await)
        } else {
            None
        };
        let make_opts = |port: u16, ipv4_only: bool| SessionOptions {
            dht: s.dht.then(|| DhtSessionConfig {
                bootstrap_addrs: bootstrap.clone(),
                persistence: Some(DhtPersistenceConfig {
                    config_filename: Some(self.data_dir.join("dht.json")),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(self.data_dir.join("session")),
            }),
            listen: Some(ListenerOptions {
                mode: if s.utp {
                    ListenerMode::TcpAndUtp
                } else {
                    ListenerMode::TcpOnly
                },
                listen_addr: if ipv4_only {
                    SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))
                } else {
                    SocketAddr::from((Ipv6Addr::UNSPECIFIED, port))
                },
                ipv4_only,
                enable_upnp_port_forwarding: s.upnp,
                ..Default::default()
            }),
            connect: Some(ConnectionOptions {
                proxy_url: s.proxy_url.clone().filter(|p| !p.trim().is_empty()),
                ..Default::default()
            }),
            ratelimits: LimitsConfig::default(),
            peer_limit: Some(s.peer_limit.max(1) as usize),
            disable_local_service_discovery: !s.lsd,
            bind_device_name: binding.device.clone(),
            bind_local_ip: binding.local_ip,
            dns_resolver: Some(crate::dns::reqwest_resolver()),
            client_name_and_version: Some(format!("Tortuga/{}", env!("CARGO_PKG_VERSION"))),
            ipv4_only,
            ..Default::default()
        };

        // Some systems have IPv6 disabled entirely; fall back to IPv4.
        let ipv4_only = !ipv6_available();

        // The previous session may still hold the port for a moment after a restart.
        let mut last_err = None;
        for attempt in 0..5 {
            let opts = make_opts(s.listen_port, ipv4_only);
            match Session::new_with_opts(s.download_dir.clone(), opts).await {
                Ok(session) => return Ok(session),
                Err(e) => {
                    warn!(attempt, "error starting session: {e:#}");
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))).await;
                }
            }
        }
        warn!("falling back to a random listen port");
        Session::new_with_opts(s.download_dir.clone(), make_opts(0, ipv4_only))
            .await
            .context(last_err.map(|e| format!("{e:#}")).unwrap_or_default())
    }

    pub(crate) async fn start_session(self: &Arc<Self>, binding: Binding) -> anyhow::Result<()> {
        let _g = self.restart_lock.lock().await;
        // Metadata lookups own the old session too. Stop them before switching
        // routes; otherwise magnets keep waiting on the discarded DHT/trackers.
        for p in self.pending.lock().values_mut() {
            if let Some(task) = p.task.take() {
                task.abort();
            }
            p.resolved_id = None;
        }
        self.restarting
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let old = self.session.write().take();
        if let Some(old) = old {
            info!("stopping session");
            old.stop().await;
            drop(old);
        }
        let res = self.build_session(&binding).await;
        self.restarting
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let session = res?;
        *self.binding.write() = binding;
        *self.session.write() = Some(session);
        self.ticks.lock().clear();
        self.apply_limits();
        self.reconcile_all();
        let pending: Vec<_> = self
            .pending
            .lock()
            .iter()
            .map(|(key, p)| (key.clone(), p.req.clone(), p.added_at))
            .collect();
        for (key, req, added_at) in pending {
            if self.pending.lock().contains_key(&key) {
                self.spawn_pending(key, req, added_at);
            }
        }
        self.save_pending();
        Ok(())
    }

    /// Restart the session with new binding, keeping torrents.
    pub(crate) async fn restart_with_binding(self: &Arc<Self>, binding: Binding) -> anyhow::Result<()> {
        if *self.binding.read() == binding && self.session.read().is_some() {
            return Ok(());
        }
        info!(?binding, "restarting session with new network binding");
        self.start_session(binding).await
    }

    fn handles(&self) -> Vec<TorrentHandle> {
        match self.session.read().as_ref() {
            Some(s) => s.with_torrents(|it| it.map(|(_, h)| h.clone()).collect()),
            None => Vec::new(),
        }
    }

    fn handle(&self, id: &str) -> anyhow::Result<TorrentHandle> {
        let hash = parse_hash(id)?;
        self.session()?
            .get(TorrentIdOrHash::Hash(hash))
            .context("torrent not found")
    }

    /// Make each torrent match Tortuga's stored state: priorities, pause state.
    fn reconcile_all(&self) {
        for h in self.handles() {
            self.reconcile(&h);
        }
    }

    fn reconcile(&self, h: &TorrentHandle) {
        let id = hash_str(&h.info_hash());
        let default_seq = self.settings.read().default_sequential;
        let meta = self.meta.get_or_default(&id, default_seq);
        if let Some(n) = h.metadata.load().as_ref().map(|m| m.file_infos.len()) {
            let _ = h.set_piece_priority(Some(meta.piece_priority(n)));
        }
        self.apply_run_state(h, &meta);
    }

    fn apply_run_state(&self, h: &TorrentHandle, meta: &TorrentMeta) {
        let Ok(session) = self.session() else { return };
        let should_run = !meta.user_paused && !self.global_pause.lock().any();
        let is_paused = h.is_paused();
        let h = h.clone();
        tokio::spawn(async move {
            let res = if should_run && is_paused {
                session.unpause(&h).await
            } else if !should_run && !is_paused {
                session.pause(&h).await
            } else {
                Ok(())
            };
            if let Err(e) = res {
                warn!("error changing torrent state: {e:#}");
            }
        });
    }

    fn set_global_pause(&self, f: impl FnOnce(&mut GlobalPause)) {
        let changed = {
            let mut g = self.global_pause.lock();
            let before = *g;
            f(&mut g);
            before.any() != g.any()
        };
        if changed {
            self.reconcile_all();
        }
    }

    pub(crate) fn set_network_blocked(&self, blocked: bool) {
        self.set_global_pause(|g| g.network = blocked);
    }

    // ---------------------------------------------------------------------
    // Settings, limits, scheduler
    // ---------------------------------------------------------------------

    pub async fn update_settings(self: &Arc<Self>, new: Settings) -> anyhow::Result<Settings> {
        let old = self.settings();
        let mut new = new;
        if new.listen_port == 0 {
            new.listen_port = old.listen_port;
        }
        if new.scheduler.grid.len() != 7 || new.scheduler.grid.iter().any(|d| d.len() != 24) {
            bail!("scheduler grid must be 7x24");
        }
        new.save(&self.settings_path)?;
        *self.settings.write() = new.clone();

        let net_changed = old.network != new.network;
        if old.session_fingerprint() != new.session_fingerprint() {
            let binding = self.current_binding();
            self.start_session(binding).await?;
        }
        if net_changed {
            crate::netguard::reset_auto(self);
            crate::netguard::evaluate(self).await;
        }
        self.apply_scheduler();
        self.apply_limits();
        Ok(new)
    }

    pub fn set_alt_speed(&self, enabled: bool) -> anyhow::Result<()> {
        {
            let mut s = self.settings.write();
            s.alt_speed_enabled = enabled;
            s.save(&self.settings_path)?;
        }
        self.apply_limits();
        Ok(())
    }

    fn alt_active(&self) -> bool {
        self.settings.read().alt_speed_enabled || *self.scheduler_slot.lock() == ScheduleSlot::Alt
    }

    fn effective_limits(&self) -> (u32, u32) {
        let s = self.settings.read();
        if s.alt_speed_enabled || *self.scheduler_slot.lock() == ScheduleSlot::Alt {
            (s.alt_download_limit, s.alt_upload_limit)
        } else {
            (s.download_limit, s.upload_limit)
        }
    }

    fn apply_limits(&self) {
        let (down, up) = self.effective_limits();
        if let Some(s) = self.session.read().as_ref() {
            s.ratelimits
                .set_download_bps(NonZeroU32::new(down.saturating_mul(1024)));
            s.ratelimits
                .set_upload_bps(NonZeroU32::new(up.saturating_mul(1024)));
        }
    }

    fn apply_scheduler(&self) {
        use chrono::{Datelike, Timelike};
        let s = self.settings.read().scheduler.clone();
        let slot = if s.enabled {
            let now = chrono::Local::now();
            s.slot(
                now.weekday().num_days_from_monday() as usize,
                now.hour() as usize,
            )
        } else {
            ScheduleSlot::Normal
        };
        let changed = {
            let mut g = self.scheduler_slot.lock();
            let changed = *g != slot;
            *g = slot;
            changed
        };
        if changed {
            self.apply_limits();
        }
        self.set_global_pause(|g| g.scheduler = slot == ScheduleSlot::Off);
    }

    // ---------------------------------------------------------------------
    // Adding torrents
    // ---------------------------------------------------------------------

    /// A 40-char hex or 32-char base32 info hash pasted alone, as a magnet link.
    fn bare_hash_magnet(source: &str) -> Option<String> {
        let s = source.trim();
        let base32 = |c: char| c.is_ascii_alphabetic() || ('2'..='7').contains(&c);
        ((s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
            || (s.len() == 32 && s.chars().all(base32)))
        .then(|| format!("magnet:?xt=urn:btih:{s}"))
    }

    fn source_to_add(source: &str) -> anyhow::Result<AddTorrent<'static>> {
        let s = source.trim();
        if s.starts_with("magnet:") || s.starts_with("http://") || s.starts_with("https://") {
            return Ok(AddTorrent::from_url(s.to_owned()));
        }
        if let Some(m) = Self::bare_hash_magnet(s) {
            return Ok(AddTorrent::from_url(m));
        }
        let path = Path::new(s);
        if path.exists() {
            let bytes = std::fs::read(path).with_context(|| format!("can't read {s}"))?;
            return Ok(AddTorrent::from_bytes(bytes));
        }
        bail!("unsupported link: expected magnet link, .torrent URL or file")
    }

    fn request_to_add(req: &AddRequest) -> anyhow::Result<AddTorrent<'static>> {
        if let Some(b64) = req.torrent_base64.as_deref().filter(|s| !s.is_empty()) {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .context("invalid base64 torrent")?;
            return Ok(AddTorrent::from_bytes(bytes));
        }
        Self::source_to_add(req.source.as_deref().context("no source")?)
    }

    fn magnet_trackers(req: &AddRequest) -> Vec<String> {
        req.source
            .as_deref()
            .filter(|s| s.trim().starts_with("magnet:"))
            .and_then(|s| Magnet::parse(s.trim()).ok())
            .map(|m| m.trackers)
            .unwrap_or_default()
    }

    /// Peers given with the request plus `x.pe` hints from a magnet link.
    fn hinted_peers(req: &AddRequest) -> Vec<SocketAddr> {
        let from_magnet = req
            .source
            .as_deref()
            .filter(|s| s.trim().starts_with("magnet:"))
            .and_then(|s| url::Url::parse(s.trim()).ok())
            .map(|u| {
                u.query_pairs()
                    .filter(|(k, _)| k == "x.pe")
                    .map(|(_, v)| v.into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        req.peers
            .iter()
            .flatten()
            .cloned()
            .chain(from_magnet)
            .filter_map(|p| p.trim().parse::<SocketAddr>().ok())
            .collect()
    }

    /// Fetch metadata without adding (for the "add torrent" dialog).
    pub async fn resolve(&self, req: &AddRequest) -> anyhow::Result<ResolveResponse> {
        let session = self.session()?;
        let add = Self::request_to_add(req)?;
        let peers = Self::hinted_peers(req);
        let resp = session
            .add_torrent(
                add,
                Some(AddTorrentOptions {
                    list_only: true,
                    initial_peers: (!peers.is_empty()).then_some(peers),
                    ..Default::default()
                }),
            )
            .await?;
        match resp {
            AddTorrentResponse::ListOnly(r) => {
                let id = hash_str(&r.info_hash);
                let files: Vec<ResolvedFile> = r
                    .info
                    .iter_file_details()
                    .enumerate()
                    .map(|(index, fd)| ResolvedFile {
                        index,
                        path: file_path_string(
                            fd.filename.iter_components().map(|c| c.into_owned()),
                        ),
                        length: fd.len,
                    })
                    .collect();
                let name = r
                    .info
                    .name()
                    .map(|n| n.into_owned())
                    .unwrap_or_else(|| id.clone());
                let already_added = session.get(TorrentIdOrHash::Hash(r.info_hash)).is_some();
                self.resolved.lock().insert(
                    id.clone(),
                    Resolved {
                        bytes: r.torrent_bytes.clone(),
                        trackers: Self::magnet_trackers(req),
                        peers: r.seen_peers.clone(),
                    },
                );
                Ok(ResolveResponse {
                    id,
                    name,
                    total_size: files.iter().map(|f| f.length).sum(),
                    files,
                    already_added,
                })
            }
            AddTorrentResponse::AlreadyManaged(_, h) => {
                let id = hash_str(&h.info_hash());
                let files = h
                    .metadata
                    .load()
                    .as_ref()
                    .map(|m| {
                        m.file_infos
                            .iter()
                            .enumerate()
                            .map(|(index, f)| ResolvedFile {
                                index,
                                path: f.relative_filename.to_string_lossy().replace('\\', "/"),
                                length: f.len,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Ok(ResolveResponse {
                    name: h.name().unwrap_or_else(|| id.clone()),
                    id,
                    total_size: files.iter().map(|f| f.length).sum(),
                    files,
                    already_added: true,
                })
            }
            AddTorrentResponse::Added(..) => bail!("unexpected add response"),
        }
    }

    /// Add a torrent. Magnets without cached metadata are resolved in the
    /// background and shown as "metadata" rows until ready.
    pub async fn add(self: &Arc<Self>, mut req: AddRequest) -> anyhow::Result<AddResponse> {
        if let Some(m) = req.source.as_deref().and_then(Self::bare_hash_magnet) {
            req.source = Some(m);
        }
        // Fast path: we already have the metadata (from resolve()) or a .torrent.
        let known_hash = match (&req.torrent_base64, req.source.as_deref()) {
            (Some(_), _) => None,
            (None, Some(s)) if s.trim().starts_with("magnet:") => Magnet::parse(s.trim())
                .ok()
                .and_then(|m| m.as_id20())
                .map(|h| hash_str(&h)),
            _ => None,
        };

        let session = self.session()?;
        if let Some(h) = known_hash.as_deref()
            && session.get(TorrentIdOrHash::Hash(parse_hash(h)?)).is_some()
        {
            return Ok(AddResponse {
                id: h.to_owned(),
                already_added: true,
            });
        }

        let cached = known_hash
            .as_deref()
            .and_then(|h| self.resolved.lock().remove(h));
        let is_url = req.torrent_base64.is_none()
            && req
                .source
                .as_deref()
                .map(|s| s.trim().starts_with("magnet:") || s.trim().starts_with("http"))
                .unwrap_or(false);

        if cached.is_none() && is_url {
            return self.add_pending(req, known_hash);
        }

        let (add, trackers, peers) = match cached {
            Some(r) => (AddTorrent::from_bytes(r.bytes), r.trackers, r.peers),
            None => (Self::request_to_add(&req)?, Vec::new(), Vec::new()),
        };
        self.add_now(&session, add, &req, trackers, peers).await
    }

    fn add_pending(
        self: &Arc<Self>,
        req: AddRequest,
        known_hash: Option<String>,
    ) -> anyhow::Result<AddResponse> {
        let key = known_hash.unwrap_or_else(|| format!("url-{}", crate::util::random_token()));
        if self.pending.lock().contains_key(&key) {
            return Ok(AddResponse {
                id: key,
                already_added: true,
            });
        }
        self.spawn_pending(key.clone(), req, now_ts());
        self.save_pending();
        Ok(AddResponse {
            id: key,
            already_added: false,
        })
    }

    fn spawn_pending(self: &Arc<Self>, key: String, req: AddRequest, added_at: i64) {
        let source = req.source.clone().unwrap_or_default();
        let name = Magnet::parse(source.trim())
            .ok()
            .and_then(|m| m.name)
            .or_else(|| (!key.starts_with("url-")).then(|| key.clone()))
            .unwrap_or_else(|| source.chars().take(80).collect());
        // Register before the task starts so a fast resolve can't finish first
        // and leave a stale "metadata" row behind.
        self.pending.lock().insert(
            key.clone(),
            Pending {
                name,
                error: None,
                added_at,
                req: req.clone(),
                resolved_id: None,
                task: None,
            },
        );

        let this = self.clone();
        let task_key = key.clone();
        let task = tokio::spawn(async move {
            let res = async {
                let resolved = this.resolve(&req).await?;
                if let Some(p) = this.pending.lock().get_mut(&task_key) {
                    p.resolved_id = Some(resolved.id.clone());
                }
                let session = this.session()?;
                let cached = this.resolved.lock().remove(&resolved.id);
                let (add, trackers, peers) = match cached {
                    Some(r) => (AddTorrent::from_bytes(r.bytes), r.trackers, r.peers),
                    None => (Self::request_to_add(&req)?, Vec::new(), Vec::new()),
                };
                this.add_now(&session, add, &req, trackers, peers).await
            }
            .await;
            match res {
                Ok(_) => {
                    this.pending.lock().remove(&task_key);
                    this.save_pending();
                }
                Err(e) => {
                    warn!("error adding torrent: {e:#}");
                    if let Some(p) = this.pending.lock().get_mut(&task_key) {
                        p.error = Some(format!("{e:#}"));
                    }
                    this.emit(Event::Error {
                        id: Some(task_key.clone()),
                        message: format!("{e:#}"),
                    });
                }
            }
        });
        if let Some(p) = self.pending.lock().get_mut(&key) {
            p.task = Some(task);
        } else {
            // Removed (or finished) in the meantime.
            task.abort();
        }
    }

    fn pending_path(&self) -> PathBuf {
        self.data_dir.join("pending.json")
    }

    fn save_pending(&self) {
        let saved: Vec<SavedPending> = self
            .pending
            .lock()
            .iter()
            .map(|(key, p)| SavedPending {
                key: key.clone(),
                added_at: p.added_at,
                req: p.req.clone(),
            })
            .collect();
        if let Err(e) = crate::util::write_json_atomic(&self.pending_path(), &saved) {
            warn!("error saving pending torrents: {e:#}");
        }
    }

    /// Magnets added last time whose metadata never arrived: try again.
    fn restore_pending(self: &Arc<Self>) {
        let saved: Vec<SavedPending> = std::fs::read(self.pending_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        for p in saved {
            let already = parse_hash(&p.key)
                .ok()
                .and_then(|h| self.session().ok()?.get(TorrentIdOrHash::Hash(h)))
                .is_some();
            if !already {
                self.spawn_pending(p.key, p.req, p.added_at);
            }
        }
        self.save_pending();
    }

    async fn add_now(
        &self,
        session: &Arc<Session>,
        add: AddTorrent<'static>,
        req: &AddRequest,
        trackers: Vec<String>,
        peers: Vec<SocketAddr>,
    ) -> anyhow::Result<AddResponse> {
        let settings = self.settings();
        let paused = req.paused.unwrap_or(settings.start_paused);
        let mut peers = peers;
        for p in Self::hinted_peers(req) {
            if !peers.contains(&p) {
                peers.push(p);
            }
        }
        let mut meta = TorrentMeta::new(req.sequential.unwrap_or(settings.default_sequential));
        meta.priorities = req.priorities.clone().unwrap_or_default();
        meta.category = req.category.clone().filter(|c| !c.trim().is_empty());
        meta.user_paused = paused;
        meta.source = req.source.clone();

        // Learn the file list first so that the output folder and priorities are right.
        let bytes = match &add {
            AddTorrent::TorrentFileBytes(b) => Some(b.clone()),
            _ => None,
        };
        let info = bytes
            .as_ref()
            .and_then(|b| librqbit::torrent_from_bytes(b).ok());
        let (file_count, name) = match &info {
            Some(t) => (
                t.info.data.files.as_ref().map(|f| f.len()).unwrap_or(1),
                t.info
                    .data
                    .name
                    .as_ref()
                    .map(|n| String::from_utf8_lossy(n.as_ref()).into_owned()),
            ),
            None => (0, None),
        };
        if req.sequential.is_none()
            && let Some(files) = info.as_ref().and_then(|t| t.info.data.files.as_ref())
        {
            let list: Vec<(String, u64)> = files
                .iter()
                .map(|f| {
                    let last = f
                        .path
                        .last()
                        .map(|p| String::from_utf8_lossy(p.as_ref()).into_owned());
                    (last.unwrap_or_default(), f.length)
                })
                .collect();
            if looks_like_series(list.iter().map(|(p, l)| (p.as_str(), *l))) {
                meta.sequential = true;
            }
        }

        let output_folder = req
            .save_path
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                let mut out = PathBuf::from(p.trim());
                if file_count > 1
                    && let Some(sub) = name.as_deref().and_then(safe_subfolder)
                {
                    out = out.join(sub);
                }
                out.to_string_lossy().into_owned()
            });

        let opts = AddTorrentOptions {
            paused: paused || self.global_pause.lock().any(),
            only_files: (file_count > 0)
                .then(|| meta.only_files(file_count))
                .flatten(),
            output_folder,
            overwrite: true,
            trackers: (!trackers.is_empty()).then_some(trackers),
            initial_peers: (!peers.is_empty()).then_some(peers),
            piece_priority: (file_count > 0).then(|| meta.piece_priority(file_count)),
            ..Default::default()
        };

        let resp = session.add_torrent(add, Some(opts)).await?;
        let (handle, already) = match resp {
            AddTorrentResponse::Added(_, h) => (h, false),
            AddTorrentResponse::AlreadyManaged(_, h) => (h, true),
            AddTorrentResponse::ListOnly(_) => bail!("unexpected list-only response"),
        };
        let id = hash_str(&handle.info_hash());
        if !already {
            self.meta.insert(&id, meta);
            let _ = self.meta.save();
            self.reconcile(&handle);
            self.emit(Event::Added {
                id: id.clone(),
                name: handle.name().unwrap_or_else(|| id.clone()),
            });
        }
        Ok(AddResponse {
            id,
            already_added: already,
        })
    }

    // ---------------------------------------------------------------------
    // Queries
    // ---------------------------------------------------------------------

    fn summary(&self, h: &TorrentHandle) -> TorrentSummary {
        let id = hash_str(&h.info_hash());
        let meta = self.meta.get(&id).unwrap_or_default();
        let stats = h.stats();
        let live = stats.live.as_ref();
        let paused = h.is_paused();
        let (down, up) = h
            .live()
            .map(|l| (l.down_speed_estimator().bps(), l.up_speed_estimator().bps()))
            .unwrap_or((0, 0));
        let peer_stats = live.map(|l| &l.snapshot.peer_stats);
        let peers = peer_stats.map(|p| p.live).unwrap_or(0);
        let state = match stats.state {
            TorrentStatsState::Error => TorrentState::Error,
            TorrentStatsState::Initializing { .. } => TorrentState::Checking,
            TorrentStatsState::Paused if stats.finished => TorrentState::Completed,
            TorrentStatsState::Paused if !meta.user_paused && self.global_pause.lock().any() => {
                TorrentState::Queued
            }
            TorrentStatsState::Paused => TorrentState::Paused,
            TorrentStatsState::Live if stats.finished => TorrentState::Seeding,
            TorrentStatsState::Live if down == 0 && peers == 0 => TorrentState::Stalled,
            TorrentStatsState::Live => TorrentState::Downloading,
        };
        let state = if paused && matches!(state, TorrentState::Checking) {
            TorrentState::Paused
        } else {
            state
        };
        let size = stats.total_bytes;
        let done = stats.progress_bytes.min(size);
        let ratio = if meta.total_downloaded > 0 {
            meta.total_uploaded as f64 / meta.total_downloaded as f64
        } else if meta.total_uploaded > 0 {
            f64::INFINITY
        } else {
            0.0
        };
        let eta = (down > 0 && done < size).then(|| (size - done) / down);
        TorrentSummary {
            name: h.name().unwrap_or_else(|| id.clone()),
            state,
            progress: if size > 0 {
                done as f64 / size as f64
            } else {
                0.0
            },
            size,
            done,
            down_speed: down,
            up_speed: up,
            eta,
            peers,
            peers_seen: peer_stats.map(|p| p.seen).unwrap_or(0),
            ratio: if ratio.is_finite() { ratio } else { 999.0 },
            downloaded: meta.total_downloaded,
            uploaded: meta.total_uploaded,
            added_at: meta.added_at,
            completed_at: meta.completed_at,
            category: meta.category,
            save_path: h.output_folder().to_string_lossy().into_owned(),
            error: stats.error,
            sequential: meta.sequential,
            user_paused: meta.user_paused,
            id,
        }
    }

    fn pending_summaries(&self) -> Vec<TorrentSummary> {
        let dir = self
            .settings
            .read()
            .download_dir
            .to_string_lossy()
            .into_owned();
        self.pending
            .lock()
            .iter()
            .map(|(id, p)| TorrentSummary {
                id: id.clone(),
                name: p.name.clone(),
                state: if p.error.is_some() {
                    TorrentState::Error
                } else {
                    TorrentState::Metadata
                },
                progress: 0.0,
                size: 0,
                done: 0,
                down_speed: 0,
                up_speed: 0,
                eta: None,
                peers: 0,
                peers_seen: 0,
                ratio: 0.0,
                downloaded: 0,
                uploaded: 0,
                added_at: p.added_at,
                completed_at: None,
                category: None,
                save_path: dir.clone(),
                error: p.error.clone(),
                sequential: false,
                user_paused: false,
            })
            .collect()
    }

    pub fn state(&self) -> StateResponse {
        let mut torrents: Vec<TorrentSummary> =
            self.handles().iter().map(|h| self.summary(h)).collect();
        // A magnet that just got its metadata is briefly both pending and added.
        let added: HashSet<String> = torrents.iter().map(|t| t.id.clone()).collect();
        let resolved: HashMap<String, String> = self
            .pending
            .lock()
            .iter()
            .filter_map(|(k, p)| Some((k.clone(), p.resolved_id.clone()?)))
            .collect();
        torrents.extend(self.pending_summaries().into_iter().filter(|p| {
            !added.contains(&p.id) && resolved.get(&p.id).is_none_or(|r| !added.contains(r))
        }));
        torrents.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(a.name.cmp(&b.name)));
        let settings = self.settings();
        let (down_limit, up_limit) = self.effective_limits();
        let session = self.session.read().clone();
        let (down_speed, up_speed) = self
            .speed_history
            .lock()
            .back()
            .map(|(_, d, u)| (*d, *u))
            .unwrap_or((0, 0));
        StateResponse {
            torrents,
            session: SessionInfo {
                down_speed,
                up_speed,
                dht_nodes: session
                    .as_ref()
                    .and_then(|s| s.get_dht())
                    .map(|d| {
                        let st = d.stats();
                        st.routing_table_size + st.routing_table_size_v6
                    })
                    .unwrap_or(0),
                listen_port: session.as_ref().and_then(|s| s.announce_port()),
                download_limit: down_limit,
                upload_limit: up_limit,
                alt_speed_active: self.alt_active(),
                scheduler_slot: *self.scheduler_slot.lock(),
                network: self.net_status.read().clone(),
                categories: self.meta.categories(),
                restarting: self.restarting.load(std::sync::atomic::Ordering::Relaxed)
                    && !settings.download_dir.as_os_str().is_empty(),
            },
        }
    }

    pub fn speed_history(&self) -> Vec<(i64, u64, u64)> {
        self.speed_history.lock().iter().copied().collect()
    }

    pub fn details(&self, id: &str) -> anyhow::Result<TorrentDetails> {
        let h = self.handle(id)?;
        let summary = self.summary(&h);
        let meta = self.meta.get(id).unwrap_or_default();
        let stats = h.stats();
        let hash = h.info_hash();
        let trackers: Vec<String> = h.shared().trackers.iter().map(|u| u.to_string()).collect();
        let md = h
            .metadata
            .load_full()
            .context("metadata not resolved yet")?;
        let info = md.info.info();
        let files = md
            .file_infos
            .iter()
            .enumerate()
            .map(|(index, f)| {
                let path = f.relative_filename.to_string_lossy().replace('\\', "/");
                FileEntry {
                    index,
                    length: f.len,
                    done: stats.file_progress.get(index).copied().unwrap_or(0),
                    priority: meta.priorities.get(index).copied().unwrap_or_default(),
                    mime: mime_guess::from_path(&path).first().map(|m| m.to_string()),
                    streamable: is_streamable(&path),
                    path,
                }
            })
            .collect::<Vec<_>>();
        let lengths = md.lengths();
        Ok(TorrentDetails {
            magnet: Magnet::from_id20(hash, trackers.clone(), None).to_string(),
            info_hash: hash_str(&hash),
            piece_length: lengths.default_piece_length(),
            pieces: lengths.total_pieces(),
            total_size: lengths.total_length(),
            files,
            trackers,
            comment: None,
            private: info.private,
            summary,
        })
    }

    pub fn pieces(&self, id: &str) -> anyhow::Result<(String, u32)> {
        let session = self.session()?;
        let api = librqbit::Api::new(session, None);
        let (bf, len) = api
            .api_dump_haves(TorrentIdOrHash::Hash(parse_hash(id)?))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok((
            base64::engine::general_purpose::STANDARD.encode(bf.as_raw_slice()),
            len,
        ))
    }

    pub fn peers(&self, id: &str) -> anyhow::Result<Vec<PeerEntry>> {
        let h = self.handle(id)?;
        let Some(live) = h.live() else {
            return Ok(Vec::new());
        };
        let snap = live.per_peer_stats_snapshot(Default::default());
        let now = Instant::now();
        let mut prev = self.peer_prev.lock();
        let p = prev.entry(id.to_owned()).or_default();
        let dt =
            p.at.map(|t| now.duration_since(t).as_secs_f64())
                .filter(|d| *d > 0.2);
        let mut next = HashMap::new();
        let mut out: Vec<PeerEntry> = snap
            .peers
            .into_iter()
            .map(|(addr, ps)| {
                let (down, up) = (ps.counters.fetched_bytes, ps.counters.uploaded_bytes);
                let (ds, us) = match (dt, p.values.get(&addr)) {
                    (Some(dt), Some((pd, pu))) => (
                        (down.saturating_sub(*pd) as f64 / dt) as u64,
                        (up.saturating_sub(*pu) as f64 / dt) as u64,
                    ),
                    _ => (0, 0),
                };
                next.insert(addr.clone(), (down, up));
                PeerEntry {
                    connection: ps.conn_kind.map(|k| format!("{k:?}").to_lowercase()),
                    client: ps.client_name,
                    downloaded: down,
                    uploaded: up,
                    down_speed: ds,
                    up_speed: us,
                    state: ps.state.to_owned(),
                    addr,
                }
            })
            .collect();
        p.at = Some(now);
        p.values = next;
        out.sort_by(|a, b| {
            b.down_speed
                .cmp(&a.down_speed)
                .then(b.downloaded.cmp(&a.downloaded))
        });
        Ok(out)
    }

    // ---------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------

    pub async fn pause(&self, id: &str) -> anyhow::Result<()> {
        let h = self.handle(id)?;
        self.meta.update(id, |m| m.user_paused = true);
        let _ = self.meta.save();
        // While checking files the torrent can't be paused yet; the tick's
        // `sync_run_state` pauses it as soon as the check is done.
        if matches!(h.stats().state, TorrentStatsState::Live) {
            self.session()?.pause(&h).await?;
        }
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> anyhow::Result<()> {
        let h = self.handle(id)?;
        // Starting a torrent whose files were all deleted / skipped downloads it again.
        let all_skipped = {
            let m = self.meta.get(id).unwrap_or_default();
            !m.priorities.is_empty() && m.priorities.iter().all(|p| *p == FilePriority::Skip)
        };
        if all_skipped {
            let n = self.meta.get(id).unwrap_or_default().priorities.len();
            let changes = (0..n).map(|i| (i, FilePriority::Normal)).collect();
            self.set_file_priorities(id, &changes).await?;
        }
        self.meta.update(id, |m| m.user_paused = false);
        let _ = self.meta.save();
        if matches!(h.stats().state, TorrentStatsState::Paused) && !self.global_pause.lock().any() {
            self.session()?.unpause(&h).await?;
        }
        Ok(())
    }

    pub async fn remove(&self, id: &str, delete_files: bool) -> anyhow::Result<()> {
        let pending = self.pending.lock().remove(id);
        if let Some(p) = pending {
            if let Some(t) = p.task {
                t.abort();
            }
            self.save_pending();
            return Ok(());
        }
        let hash = parse_hash(id)?;
        self.session()?
            .delete(TorrentIdOrHash::Hash(hash), delete_files)
            .await?;
        self.meta.remove(id);
        self.ticks.lock().remove(id);
        let _ = self.meta.save();
        Ok(())
    }

    pub async fn set_file_priorities(
        &self,
        id: &str,
        changes: &HashMap<usize, FilePriority>,
    ) -> anyhow::Result<()> {
        let h = self.handle(id)?;
        let n = h
            .metadata
            .load()
            .as_ref()
            .map(|m| m.file_infos.len())
            .context("metadata not resolved yet")?;
        let meta = self.meta.update(id, |m| {
            m.priorities.resize(n, FilePriority::Normal);
            for (idx, p) in changes {
                if *idx < n {
                    m.priorities[*idx] = *p;
                }
            }
            m.clone()
        });
        let _ = self.meta.save();

        let wanted: HashSet<usize> = (0..n)
            .filter(|i| meta.priorities[*i] != FilePriority::Skip)
            .collect();
        let current: HashSet<usize> = h
            .only_files()
            .map(|v| v.into_iter().collect())
            .unwrap_or_else(|| (0..n).collect());
        if wanted != current {
            if wanted.is_empty() {
                // Nothing selected: keep files as is but stop downloading.
                self.pause(id).await?;
            } else if matches!(h.stats().state, TorrentStatsState::Initializing { .. }) {
                // Still checking files; `sync_only_files` applies it when done.
            } else {
                self.session()?.update_only_files(&h, &wanted).await?;
            }
        }
        h.set_piece_priority(Some(meta.piece_priority(n)))?;
        Ok(())
    }

    pub fn add_peers(&self, id: &str, peers: &[String]) -> anyhow::Result<usize> {
        let addrs: Vec<SocketAddr> = peers.iter().filter_map(|p| p.trim().parse().ok()).collect();
        if addrs.is_empty() {
            bail!("expected peers as ip:port");
        }
        Ok(self.handle(id)?.add_peers(&addrs))
    }

    /// "Download in order": the given files get high priority and the torrent
    /// switches to sequential mode, so they arrive one after another by name.
    pub async fn download_in_order(&self, id: &str, files: &[usize]) -> anyhow::Result<()> {
        let changes: HashMap<usize, FilePriority> =
            files.iter().map(|f| (*f, FilePriority::High)).collect();
        self.meta.update(id, |m| m.sequential = true);
        self.set_file_priorities(id, &changes).await?;
        if self.handle(id)?.is_paused() && !self.meta.get(id).unwrap_or_default().user_paused {
            self.resume(id).await?;
        }
        Ok(())
    }

    pub fn set_options(
        &self,
        id: &str,
        sequential: Option<bool>,
        category: Option<Option<String>>,
    ) -> anyhow::Result<()> {
        let h = self.handle(id)?;
        let meta = self.meta.update(id, |m| {
            if let Some(s) = sequential {
                m.sequential = s;
            }
            if let Some(c) = category {
                m.category = c.filter(|c| !c.trim().is_empty());
            }
            m.clone()
        });
        let _ = self.meta.save();
        if let Some(n) = h.metadata.load().as_ref().map(|m| m.file_infos.len()) {
            h.set_piece_priority(Some(meta.piece_priority(n)))?;
        }
        Ok(())
    }

    /// Get a torrent ready for streaming a file: make sure it runs and the file
    /// is selected with high priority.
    pub async fn prepare_stream(&self, id: &str, file: usize) -> anyhow::Result<()> {
        let h = self.handle(id)?;
        let n = h
            .metadata
            .load()
            .as_ref()
            .map(|m| m.file_infos.len())
            .context("metadata not resolved yet")?;
        if file >= n {
            bail!("no such file");
        }
        let meta = self.meta.get(id).unwrap_or_default();
        let current = meta.priorities.get(file).copied().unwrap_or_default();
        if current != FilePriority::High {
            let mut changes = HashMap::new();
            changes.insert(file, FilePriority::High);
            self.set_file_priorities(id, &changes).await?;
        }
        if h.is_paused() {
            self.resume(id).await?;
        }
        Ok(())
    }

    pub async fn stream(
        &self,
        id: &str,
        file: usize,
    ) -> anyhow::Result<(librqbit::FileStream, String)> {
        let h = self.handle(id)?;
        let name = h
            .metadata
            .load()
            .as_ref()
            .and_then(|m| m.file_infos.get(file).map(|f| f.relative_filename.clone()))
            .context("no such file")?;
        // Right after a resume or a file-selection change the torrent may be
        // between states for a moment; don't fail the player's request for that.
        let mut attempt = 0;
        let stream = loop {
            match h.clone().stream(file).await {
                Ok(s) => break s,
                Err(e) if attempt < 50 && !matches!(h.stats().state, TorrentStatsState::Error) => {
                    debug!(%id, "stream not ready yet: {e:#}");
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(e),
            }
        };
        Ok((stream, name.to_string_lossy().into_owned()))
    }

    /// Pause every torrent; one failing torrent doesn't stop the rest.
    pub async fn pause_all(&self) -> anyhow::Result<()> {
        let mut last_err = None;
        for h in self.handles() {
            let id = hash_str(&h.info_hash());
            if let Err(e) = self.pause(&id).await {
                warn!(%id, "pause failed: {e:#}");
                last_err = Some(e);
            }
        }
        last_err.map_or(Ok(()), Err)
    }

    pub async fn resume_all(&self) -> anyhow::Result<()> {
        let mut last_err = None;
        for h in self.handles() {
            let id = hash_str(&h.info_hash());
            if let Err(e) = self.resume(&id).await {
                warn!(%id, "resume failed: {e:#}");
                last_err = Some(e);
            }
        }
        last_err.map_or(Ok(()), Err)
    }

    pub fn file_path(&self, id: &str, file: usize) -> anyhow::Result<PathBuf> {
        let h = self.handle(id)?;
        let rel = h
            .metadata
            .load()
            .as_ref()
            .and_then(|m| m.file_infos.get(file).map(|f| f.relative_filename.clone()))
            .context("no such file")?;
        Ok(h.output_folder().join(rel))
    }

    pub fn torrent_folder(&self, id: &str) -> anyhow::Result<PathBuf> {
        Ok(self.handle(id)?.output_folder().to_path_buf())
    }

    pub fn torrent_file_bytes(&self, id: &str) -> anyhow::Result<(String, Bytes)> {
        let h = self.handle(id)?;
        let md = h
            .metadata
            .load_full()
            .context("metadata not resolved yet")?;
        Ok((
            h.name().unwrap_or_else(|| id.to_owned()),
            md.torrent_bytes.clone(),
        ))
    }

    // ---------------------------------------------------------------------
    // Background work
    // ---------------------------------------------------------------------

    fn spawn_background(self: Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut i: u64 = 0;
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = this.cancel.cancelled() => break,
                    _ = interval.tick() => {}
                }
                i += 1;
                this.tick();
                if i.is_multiple_of(30) {
                    this.apply_scheduler();
                }
                if i.is_multiple_of(10) {
                    let _ = this.meta.save();
                }
            }
        });
        crate::netguard::spawn(self.clone());
        crate::rss::spawn(self);
    }

    /// Stop background work and the BitTorrent session, saving state.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let _ = self.meta.save();
        let session = self.session.write().take();
        if let Some(s) = session {
            s.stop().await;
        }
    }

    /// React to files that vanished from disk while we were not running
    /// (detected by the patched startup check in librqbit).
    fn handle_missing_files(self: &Arc<Self>, h: &TorrentHandle) {
        if matches!(h.stats().state, TorrentStatsState::Initializing { .. }) {
            return;
        }
        let missing = h.shared().take_missing_files();
        if missing.is_empty() {
            return;
        }
        let id = hash_str(&h.info_hash());
        let name = h.name().unwrap_or_else(|| id.clone());
        let nothing_left = h.stats().progress_bytes == 0;
        let this = self.clone();
        tokio::spawn(async move {
            let res = if nothing_left {
                // The whole download is gone: stop and wait for the user.
                // Pressing "Start" downloads it again.
                this.pause(&id).await
            } else {
                // Some files were deleted, most likely on purpose (e.g. a watched
                // episode): don't download them again unless the user asks.
                let changes = missing.iter().map(|f| (*f, FilePriority::Skip)).collect();
                this.set_file_priorities(&id, &changes).await
            };
            if let Err(e) = res {
                warn!(%id, "error handling missing files: {e:#}");
            }
            this.emit(Event::FilesMissing {
                id,
                name,
                files: missing.len(),
                all: nothing_left,
            });
        });
    }

    /// Bring librqbit's file selection in line with the stored priorities
    /// (changes made while a torrent was still checking are applied here).
    fn sync_only_files(self: &Arc<Self>, h: &TorrentHandle) {
        if matches!(h.stats().state, TorrentStatsState::Initializing { .. }) {
            return;
        }
        let Some(n) = h.metadata.load().as_ref().map(|m| m.file_infos.len()) else {
            return;
        };
        let id = hash_str(&h.info_hash());
        let Some(meta) = self.meta.get(&id) else {
            return;
        };
        let wanted: HashSet<usize> = (0..n)
            .filter(|i| meta.priorities.get(*i).copied().unwrap_or_default() != FilePriority::Skip)
            .collect();
        let current: HashSet<usize> = h
            .only_files()
            .map(|v| v.into_iter().collect())
            .unwrap_or_else(|| (0..n).collect());
        if wanted.is_empty() || wanted == current || !self.syncing_files.lock().insert(id.clone()) {
            return;
        }
        let this = self.clone();
        let h = h.clone();
        tokio::spawn(async move {
            if let Ok(session) = this.session()
                && let Err(e) = session.update_only_files(&h, &wanted).await
            {
                warn!(%id, "error updating selected files: {e:#}");
            }
            this.syncing_files.lock().remove(&id);
        });
    }

    /// Make the torrent's running state match what the user chose (a Start or
    /// Pause pressed while it was still checking files would otherwise be lost).
    fn sync_run_state(self: &Arc<Self>, h: &TorrentHandle) {
        let is_paused = match h.stats().state {
            TorrentStatsState::Paused => true,
            TorrentStatsState::Live => false,
            _ => return,
        };
        let id = hash_str(&h.info_hash());
        let Some(meta) = self.meta.get(&id) else {
            return;
        };
        let should_run = !meta.user_paused && !self.global_pause.lock().any();
        if should_run != is_paused || !self.syncing_run.lock().insert(id.clone()) {
            return;
        }
        let this = self.clone();
        let h = h.clone();
        tokio::spawn(async move {
            if let Ok(session) = this.session() {
                let res = if should_run {
                    session.unpause(&h).await
                } else {
                    session.pause(&h).await
                };
                if let Err(e) = res {
                    warn!(%id, "error changing torrent state: {e:#}");
                }
            }
            this.syncing_run.lock().remove(&id);
        });
    }

    fn tick(self: &Arc<Self>) {
        // Session-wide speed history for the graph.
        if let Some(s) = self.session.read().as_ref() {
            let st = s.stats_snapshot();
            let to_bps = |mbps: f64| (mbps * 1024.0 * 1024.0) as u64;
            let mut hist = self.speed_history.lock();
            hist.push_back((
                now_ts(),
                to_bps(st.download_speed.mbps),
                to_bps(st.upload_speed.mbps),
            ));
            while hist.len() > 600 {
                hist.pop_front();
            }
        }

        let ratio_limit = self.settings.read().seed_ratio_limit;
        for h in self.handles() {
            self.handle_missing_files(&h);
            self.sync_only_files(&h);
            self.sync_run_state(&h);
            let id = hash_str(&h.info_hash());
            let stats = h.stats();
            let Some(live) = stats.live.as_ref() else {
                continue;
            };
            let fetched = live.snapshot.fetched_bytes;
            let uploaded = live.snapshot.uploaded_bytes;
            let (d_down, d_up, just_completed) = {
                let mut ticks = self.ticks.lock();
                let t = ticks.entry(id.clone()).or_default();
                let d_down = fetched.saturating_sub(t.last_fetched);
                let d_up = uploaded.saturating_sub(t.last_uploaded);
                t.last_fetched = fetched;
                t.last_uploaded = uploaded;
                let just_completed = t.seen_unfinished && stats.finished;
                t.seen_unfinished = !stats.finished;
                (d_down, d_up, just_completed)
            };
            let meta = self.meta.update(&id, |m| {
                m.total_downloaded += d_down;
                m.total_uploaded += d_up;
                if just_completed {
                    m.completed_at = Some(now_ts());
                }
                m.clone()
            });
            if just_completed {
                self.emit(Event::Completed {
                    id: id.clone(),
                    name: h.name().unwrap_or_else(|| id.clone()),
                });
            }
            if let Some(limit) = ratio_limit
                && stats.finished
                && limit > 0.0
                && meta.total_downloaded > 0
                && meta.total_uploaded as f64 / meta.total_downloaded as f64 >= limit
                && !meta.user_paused
            {
                info!(%id, "seed ratio limit reached, pausing");
                self.meta.update(&id, |m| m.user_paused = true);
                self.apply_run_state(&h, &self.meta.get(&id).unwrap_or_default());
            }
        }
    }

    /// Aggregate download health used by the automatic network mode.
    pub(crate) fn health(&self) -> Health {
        let mut h = Health::default();
        if !self.global_pause.lock().any() {
            let start_paused = self.settings.read().start_paused;
            h.downloading = self
                .pending
                .lock()
                .values()
                .filter(|p| p.error.is_none() && !p.req.paused.unwrap_or(start_paused))
                .count() as u32;
        }
        for t in self.handles() {
            let stats = t.stats();
            if let (TorrentStatsState::Live, false) = (&stats.state, stats.finished) {
                h.downloading += 1;
                if let Some(live) = t.live() {
                    h.down_bps += live.down_speed_estimator().bps();
                }
                if let Some(l) = stats.live.as_ref() {
                    h.live_peers += l.snapshot.peer_stats.live;
                    h.seen_peers += l.snapshot.peer_stats.seen;
                }
            }
        }
        h.dht_nodes = self
            .session
            .read()
            .as_ref()
            .and_then(|s| s.get_dht())
            .map(|d| d.stats().routing_table_size)
            .unwrap_or(0);
        h
    }

    pub fn network_mode(&self) -> NetworkMode {
        self.settings.read().network.mode
    }
}

#[cfg(test)]
mod tests {
    use super::looks_like_series;

    #[test]
    fn series_detection() {
        let gb = 1 << 30;
        assert!(looks_like_series([
            ("e1.mkv", gb),
            ("e2.mkv", gb),
            ("s.srt", 1000)
        ]));
        assert!(!looks_like_series([
            ("movie.mkv", 4 * gb),
            ("poster.jpg", 100_000)
        ]));
        assert!(!looks_like_series([
            ("a.mp4", 1),
            ("b.mp4", 1),
            ("game.iso", gb)
        ]));
    }
}

#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct Health {
    pub downloading: u32,
    pub down_bps: u64,
    pub live_peers: u32,
    pub seen_peers: u32,
    pub dht_nodes: usize,
}
