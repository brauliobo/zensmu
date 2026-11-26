use crate::error::{Error, Result};
use crate::types::CoreInfo;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Detected physical CPU topology (CCDs, CCXs, physical cores, Linux SMT siblings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuTopology {
    pub ccd_count: u32,
    pub physical_cores: usize,
    pub logical_cpus: usize,
    pub cores: Vec<CoreInfo>,
}

impl CpuTopology {
    /// Detect topology from `/sys/devices/system/cpu/cpu*/topology/`.
    pub fn detect_linux() -> Result<Self> {
        Self::detect_from_path(Path::new("/sys/devices/system/cpu"))
    }

    pub fn detect_from_path(cpu_root: &Path) -> Result<Self> {
        let entries = fs::read_dir(cpu_root).map_err(|e| Error::Io {
            path: cpu_root.display().to_string(),
            source: e,
        })?;

        // Map (ccd_group, physical_core_id) -> Vec<linux_cpu_id>
        // We group CCDs by L3 cache `shared_cpu_list` (each Zen CCD has its own L3),
        // falling back to `die_id`.
        let mut l3_groups: Vec<String> = Vec::new();
        let mut raw: Vec<(u32, u32, u32)> = Vec::new(); // (ccd_idx, core_id, cpu_num)

        let mut cpu_dirs: Vec<(u32, std::path::PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if let Some(rest) = s.strip_prefix("cpu") {
                if let Ok(cpu_num) = rest.parse::<u32>() {
                    cpu_dirs.push((cpu_num, entry.path()));
                }
            }
        }
        cpu_dirs.sort_by_key(|(n, _)| *n);

        for (cpu_num, path) in &cpu_dirs {
            let topo = path.join("topology");
            let core_id_path = topo.join("core_id");
            if !core_id_path.exists() {
                continue;
            }
            let core_id = read_u32(&core_id_path).unwrap_or(*cpu_num);

            // Determine CCD index via L3 cache shared_cpu_list when present (Zen 3/4/5 = 1 L3 per CCD)
            let l3_list = fs::read_to_string(path.join("cache/index3/shared_cpu_list"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            let ccd_idx = if let Some(l3) = l3_list {
                if let Some(pos) = l3_groups.iter().position(|g| g == &l3) {
                    pos as u32
                } else {
                    l3_groups.push(l3);
                    (l3_groups.len() - 1) as u32
                }
            } else {
                read_u32(&topo.join("die_id")).unwrap_or(0)
            };

            raw.push((ccd_idx, core_id, *cpu_num));
        }

        if raw.is_empty() {
            return Err(Error::InvalidInput(format!(
                "no CPU topology entries found under {}",
                cpu_root.display()
            )));
        }

        let logical_cpus = raw.len();
        let mut grouped: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
        for (ccd, core_id, cpu_num) in raw {
            grouped.entry((ccd, core_id)).or_default().push(cpu_num);
        }

        let mut ccd_max = 0u32;
        let mut cores = Vec::with_capacity(grouped.len());
        let mut per_ccd_counter: BTreeMap<u32, u32> = BTreeMap::new();

        for (idx, ((ccd, _core_id), mut cpus)) in grouped.into_iter().enumerate() {
            cpus.sort_unstable();
            ccd_max = ccd_max.max(ccd);
            let local_core = *per_ccd_counter.entry(ccd).or_insert(0);
            *per_ccd_counter.get_mut(&ccd).unwrap() += 1;

            cores.push(CoreInfo {
                index: idx as u32,
                ccd,
                ccx: 0,
                core_in_ccd: local_core,
                linux_cpus: cpus,
            });
        }

        Ok(Self {
            ccd_count: ccd_max + 1,
            physical_cores: cores.len(),
            logical_cpus,
            cores,
        })
    }

    /// Construct a deterministic synthetic topology (for tests & fallbacks).
    pub fn synthetic(ccds: u32, cores_per_ccd: u32, threads_per_core: u32) -> Self {
        let mut cores = Vec::new();
        let mut idx = 0u32;
        let total_phys = (ccds * cores_per_ccd) as usize;
        for ccd in 0..ccds {
            for local in 0..cores_per_ccd {
                let mut linux_cpus = vec![idx];
                if threads_per_core > 1 {
                    linux_cpus.push(idx + total_phys as u32);
                }
                cores.push(CoreInfo {
                    index: idx,
                    ccd,
                    ccx: 0,
                    core_in_ccd: local,
                    linux_cpus,
                });
                idx += 1;
            }
        }
        Self {
            ccd_count: ccds,
            physical_cores: cores.len(),
            logical_cpus: cores.len() * threads_per_core as usize,
            cores,
        }
    }

    pub fn fallback(physical_cores: u32) -> Self {
        Self::synthetic(1, physical_cores.max(1), 1)
    }

    pub fn core(&self, index: u32) -> Option<&CoreInfo> {
        self.cores.iter().find(|c| c.index == index)
    }
}

fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_dual_ccd_masks_are_correct() {
        // 12-core 9900X: 2 CCDs x 6 cores
        let topo = CpuTopology::synthetic(2, 6, 2);
        assert_eq!(topo.ccd_count, 2);
        assert_eq!(topo.physical_cores, 12);
        assert_eq!(topo.logical_cpus, 24);
        assert_eq!(topo.core(0).unwrap().smu_mask(false, false), 0x0000_0000);
        assert_eq!(topo.core(5).unwrap().smu_mask(false, false), 0x0050_0000);
        // Core 6 is CCD 1, core 0 -> (1 << 28) | (0 << 20)
        assert_eq!(topo.core(6).unwrap().smu_mask(false, false), 0x1000_0000);
        assert_eq!(topo.core(11).unwrap().smu_mask(false, false), 0x1050_0000);
    }

    #[test]
    fn synthetic_single_ccd_9700x_masks() {
        let topo = CpuTopology::synthetic(1, 8, 2);
        for i in 0..8u32 {
            assert_eq!(topo.core(i).unwrap().smu_mask(false, false), i << 20);
        }
    }
}
