pub mod apu;
pub mod co;
pub mod pbo;
pub mod pm_table;
pub mod shaper;

use crate::error::{ClampedLimit, Error, Result};
use crate::hw::identity::HardwareContext;
use crate::hw::profiles::{DualOpcode, OpcodeSpec};
use crate::ops::co::{check_mailbox_health, send_dual_verified, send_verified};
use crate::transport::SmuTransport;
use crate::types::{Mailbox, SmuArgs};

/// Internal execution descriptor shared by batched PBO and APU write operations.
pub(crate) enum ExecOp {
    Single(OpcodeSpec, SmuArgs),
    Dual(DualOpcode, SmuArgs),
}

impl ExecOp {
    fn primary_mailbox(&self) -> Mailbox {
        match self {
            Self::Single(s, _) => s.mailbox,
            Self::Dual(d, _) => d.primary.mailbox,
        }
    }
}

/// Health-check every distinct target mailbox once, then execute all queued operations
/// in order with full SMN RSP verification.
pub(crate) fn execute_ops(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    exec_ops: Vec<ExecOp>,
) -> Result<()> {
    let mut checked_rsmu = false;
    let mut checked_mp1 = false;
    for op in &exec_ops {
        match op.primary_mailbox() {
            Mailbox::Rsmu if !checked_rsmu => {
                check_mailbox_health(transport, hw, Mailbox::Rsmu)?;
                checked_rsmu = true;
            }
            Mailbox::Mp1 if !checked_mp1 => {
                check_mailbox_health(transport, hw, Mailbox::Mp1)?;
                checked_mp1 = true;
            }
            _ => {}
        }
    }

    for op in exec_ops {
        match op {
            ExecOp::Single(spec, args) => {
                send_verified(transport, hw, spec, args)?;
            }
            ExecOp::Dual(dual, args) => {
                send_dual_verified(transport, hw, dual, args)?;
            }
        }
    }

    Ok(())
}

/// Verify all requested PM-table limits against observed hardware telemetry.
///
/// Checks every requested field rather than short-circuiting on the first
/// clamped limit so callers receive the complete list of board/BIOS caps in
/// [`Error::HardwareClamped`]. Non-finite readings or observed values exceeding
/// the requested target still return [`Error::VerificationFailed`].
pub(crate) fn verify_pm_limits(checks: &[(&str, Option<u32>, Option<f32>, &str)]) -> Result<()> {
    let mut clamped = Vec::new();

    for &(field, requested, observed, unit) in checks {
        let (Some(exp), Some(act)) = (requested, observed) else {
            continue;
        };
        if !act.is_finite() {
            return Err(Error::VerificationFailed {
                field: field.into(),
                expected: format!("{exp} {unit}"),
                actual: format!("{act:.1} {unit}"),
            });
        }
        let exp_f = exp as f32;
        if (act - exp_f).abs() <= 1.0 {
            continue;
        }
        if act < exp_f - 1.0 {
            clamped.push(ClampedLimit {
                field: field.into(),
                requested: format!("{exp} {unit}"),
                actual: format!("{act:.0} {unit}"),
                actual_value: act.round().max(0.0) as u32,
                unit: unit.into(),
            });
        } else {
            return Err(Error::VerificationFailed {
                field: field.into(),
                expected: format!("{exp} {unit}"),
                actual: format!("{act:.1} {unit}"),
            });
        }
    }

    if !clamped.is_empty() {
        let summary = clamped
            .iter()
            .map(|c| format!("{} (req {} -> cap {})", c.field, c.requested, c.actual))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::HardwareClamped { summary, clamped });
    }

    Ok(())
}
