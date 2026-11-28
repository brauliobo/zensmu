//! Monitoring, recording, benchmark, and metrics exporter subcommand definitions.

use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;
use zensmu_core::bench::BenchWorkload;
use zensmu_core::metrics::DEFAULT_METRICS_LISTEN;
use zensmu_core::recorder::RecordFormat;

#[derive(Debug, Subcommand)]
pub enum TelemetryAction {
    /// Continuously refresh PM table telemetry until Ctrl-C
    Watch {
        /// Polling interval (e.g. 500ms, 1s)
        #[arg(long, default_value = "1s", value_name = "DURATION")]
        interval: String,
    },

    /// Safely dump raw pm_table bytes to a file for offline inspection
    Dump {
        /// Output file path
        #[arg(value_name = "OUT_PATH")]
        out_path: PathBuf,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordFormatArg {
    Csv,
    Jsonl,
    Pts,
}

impl From<RecordFormatArg> for RecordFormat {
    fn from(value: RecordFormatArg) -> Self {
        match value {
            RecordFormatArg::Csv => RecordFormat::Csv,
            RecordFormatArg::Jsonl => RecordFormat::Jsonl,
            RecordFormatArg::Pts => RecordFormat::Pts,
        }
    }
}

#[derive(Debug, Args)]
pub struct RecordCommand {
    /// Output file path (defaults to ~/.local/share/zensmu/recordings/zensmu-<cpu>-<ts>.<ext>,
    /// or ~/.local/share/zensmu/telemetry.csv when --append is set without -o)
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Shortcut: set --format pts and write directly to ~/.phoronix-test-suite/test-results/<NAME>/composite.xml
    #[arg(long, value_name = "NAME")]
    pub pts_name: Option<String>,

    /// Output format (inferred from --output extension if omitted, else csv)
    #[arg(short = 'f', long, value_enum)]
    pub format: Option<RecordFormatArg>,

    /// Sampling interval (e.g. 500ms, 1s, 2s; minimum 100ms)
    #[arg(short = 'i', long, default_value = "1s", value_name = "DURATION")]
    pub interval: String,

    /// Optional recording duration before stopping cleanly (e.g. 60s, 10m, 1h)
    #[arg(short = 'd', long, alias = "period", value_name = "DURATION")]
    pub duration: Option<String>,

    /// Optional maximum number of samples to record before exiting
    #[arg(short = 'n', long, value_name = "N")]
    pub samples: Option<u64>,

    /// Optional maximum file size in MiB before size-based log rotation (CSV/JSONL)
    #[arg(long, value_name = "MB")]
    pub max_size: Option<u64>,

    /// Number of rotated backup files to retain when --max-size is set
    #[arg(long, default_value_t = 5, value_name = "N")]
    pub rotate: usize,

    /// Append to an existing file without duplicating the CSV header
    #[arg(long)]
    pub append: bool,

    /// Optional workload tag recorded with each sample (e.g. "prime95", "co-20")
    #[arg(long, value_name = "LABEL")]
    pub tag: Option<String>,

    #[command(subcommand)]
    pub action: Option<RecordAction>,
}

#[derive(Debug, Subcommand)]
pub enum RecordAction {
    /// Inspect, enable, or disable the unprivileged user systemd recording daemon (zensmu-record.service)
    Service {
        #[command(subcommand)]
        action: RecordServiceAction,
    },

    /// Convert an existing CSV or JSONL recording to a Phoronix Test Suite composite.xml
    ExportPts {
        /// Input CSV or JSONL recording path
        #[arg(value_name = "INPUT")]
        input: PathBuf,

        /// Destination XML path (defaults to <input_stem>-pts.xml unless --pts-name is set)
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,

        /// Write directly into ~/.phoronix-test-suite/test-results/<NAME>/composite.xml
        #[arg(long, value_name = "NAME")]
        pts_name: Option<String>,

        /// Custom PTS result title
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,

        /// Custom PTS system/run identifier
        #[arg(long, value_name = "ID")]
        identifier: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RecordServiceAction {
    /// Inspect status of the unprivileged user zensmu-record.service daemon
    Status,

    /// Enable and start zensmu-record.service via `systemctl --user enable --now`
    Enable,

    /// Disable and stop zensmu-record.service via `systemctl --user disable --now`
    Disable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BenchWorkloadArg {
    /// Interleaved ALU/integer, FP64 FMA, and L1/L2 cache stress
    Mixed,
    /// Multi-lane 64-bit integer ALU/IMUL/Feistel stress
    Integer,
    /// Multi-lane IEEE-754 FP64 FMA multiply-add stress
    #[value(name = "fp", alias = "floating-point")]
    Fp,
    /// Prime-strided 64-byte cache-line L1/L2 read-modify-write stress
    Cache,
}

impl From<BenchWorkloadArg> for BenchWorkload {
    fn from(value: BenchWorkloadArg) -> Self {
        match value {
            BenchWorkloadArg::Mixed => BenchWorkload::Mixed,
            BenchWorkloadArg::Integer => BenchWorkload::Integer,
            BenchWorkloadArg::Fp => BenchWorkload::FloatingPoint,
            BenchWorkloadArg::Cache => BenchWorkload::Cache,
        }
    }
}

#[derive(Debug, Args)]
pub struct TestCommand {
    /// Physical core selection (e.g. "0,1,4..7" or "0-3"; default: all physical cores)
    #[arg(long, value_name = "SPEC")]
    pub cores: Option<String>,

    /// Stress duration per core per pass (e.g. 30s, 2m; 1s..=3600s)
    #[arg(short = 'd', long, default_value = "30s", value_name = "DURATION")]
    pub duration: String,

    /// Number of full passes across the selected cores (1..=100)
    #[arg(short = 'p', long, default_value_t = 1, value_name = "N")]
    pub passes: u32,

    /// Deterministic self-verifying stress workload (mixed, integer, fp, cache)
    #[arg(short = 'w', long, value_enum, default_value_t = BenchWorkloadArg::Mixed)]
    pub workload: BenchWorkloadArg,

    /// Telemetry and progress poll interval (e.g. 500ms, 1s; minimum 100ms)
    #[arg(short = 'i', long, default_value = "500ms", value_name = "DURATION")]
    pub interval: String,

    /// Continue testing remaining cores after an MCE or compute self-check failure
    #[arg(long)]
    pub continue_on_error: bool,

    /// Automatically restore last-state.json if any core fails with MCE/compute errors (requires root)
    #[arg(long)]
    pub rollback_on_fail: bool,
}

#[derive(Debug, Args)]
pub struct ExportCommand {
    /// Listen address for the HTTP/1.1 Prometheus exporter (default: 127.0.0.1:9452)
    #[arg(long, default_value = DEFAULT_METRICS_LISTEN, value_name = "ADDR")]
    pub listen: String,

    /// Omit per-core metrics and export package/socket-level metrics only
    #[arg(long)]
    pub no_per_core: bool,

    #[command(subcommand)]
    pub action: Option<ExportAction>,
}

#[derive(Debug, Subcommand)]
pub enum ExportAction {
    /// Inspect, enable, or disable the unprivileged user systemd exporter daemon (zensmu-exporter.service)
    Service {
        #[command(subcommand)]
        action: ExportServiceAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ExportServiceAction {
    /// Inspect status of the unprivileged user zensmu-exporter.service daemon
    Status,

    /// Enable and start zensmu-exporter.service via `systemctl --user enable --now`
    Enable,

    /// Disable and stop zensmu-exporter.service via `systemctl --user disable --now`
    Disable,
}
