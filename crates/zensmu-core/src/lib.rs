//! `zensmu-core`: Safety-first library for AMD Ryzen SMU control and telemetry.

pub mod bench;
pub mod config;
pub mod driver;
pub mod error;
pub mod hw;
pub mod metrics;
pub mod ops;
pub mod recorder;
pub mod report;
pub mod safety;
pub mod session;
pub mod transport;
pub mod types;

pub use error::{ClampedLimit, Error, Result, SmuStatus};
pub use types::{
    ApuPowerMode, Codename, CoreInfo, CurveShaperState, FreqTier, Mailbox, PlannedOp,
    ShaperTierMargins, SmuArgs, TempBand,
};
