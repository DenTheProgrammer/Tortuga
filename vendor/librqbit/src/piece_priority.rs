//! Tortuga patch: configurable file priorities and piece picking order.
//!
//! Upstream librqbit always downloads files one after another (sorted by name),
//! pieces in order. This module lets the caller assign a priority tier to each
//! file and choose between sequential and randomized piece order.

use crate::type_aliases::FileInfos;

/// Default tier for files that have no explicit priority.
pub const DEFAULT_FILE_TIER: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiecePriorityConfig {
    /// Priority tier per file id; files with a higher tier are downloaded first.
    /// Missing entries use [`DEFAULT_FILE_TIER`]. Skipping a file is done
    /// via `only_files`, not via tiers.
    pub file_tiers: Vec<u8>,
    /// Download pieces in order within each tier (good for previewing media).
    /// When false, pieces within each tier are picked in a randomized order,
    /// which is friendlier to the swarm.
    pub sequential: bool,
}

impl Default for PiecePriorityConfig {
    fn default() -> Self {
        Self {
            file_tiers: Vec::new(),
            sequential: true,
        }
    }
}

impl PiecePriorityConfig {
    fn tier(&self, file_id: usize) -> u8 {
        self.file_tiers
            .get(file_id)
            .copied()
            .unwrap_or(DEFAULT_FILE_TIER)
    }
}

/// Precomputed download order used by the piece tracker.
#[derive(Debug, Default)]
pub(crate) struct FilePriorities {
    /// File ids in download order.
    pub order: Vec<usize>,
    /// When set, overrides per-file iteration with an explicit piece order.
    pub piece_order: Option<Vec<u32>>,
}

impl FilePriorities {
    /// Upstream behaviour: files sorted by name, pieces sequential.
    pub fn upstream_default(file_infos: &FileInfos) -> Self {
        let mut order = (0..file_infos.len()).collect::<Vec<usize>>();
        order.sort_by(|a, b| {
            file_infos[*a]
                .relative_filename
                .cmp(&file_infos[*b].relative_filename)
        });
        Self {
            order,
            piece_order: None,
        }
    }

    pub fn compute(
        file_infos: &FileInfos,
        config: Option<&PiecePriorityConfig>,
        seed: u64,
    ) -> Self {
        let Some(config) = config else {
            return Self::upstream_default(file_infos);
        };

        let mut order = (0..file_infos.len()).collect::<Vec<usize>>();
        // Stable sort keeps the torrent's natural file order inside a tier.
        order.sort_by_key(|id| std::cmp::Reverse(config.tier(*id)));

        if config.sequential {
            return Self {
                order,
                piece_order: None,
            };
        }

        // A piece spanning several files gets the highest tier among them.
        let num_pieces = file_infos
            .iter()
            .map(|f| f.piece_range.end)
            .max()
            .unwrap_or(0) as usize;
        let mut piece_tier = vec![0u8; num_pieces];
        for (id, fi) in file_infos.iter().enumerate() {
            let t = config.tier(id);
            for p in fi.piece_range_usize() {
                piece_tier[p] = piece_tier[p].max(t);
            }
        }
        let mut pieces = (0..num_pieces as u32).collect::<Vec<u32>>();
        pieces.sort_by_key(|p| (std::cmp::Reverse(piece_tier[*p as usize]), mix(*p as u64 ^ seed)));
        Self {
            order,
            piece_order: Some(pieces),
        }
    }
}

// splitmix64 finalizer: cheap deterministic shuffle key.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_info::FileInfo;

    fn files(ranges: &[(u32, u32)]) -> FileInfos {
        ranges
            .iter()
            .enumerate()
            .map(|(i, (s, e))| FileInfo {
                relative_filename: format!("f{i}").into(),
                offset_in_torrent: 0,
                piece_range: *s..*e,
                attrs: Default::default(),
                len: 1,
            })
            .collect()
    }

    #[test]
    fn sequential_orders_files_by_tier() {
        let fi = files(&[(0, 2), (2, 4), (4, 6)]);
        let cfg = PiecePriorityConfig {
            file_tiers: vec![1, 3, 2],
            sequential: true,
        };
        let p = FilePriorities::compute(&fi, Some(&cfg), 0);
        assert_eq!(p.order, vec![1, 2, 0]);
        assert!(p.piece_order.is_none());
    }

    #[test]
    fn random_respects_tiers() {
        let fi = files(&[(0, 50), (50, 100)]);
        let cfg = PiecePriorityConfig {
            file_tiers: vec![1, 3],
            sequential: false,
        };
        let p = FilePriorities::compute(&fi, Some(&cfg), 42);
        let po = p.piece_order.unwrap();
        assert_eq!(po.len(), 100);
        assert!(po[..50].iter().all(|p| *p >= 50));
        assert!(po[50..].iter().all(|p| *p < 50));
        // Not sequential.
        assert_ne!(po[..50], (50..100).collect::<Vec<u32>>()[..]);
    }
}
