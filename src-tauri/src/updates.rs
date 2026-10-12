//! Self-updates from GitHub Releases (signed, see `plugins.updater` in tauri.conf.json).
//!
//! Checks shortly after start and then every few hours; the UI shows a banner
//! and installs on request.

use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK: Duration = Duration::from_secs(15);
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
}

#[derive(Default)]
pub struct UpdateState {
    pending: Mutex<Option<Update>>,
}

fn info(u: &Update) -> UpdateInfo {
    UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u.body.clone(),
    }
}

async fn check(app: &AppHandle) -> anyhow::Result<Option<UpdateInfo>> {
    let Some(update) = app.updater()?.check().await? else {
        return Ok(None);
    };
    let i = info(&update);
    *app.state::<UpdateState>().pending.lock() = Some(update);
    Ok(Some(i))
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            match check(&app).await {
                Ok(Some(i)) => {
                    tracing::info!(version = %i.version, "update available");
                    let _ = app.emit("update-available", i);
                }
                Ok(None) => tracing::debug!("no updates"),
                Err(e) => tracing::info!("update check failed: {e:#}"),
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

#[tauri::command]
pub fn pending_update(state: tauri::State<UpdateState>) -> Option<UpdateInfo> {
    state.pending.lock().as_ref().map(info)
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    check(&app).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let update = app
        .state::<UpdateState>()
        .pending
        .lock()
        .take()
        .ok_or("no update available")?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}
