//! Zero-dependency unprivileged Prometheus / OpenMetrics telemetry exporter.
//!
//! Exposes live `ryzen_smu` PM table telemetry over HTTP/1.1 in Prometheus text
//! exposition format (`0.0.4`) with no root privileges required, plus user-level
//! `systemd` service inspection/control helpers for `zensmu-exporter.service`.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::ops::pm_table::{self, TelemetrySnapshot};
use crate::transport::SmuTransport;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Default loopback bind address for the unprivileged Prometheus / OpenMetrics exporter.
pub const DEFAULT_METRICS_LISTEN: &str = "127.0.0.1:9452";

/// User-level systemd unit name for the background Prometheus / OpenMetrics exporter.
pub const USER_EXPORTER_SERVICE_NAME: &str = "zensmu-exporter.service";

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_HTTP_REQUEST_BYTES: usize = 4096;

const INDEX_HTML: &str = concat!(
    "<!DOCTYPE html>\n",
    "<html lang=\"en\">\n",
    "<head><meta charset=\"utf-8\"><title>zensmu exporter</title></head>\n",
    "<body>\n",
    "<h1>zensmu Prometheus exporter</h1>\n",
    "<p><a href=\"/metrics\">Metrics</a> | <a href=\"/healthz\">Health</a></p>\n",
    "</body>\n",
    "</html>\n",
);

/// Runtime configuration for [`serve_metrics`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsServerConfig {
    pub listen_addr: String,
    pub include_per_core: bool,
}

impl Default for MetricsServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: DEFAULT_METRICS_LISTEN.to_string(),
            include_per_core: true,
        }
    }
}

/// Optional per-core telemetry readings exported when `include_per_core` is enabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CoreMetricsSample {
    pub core: u32,
    pub clock_mhz: Option<f32>,
    pub effective_clock_mhz: Option<f32>,
    pub temperature_c: Option<f32>,
    pub voltage_v: Option<f32>,
    pub power_w: Option<f32>,
    pub c0_percent: Option<f32>,
}

/// Optional extended telemetry readings supplementing [`TelemetrySnapshot`]
/// (domain power rails, Vcore, iGPU telemetry, and per-core samples).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtendedTelemetry {
    pub package_power_w: Option<f32>,
    pub core_power_w: Option<f32>,
    pub soc_power_w: Option<f32>,
    pub vcore_v: Option<f32>,
    pub igpu_clock_mhz: Option<f32>,
    pub igpu_temp_c: Option<f32>,
    pub igpu_power_w: Option<f32>,
    pub fast_limit_w: Option<f32>,
    pub cores: Vec<CoreMetricsSample>,
}

/// Read-only status of the unprivileged user-level `zensmu-exporter.service` unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExporterServiceStatus {
    pub service_name: String,
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
    pub state: String,
    pub endpoint: String,
}

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

/// Run the zero-dependency HTTP/1.1 Prometheus / OpenMetrics exporter until `stop_flag` is set.
///
/// Binds `config.listen_addr` in non-blocking mode, invokes `on_ready(local_addr)` with the
/// actual bound address (useful for ephemeral `127.0.0.1:0` binds), and serves:
/// - `GET /metrics` (or `HEAD /metrics`): live telemetry via [`pm_table::read_telemetry`]
/// - `GET /healthz`: `200 OK` (`ok\n`)
/// - `GET /`: `200 OK` HTML index linking to `/metrics`
/// - Any other path: `404 Not Found`
/// - Non-`GET`/`HEAD` methods: `405 Method Not Allowed`
pub fn serve_metrics<F>(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    config: &MetricsServerConfig,
    stop_flag: &Arc<AtomicBool>,
    mut on_ready: F,
) -> Result<()>
where
    F: FnMut(SocketAddr),
{
    let listener = TcpListener::bind(&config.listen_addr).map_err(|source| Error::Io {
        path: format!("tcp://{}", config.listen_addr),
        source,
    })?;
    let local_addr = listener.local_addr().map_err(|source| Error::Io {
        path: format!("tcp://{}", config.listen_addr),
        source,
    })?;
    listener.set_nonblocking(true).map_err(|source| Error::Io {
        path: format!("tcp://{local_addr}"),
        source,
    })?;

    on_ready(local_addr);

    while !stop_flag.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _peer)) => {
                handle_connection(&mut stream, transport, hw, config);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {
                continue;
            }
            Err(_) => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }

    Ok(())
}

fn contains_header_terminator(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|w| w == b"\r\n\r\n") || bytes.windows(2).any(|w| w == b"\n\n")
}

fn handle_connection(
    stream: &mut TcpStream,
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    config: &MetricsServerConfig,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT));

    let mut buf = [0u8; MAX_HTTP_REQUEST_BYTES];
    let mut total = 0usize;
    while total < buf.len() {
        match stream.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if contains_header_terminator(&buf[..total]) {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
            Err(_) => return,
        }
    }

    if total == 0 {
        return;
    }

    let request = String::from_utf8_lossy(&buf[..total]);
    let first_line = request.lines().next().map(str::trim).unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        let _ = write_http_response(
            stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            "bad request\n",
            false,
            None,
        );
        return;
    };

    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => {
            let _ = write_http_response(
                stream,
                "405 Method Not Allowed",
                "text/plain; charset=utf-8",
                "method not allowed\n",
                false,
                Some("Allow: GET, HEAD"),
            );
            return;
        }
    };

    let path = target.split('?').next().unwrap_or(target);
    match path {
        "/metrics" => match pm_table::read_telemetry(transport, hw) {
            Ok(snap) => {
                let body = render_prometheus_metrics(hw, &snap, config.include_per_core);
                let _ = write_http_response(
                    stream,
                    "200 OK",
                    PROMETHEUS_CONTENT_TYPE,
                    &body,
                    head_only,
                    None,
                );
            }
            Err(err) => {
                let body = format!("telemetry unavailable: {err}\n");
                let _ = write_http_response(
                    stream,
                    "503 Service Unavailable",
                    "text/plain; charset=utf-8",
                    &body,
                    head_only,
                    None,
                );
            }
        },
        "/healthz" => {
            let _ = write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "ok\n",
                head_only,
                None,
            );
        }
        "/" => {
            let _ = write_http_response(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                INDEX_HTML,
                head_only,
                None,
            );
        }
        _ => {
            let _ = write_http_response(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found\n",
                head_only,
                None,
            );
        }
    }
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    head_only: bool,
    extra_header: Option<&str>,
) -> std::io::Result<()> {
    let mut header = String::with_capacity(192);
    let _ = write!(header, "HTTP/1.1 {status}\r\n");
    let _ = write!(header, "Content-Type: {content_type}\r\n");
    let _ = write!(header, "Content-Length: {}\r\n", body.len());
    let _ = write!(header, "Cache-Control: no-store\r\n");
    if let Some(extra) = extra_header {
        let _ = write!(header, "{extra}\r\n");
    }
    header.push_str("Connection: close\r\n\r\n");

    stream.write_all(header.as_bytes())?;
    if !head_only {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn user_unit_candidate_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/usr/lib/systemd/user").join(USER_EXPORTER_SERVICE_NAME),
        PathBuf::from("/etc/systemd/user").join(USER_EXPORTER_SERVICE_NAME),
    ];
    if let Some(xdg_config) = env_path("XDG_CONFIG_HOME") {
        paths.push(
            xdg_config
                .join("systemd")
                .join("user")
                .join(USER_EXPORTER_SERVICE_NAME),
        );
    }
    if let Some(home) = env_path("HOME") {
        paths.push(
            home.join(".config")
                .join("systemd")
                .join("user")
                .join(USER_EXPORTER_SERVICE_NAME),
        );
    }
    paths
}

fn query_user_systemctl(subcommand: &str) -> (bool, String) {
    match Command::new("systemctl")
        .args(["--user", subcommand, USER_EXPORTER_SERVICE_NAME])
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let state = if !stdout.is_empty() {
                stdout
            } else if !stderr.is_empty() {
                stderr
            } else if out.status.success() {
                "ok".to_string()
            } else {
                "unknown".to_string()
            };
            (out.status.success(), state)
        }
        Err(_) => (false, "systemctl-unavailable".to_string()),
    }
}

fn is_installed_unit_file_state(state: &str) -> bool {
    matches!(
        state,
        "enabled"
            | "enabled-runtime"
            | "linked"
            | "linked-runtime"
            | "alias"
            | "masked"
            | "masked-runtime"
            | "static"
            | "indirect"
            | "disabled"
            | "generated"
            | "transient"
    )
}

/// Inspect the unprivileged user-level `zensmu-exporter.service` unit status.
pub fn read_exporter_service_status() -> ExporterServiceStatus {
    let unit_file_exists = user_unit_candidate_paths().iter().any(|p| p.exists());
    let (enabled_ok, enabled_state) = query_user_systemctl("is-enabled");
    let (active_ok, active_state) = query_user_systemctl("is-active");

    let installed = unit_file_exists || is_installed_unit_file_state(&enabled_state);
    let enabled =
        enabled_ok && matches!(enabled_state.as_str(), "enabled" | "enabled-runtime" | "ok");
    let active = active_ok && matches!(active_state.as_str(), "active" | "ok");

    let state = if active {
        "active".to_string()
    } else if matches!(
        active_state.as_str(),
        "activating" | "deactivating" | "reloading" | "failed"
    ) {
        active_state
    } else if enabled {
        "enabled".to_string()
    } else if installed {
        if is_installed_unit_file_state(&enabled_state) {
            enabled_state
        } else {
            "disabled".to_string()
        }
    } else if enabled_state == "systemctl-unavailable" {
        "systemctl-unavailable".to_string()
    } else {
        "not-installed".to_string()
    };

    ExporterServiceStatus {
        service_name: USER_EXPORTER_SERVICE_NAME.to_string(),
        installed,
        enabled,
        active,
        state,
        endpoint: format!("http://{DEFAULT_METRICS_LISTEN}/metrics"),
    }
}

/// Enable (`enable --now`) or disable (`disable --now`) the unprivileged user-level
/// `zensmu-exporter.service` unit and return its refreshed status.
pub fn set_exporter_service_enabled(enable: bool) -> Result<ExporterServiceStatus> {
    let current = read_exporter_service_status();
    if !current.installed {
        return Err(Error::InvalidInput(format!(
            "{USER_EXPORTER_SERVICE_NAME} is not installed (expected in /usr/lib/systemd/user/ or ~/.config/systemd/user/)"
        )));
    }

    let verb = if enable { "enable" } else { "disable" };
    let out = Command::new("systemctl")
        .args(["--user", verb, "--now", USER_EXPORTER_SERVICE_NAME])
        .output()
        .map_err(|e| {
            Error::InvalidInput(format!(
                "failed to execute systemctl --user {verb} --now {USER_EXPORTER_SERVICE_NAME}: {e}"
            ))
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        return Err(Error::InvalidInput(format!(
            "systemctl --user {verb} --now {USER_EXPORTER_SERVICE_NAME} failed: {msg}"
        )));
    }

    Ok(read_exporter_service_status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::identity::CpuidInfo;
    use crate::hw::profiles::capabilities_for;
    use crate::hw::topology::CpuTopology;
    use crate::transport::mock::MockTransport;
    use crate::types::Codename;
    use std::collections::HashSet;
    use std::sync::mpsc;

    fn granite_ridge_hw(transport: &MockTransport) -> HardwareContext {
        HardwareContext {
            driver: transport.read_info().unwrap(),
            codename: Codename::GraniteRidge,
            cpuid: Some(CpuidInfo {
                vendor: "AuthenticAMD".into(),
                brand: "AMD Ryzen 7 9700X 8-Core Processor".into(),
                family: 0x1A,
                model: 0x44,
                stepping: 0,
            }),
            topology: CpuTopology::synthetic(1, 8, 2),
            capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
        }
    }

    fn phoenix_hw(transport: &MockTransport) -> HardwareContext {
        HardwareContext {
            driver: transport.read_info().unwrap(),
            codename: Codename::Phoenix,
            cpuid: Some(CpuidInfo {
                vendor: "AuthenticAMD".into(),
                brand: "AMD Ryzen 7 7840U w/ Radeon 780M Graphics".into(),
                family: 0x19,
                model: 0x74,
                stepping: 1,
            }),
            topology: CpuTopology::synthetic(1, 8, 2),
            capabilities: capabilities_for(Codename::Phoenix).unwrap(),
        }
    }

    fn assert_no_duplicate_metric_headers(text: &str) {
        let mut seen_help = HashSet::new();
        let mut seen_type = HashSet::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("# HELP ") {
                let name = rest.split_whitespace().next().unwrap();
                assert!(
                    seen_help.insert(name.to_string()),
                    "duplicate # HELP for metric {name}"
                );
            } else if let Some(rest) = line.strip_prefix("# TYPE ") {
                let name = rest.split_whitespace().next().unwrap();
                assert!(
                    seen_type.insert(name.to_string()),
                    "duplicate # TYPE for metric {name}"
                );
            }
        }
    }

    fn http_request(addr: SocketAddr, req: &str) -> String {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        resp
    }

    #[test]
    fn renders_granite_ridge_prometheus_metrics() {
        let transport = MockTransport::granite_ridge_9700x();
        let hw = granite_ridge_hw(&transport);
        let snap = pm_table::read_telemetry(&transport, &hw).unwrap();

        let body = render_prometheus_metrics(&hw, &snap, true);
        assert_no_duplicate_metric_headers(&body);

        assert!(body.contains(
            "zensmu_info{codename=\"granite-ridge\",drv_version=\"0.1.7\",smu_version=\"98.79.0\",cpu_name=\"AMD Ryzen 7 9700X 8-Core Processor\"} 1"
        ));
        assert!(body.contains("# TYPE zensmu_ppt_watts gauge\nzensmu_ppt_watts 42.5\n"));
        assert!(body.contains("zensmu_ppt_limit_watts 88\n"));
        assert!(body.contains("zensmu_tdc_amperes 21\n"));
        assert!(body.contains("zensmu_tdc_limit_amperes 75\n"));
        assert!(body.contains("zensmu_edc_amperes 38\n"));
        assert!(body.contains("zensmu_edc_limit_amperes 150\n"));
        assert!(body.contains("zensmu_temperature_celsius{sensor=\"tctl\"} 54.2\n"));
        assert!(body.contains("zensmu_temperature_limit_celsius{sensor=\"tctl\"} 95\n"));
        assert!(body.contains("zensmu_voltage_volts{rail=\"vsoc\"} 1.24\n"));
        assert!(body.contains("zensmu_voltage_volts{rail=\"vddcr_misc\"} 1.1\n"));
        assert!(body.contains("zensmu_voltage_volts{rail=\"cldo_vddp\"} 1.05\n"));
        assert!(body.contains("zensmu_clock_mhz{domain=\"fclk\"} 2000\n"));
        assert!(body.contains("zensmu_clock_mhz{domain=\"uclk\"} 3000\n"));
        assert!(body.contains("zensmu_clock_mhz{domain=\"mclk\"} 3000\n"));

        // APU-only metrics should not appear on desktop telemetry
        assert!(!body.contains("zensmu_stapm_watts"));
        assert!(!body.contains("zensmu_fast_limit_watts"));
        assert!(!body.contains("zensmu_slow_limit_watts"));
        assert!(!body.contains("zensmu_soc_tdc_amperes"));
    }

    #[test]
    fn renders_phoenix_apu_metrics_under_single_headers() {
        let transport = MockTransport::phoenix_apu();
        let hw = phoenix_hw(&transport);
        let snap = pm_table::read_telemetry(&transport, &hw).unwrap();

        let body = render_prometheus_metrics(&hw, &snap, true);
        assert_no_duplicate_metric_headers(&body);

        assert!(body.contains("zensmu_stapm_watts 15\n"));
        assert!(body.contains("zensmu_stapm_limit_watts 28\n"));
        assert!(body.contains("zensmu_fast_limit_watts 45\n"));
        assert!(body.contains("zensmu_slow_limit_watts 35\n"));
        assert!(body.contains("zensmu_soc_tdc_amperes 8\n"));
        assert!(body.contains("zensmu_soc_tdc_limit_amperes 20\n"));
        assert!(body.contains("zensmu_soc_edc_amperes 12\n"));
        assert!(body.contains("zensmu_soc_edc_limit_amperes 35\n"));
        assert!(body.contains("zensmu_temperature_celsius{sensor=\"tctl\"} 52\n"));
        assert!(body.contains("zensmu_temperature_celsius{sensor=\"apu_skin\"} 42\n"));
        assert!(body.contains("zensmu_temperature_celsius{sensor=\"dgpu_skin\"} 40\n"));
    }

    #[test]
    fn escapes_labels_and_omits_none_and_non_finite_values() {
        assert_eq!(
            escape_prom_label("AMD \"Zen\" \\ line1\r\nline2"),
            "AMD \\\"Zen\\\" \\\\ line1\\nline2"
        );

        let transport = MockTransport::granite_ridge_9700x();
        let mut hw = granite_ridge_hw(&transport);
        hw.cpuid.as_mut().unwrap().brand = "AMD \"Zen5\" \\ Rev\nA".to_string();

        let mut snap = pm_table::read_telemetry(&transport, &hw).unwrap();
        snap.ppt_value_w = Some(f32::NAN);
        snap.ppt_limit_w = Some(f32::INFINITY);
        snap.tdc_value_a = Some(f32::NEG_INFINITY);
        snap.tdc_limit_a = None;
        snap.edc_value_a = Some(-0.0);

        let body = render_prometheus_metrics(&hw, &snap, true);
        assert_no_duplicate_metric_headers(&body);

        assert!(body.contains("cpu_name=\"AMD \\\"Zen5\\\" \\\\ Rev\\nA\""));
        assert!(!body.contains("zensmu_ppt_watts"));
        assert!(!body.contains("zensmu_ppt_limit_watts"));
        assert!(!body.contains("zensmu_tdc_amperes"));
        assert!(!body.contains("zensmu_tdc_limit_amperes"));
        assert!(body.contains("zensmu_edc_amperes 0\n"));
        for line in body
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let val_str = line.rsplit(' ').next().unwrap();
            assert_ne!(val_str, "-0", "negative zero emitted in line: {line}");
            let parsed: f64 = val_str.parse().unwrap_or_else(|e| {
                panic!("non-numeric sample value '{val_str}' in '{line}': {e}")
            });
            assert!(
                parsed.is_finite(),
                "non-finite sample value '{val_str}' in '{line}'"
            );
        }
    }

    #[test]
    fn renders_extended_and_toggles_per_core_metrics() {
        let transport = MockTransport::granite_ridge_9700x();
        let hw = granite_ridge_hw(&transport);
        let snap = pm_table::read_telemetry(&transport, &hw).unwrap();

        let extended = ExtendedTelemetry {
            package_power_w: Some(64.5),
            core_power_w: Some(41.25),
            soc_power_w: Some(14.5),
            vcore_v: Some(1.18),
            igpu_clock_mhz: Some(2200.0),
            igpu_temp_c: Some(46.0),
            igpu_power_w: Some(6.5),
            fast_limit_w: None,
            cores: vec![
                CoreMetricsSample {
                    core: 0,
                    clock_mhz: Some(5450.0),
                    effective_clock_mhz: Some(5410.0),
                    temperature_c: Some(61.5),
                    voltage_v: Some(1.22),
                    power_w: Some(9.5),
                    c0_percent: Some(98.5),
                },
                CoreMetricsSample {
                    core: 1,
                    clock_mhz: Some(f32::NAN),
                    effective_clock_mhz: Some(4800.0),
                    temperature_c: None,
                    voltage_v: Some(f32::INFINITY),
                    power_w: Some(4.25),
                    c0_percent: Some(32.0),
                },
            ],
        };

        let with_cores = render_prometheus_metrics_with_extended(&hw, &snap, Some(&extended), true);
        assert_no_duplicate_metric_headers(&with_cores);
        assert!(with_cores.contains("zensmu_package_power_watts 64.5\n"));
        assert!(with_cores.contains("zensmu_core_power_watts 41.25\n"));
        assert!(with_cores.contains("zensmu_soc_power_watts 14.5\n"));
        assert!(with_cores.contains("zensmu_voltage_volts{rail=\"vcore\"} 1.18\n"));
        assert!(with_cores.contains("zensmu_clock_mhz{domain=\"igpu\"} 2200\n"));
        assert!(with_cores.contains("zensmu_temperature_celsius{sensor=\"igpu\"} 46\n"));
        assert!(with_cores.contains("zensmu_igpu_power_watts 6.5\n"));
        assert!(with_cores.contains("zensmu_core_clock_mhz{core=\"0\"} 5450\n"));
        assert!(!with_cores.contains("zensmu_core_clock_mhz{core=\"1\"}"));
        assert!(with_cores.contains("zensmu_core_effective_clock_mhz{core=\"0\"} 5410\n"));
        assert!(with_cores.contains("zensmu_core_effective_clock_mhz{core=\"1\"} 4800\n"));
        assert!(with_cores.contains("zensmu_core_temperature_celsius{core=\"0\"} 61.5\n"));
        assert!(with_cores.contains("zensmu_core_voltage_volts{core=\"0\"} 1.22\n"));
        assert!(!with_cores.contains("zensmu_core_voltage_volts{core=\"1\"}"));
        assert!(with_cores.contains("zensmu_core_power_watts{core=\"0\"} 9.5\n"));
        assert!(with_cores.contains("zensmu_core_power_watts{core=\"1\"} 4.25\n"));
        assert!(with_cores.contains("zensmu_core_c0_percent{core=\"0\"} 98.5\n"));
        assert!(with_cores.contains("zensmu_core_c0_percent{core=\"1\"} 32\n"));

        let without_cores =
            render_prometheus_metrics_with_extended(&hw, &snap, Some(&extended), false);
        assert_no_duplicate_metric_headers(&without_cores);
        assert!(without_cores.contains("zensmu_core_power_watts 41.25\n"));
        assert!(!without_cores.contains("{core="));
        assert!(!without_cores.contains("zensmu_core_clock_mhz"));
        assert!(!without_cores.contains("zensmu_core_effective_clock_mhz"));
        assert!(!without_cores.contains("zensmu_core_temperature_celsius"));
        assert!(!without_cores.contains("zensmu_core_voltage_volts"));
        assert!(!without_cores.contains("zensmu_core_c0_percent"));
    }

    #[test]
    fn serves_metrics_over_loopback_and_exits_cleanly_on_stop_flag() {
        let transport = Arc::new(MockTransport::granite_ridge_9700x());
        let hw = granite_ridge_hw(&transport);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let srv_transport = Arc::clone(&transport);
        let srv_stop = Arc::clone(&stop_flag);
        let handle = thread::spawn(move || {
            let cfg = MetricsServerConfig {
                listen_addr: "127.0.0.1:0".to_string(),
                include_per_core: true,
            };
            serve_metrics(srv_transport.as_ref(), &hw, &cfg, &srv_stop, |addr| {
                let _ = tx.send(addr);
            })
        });

        let addr = rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // GET /metrics
        let metrics_resp = http_request(addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(metrics_resp.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(metrics_resp.contains("Content-Type: text/plain; version=0.0.4; charset=utf-8\r\n"));
        assert!(metrics_resp.contains("Connection: close\r\n"));
        assert!(metrics_resp.contains("zensmu_info{codename=\"granite-ridge\""));
        assert!(metrics_resp.contains("zensmu_ppt_watts 42.5\n"));

        // HEAD /metrics (headers only, no body)
        let head_resp = http_request(addr, "HEAD /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(head_resp.starts_with("HTTP/1.1 200 OK\r\n"));
        let (_, head_body) = head_resp.split_once("\r\n\r\n").unwrap();
        assert!(head_body.is_empty());

        // GET /healthz
        let health_resp = http_request(addr, "GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(health_resp.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(health_resp.ends_with("\r\n\r\nok\n"));

        // GET /
        let index_resp = http_request(addr, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(index_resp.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(index_resp.contains("href=\"/metrics\""));

        // GET /missing -> 404
        let missing_resp = http_request(addr, "GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(missing_resp.starts_with("HTTP/1.1 404 Not Found\r\n"));

        // POST /metrics -> 405
        let method_resp = http_request(addr, "POST /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(method_resp.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        assert!(method_resp.contains("Allow: GET, HEAD\r\n"));

        stop_flag.store(true, Ordering::Relaxed);
        let result = handle.join().unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn returns_503_when_telemetry_is_unavailable() {
        let transport = Arc::new(MockTransport::granite_ridge_9700x());
        let mut hw = granite_ridge_hw(&transport);
        hw.driver.has_pm_table = false;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let srv_transport = Arc::clone(&transport);
        let srv_stop = Arc::clone(&stop_flag);
        let handle = thread::spawn(move || {
            let cfg = MetricsServerConfig {
                listen_addr: "127.0.0.1:0".to_string(),
                include_per_core: false,
            };
            serve_metrics(srv_transport.as_ref(), &hw, &cfg, &srv_stop, |addr| {
                let _ = tx.send(addr);
            })
        });

        let addr = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let resp = http_request(addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(resp.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(resp.contains("telemetry unavailable:"));

        stop_flag.store(true, Ordering::Relaxed);
        assert!(handle.join().unwrap().is_ok());
    }

    #[test]
    fn config_default_and_service_status_helpers_are_well_formed() {
        let cfg = MetricsServerConfig::default();
        assert_eq!(cfg.listen_addr, DEFAULT_METRICS_LISTEN);
        assert!(cfg.include_per_core);

        let status = read_exporter_service_status();
        assert_eq!(status.service_name, USER_EXPORTER_SERVICE_NAME);
        assert_eq!(status.endpoint, "http://127.0.0.1:9452/metrics");
        assert!(!status.state.is_empty());
    }
}
