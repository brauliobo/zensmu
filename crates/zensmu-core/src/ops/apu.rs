//! Mobile APU Dynamic Power and Thermal Configuration (DPTC) SMU controls.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::hw::profiles::{DualOpcode, OpcodeSpec};
use crate::ops::co::{check_mailbox_health, send_dual_verified, send_verified};
use crate::transport::SmuTransport;
use crate::types::{ApuPowerMode, Mailbox, PlannedOp, SmuArgs};
use serde::{Deserialize, Serialize};

// Conservative mobile APU bounds.
pub const APU_POWER_MIN_W: u32 = 5;
pub const APU_POWER_MAX_W: u32 = 150;
pub const APU_POWER_SOFT_MAX_W: u32 = 100;
pub const APU_FAST_POWER_SOFT_MAX_W: u32 = 120;

pub const APU_TIME_MIN_S: u32 = 1;
pub const APU_TIME_MAX_S: u32 = 600;

pub const APU_SOC_TDC_MIN_A: u32 = 5;
pub const APU_SOC_TDC_MAX_A: u32 = 100;
pub const APU_SOC_TDC_SOFT_MAX_A: u32 = 60;

pub const APU_SOC_EDC_MIN_A: u32 = 5;
pub const APU_SOC_EDC_MAX_A: u32 = 150;
pub const APU_SOC_EDC_SOFT_MAX_A: u32 = 90;

pub const APU_SKIN_TEMP_MIN_C: u32 = 30;
pub const APU_SKIN_TEMP_MAX_C: u32 = 95;
pub const APU_SKIN_TEMP_SOFT_MAX_C: u32 = 80;

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

fn validate_range(
    field: &str,
    value: u32,
    min: u32,
    max: u32,
    soft_max: Option<u32>,
    allow_extreme: bool,
) -> Result<()> {
    if value < min || value > max {
        return Err(Error::OutOfBounds {
            field: field.into(),
            value: value.to_string(),
            bounds: format!("{min}..={max}"),
        });
    }
    if let Some(soft) = soft_max {
        if value > soft && !allow_extreme {
            return Err(Error::ExtremeValueBlocked {
                field: field.into(),
                value: value.to_string(),
                threshold: format!("<= {soft}"),
            });
        }
    }
    Ok(())
}

enum ApuExecOp {
    Single(OpcodeSpec, SmuArgs),
    Dual(DualOpcode, SmuArgs),
}

impl ApuExecOp {
    fn primary_mailbox(&self) -> Mailbox {
        match self {
            Self::Single(s, _) => s.mailbox,
            Self::Dual(d, _) => d.primary.mailbox,
        }
    }
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
        validate_range(
            "stapm_w",
            stapm_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
            Some(APU_POWER_SOFT_MAX_W),
            allow_extreme,
        )?;
        let args = [stapm_w.saturating_mul(1000), 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU STAPM limit to {stapm_w} W"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{stapm_w} W"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
    }

    if let Some(stapm_time_s) = req.stapm_time_s {
        let op = caps
            .set_stapm_time
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_stapm_time",
                codename: codename.clone(),
            })?;
        validate_range(
            "stapm_time_s",
            stapm_time_s,
            APU_TIME_MIN_S,
            APU_TIME_MAX_S,
            None,
            allow_extreme,
        )?;
        let args = [stapm_time_s, 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU STAPM time constant to {stapm_time_s} s"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{stapm_time_s} s"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
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
        validate_range(
            "fast_w",
            fast_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
            Some(APU_FAST_POWER_SOFT_MAX_W),
            allow_extreme,
        )?;
        let args = [fast_w.saturating_mul(1000), 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU Fast PPT limit to {fast_w} W"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{fast_w} W"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
    }

    if let Some(slow_w) = req.slow_w {
        let op = caps
            .set_slow_limit
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_slow_limit",
                codename: codename.clone(),
            })?;
        validate_range(
            "slow_w",
            slow_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
            Some(APU_POWER_SOFT_MAX_W),
            allow_extreme,
        )?;
        let args = [slow_w.saturating_mul(1000), 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU Slow PPT limit to {slow_w} W"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{slow_w} W"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
    }

    if let Some(slow_time_s) = req.slow_time_s {
        let op = caps
            .set_slow_time
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_slow_time",
                codename: codename.clone(),
            })?;
        validate_range(
            "slow_time_s",
            slow_time_s,
            APU_TIME_MIN_S,
            APU_TIME_MAX_S,
            None,
            allow_extreme,
        )?;
        let args = [slow_time_s, 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU Slow PPT time constant to {slow_time_s} s"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{slow_time_s} s"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
    }

    if let Some(apu_slow_w) = req.apu_slow_w {
        let op = caps
            .set_apu_slow_limit
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_apu_slow_limit",
                codename: codename.clone(),
            })?;
        validate_range(
            "apu_slow_w",
            apu_slow_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
            Some(APU_POWER_SOFT_MAX_W),
            allow_extreme,
        )?;
        let args = [apu_slow_w.saturating_mul(1000), 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU-only Slow PPT limit to {apu_slow_w} W"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{apu_slow_w} W"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
    }

    if let Some(tdc_soc_a) = req.tdc_soc_a {
        let op = caps
            .set_tdc_soc
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_tdc_soc",
                codename: codename.clone(),
            })?;
        validate_range(
            "tdc_soc_a",
            tdc_soc_a,
            APU_SOC_TDC_MIN_A,
            APU_SOC_TDC_MAX_A,
            Some(APU_SOC_TDC_SOFT_MAX_A),
            allow_extreme,
        )?;
        let args = [tdc_soc_a.saturating_mul(1000), 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU SoC TDC limit to {tdc_soc_a} A"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{tdc_soc_a} A"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
    }

    if let Some(edc_soc_a) = req.edc_soc_a {
        let op = caps
            .set_edc_soc
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_edc_soc",
                codename: codename.clone(),
            })?;
        validate_range(
            "edc_soc_a",
            edc_soc_a,
            APU_SOC_EDC_MIN_A,
            APU_SOC_EDC_MAX_A,
            Some(APU_SOC_EDC_SOFT_MAX_A),
            allow_extreme,
        )?;
        let args = [edc_soc_a.saturating_mul(1000), 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU SoC EDC limit to {edc_soc_a} A"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{edc_soc_a} A"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
    }

    if let Some(apu_skin_temp_c) = req.apu_skin_temp_c {
        let op = caps
            .set_apu_skin_temp
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_apu_skin_temp",
                codename: codename.clone(),
            })?;
        validate_range(
            "apu_skin_temp_c",
            apu_skin_temp_c,
            APU_SKIN_TEMP_MIN_C,
            APU_SKIN_TEMP_MAX_C,
            Some(APU_SKIN_TEMP_SOFT_MAX_C),
            allow_extreme,
        )?;
        // AMD APU skin temp uses Q8.8 fixed-point encoding (temp_c << 8).
        let args = [apu_skin_temp_c << 8, 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set APU skin temperature limit to {apu_skin_temp_c} C"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{apu_skin_temp_c} C"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
    }

    if let Some(dgpu_skin_temp_c) = req.dgpu_skin_temp_c {
        let op = caps
            .set_dgpu_skin_temp
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_dgpu_skin_temp",
                codename: codename.clone(),
            })?;
        validate_range(
            "dgpu_skin_temp_c",
            dgpu_skin_temp_c,
            APU_SKIN_TEMP_MIN_C,
            APU_SKIN_TEMP_MAX_C,
            Some(APU_SKIN_TEMP_SOFT_MAX_C),
            allow_extreme,
        )?;
        let args = [dgpu_skin_temp_c << 8, 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set dGPU skin temperature limit to {dgpu_skin_temp_c} C"),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args,
            current: None,
            target: format!("{dgpu_skin_temp_c} C"),
        });
        exec_ops.push(ApuExecOp::Dual(op, args));
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
        exec_ops.push(ApuExecOp::Single(op, args));
    }

    if dry_run || exec_ops.is_empty() {
        return Ok(planned);
    }

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
            ApuExecOp::Single(spec, args) => {
                send_verified(transport, hw, spec, args)?;
            }
            ApuExecOp::Dual(dual, args) => {
                send_dual_verified(transport, hw, dual, args)?;
            }
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
        let transport = MockTransport::granite_ridge_9700x();
        let mut driver = transport.read_info().unwrap();
        driver.codename_id = Codename::Phoenix as u32;
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
}
