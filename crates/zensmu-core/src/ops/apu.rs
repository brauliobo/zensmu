//! Mobile APU Dynamic Power and Thermal Configuration (DPTC) SMU controls.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::hw::profiles::DualOpcode;
use crate::ops::{execute_ops, pm_table, verify_pm_limits, ExecOp};
use crate::safety::bounds;
use crate::transport::SmuTransport;
use crate::types::{ApuPowerMode, PlannedOp, SmuArgs};
use serde::{Deserialize, Serialize};

// Conservative mobile APU bounds (unified with `crate::safety::bounds`).
pub const APU_POWER_MIN_W: u32 = bounds::APU_POWER_MIN_W;
pub const APU_POWER_MAX_W: u32 = bounds::APU_POWER_MAX_W;
pub const APU_POWER_SOFT_MAX_W: u32 = bounds::APU_POWER_SOFT_MAX_W;
pub const APU_FAST_POWER_SOFT_MAX_W: u32 = bounds::APU_FAST_POWER_SOFT_MAX_W;

pub const APU_TIME_MIN_S: u32 = bounds::APU_TIME_MIN_S;
pub const APU_TIME_MAX_S: u32 = bounds::APU_TIME_MAX_S;

pub const APU_SOC_TDC_MIN_A: u32 = bounds::APU_SOC_TDC_MIN_A;
pub const APU_SOC_TDC_MAX_A: u32 = bounds::APU_SOC_TDC_MAX_A;
pub const APU_SOC_TDC_SOFT_MAX_A: u32 = bounds::APU_SOC_TDC_SOFT_MAX_A;

pub const APU_SOC_EDC_MIN_A: u32 = bounds::APU_SOC_EDC_MIN_A;
pub const APU_SOC_EDC_MAX_A: u32 = bounds::APU_SOC_EDC_MAX_A;
pub const APU_SOC_EDC_SOFT_MAX_A: u32 = bounds::APU_SOC_EDC_SOFT_MAX_A;

pub const APU_SKIN_TEMP_MIN_C: u32 = bounds::SKIN_TEMP_MIN_C;
pub const APU_SKIN_TEMP_MAX_C: u32 = bounds::SKIN_TEMP_MAX_C;
pub const APU_SKIN_TEMP_SOFT_MAX_C: u32 = bounds::SKIN_TEMP_SOFT_MAX_C;

/// Request to update one or more mobile APU DPTC parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApuSetRequest {
    pub stapm_w: Option<u32>,
    pub stapm_time_s: Option<u32>,
    pub fast_w: Option<u32>,
    pub slow_w: Option<u32>,
    pub slow_time_s: Option<u32>,
    pub apu_slow_w: Option<u32>,
    pub tdc_soc_a: Option<u32>,
    pub edc_soc_a: Option<u32>,
    pub apu_skin_temp_c: Option<u32>,
    pub dgpu_skin_temp_c: Option<u32>,
    pub power_mode: Option<ApuPowerMode>,
}

impl ApuSetRequest {
    /// Returns true if no APU limit or power-mode fields are set.
    pub fn is_empty(&self) -> bool {
        self.stapm_w.is_none()
            && self.stapm_time_s.is_none()
            && self.fast_w.is_none()
            && self.slow_w.is_none()
            && self.slow_time_s.is_none()
            && self.apu_slow_w.is_none()
            && self.tdc_soc_a.is_none()
            && self.edc_soc_a.is_none()
            && self.apu_skin_temp_c.is_none()
            && self.dgpu_skin_temp_c.is_none()
            && self.power_mode.is_none()
    }
}

fn push_dual_op(
    planned: &mut Vec<PlannedOp>,
    exec_ops: &mut Vec<ExecOp>,
    op: DualOpcode,
    args: SmuArgs,
    description: String,
    target: String,
) {
    planned.push(PlannedOp {
        description,
        mailbox: op.primary.mailbox,
        opcode: op.primary.opcode,
        args,
        current: None,
        target,
    });
    exec_ops.push(ExecOp::Dual(op, args));
}

/// Apply mobile APU DPTC power, current, thermal skin, and power-mode settings.
///
/// All requested fields and platform capabilities are validated BEFORE any SMU
/// command is sent. When `dry_run` is true, returns the planned operations
/// without touching hardware.
pub fn set_apu(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    req: &ApuSetRequest,
    allow_extreme: bool,
    dry_run: bool,
) -> Result<Vec<PlannedOp>> {
    let caps = &hw.capabilities;
    let codename = hw.codename.display_name().to_string();

    let mut planned = Vec::new();
    let mut exec_ops = Vec::new();

    if let Some(stapm_w) = req.stapm_w {
        let op = caps
            .set_stapm_limit
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_stapm_limit",
                codename: codename.clone(),
            })?;
        bounds::validate_apu_watts("apu.stapm_w", stapm_w, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [stapm_w.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set APU STAPM limit to {stapm_w} W"),
            format!("{stapm_w} W"),
        );
    }

    if let Some(stapm_time_s) = req.stapm_time_s {
        let op = caps
            .set_stapm_time
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_stapm_time",
                codename: codename.clone(),
            })?;
        bounds::validate_apu_time_secs("apu.stapm_time_s", stapm_time_s)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [stapm_time_s, 0, 0, 0, 0, 0],
            format!("Set APU STAPM time constant to {stapm_time_s} s"),
            format!("{stapm_time_s} s"),
        );
    }

    if let Some(fast_w) = req.fast_w {
        if !hw.codename.is_apu() {
            return Err(Error::UnsupportedCapability {
                capability: "set_apu_fast_limit",
                codename: codename.clone(),
            });
        }
        let op = caps.set_ppt.ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_apu_fast_limit",
            codename: codename.clone(),
        })?;
        bounds::validate_apu_fast_watts("apu.fast_w", fast_w, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [fast_w.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set APU Fast PPT limit to {fast_w} W"),
            format!("{fast_w} W"),
        );
    }

    if let Some(slow_w) = req.slow_w {
        if !hw.codename.is_apu() {
            return Err(Error::UnsupportedCapability {
                capability: "set_slow_limit",
                codename: codename.clone(),
            });
        }
        let op = caps
            .set_slow_limit
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_slow_limit",
                codename: codename.clone(),
            })?;
        bounds::validate_apu_watts("apu.slow_w", slow_w, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [slow_w.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set APU Slow PPT limit to {slow_w} W"),
            format!("{slow_w} W"),
        );
    }

    if let Some(slow_time_s) = req.slow_time_s {
        let op = caps
            .set_slow_time
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_slow_time",
                codename: codename.clone(),
            })?;
        bounds::validate_apu_time_secs("apu.slow_time_s", slow_time_s)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [slow_time_s, 0, 0, 0, 0, 0],
            format!("Set APU Slow PPT time constant to {slow_time_s} s"),
            format!("{slow_time_s} s"),
        );
    }

    if let Some(apu_slow_w) = req.apu_slow_w {
        let op = caps
            .set_apu_slow_limit
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_apu_slow_limit",
                codename: codename.clone(),
            })?;
        bounds::validate_apu_watts("apu.apu_slow_w", apu_slow_w, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [apu_slow_w.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set APU-only Slow PPT limit to {apu_slow_w} W"),
            format!("{apu_slow_w} W"),
        );
    }

    if let Some(tdc_soc_a) = req.tdc_soc_a {
        let op = caps
            .set_tdc_soc
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_tdc_soc",
                codename: codename.clone(),
            })?;
        bounds::validate_apu_soc_tdc_amps("apu.tdc_soc_a", tdc_soc_a, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [tdc_soc_a.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set APU SoC TDC limit to {tdc_soc_a} A"),
            format!("{tdc_soc_a} A"),
        );
    }

    if let Some(edc_soc_a) = req.edc_soc_a {
        let op = caps
            .set_edc_soc
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_edc_soc",
                codename: codename.clone(),
            })?;
        bounds::validate_apu_soc_edc_amps("apu.edc_soc_a", edc_soc_a, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [edc_soc_a.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set APU SoC EDC limit to {edc_soc_a} A"),
            format!("{edc_soc_a} A"),
        );
    }

    if let Some(apu_skin_temp_c) = req.apu_skin_temp_c {
        let op = caps
            .set_apu_skin_temp
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_apu_skin_temp",
                codename: codename.clone(),
            })?;
        bounds::validate_skin_temp_celsius("apu.apu_skin_temp_c", apu_skin_temp_c, allow_extreme)?;
        // AMD APU skin temp uses Q8.8 fixed-point encoding (temp_c << 8).
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [apu_skin_temp_c << 8, 0, 0, 0, 0, 0],
            format!("Set APU skin temperature limit to {apu_skin_temp_c} C"),
            format!("{apu_skin_temp_c} C"),
        );
    }

    if let Some(dgpu_skin_temp_c) = req.dgpu_skin_temp_c {
        let op = caps
            .set_dgpu_skin_temp
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_dgpu_skin_temp",
                codename: codename.clone(),
            })?;
        bounds::validate_skin_temp_celsius(
            "apu.dgpu_skin_temp_c",
            dgpu_skin_temp_c,
            allow_extreme,
        )?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [dgpu_skin_temp_c << 8, 0, 0, 0, 0, 0],
            format!("Set dGPU skin temperature limit to {dgpu_skin_temp_c} C"),
            format!("{dgpu_skin_temp_c} C"),
        );
    }

    if let Some(mode) = req.power_mode {
        let (op, cap_name, label) = match mode {
            ApuPowerMode::MaxPerformance => (
                caps.set_max_performance,
                "set_max_performance",
                "max-performance",
            ),
            ApuPowerMode::PowerSaving => {
                (caps.set_power_saving, "set_power_saving", "power-saving")
            }
        };
        let op = op.ok_or_else(|| Error::UnsupportedCapability {
            capability: cap_name,
            codename: codename.clone(),
        })?;
        let args = [0; 6];
        planned.push(PlannedOp {
            description: format!("Set APU power mode to {label}"),
            mailbox: op.mailbox,
            opcode: op.opcode,
            args,
            current: None,
            target: label.into(),
        });
        exec_ops.push(ExecOp::Single(op, args));
    }

    if dry_run || exec_ops.is_empty() {
        return Ok(planned);
    }

    execute_ops(transport, hw, exec_ops)?;

    let need_pm_verify = req.stapm_w.is_some()
        || req.fast_w.is_some()
        || req.slow_w.is_some()
        || req.apu_slow_w.is_some()
        || req.tdc_soc_a.is_some()
        || req.edc_soc_a.is_some()
        || req.apu_skin_temp_c.is_some()
        || req.dgpu_skin_temp_c.is_some();
    if need_pm_verify && hw.driver.has_pm_table {
        if let Ok(telem) = pm_table::read_telemetry(transport, hw) {
            verify_pm_limits(&[
                ("apu.stapm_w", req.stapm_w, telem.stapm_limit_w, "W"),
                ("apu.fast_w", req.fast_w, telem.ppt_limit_w, "W"),
                ("apu.slow_w", req.slow_w, telem.slow_limit_w, "W"),
                (
                    "apu.apu_slow_w",
                    req.apu_slow_w,
                    telem.apu_slow_limit_w,
                    "W",
                ),
                ("apu.tdc_soc_a", req.tdc_soc_a, telem.tdc_soc_limit_a, "A"),
                ("apu.edc_soc_a", req.edc_soc_a, telem.edc_soc_limit_a, "A"),
                (
                    "apu.apu_skin_temp_c",
                    req.apu_skin_temp_c,
                    telem.apu_skin_temp_limit_c,
                    "C",
                ),
                (
                    "apu.dgpu_skin_temp_c",
                    req.dgpu_skin_temp_c,
                    telem.dgpu_skin_temp_limit_c,
                    "C",
                ),
            ])?;
        }
    }

    Ok(planned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::profiles::capabilities_for;
    use crate::hw::topology::CpuTopology;
    use crate::transport::mock::MockTransport;
    use crate::types::Codename;

    fn mock_desktop_ctx() -> (MockTransport, HardwareContext) {
        let transport = MockTransport::granite_ridge_9700x();
        let driver = transport.read_info().unwrap();
        let hw = HardwareContext {
            driver,
            codename: Codename::GraniteRidge,
            cpuid: None,
            topology: CpuTopology::synthetic(1, 8, 2),
            capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
        };
        (transport, hw)
    }

    fn mock_phoenix_ctx() -> (MockTransport, HardwareContext) {
        let transport = MockTransport::phoenix_apu();
        let driver = transport.read_info().unwrap();
        let hw = HardwareContext {
            driver,
            codename: Codename::Phoenix,
            cpuid: None,
            topology: CpuTopology::synthetic(1, 8, 2),
            capabilities: capabilities_for(Codename::Phoenix).unwrap(),
        };
        (transport, hw)
    }

    #[test]
    fn desktop_rejects_apu_only_controls() {
        let (transport, hw) = mock_desktop_ctx();
        let req = ApuSetRequest {
            stapm_w: Some(45),
            ..Default::default()
        };
        assert!(matches!(
            set_apu(&transport, &hw, &req, false, true),
            Err(Error::UnsupportedCapability {
                capability: "set_stapm_limit",
                ..
            })
        ));
        assert!(transport.history().is_empty());
    }

    #[test]
    fn phoenix_apu_validates_bounds_and_dry_run_encodes_properly() {
        let (transport, hw) = mock_phoenix_ctx();

        // Out of hard bounds
        let bad_power = ApuSetRequest {
            stapm_w: Some(200),
            ..Default::default()
        };
        assert!(matches!(
            set_apu(&transport, &hw, &bad_power, true, true),
            Err(Error::OutOfBounds { .. })
        ));

        let bad_skin = ApuSetRequest {
            apu_skin_temp_c: Some(96),
            ..Default::default()
        };
        assert!(matches!(
            set_apu(&transport, &hw, &bad_skin, true, true),
            Err(Error::OutOfBounds { .. })
        ));

        let bad_time = ApuSetRequest {
            stapm_time_s: Some(0),
            ..Default::default()
        };
        assert!(matches!(
            set_apu(&transport, &hw, &bad_time, true, true),
            Err(Error::OutOfBounds { .. })
        ));

        // Soft max requires allow_extreme
        let soft_skin = ApuSetRequest {
            apu_skin_temp_c: Some(85),
            ..Default::default()
        };
        assert!(matches!(
            set_apu(&transport, &hw, &soft_skin, false, true),
            Err(Error::ExtremeValueBlocked { .. })
        ));

        // Valid full APU request in dry_run performs zero writes and encodes units
        let req = ApuSetRequest {
            stapm_w: Some(45),
            stapm_time_s: Some(120),
            fast_w: Some(54),
            slow_w: Some(45),
            slow_time_s: Some(10),
            apu_slow_w: Some(35),
            tdc_soc_a: Some(30),
            edc_soc_a: Some(50),
            apu_skin_temp_c: Some(55),
            dgpu_skin_temp_c: Some(60),
            power_mode: Some(ApuPowerMode::MaxPerformance),
        };
        let ops = set_apu(&transport, &hw, &req, false, true).unwrap();
        assert_eq!(ops.len(), 11);
        assert_eq!(ops[0].args[0], 45_000); // STAPM mW
        assert_eq!(ops[1].args[0], 120); // STAPM time s
        assert_eq!(ops[8].args[0], 55 << 8); // APU skin temp Q8.8
        assert_eq!(ops[9].args[0], 60 << 8); // dGPU skin temp Q8.8
        assert!(transport.history().is_empty());
    }

    #[test]
    fn phoenix_apu_detects_pm_table_stapm_clamp() {
        let (transport, hw) = mock_phoenix_ctx();
        transport.set_clamp_stapm_w(Some(30));

        let req = ApuSetRequest {
            stapm_w: Some(45),
            ..Default::default()
        };
        let err = set_apu(&transport, &hw, &req, false, false).unwrap_err();
        match err {
            Error::HardwareClamped { summary, clamped } => {
                assert_eq!(clamped.len(), 1);
                assert_eq!(clamped[0].field, "apu.stapm_w");
                assert_eq!(clamped[0].requested, "45 W");
                assert_eq!(clamped[0].actual, "30 W");
                assert_eq!(clamped[0].actual_value, 30);
                assert_eq!(clamped[0].unit, "W");
                assert!(summary.contains("apu.stapm_w (req 45 W -> cap 30 W)"));
            }
            other => panic!("expected Error::HardwareClamped, got {other:?}"),
        }
    }
}
