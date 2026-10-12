use serde::{Deserialize, Serialize};

/// Transfer measurements accumulated while the torrent is active. Old lifetime
/// totals have no timing data, so they are not mixed into the measured averages.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferStats {
    pub active_ms: u64,
    pub download_ms: u64,
    pub measured_downloaded: u64,
    pub measured_uploaded: u64,
    pub peak_down_bps: u64,
    pub peak_up_bps: u64,
}

impl TransferStats {
    pub fn record(
        &mut self,
        elapsed_ms: u64,
        downloading: bool,
        down: u64,
        up: u64,
        speeds: (u64, u64),
    ) {
        self.active_ms += elapsed_ms;
        if downloading {
            self.download_ms += elapsed_ms;
        }
        self.measured_downloaded += down;
        self.measured_uploaded += up;
        self.peak_down_bps = self.peak_down_bps.max(speeds.0);
        self.peak_up_bps = self.peak_up_bps.max(speeds.1);
    }

    pub fn snapshot(&self) -> TransferSnapshot {
        let average = |bytes: u64, ms: u64| {
            if ms == 0 {
                0
            } else {
                (u128::from(bytes) * 1000 / u128::from(ms)) as u64
            }
        };
        TransferSnapshot {
            active_ms: self.active_ms,
            download_ms: self.download_ms,
            avg_down_bps: average(self.measured_downloaded, self.download_ms),
            avg_up_bps: average(self.measured_uploaded, self.active_ms),
            peak_down_bps: self.peak_down_bps,
            peak_up_bps: self.peak_up_bps,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TransferSnapshot {
    pub active_ms: u64,
    pub download_ms: u64,
    pub avg_down_bps: u64,
    pub avg_up_bps: u64,
    pub peak_down_bps: u64,
    pub peak_up_bps: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_average_survives_restart_and_seeding_does_not_dilute_downloads() {
        let mut stats = TransferStats::default();
        stats.record(1000, true, 1000, 100, (1000, 100));
        stats.record(3000, true, 9000, 900, (3000, 300));
        let saved = serde_json::to_vec(&stats).unwrap();
        let mut restored: TransferStats = serde_json::from_slice(&saved).unwrap();
        restored.record(6000, false, 0, 2000, (0, 400));
        let s = restored.snapshot();
        assert_eq!(s.avg_down_bps, 2500);
        assert_eq!(s.avg_up_bps, 300);
        assert_eq!(s.peak_down_bps, 3000);
        assert_eq!(s.peak_up_bps, 400);
        assert_eq!(s.active_ms, 10000);
        assert_eq!(s.download_ms, 4000);
    }
}
