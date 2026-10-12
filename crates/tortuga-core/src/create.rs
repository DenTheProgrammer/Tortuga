//! Creating .torrent files from local files or folders.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, bail};
use base64::Engine as _;
use librqbit::{
    AddTorrent, AddTorrentOptions, ByteBufOwned, CreateTorrentOptions, spawn_utils::BlockingSpawner,
};
use serde::{Deserialize, Serialize};

use crate::{engine::Engine, meta::TorrentMeta};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CreateRequest {
    pub path: String,
    pub name: Option<String>,
    pub trackers: Vec<String>,
    /// Piece size in bytes; None = automatic.
    pub piece_length: Option<u32>,
    pub comment: Option<String>,
    /// Start seeding right away.
    pub seed: bool,
    /// Where to write the .torrent file.
    pub save_to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateResponse {
    pub id: String,
    pub name: String,
    pub magnet: String,
    pub torrent_base64: String,
    pub saved_to: Option<String>,
}

/// True if `path` is a non-empty file or a folder containing one.
fn has_data(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m.len() > 0,
        Ok(m) if m.is_dir() => std::fs::read_dir(path)
            .map(|rd| rd.flatten().any(|e| has_data(&e.path())))
            .unwrap_or(false),
        _ => false,
    }
}

pub async fn create(engine: &Arc<Engine>, req: CreateRequest) -> anyhow::Result<CreateResponse> {
    let path = PathBuf::from(req.path.trim());
    if !path.exists() {
        bail!("path does not exist: {}", path.display());
    }
    if !has_data(&path) {
        bail!(
            "nothing to share: {} has no non-empty files",
            path.display()
        );
    }
    if let Some(pl) = req.piece_length
        && (!pl.is_power_of_two() || pl < 16 * 1024)
    {
        bail!("piece size must be a power of two and at least 16 KiB");
    }
    let trackers: Vec<String> = req
        .trackers
        .iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect();

    let spawner = BlockingSpawner::new(2);
    let mut result = librqbit::create_torrent(
        &path,
        CreateTorrentOptions {
            name: req.name.as_deref().filter(|n| !n.trim().is_empty()),
            trackers: trackers.clone(),
            piece_length: req.piece_length,
        },
        &spawner,
    )
    .await
    .context("error creating torrent")?;

    result.meta.created_by = Some(ByteBufOwned::from(
        format!("Tortuga {}", env!("CARGO_PKG_VERSION")).as_bytes(),
    ));
    result.meta.creation_date = Some(chrono::Utc::now().timestamp() as usize);
    if let Some(c) = req.comment.as_deref().filter(|c| !c.trim().is_empty()) {
        result.meta.comment = Some(ByteBufOwned::from(c.as_bytes()));
    }
    if let Some(first) = trackers.first() {
        result.meta.announce = Some(ByteBufOwned::from(first.as_bytes()));
    }

    let bytes = result.as_bytes()?;
    let id = result.info_hash().as_string();
    let name = result
        .meta
        .info
        .data
        .name
        .as_ref()
        .map(|n| String::from_utf8_lossy(n.as_ref()).into_owned())
        .unwrap_or_else(|| id.clone());

    let saved_to = match req.save_to.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(p) => {
            let mut out = PathBuf::from(p.trim());
            if out.is_dir() {
                out = out.join(format!("{name}.torrent"));
            }
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("can't create {}", parent.display()))?;
            }
            std::fs::write(&out, &bytes)
                .with_context(|| format!("can't write {}", out.display()))?;
            Some(out.to_string_lossy().into_owned())
        }
        None => None,
    };

    if req.seed {
        let session = engine.session()?;
        engine.meta.insert(&id, TorrentMeta::new(false));
        session
            .add_torrent(
                AddTorrent::from_bytes(bytes.clone()),
                Some(AddTorrentOptions {
                    overwrite: true,
                    output_folder: Some(result.output_folder.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
            )
            .await
            .context("error adding torrent for seeding")?;
        let _ = engine.meta.save();
    }

    Ok(CreateResponse {
        magnet: result.as_magnet().to_string(),
        torrent_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        id,
        name,
        saved_to,
    })
}
