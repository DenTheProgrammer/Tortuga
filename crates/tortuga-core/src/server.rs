//! Local HTTP API used by the UI, plus range-capable file streaming for the
//! built-in player and external players (VLC, mpv, MPC...).
//!
//! The server listens on 127.0.0.1 only and every request must carry the
//! session token (header `x-tortuga-token` or `?token=` query parameter).

use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tower_http::cors::{Any, CorsLayer};

use crate::{
    create,
    engine::{AddRequest, Engine},
    meta::FilePriority,
    netguard, rss,
    settings::Settings,
};

type AppState = Arc<Engine>;

pub struct ApiError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let msg = format!("{:#}", self.0);
        let status = if msg.contains("not found") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn ok() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

async fn auth(State(engine): State<AppState>, req: Request, next: Next) -> Response {
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
    let header_ok = req
        .headers()
        .get("x-tortuga-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t == engine.token);
    let query_ok = req.uri().query().is_some_and(|q| {
        url::form_urlencoded::parse(q.as_bytes()).any(|(k, v)| k == "token" && v == engine.token)
    });
    if header_ok || query_ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn h_state(State(e): State<AppState>) -> impl IntoResponse {
    Json(e.state())
}

async fn h_speed(State(e): State<AppState>) -> impl IntoResponse {
    Json(e.speed_history())
}

async fn h_get_settings(State(e): State<AppState>) -> impl IntoResponse {
    Json(e.settings())
}

async fn h_put_settings(
    State(e): State<AppState>,
    Json(s): Json<Settings>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(e.update_settings(s).await?))
}

#[derive(Deserialize)]
struct AltSpeed {
    enabled: bool,
}

async fn h_alt_speed(
    State(e): State<AppState>,
    Json(b): Json<AltSpeed>,
) -> ApiResult<impl IntoResponse> {
    e.set_alt_speed(b.enabled)?;
    Ok(ok())
}

async fn h_resolve(
    State(e): State<AppState>,
    Json(req): Json<AddRequest>,
) -> ApiResult<impl IntoResponse> {
    let res = tokio::time::timeout(std::time::Duration::from_secs(180), e.resolve(&req))
        .await
        .map_err(|_| anyhow::anyhow!("timed out fetching metadata"))??;
    Ok(Json(res))
}

async fn h_add(
    State(e): State<AppState>,
    Json(req): Json<AddRequest>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(e.add(req).await?))
}

async fn h_details(
    State(e): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(e.details(&id)?))
}

async fn h_peers(
    State(e): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(e.peers(&id)?))
}

async fn h_pieces(
    State(e): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let (bits, total) = e.pieces(&id)?;
    Ok(Json(json!({ "bitfield": bits, "total": total })))
}

async fn h_pause(
    State(e): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    e.pause(&id).await?;
    Ok(ok())
}

async fn h_resume(
    State(e): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    e.resume(&id).await?;
    Ok(ok())
}

async fn h_pause_all(State(e): State<AppState>) -> ApiResult<impl IntoResponse> {
    e.pause_all().await?;
    Ok(ok())
}

async fn h_resume_all(State(e): State<AppState>) -> ApiResult<impl IntoResponse> {
    e.resume_all().await?;
    Ok(ok())
}

#[derive(Deserialize)]
struct RemoveQuery {
    #[serde(default)]
    delete_files: bool,
}

async fn h_remove(
    State(e): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<impl IntoResponse> {
    e.remove(&id, q.delete_files).await?;
    Ok(ok())
}

#[derive(Deserialize)]
struct PrioritiesBody {
    priorities: HashMap<usize, FilePriority>,
}

async fn h_priorities(
    State(e): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<PrioritiesBody>,
) -> ApiResult<impl IntoResponse> {
    e.set_file_priorities(&id, &b.priorities).await?;
    Ok(ok())
}

#[derive(Deserialize)]
struct OptionsBody {
    sequential: Option<bool>,
    #[serde(default, deserialize_with = "serde_with_double_option::deserialize")]
    category: Option<Option<String>>,
}

/// Distinguish "field missing" from "field: null" for category.
mod serde_with_double_option {
    use serde::{Deserialize, Deserializer};
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<String>>, D::Error> {
        Ok(Some(Option::<String>::deserialize(d)?))
    }
}

async fn h_options(
    State(e): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<OptionsBody>,
) -> ApiResult<impl IntoResponse> {
    e.set_options(&id, b.sequential, b.category)?;
    Ok(ok())
}

#[derive(Deserialize)]
struct InOrderBody {
    files: Vec<usize>,
}

async fn h_in_order(
    State(e): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<InOrderBody>,
) -> ApiResult<impl IntoResponse> {
    e.download_in_order(&id, &b.files).await?;
    Ok(ok())
}

fn stream_path(e: &Engine, id: &str, file: usize, name: &str) -> String {
    let name: String =
        url::form_urlencoded::byte_serialize(name.rsplit('/').next().unwrap_or(name).as_bytes())
            .collect();
    format!("/stream/{id}/{file}/{name}?token={}", e.token)
}

async fn h_play(
    State(e): State<AppState>,
    Path((id, file)): Path<(String, usize)>,
) -> ApiResult<impl IntoResponse> {
    e.prepare_stream(&id, file).await?;
    let path = e.file_path(&id, file)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    Ok(Json(json!({
        "path": stream_path(&e, &id, file, &name),
        "local_path": path,
        "name": name,
    })))
}

async fn h_file_path(
    State(e): State<AppState>,
    Path((id, file)): Path<(String, usize)>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(
        json!({ "path": e.file_path(&id, file)?, "folder": e.torrent_folder(&id)? }),
    ))
}

async fn h_torrent_file(State(e): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let (name, bytes) = e.torrent_file_bytes(&id)?;
    let fname: String =
        url::form_urlencoded::byte_serialize(format!("{name}.torrent").as_bytes()).collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-bittorrent".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename*=UTF-8''{fname}"),
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn h_network(State(e): State<AppState>) -> impl IntoResponse {
    let snap = tokio::task::spawn_blocking(netguard::interfaces).await.ok();
    Json(json!({ "status": e.state().session.network, "snapshot": snap }))
}

async fn h_create(
    State(e): State<AppState>,
    Json(req): Json<create::CreateRequest>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(create::create(&e, req).await?))
}

// RSS

async fn h_rss(State(e): State<AppState>) -> impl IntoResponse {
    Json(rss::store(&e).snapshot())
}

async fn h_rss_feed(
    State(e): State<AppState>,
    Json(f): Json<rss::Feed>,
) -> ApiResult<impl IntoResponse> {
    let feed = rss::store(&e).upsert_feed(f)?;
    let e2 = e.clone();
    let id = feed.id.clone();
    tokio::spawn(async move {
        let _ = rss::refresh_feed(&e2, &id).await;
    });
    Ok(Json(feed))
}

async fn h_rss_feed_delete(State(e): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    rss::store(&e).delete_feed(&id);
    ok()
}

async fn h_rss_refresh(
    State(e): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    rss::refresh_feed(&e, &id).await?;
    Ok(ok())
}

async fn h_rss_rule(
    State(e): State<AppState>,
    Json(r): Json<rss::Rule>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(rss::store(&e).upsert_rule(r)?))
}

async fn h_rss_rule_delete(State(e): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    rss::store(&e).delete_rule(&id);
    ok()
}

#[derive(Deserialize)]
struct RssItemRef {
    feed_id: String,
    guid: String,
}

async fn h_rss_download(
    State(e): State<AppState>,
    Json(r): Json<RssItemRef>,
) -> ApiResult<impl IntoResponse> {
    rss::download_item(&e, &r.feed_id, &r.guid).await?;
    Ok(ok())
}

// Streaming

fn parse_range(headers: &HeaderMap, len: u64) -> Option<(u64, u64)> {
    let v = headers.get(header::RANGE)?.to_str().ok()?;
    let spec = v.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (s, e) = spec.split_once('-')?;
    let (start, end) = if s.is_empty() {
        // suffix range: last N bytes
        let n: u64 = e.parse().ok()?;
        (len.saturating_sub(n), len.checked_sub(1)?)
    } else {
        let start: u64 = s.parse().ok()?;
        let end = if e.is_empty() {
            len.checked_sub(1)?
        } else {
            e.parse::<u64>().ok()?.min(len.checked_sub(1)?)
        };
        (start, end)
    };
    (start <= end && start < len).then_some((start, end))
}

async fn h_stream(
    State(e): State<AppState>,
    Path((id, file, name)): Path<(String, usize, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (mut stream, _) = e.stream(&id, file).await?;
    let len = stream.len();
    let mime = mime_guess::from_path(&name)
        .first_raw()
        .unwrap_or("application/octet-stream");
    // Browsers refuse Matroska as video/x-matroska but play it as webm.
    let mime = if mime == "video/x-matroska" {
        "video/webm"
    } else {
        mime
    };

    let mut builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "no-store");

    let (status, start, end) = match parse_range(&headers, len) {
        Some((s, en)) => (StatusCode::PARTIAL_CONTENT, s, en),
        None if headers.contains_key(header::RANGE) && len > 0 => {
            return Ok(Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Body::empty())?);
        }
        None => (StatusCode::OK, 0, len.saturating_sub(1)),
    };
    let body_len = if len == 0 { 0 } else { end - start + 1 };
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    if start > 0 {
        stream.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let reader = stream.take(body_len);
    let body = Body::from_stream(tokio_util::io::ReaderStream::with_capacity(
        reader,
        64 * 1024,
    ));
    Ok(builder
        .status(status)
        .header(header::CONTENT_LENGTH, body_len)
        .body(body)?)
}

async fn h_ping() -> impl IntoResponse {
    Json(json!({ "app": "tortuga", "version": env!("CARGO_PKG_VERSION") }))
}

pub fn router(engine: Arc<Engine>, ui_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        .route("/api/state", get(h_state))
        .route("/api/speed", get(h_speed))
        .route("/api/settings", get(h_get_settings).put(h_put_settings))
        .route("/api/alt-speed", post(h_alt_speed))
        .route("/api/resolve", post(h_resolve))
        .route("/api/torrents", post(h_add))
        .route("/api/torrents/pause-all", post(h_pause_all))
        .route("/api/torrents/resume-all", post(h_resume_all))
        .route("/api/torrents/{id}", get(h_details).delete(h_remove))
        .route("/api/torrents/{id}/peers", get(h_peers))
        .route("/api/torrents/{id}/pieces", get(h_pieces))
        .route("/api/torrents/{id}/pause", post(h_pause))
        .route("/api/torrents/{id}/resume", post(h_resume))
        .route("/api/torrents/{id}/files", post(h_priorities))
        .route("/api/torrents/{id}/options", post(h_options))
        .route("/api/torrents/{id}/in-order", post(h_in_order))
        .route("/api/torrents/{id}/files/{file}/play", post(h_play))
        .route("/api/torrents/{id}/files/{file}/path", get(h_file_path))
        .route("/api/torrents/{id}/torrent-file", get(h_torrent_file))
        .route("/api/network", get(h_network))
        .route("/api/create", post(h_create))
        .route("/api/rss", get(h_rss))
        .route("/api/rss/feeds", post(h_rss_feed))
        .route("/api/rss/feeds/{id}", delete(h_rss_feed_delete))
        .route("/api/rss/feeds/{id}/refresh", post(h_rss_refresh))
        .route("/api/rss/rules", post(h_rss_rule))
        .route("/api/rss/rules/{id}", delete(h_rss_rule_delete))
        .route("/api/rss/download", post(h_rss_download))
        .route("/stream/{id}/{file}/{name}", get(h_stream))
        .layer(middleware::from_fn_with_state(engine.clone(), auth));

    let mut app = Router::new()
        .route("/api/ping", get(h_ping))
        .merge(api)
        .with_state(engine);

    if let Some(dir) = ui_dir {
        app = app.fallback_service(
            tower_http::services::ServeDir::new(&dir)
                .fallback(tower_http::services::ServeFile::new(dir.join("index.html"))),
        );
    }

    app.layer(
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
            .expose_headers([header::CONTENT_RANGE, header::CONTENT_LENGTH]),
    )
    .layer(middleware::from_fn(no_sniff))
}

async fn no_sniff(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    resp.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    resp
}

/// Bind the API on localhost. Returns the bound address.
pub async fn serve(
    engine: Arc<Engine>,
    addr: SocketAddr,
    ui_dir: Option<PathBuf>,
) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let app = router(engine, ui_dir);
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("http server error: {e:#}");
        }
    });
    Ok((local, task))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(v: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(header::RANGE, v.parse().unwrap());
        m
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range(&h("bytes=0-99"), 1000), Some((0, 99)));
        assert_eq!(parse_range(&h("bytes=500-"), 1000), Some((500, 999)));
        assert_eq!(parse_range(&h("bytes=-100"), 1000), Some((900, 999)));
        assert_eq!(parse_range(&h("bytes=900-5000"), 1000), Some((900, 999)));
        assert_eq!(parse_range(&h("bytes=1000-"), 1000), None);
        assert_eq!(parse_range(&HeaderMap::new(), 1000), None);
    }
}
