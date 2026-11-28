//! Tuning subcommand definitions (`co`, `shaper`, `pbo`, `apu`).

use clap::{Subcommand, ValueEnum};
use zensmu_core::types::ApuPowerMode;

#[derive(Debug, Subcommand)]
pub enum CoAction {
    /// Read current per-core and iGPU Curve Optimizer margins
    Get,

    /// Set all-core, per-core, and/or iGPU Curve Optimizer margins
    Set {
        /// Set an all-core Curve Optimizer margin (e.g. -20)
        #[arg(long, allow_hyphen_values = true, value_name = "MARGIN")]
        all: Option<i32>,

        /// Set per-core margins (repeatable), e.g. --core 0=-25 --core 2..7=-20
        #[arg(long = "core", allow_hyphen_values = true, value_name = "SPEC")]
        cores: Vec<String>,

        /// Set iGPU Curve Optimizer margin (e.g. -15)
        #[arg(long, allow_hyphen_values = true, value_name = "MARGIN")]
        igpu: Option<i32>,
    },

    /// Reset all Curve Optimizer margins (and iGPU if supported) to 0
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum ShaperAction {
    /// Read the full 5x3 Zen 5 Curve Shaper matrix
    Get,

    /// Set Curve Shaper margins for a specific frequency tier or across all tiers
    Set {
        /// Frequency tier: min, low, med, high, or max
        #[arg(long, value_name = "TIER")]
        freq: Option<String>,

        /// Temperature band spec, e.g. low=-10,med=-15,high=-5 or all=-10
        #[arg(long, allow_hyphen_values = true, value_name = "SPEC")]
        temp: Option<String>,

        /// Set all 15 Curve Shaper points (5 tiers x 3 bands) to a single margin
        #[arg(long, allow_hyphen_values = true, value_name = "MARGIN")]
        all: Option<i32>,
    },

    /// Reset all 5 Curve Shaper frequency tiers to (0, 0, 0)
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum PboAction {
    /// Read active PM table limits, fused factory limits, scalar, Fmax, flags, and CPPC scores
    Get,

    /// Set PBO power/current/thermal limits, scalar, and/or Fmax boost clock
    Set {
        /// Package Power Tracking limit (e.g. 142, 142W, 142000mW)
        #[arg(long, value_name = "WATTS")]
        ppt: Option<String>,

        /// Thermal Design Current limit (e.g. 110, 110A, 110000mA)
        #[arg(long, value_name = "AMPS")]
        tdc: Option<String>,

        /// Electrical Design Current limit (e.g. 170, 170A, 170000mA)
        #[arg(long, value_name = "AMPS")]
        edc: Option<String>,

        /// Thermal throttle limit Tctl/TjMax (e.g. 85, 85C)
        #[arg(long, alias = "tjmax", value_name = "CELSIUS")]
        tctl: Option<String>,

        /// PBO scalar multiplier (1..=10)
        #[arg(long, value_name = "1..=10")]
        scalar: Option<u32>,

        /// Fmax boost clock ceiling (e.g. 5550, 5550MHz, 5.55GHz)
        #[arg(long, value_name = "FREQ")]
        fmax: Option<String>,
    },

    /// Restore fused factory PBO limits (and boot baseline PBO state if available)
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum ApuAction {
    /// Set mobile APU DPTC power, current, thermal skin, and power mode parameters
    Set {
        /// Sustained power limit STAPM (e.g. 28, 28W)
        #[arg(long, value_name = "WATTS")]
        stapm: Option<String>,

        /// STAPM time constant in seconds (e.g. 120, 120s)
        #[arg(long, value_name = "SECS")]
        stapm_time: Option<String>,

        /// Fast PPT boost power limit (e.g. 45, 45W)
        #[arg(long, value_name = "WATTS")]
        fast: Option<String>,

        /// Slow PPT sustained power limit (e.g. 35, 35W)
        #[arg(long, value_name = "WATTS")]
        slow: Option<String>,

        /// Slow PPT time constant in seconds (e.g. 10, 10s)
        #[arg(long, value_name = "SECS")]
        slow_time: Option<String>,

        /// APU-only slow PPT power limit (e.g. 30, 30W)
        #[arg(long, value_name = "WATTS")]
        apu_slow: Option<String>,

        /// SoC TDC current limit (e.g. 30, 30A)
        #[arg(long, value_name = "AMPS")]
        tdc_soc: Option<String>,

        /// SoC EDC current limit (e.g. 50, 50A)
        #[arg(long, value_name = "AMPS")]
        edc_soc: Option<String>,

        /// APU skin temperature target (e.g. 45, 45C)
        #[arg(long, alias = "apu-skin-temp", value_name = "CELSIUS")]
        apu_skin: Option<String>,

        /// dGPU skin temperature target (e.g. 60, 60C)
        #[arg(long, alias = "dgpu-skin-temp", value_name = "CELSIUS")]
        dgpu_skin: Option<String>,

        /// APU power/performance mode
        #[arg(long, value_enum)]
        mode: Option<ApuModeArg>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApuModeArg {
    #[value(name = "max-performance")]
    MaxPerformance,
    #[value(name = "power-saving")]
    PowerSaving,
}

impl From<ApuModeArg> for ApuPowerMode {
    fn from(value: ApuModeArg) -> Self {
        match value {
            ApuModeArg::MaxPerformance => ApuPowerMode::MaxPerformance,
            ApuModeArg::PowerSaving => ApuPowerMode::PowerSaving,
        }
    }
}
