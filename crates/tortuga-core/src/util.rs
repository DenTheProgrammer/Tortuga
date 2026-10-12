use std::path::Path;

use serde::Serialize;

/// Write JSON to `path` via a temporary file so a crash never leaves it half-written.
pub fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn random_token() -> String {
    let bytes: [u8; 24] = rand::random();
    hex::encode(bytes)
}
