use super::{CoreMetricsSample, ExtendedTelemetry};
use crate::hw::identity::HardwareContext;
use crate::ops::pm_table::TelemetrySnapshot;
use std::fmt::Write as _;

/// Escape a Prometheus / OpenMetrics 0.0.4 label value (`\` -> `\\`, `"` -> `\"`,
/// `\n` -> `\n`; `\r` is stripped to keep every metric record on a single line).
pub fn escape_prom_label(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

fn finite_val(val: Option<f32>) -> Option<f32> {
    val.filter(|v| v.is_finite())
        .map(|v| if v == 0.0 { 0.0 } else { v })
}

fn push_scalar_gauge(out: &mut String, name: &str, help: &str, val: Option<f32>) {
    let Some(v) = finite_val(val) else {
        return;
    };
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {v}");
}

fn push_labeled_gauge(
    out: &mut String,
    name: &str,
    help: &str,
    label_key: &str,
    samples: &[(&str, Option<f32>)],
) {
    let mut header_written = false;
    for &(label_val, raw) in samples {
        let Some(v) = finite_val(raw) else {
            continue;
        };
        if !header_written {
            let _ = writeln!(out, "# HELP {name} {help}");
            let _ = writeln!(out, "# TYPE {name} gauge");
            header_written = true;
        }
        let escaped = escape_prom_label(label_val);
        let _ = writeln!(out, "{name}{{{label_key}=\"{escaped}\"}} {v}");
    }
}

fn push_per_core_gauge<F>(
    out: &mut String,
    name: &str,
    help: &str,
    cores: &[CoreMetricsSample],
    extract: F,
) where
    F: Fn(&CoreMetricsSample) -> Option<f32>,
{
    let mut header_written = false;
    for core in cores {
        let Some(v) = finite_val(extract(core)) else {
            continue;
        };
        if !header_written {
            let _ = writeln!(out, "# HELP {name} {help}");
            let _ = writeln!(out, "# TYPE {name} gauge");
            header_written = true;
        }
        let _ = writeln!(out, "{name}{{core=\"{}\"}} {v}", core.core);
    }
}

/// Render a [`TelemetrySnapshot`] in Prometheus / OpenMetrics `0.0.4` text exposition format.
///
/// Non-finite (`NaN`, `±inf`) and missing (`None`) values are omitted cleanly.
pub fn render_prometheus_metrics(
    hw: &HardwareContext,
    snap: &TelemetrySnapshot,
    include_per_core: bool,
) -> String {
    render_prometheus_metrics_with_extended(hw, snap, None, include_per_core)
}

/// Render a [`TelemetrySnapshot`] along with optional [`ExtendedTelemetry`] (domain power rails,
/// Vcore, iGPU telemetry, and per-core readings) in Prometheus / OpenMetrics `0.0.4` text format.
///
/// Non-finite (`NaN`, `±inf`) and missing (`None`) values are omitted cleanly, and each metric
/// family emits at most one `# HELP` / `# TYPE` header block.
pub fn render_prometheus_metrics_with_extended(
    hw: &HardwareContext,
    snap: &TelemetrySnapshot,
    extended: Option<&ExtendedTelemetry>,
    include_per_core: bool,
) -> String {
    let mut out = String::with_capacity(2048);

    let codename = escape_prom_label(hw.codename.slug());
    let drv_version = escape_prom_label(hw.driver.drv_version.trim());
    let smu_version = escape_prom_label(hw.driver.smu_version.trim());
    let cpu_name_raw = hw
        .cpuid
        .as_ref()
        .map(|c| c.brand.trim())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| hw.codename.display_name());
    let cpu_name = escape_prom_label(cpu_name_raw);

    let _ = writeln!(
        out,
        "# HELP zensmu_info Static hardware and ryzen_smu driver identification metadata."
    );
    let _ = writeln!(out, "# TYPE zensmu_info gauge");
    let _ = writeln!(
        out,
        "zensmu_info{{codename=\"{codename}\",drv_version=\"{drv_version}\",smu_version=\"{smu_version}\",cpu_name=\"{cpu_name}\"}} 1"
    );

    // Core PBO / socket power and current limits
    push_scalar_gauge(
        &mut out,
        "zensmu_ppt_watts",
        "Current Package Power Tracking (PPT) draw in watts.",
        snap.ppt_value_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_ppt_limit_watts",
        "Active Package Power Tracking (PPT) limit in watts.",
        snap.ppt_limit_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_tdc_amperes",
        "Current VDD Thermal Design Current (TDC) in amperes.",
        snap.tdc_value_a,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_tdc_limit_amperes",
        "Active VDD Thermal Design Current (TDC) limit in amperes.",
        snap.tdc_limit_a,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_edc_amperes",
        "Current VDD Electrical Design Current (EDC) in amperes.",
        snap.edc_value_a,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_edc_limit_amperes",
        "Active VDD Electrical Design Current (EDC) limit in amperes.",
        snap.edc_limit_a,
    );

    // Extended domain power metrics (when available)
    let ext_package_power = extended.and_then(|e| e.package_power_w);
    let ext_core_power = extended.and_then(|e| e.core_power_w);
    let ext_soc_power = extended.and_then(|e| e.soc_power_w);
    let ext_vcore = extended.and_then(|e| e.vcore_v);
    let ext_igpu_clock = extended.and_then(|e| e.igpu_clock_mhz);
    let ext_igpu_temp = extended.and_then(|e| e.igpu_temp_c);
    let ext_igpu_power = extended.and_then(|e| e.igpu_power_w);

    push_scalar_gauge(
        &mut out,
        "zensmu_package_power_watts",
        "Current total CPU package power in watts.",
        ext_package_power,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_soc_power_watts",
        "Current SoC domain power in watts.",
        ext_soc_power,
    );

    // Temperatures (all sensors grouped under one metric family)
    push_labeled_gauge(
        &mut out,
        "zensmu_temperature_celsius",
        "Current temperature sensor reading in degrees Celsius.",
        "sensor",
        &[
            ("tctl", snap.tctl_value_c),
            ("apu_skin", snap.apu_skin_temp_value_c),
            ("dgpu_skin", snap.dgpu_skin_temp_value_c),
            ("igpu", ext_igpu_temp),
        ],
    );
    push_labeled_gauge(
        &mut out,
        "zensmu_temperature_limit_celsius",
        "Active thermal limit in degrees Celsius.",
        "sensor",
        &[
            ("tctl", snap.tctl_limit_c),
            ("apu_skin", snap.apu_skin_temp_limit_c),
            ("dgpu_skin", snap.dgpu_skin_temp_limit_c),
        ],
    );

    // Voltages (all rails grouped under one metric family)
    push_labeled_gauge(
        &mut out,
        "zensmu_voltage_volts",
        "Current voltage rail reading in volts.",
        "rail",
        &[
            ("vcore", ext_vcore),
            ("vsoc", snap.vddcr_soc_v),
            ("vddcr_misc", snap.vddcr_misc_v),
            ("cldo_vddp", snap.cldo_vddp_v),
            ("cldo_vddg_iod", snap.cldo_vddg_iod_v),
            ("cldo_vddg_ccd", snap.cldo_vddg_ccd_v),
        ],
    );

    // Clocks (all domains grouped under one metric family)
    push_labeled_gauge(
        &mut out,
        "zensmu_clock_mhz",
        "Current clock domain frequency in MHz.",
        "domain",
        &[
            ("fclk", snap.fclk_mhz),
            ("uclk", snap.uclk_mhz),
            ("mclk", snap.mclk_mhz),
            ("igpu", ext_igpu_clock),
        ],
    );

    // APU power, thermal-envelope, and SoC current telemetry
    let is_apu = hw.codename.is_apu() || snap.codename.is_apu();
    let fast_limit_w = extended
        .and_then(|e| e.fast_limit_w)
        .or_else(|| is_apu.then_some(snap.ppt_limit_w).flatten());

    push_scalar_gauge(
        &mut out,
        "zensmu_stapm_watts",
        "Current Skin Temperature Aware Power Management (STAPM) power in watts.",
        snap.stapm_value_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_stapm_limit_watts",
        "Active Skin Temperature Aware Power Management (STAPM) limit in watts.",
        snap.stapm_limit_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_fast_limit_watts",
        "Active APU Fast PPT limit in watts.",
        fast_limit_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_slow_watts",
        "Current APU Slow PPT power in watts.",
        snap.slow_value_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_slow_limit_watts",
        "Active APU Slow PPT limit in watts.",
        snap.slow_limit_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_apu_slow_watts",
        "Current APU-only Slow PPT power in watts.",
        snap.apu_slow_value_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_apu_slow_limit_watts",
        "Active APU-only Slow PPT limit in watts.",
        snap.apu_slow_limit_w,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_igpu_power_watts",
        "Current integrated GPU power in watts.",
        ext_igpu_power,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_soc_tdc_amperes",
        "Current SoC Thermal Design Current (TDC) in amperes.",
        snap.tdc_soc_value_a,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_soc_tdc_limit_amperes",
        "Active SoC Thermal Design Current (TDC) limit in amperes.",
        snap.tdc_soc_limit_a,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_soc_edc_amperes",
        "Current SoC Electrical Design Current (EDC) in amperes.",
        snap.edc_soc_value_a,
    );
    push_scalar_gauge(
        &mut out,
        "zensmu_soc_edc_limit_amperes",
        "Active SoC Electrical Design Current (EDC) limit in amperes.",
        snap.edc_soc_limit_a,
    );

    // Core power family: share a single HELP/TYPE header when aggregate and/or per-core
    // core power metrics are present to stay strictly valid under OpenMetrics / Prometheus.
    let cores = extended.map(|e| e.cores.as_slice()).unwrap_or(&[]);
    let mut core_power_header = false;
    if let Some(v) = finite_val(ext_core_power) {
        let _ = writeln!(
            out,
            "# HELP zensmu_core_power_watts Current CPU core power in watts."
        );
        let _ = writeln!(out, "# TYPE zensmu_core_power_watts gauge");
        let _ = writeln!(out, "zensmu_core_power_watts {v}");
        core_power_header = true;
    }
    if include_per_core {
        for core in cores {
            if let Some(v) = finite_val(core.power_w) {
                if !core_power_header {
                    let _ = writeln!(
                        out,
                        "# HELP zensmu_core_power_watts Current CPU core power in watts."
                    );
                    let _ = writeln!(out, "# TYPE zensmu_core_power_watts gauge");
                    core_power_header = true;
                }
                let _ = writeln!(out, "zensmu_core_power_watts{{core=\"{}\"}} {v}", core.core);
            }
        }

        push_per_core_gauge(
            &mut out,
            "zensmu_core_clock_mhz",
            "Current per-core clock frequency in MHz.",
            cores,
            |c| c.clock_mhz,
        );
        push_per_core_gauge(
            &mut out,
            "zensmu_core_effective_clock_mhz",
            "Current per-core effective clock frequency in MHz.",
            cores,
            |c| c.effective_clock_mhz,
        );
        push_per_core_gauge(
            &mut out,
            "zensmu_core_temperature_celsius",
            "Current per-core temperature in degrees Celsius.",
            cores,
            |c| c.temperature_c,
        );
        push_per_core_gauge(
            &mut out,
            "zensmu_core_voltage_volts",
            "Current per-core voltage in volts.",
            cores,
            |c| c.voltage_v,
        );
        push_per_core_gauge(
            &mut out,
            "zensmu_core_c0_percent",
            "Current per-core C0 active residency percentage.",
            cores,
            |c| c.c0_percent,
        );
    }

    out
}
