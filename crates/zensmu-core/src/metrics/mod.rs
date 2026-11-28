//! Zero-dependency unprivileged Prometheus / OpenMetrics telemetry exporter.
//!
//! Exposes live `ryzen_smu` PM table telemetry over HTTP/1.1 in Prometheus text
//! exposition format (`0.0.4`) with no root privileges required, plus user-level
//! `systemd` service inspection/control helpers for `zensmu-exporter.service`.

mod render;
mod server;
mod service;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

pub use render::{
    escape_prom_label, render_prometheus_metrics, render_prometheus_metrics_with_extended,
};
pub use server::serve_metrics;
pub use service::{
    read_exporter_service_status, set_exporter_service_enabled, ExporterServiceStatus,
    USER_EXPORTER_SERVICE_NAME,
};

/// Default loopback bind address for the unprivileged Prometheus / OpenMetrics exporter.
pub const DEFAULT_METRICS_LISTEN: &str = "127.0.0.1:9452";

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

/// Optional extended telemetry readings supplementing [`crate::ops::pm_table::TelemetrySnapshot`]
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
