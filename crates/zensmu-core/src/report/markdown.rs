use super::system::SystemBoardInfo;
use super::DebugReport;
use crate::types::{Codename, FreqTier};
use std::fmt::Write as _;

impl DebugReport {
    fn cpu_label(&self) -> String {
        if let Some(ref c) = self.cpuid {
            let trimmed = c.brand.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        if let Some(cn) = self.codename {
            return cn.display_name().to_string();
        }
        if let Some(ref drv) = self.driver {
            return format!("codename id={}", drv.codename_id);
        }
        "unknown CPU".to_string()
    }

    /// Suggest a concise GitHub issue title describing this hardware report.
    pub fn suggested_issue_title(&self) -> String {
        let cpu = self.cpu_label();
        if !self.capabilities_supported {
            if let Some(ref c) = self.cpuid {
                format!(
                    "CPU support request: {cpu} (family 0x{:X}, model 0x{:X})",
                    c.family, c.model
                )
            } else if let Some(ref drv) = self.driver {
                format!(
                    "CPU support request: {cpu} (codename id={})",
                    drv.codename_id
                )
            } else {
                format!("CPU support request: {cpu}")
            }
        } else if self.telemetry_error.is_some() {
            let ver = self
                .driver
                .as_ref()
                .and_then(|d| d.pm_table_version)
                .map(|v| format!("0x{v:08X}"))
                .unwrap_or_else(|| "unknown".to_string());
            let size = self
                .driver
                .as_ref()
                .and_then(|d| d.pm_table_size)
                .map(|s| format!("{s} bytes"))
                .unwrap_or_else(|| "unknown size".to_string());
            format!("PM table support: {cpu} (PM table {ver}, {size})")
        } else {
            let cn = self
                .codename
                .map(Codename::display_name)
                .unwrap_or("unknown");
            format!("Hardware report / issue: {cpu} ({cn})")
        }
    }

    /// Suggest GitHub issue labels for this report.
    pub fn suggested_labels(&self) -> Vec<&'static str> {
        if !self.capabilities_supported || self.telemetry_error.is_some() {
            vec!["hardware-support"]
        } else {
            vec!["bug"]
        }
    }

    /// Render the diagnostic report as clean GitHub-flavored Markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = String::with_capacity(2048);

        // User notes section
        md.push_str("### Description / Notes\n\n");
        if let Some(ref note) = self.user_note {
            md.push_str(note);
            md.push_str("\n\n");
        } else {
            md.push_str("_Describe your issue, request, or observations here._\n\n");
        }

        // System & Hardware Summary table
        md.push_str("### System & Hardware Summary\n\n");
        md.push_str("| Field | Value |\n");
        md.push_str("| --- | --- |\n");

        let _ = writeln!(
            md,
            "| zensmu version | `{}` |",
            escape_md_cell(&self.zensmu_version)
        );

        let os_str = self
            .system
            .os_pretty_name
            .as_deref()
            .unwrap_or("unavailable");
        let _ = writeln!(md, "| OS | {} |", escape_md_cell(os_str));

        let kernel_str = self
            .system
            .kernel_release
            .as_deref()
            .unwrap_or("unavailable");
        let _ = writeln!(md, "| Kernel | `{}` |", escape_md_cell(kernel_str));

        if let Some(ref sb) = self.secure_boot {
            let sb_state = match sb.secure_boot_enabled {
                Some(true) => "enabled",
                Some(false) => "disabled",
                None => "unknown",
            };
            let lockdown = match sb.lockdown_mode.as_deref() {
                Some(mode) => format!("lockdown: `{}`", escape_md_cell(mode)),
                None => "lockdown: unavailable".to_string(),
            };
            let suffix = if sb.blocks_unsigned_modules {
                " (blocks unsigned modules)"
            } else {
                ""
            };
            let _ = writeln!(
                md,
                "| Secure Boot / Lockdown | {sb_state}, {lockdown}{suffix} |"
            );
        }

        let board_str = format_board_bios(&self.system);
        let _ = writeln!(
            md,
            "| Motherboard & BIOS | {} |",
            escape_md_cell(&board_str)
        );

        let brand_str = self
            .cpuid
            .as_ref()
            .map(|c| c.brand.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("unavailable");
        let _ = writeln!(md, "| CPU Brand | {} |", escape_md_cell(brand_str));

        let cpuid_str = match self.cpuid {
            Some(ref c) => format!(
                "`{}` family `0x{:X}` model `0x{:X}` stepping `0x{:X}`",
                escape_md_cell(&c.vendor),
                c.family,
                c.model,
                c.stepping
            ),
            None => "unavailable".to_string(),
        };
        let _ = writeln!(md, "| CPUID | {cpuid_str} |");

        let codename_str = match (self.codename, &self.driver) {
            (Some(cn), Some(drv)) => format!(
                "`{}` (id `{}`, capabilities: {})",
                cn.display_name(),
                drv.codename_id,
                if self.capabilities_supported {
                    "supported"
                } else {
                    "unsupported"
                }
            ),
            (None, Some(drv)) => format!(
                "unrecognized (id `{}`, capabilities: unsupported)",
                drv.codename_id
            ),
            (Some(cn), None) => format!(
                "`{}` (capabilities: {})",
                cn.display_name(),
                if self.capabilities_supported {
                    "supported"
                } else {
                    "unsupported"
                }
            ),
            (None, None) => "unavailable".to_string(),
        };
        let _ = writeln!(md, "| Codename / ID | {codename_str} |");

        let topo_str = match self.topology {
            Some(ref t) => format!(
                "{} CCD(s), {} physical core(s), {} logical CPU(s)",
                t.ccd_count, t.physical_cores, t.logical_cpus
            ),
            None => "unavailable".to_string(),
        };
        let _ = writeln!(md, "| Topology | {topo_str} |");

        let driver_str = match &self.driver {
            Some(drv) => format!(
                "`ryzen_smu {}`, SMU `{}`, MP1 IF `{}`, RSMU: {}",
                escape_md_cell(&drv.drv_version),
                escape_md_cell(&drv.smu_version),
                drv.mp1_if_version,
                if drv.has_rsmu { "yes" } else { "no" }
            ),
            None => match &self.driver_error {
                Some(err) => format!("error: `{}`", escape_md_cell(err)),
                None => "unavailable".to_string(),
            },
        };
        let _ = writeln!(md, "| Driver / SMU | {driver_str} |");

        let pm_str = match &self.driver {
            Some(drv) if drv.has_pm_table => {
                let ver = drv
                    .pm_table_version
                    .map(|v| format!("`0x{v:08X}`"))
                    .unwrap_or_else(|| "unknown".to_string());
                let size = drv
                    .pm_table_size
                    .map(|s| format!("`{s}` bytes (`0x{s:X}`)"))
                    .unwrap_or_else(|| "unknown".to_string());
                format!("version {ver}, size {size}")
            }
            Some(_) => "not exposed by driver".to_string(),
            None => "unavailable".to_string(),
        };
        let _ = writeln!(md, "| PM Table | {pm_str} |");

        if self.driver.is_some() {
            if let Some(ref err) = self.driver_error {
                let _ = writeln!(
                    md,
                    "\n> **Driver / identity note:** `{}`",
                    escape_md_cell(err)
                );
            }
        }
        if self.driver.is_none() {
            if let Some(ref sb) = self.secure_boot {
                if sb.blocks_unsigned_modules {
                    if let Some(ref hint) = sb.hint {
                        let _ = writeln!(md, "\n> **Secure Boot / lockdown note:** {hint}");
                    }
                }
            }
        }

        // Telemetry & SMU Probes
        md.push_str("\n### Telemetry & SMU Probes\n\n");

        if let Some(ref t) = self.telemetry {
            md.push_str("- **Decoded PM Table Telemetry**:\n");
            if let Some(line) = format_limit_value("PPT", t.ppt_value_w, t.ppt_limit_w, "W") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("TDC", t.tdc_value_a, t.tdc_limit_a, "A") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("EDC", t.edc_value_a, t.edc_limit_a, "A") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("Tctl", t.tctl_value_c, t.tctl_limit_c, "°C") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("STAPM", t.stapm_value_w, t.stapm_limit_w, "W") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("Slow PPT", t.slow_value_w, t.slow_limit_w, "W")
            {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) =
                format_limit_value("APU Slow", t.apu_slow_value_w, t.apu_slow_limit_w, "W")
            {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) =
                format_limit_value("TDC SoC", t.tdc_soc_value_a, t.tdc_soc_limit_a, "A")
            {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) =
                format_limit_value("EDC SoC", t.edc_soc_value_a, t.edc_soc_limit_a, "A")
            {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value(
                "APU Skin Temp",
                t.apu_skin_temp_value_c,
                t.apu_skin_temp_limit_c,
                "°C",
            ) {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value(
                "dGPU Skin Temp",
                t.dgpu_skin_temp_value_c,
                t.dgpu_skin_temp_limit_c,
                "°C",
            ) {
                let _ = writeln!(md, "  - {line}");
            }
            let clocks = format_optional_metrics(&[
                ("FCLK", t.fclk_mhz, "MHz", 0),
                ("UCLK", t.uclk_mhz, "MHz", 0),
                ("MCLK", t.mclk_mhz, "MHz", 0),
            ]);
            if !clocks.is_empty() {
                let _ = writeln!(md, "  - Clocks: {clocks}");
            }
            let rails = format_optional_metrics(&[
                ("VDDCR_SOC", t.vddcr_soc_v, "V", 3),
                ("VDDCR_MISC", t.vddcr_misc_v, "V", 3),
                ("CLDO_VDDP", t.cldo_vddp_v, "V", 3),
                ("CLDO_VDDG_IOD", t.cldo_vddg_iod_v, "V", 3),
                ("CLDO_VDDG_CCD", t.cldo_vddg_ccd_v, "V", 3),
            ]);
            if !rails.is_empty() {
                let _ = writeln!(md, "  - Voltages: {rails}");
            }
        } else if let Some(ref err) = self.telemetry_error {
            let _ = writeln!(md, "- **Telemetry**: error — `{err}`");
        } else {
            md.push_str("- **Telemetry**: unavailable\n");
        }

        let any_mailbox = self.rsmu_ping.is_some()
            || self.mp1_ping.is_some()
            || self.pbo_status.is_some()
            || self.pbo_error.is_some()
            || self.co_snapshot.is_some()
            || self.co_error.is_some()
            || self.shaper_state.is_some()
            || self.shaper_error.is_some();

        if let Some(ref ping) = self.rsmu_ping {
            let _ = writeln!(md, "- **RSMU ping (`0x01`)**: `{ping}`");
        }
        if let Some(ref ping) = self.mp1_ping {
            let _ = writeln!(md, "- **MP1 ping (`0x01`)**: `{ping}`");
        }

        if let Some(ref pbo) = self.pbo_status {
            let mut parts = Vec::new();
            if let Some(s) = pbo.scalar {
                parts.push(format!("scalar={s:.2}x"));
            }
            if let Some(fmax) = pbo.fmax_mhz {
                parts.push(format!("fmax={fmax} MHz"));
            }
            if let Some(v) = pbo.fused.ppt_w {
                parts.push(format!("fused_ppt={v}W"));
            }
            if let Some(v) = pbo.fused.vdd_tdc_a {
                parts.push(format!("fused_tdc={v}A"));
            }
            if let Some(v) = pbo.fused.edc_a {
                parts.push(format!("fused_edc={v}A"));
            }
            if let Some(v) = pbo.fused.tdp_w {
                parts.push(format!("tdp={v}W"));
            }
            if let Some(v) = pbo.fused.tjmax_c {
                parts.push(format!("tjmax={v}°C"));
            }
            if let Some(v) = pbo.flags.overclockable {
                parts.push(format!("oc={v}"));
            }
            if let Some(v) = pbo.flags.expo_active {
                parts.push(format!("expo={v}"));
            }
            if let Some(v) = pbo.flags.ln2_mode {
                parts.push(format!("ln2={v}"));
            }
            if !pbo.core_perf_scores.is_empty() {
                parts.push(format!("cppc_cores={}", pbo.core_perf_scores.len()));
            }
            let summary = if parts.is_empty() {
                "no fields reported".to_string()
            } else {
                parts.join(", ")
            };
            let _ = writeln!(md, "- **PBO Status**: {summary}");
        } else if let Some(ref err) = self.pbo_error {
            let _ = writeln!(md, "- **PBO Status**: error — `{err}`");
        }

        if let Some(ref co) = self.co_snapshot {
            let cores: Vec<String> = co
                .cores
                .iter()
                .map(|c| format!("c{}:{}", c.core_index, c.margin))
                .collect();
            let mut summary = cores.join(", ");
            if let Some(igpu) = co.igpu {
                if !summary.is_empty() {
                    summary.push_str("; ");
                }
                let _ = write!(summary, "igpu:{igpu}");
            }
            let _ = writeln!(md, "- **Curve Optimizer**: `{summary}`");
        } else if let Some(ref err) = self.co_error {
            let _ = writeln!(md, "- **Curve Optimizer**: error — `{err}`");
        }

        if let Some(ref cs) = self.shaper_state {
            let tiers: Vec<String> = FreqTier::ALL
                .iter()
                .map(|&t| {
                    let m = cs.tier(t);
                    format!("{}={}/{}/{}", t.label(), m.low, m.med, m.high)
                })
                .collect();
            let _ = writeln!(
                md,
                "- **Curve Shaper** (low/med/high temp): `{}`",
                tiers.join(", ")
            );
        } else if let Some(ref err) = self.shaper_error {
            let _ = writeln!(md, "- **Curve Shaper**: error — `{err}`");
        }

        if !any_mailbox {
            md.push_str(
                "- **SMU Mailbox Probes**: not run (enable privileged mailbox probes if needed)\n",
            );
        }

        // hwmon reference sensors
        if !self.hwmon_reference.is_empty() {
            md.push_str(
                "\n<details>\n<summary>hwmon reference sensors (k10temp/zenpower)</summary>\n\n",
            );
            md.push_str("| Chip | Sensor | Value |\n");
            md.push_str("| --- | --- | --- |\n");
            for r in &self.hwmon_reference {
                let _ = writeln!(
                    md,
                    "| `{}` | {} | {} |",
                    escape_md_cell(&r.chip),
                    escape_md_cell(&r.label),
                    escape_md_cell(&r.value)
                );
            }
            md.push_str("\n</details>\n");
        }

        // Raw PM table non-zero floats
        if !self.raw_pm_table_floats.is_empty() {
            let _ = write!(
                md,
                "\n<details>\n<summary>Raw PM table non-zero floats ({} entries)</summary>\n\n```text\n",
                self.raw_pm_table_floats.len()
            );
            for entry in &self.raw_pm_table_floats {
                let _ = writeln!(md, "{} = {:.3}", entry.offset_hex, entry.value);
            }
            md.push_str("```\n\n</details>\n");
        }

        // Topology details
        if let Some(ref topo) = self.topology {
            md.push_str("\n<details>\n<summary>Topology</summary>\n\n```text\n");
            let _ = writeln!(
                md,
                "ccds={} physical_cores={} logical_cpus={}",
                topo.ccd_count, topo.physical_cores, topo.logical_cpus
            );
            for core in &topo.cores {
                let _ = writeln!(
                    md,
                    "core {}: ccd={} ccx={} local={} cpus={:?}",
                    core.index, core.ccd, core.ccx, core.core_in_ccd, core.linux_cpus
                );
            }
            md.push_str("```\n\n</details>\n");
        }

        // Privacy footer
        let _ = write!(
            md,
            "\n---\n_Generated by zensmu v{} (strictly read-only). No hostnames, usernames, IP/MAC addresses, or hardware serial numbers were collected._\n",
            self.zensmu_version
        );

        md
    }
}

fn format_board_bios(sys: &SystemBoardInfo) -> String {
    let mut board = String::new();
    if let Some(ref vendor) = sys.board_vendor {
        board.push_str(vendor);
    }
    if let Some(ref name) = sys.board_name {
        if !board.is_empty() {
            board.push(' ');
        }
        board.push_str(name);
    }

    let mut bios_parts = Vec::new();
    if let Some(ref ver) = sys.bios_version {
        bios_parts.push(format!("BIOS {ver}"));
    }
    if let Some(ref date) = sys.bios_date {
        bios_parts.push(date.clone());
    }

    match (board.is_empty(), bios_parts.is_empty()) {
        (false, false) => format!("{board} ({})", bios_parts.join(", ")),
        (false, true) => board,
        (true, false) => bios_parts.join(", "),
        (true, true) => "unavailable".to_string(),
    }
}

fn format_limit_value(
    name: &str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &str,
) -> Option<String> {
    match (value, limit) {
        (Some(v), Some(l)) => Some(format!("{name}: {v:.1} / {l:.1} {unit}")),
        (Some(v), None) => Some(format!("{name}: {v:.1} {unit}")),
        (None, Some(l)) => Some(format!("{name} limit: {l:.1} {unit}")),
        (None, None) => None,
    }
}

fn format_optional_metrics(items: &[(&str, Option<f32>, &str, usize)]) -> String {
    items
        .iter()
        .filter_map(|&(label, val, unit, prec)| val.map(|v| format!("{label}={v:.prec$} {unit}")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn escape_md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\r', '\n'], " ")
}
