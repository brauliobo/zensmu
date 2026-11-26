//! `zensmu-core`: Safety-first library for AMD Ryzen SMU control and telemetry.

pub mod config;
pub mod error;
pub mod hw;
pub mod ops;
pub mod safety;
pub mod session;
pub mod transport;
pub mod types;

pub use error::{Error, Result, SmuStatus};
pub use types::{
    ApuPowerMode, Codename, CoreInfo, CurveShaperState, FreqTier, Mailbox, PlannedOp,
    ShaperTierMargins, SmuArgs, TempBand,
};
