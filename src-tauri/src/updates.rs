//! Self-updates from GitHub Releases.
//!
//! Checks shortly after start and then every few hours; the UI shows a banner
//! and installs on request: on Windows the NSIS installer from the latest
//! release is downloaded and run in passive mode, elsewhere the release page
//! is opened.

use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

const REPO: &str = "DenTheProgrammer/Tortuga";
const FIRST_CHECK: Duration = Duration::from_secs(15);
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
}

#[derive(Clone, Deserialize)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Clone, Deserialize)]
struct Release {
    tag_name: String,
    #[cfg_attr(windows, allow(dead_code))]
    html_url: String,
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Default)]
pub struct UpdateState {
    pending: Mutex<Option<(Release, UpdateInfo)>>,
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Tortuga/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(600))
        .build()?)
}

fn parse_version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.trim().trim_start_matches(['v', 'V'])).ok()
}

/// NSIS installer for this machine, e.g. `Tortuga_0.2.0_x64-setup.exe`.
fn installer(release: &Release) -> Option<&Asset> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    release
        .assets
        .iter()
        .find(|a| a.name.ends_with(&format!("_{arch}-setup.exe")))
}

async fn check(app: &AppHandle) -> anyhow::Result<Option<UpdateInfo>> {
    let release: Release = client()?
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let current = app.package_info().version.clone();
    let newer = parse_version(&release.tag_name).is_some_and(|v| v > current);
    if !newer || release.draft || release.prerelease {
        *app.state::<UpdateState>().pending.lock() = None;
        return Ok(None);
    }
    if cfg!(windows) && installer(&release).is_none() {
        anyhow::bail!("release {} has no installer", release.tag_name);
    }
    let info = UpdateInfo {
        version: release.tag_name.trim_start_matches(['v', 'V']).to_owned(),
        current_version: current.to_string(),
        notes: release.body.clone().filter(|b| !b.trim().is_empty()),
    };
    *app.state::<UpdateState>().pending.lock() = Some((release, info.clone()));
    Ok(Some(info))
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
    state.pending.lock().as_ref().map(|(_, i)| i.clone())
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    check(&app).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let release = app
        .state::<UpdateState>()
        .pending
        .lock()
        .as_ref()
        .map(|(r, _)| r.clone())
        .ok_or("no update available")?;
    install(&app, &release).await.map_err(|e| format!("{e:#}"))
}

#[cfg(windows)]
async fn install(app: &AppHandle, release: &Release) -> anyhow::Result<()> {
    let asset = installer(release).ok_or_else(|| anyhow::anyhow!("no installer"))?;
    let bytes = client()?
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    anyhow::ensure!(
        bytes.len() as u64 == asset.size,
        "download is incomplete ({} of {} bytes)",
        bytes.len(),
        asset.size
    );
    let dir = std::env::temp_dir().join("tortuga-update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&asset.name);
    std::fs::write(&path, &bytes)?;

    // Save resume data before the installer replaces (and may stop) the app.
    crate::stop_engine(app).await;

    // Same flags as the Tauri updater: passive UI, update mode, relaunch after install.
    std::process::Command::new(&path)
        .args(["/P", "/UPDATE", "/R"])
        .spawn()
        .map_err(|e| anyhow::anyhow!("can't start installer: {e}"))?;
    crate::quit(app);
    Ok(())
}

#[cfg(not(windows))]
async fn install(app: &AppHandle, release: &Release) -> anyhow::Result<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(&release.html_url, None::<&str>)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_installer_and_version() {
        let r: Release = serde_json::from_str(
            r#"{"tag_name":"v0.2.0","html_url":"https://x","body":"","assets":[
                {"name":"Tortuga_0.2.0_x64_en-US.msi","browser_download_url":"a","size":1},
                {"name":"Tortuga_0.2.0_x64-setup.exe","browser_download_url":"b","size":2}]}"#,
        )
        .unwrap();
        assert_eq!(installer(&r).unwrap().browser_download_url, "b");
        assert!(parse_version(&r.tag_name).unwrap() > semver::Version::new(0, 1, 0));
        assert!(parse_version("nightly").is_none());
    }
}
