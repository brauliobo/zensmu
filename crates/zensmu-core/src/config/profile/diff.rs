use super::{Profile, ProfileDiffEntry};
use crate::safety::snapshot::SmuStateSnapshot;
use crate::types::ApuPowerMode;
use std::collections::BTreeMap;

impl Profile {
    /// Compute human-readable differences between this desired profile and a current snapshot.
    pub fn diff_against(&self, current: &SmuStateSnapshot) -> Vec<ProfileDiffEntry> {
        let mut diffs = Vec::new();

        if let Some(co) = &self.curve_optimizer {
            let current_cores: BTreeMap<u32, i32> = current
                .co_per_core
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|&(idx, val)| (idx, i32::from(val)))
                .collect();

            if let Some(all_val) = co.all {
                let core_count = current.physical_cores as u32;
                let mut core_indices: Vec<u32> = (0..core_count).collect();
                for &idx in current_cores.keys().chain(co.per_core.keys()) {
                    if !core_indices.contains(&idx) {
                        core_indices.push(idx);
                    }
                }
                core_indices.sort_unstable();

                if core_indices.is_empty() {
                    diffs.push(ProfileDiffEntry {
                        field: "curve_optimizer.all".into(),
                        current: "unknown".into(),
                        desired: format_signed(all_val),
                    });
                } else {
                    for core_idx in core_indices {
                        let desired = co.per_core.get(&core_idx).copied().unwrap_or(all_val);
                        push_diff_signed(
                            &mut diffs,
                            format!("curve_optimizer.core.{core_idx}"),
                            current_cores.get(&core_idx).copied(),
                            desired,
                        );
                    }
                }
            } else {
                for (&core_idx, &desired) in &co.per_core {
                    push_diff_signed(
                        &mut diffs,
                        format!("curve_optimizer.core.{core_idx}"),
                        current_cores.get(&core_idx).copied(),
                        desired,
                    );
                }
            }

            if let Some(desired_igpu) = co.igpu {
                push_diff_signed(
                    &mut diffs,
                    "curve_optimizer.igpu".into(),
                    current.igpu_co.map(i32::from),
                    desired_igpu,
                );
            }
        }

        if let Some(shaper) = &self.curve_shaper {
            for (&tier, desired_tier) in shaper {
                let cur_tier = current.curve_shaper.map(|cs| cs.tier(tier));
                for (band, cur_val, des_val) in [
                    ("low", cur_tier.map(|m| i32::from(m.low)), desired_tier.low),
                    ("med", cur_tier.map(|m| i32::from(m.med)), desired_tier.med),
                    (
                        "high",
                        cur_tier.map(|m| i32::from(m.high)),
                        desired_tier.high,
                    ),
                ] {
                    if let Some(d) = des_val {
                        push_diff_signed(
                            &mut diffs,
                            format!("curve_shaper.{}.{band}", tier.label()),
                            cur_val,
                            i32::from(d),
                        );
                    }
                }
            }
        }

        if let Some(pbo) = &self.pbo {
            push_diff_u32(&mut diffs, "pbo.ppt_w", current.ppt_w, pbo.ppt_w, "W");
            push_diff_u32(&mut diffs, "pbo.tdc_a", current.tdc_a, pbo.tdc_a, "A");
            push_diff_u32(&mut diffs, "pbo.edc_a", current.edc_a, pbo.edc_a, "A");
            push_diff_u32(&mut diffs, "pbo.tctl_c", current.tctl_c, pbo.tctl_c, "°C");
            let cur_scalar = current
                .pbo_scalar
                .filter(|s| s.is_finite() && *s >= 0.0)
                .map(|s| s.round() as u32);
            push_diff_u32(&mut diffs, "pbo.scalar", cur_scalar, pbo.scalar, "x");
            push_diff_u32(
                &mut diffs,
                "pbo.fmax_mhz",
                current.fmax_mhz,
                pbo.fmax_mhz,
                "MHz",
            );
        }

        if let Some(apu) = &self.apu {
            push_diff_u32(&mut diffs, "apu.stapm_w", current.stapm_w, apu.stapm_w, "W");
            push_diff_u32(&mut diffs, "apu.stapm_time_s", None, apu.stapm_time_s, "s");
            push_diff_u32(&mut diffs, "apu.fast_w", current.ppt_w, apu.fast_w, "W");
            push_diff_u32(&mut diffs, "apu.slow_w", current.slow_w, apu.slow_w, "W");
            push_diff_u32(&mut diffs, "apu.slow_time_s", None, apu.slow_time_s, "s");
            push_diff_u32(
                &mut diffs,
                "apu.apu_slow_w",
                current.apu_slow_w,
                apu.apu_slow_w,
                "W",
            );
            push_diff_u32(
                &mut diffs,
                "apu.tdc_soc_a",
                current.tdc_soc_a,
                apu.tdc_soc_a,
                "A",
            );
            push_diff_u32(
                &mut diffs,
                "apu.edc_soc_a",
                current.edc_soc_a,
                apu.edc_soc_a,
                "A",
            );
            push_diff_u32(
                &mut diffs,
                "apu.apu_skin_temp_c",
                current.apu_skin_temp_c,
                apu.apu_skin_temp_c,
                "°C",
            );
            push_diff_u32(
                &mut diffs,
                "apu.dgpu_skin_temp_c",
                current.dgpu_skin_temp_c,
                apu.dgpu_skin_temp_c,
                "°C",
            );
            if let Some(mode) = apu.power_mode {
                let label = match mode {
                    ApuPowerMode::MaxPerformance => "max-performance",
                    ApuPowerMode::PowerSaving => "power-saving",
                };
                diffs.push(ProfileDiffEntry {
                    field: "apu.power_mode".into(),
                    current: "unknown".into(),
                    desired: label.into(),
                });
            }
        }

        diffs
    }
}

fn push_diff_signed(
    diffs: &mut Vec<ProfileDiffEntry>,
    field: String,
    current: Option<i32>,
    desired: i32,
) {
    if current != Some(desired) {
        diffs.push(ProfileDiffEntry {
            field,
            current: current
                .map(format_signed)
                .unwrap_or_else(|| "unknown".into()),
            desired: format_signed(desired),
        });
    }
}

fn push_diff_u32(
    diffs: &mut Vec<ProfileDiffEntry>,
    field: &str,
    current: Option<u32>,
    desired: Option<u32>,
    unit: &str,
) {
    if let Some(d) = desired {
        if current != Some(d) {
            diffs.push(ProfileDiffEntry {
                field: field.into(),
                current: fmt_opt_unit(current, unit),
                desired: format!("{d}{unit}"),
            });
        }
    }
}

fn format_signed(v: i32) -> String {
    if v > 0 {
        format!("+{v}")
    } else {
        v.to_string()
    }
}

fn fmt_opt_unit(val: Option<u32>, unit: &str) -> String {
    match val {
        Some(v) => format!("{v}{unit}"),
        None => "unknown".into(),
    }
}
