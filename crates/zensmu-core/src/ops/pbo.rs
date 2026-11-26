//! Precision Boost Overdrive (PBO) limits, scalar, Fmax boost limit, fused
//! factory limits, and platform flags.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::hw::profiles::{DualOpcode, OpcodeSpec};
use crate::ops::co::{check_mailbox_health, send_verified};
use crate::ops::{execute_ops, pm_table, verify_pm_limits, ExecOp};
use crate::safety::bounds;
use crate::transport::SmuTransport;
use crate::types::{Mailbox, PlannedOp, SmuArgs};
use serde::{Deserialize, Serialize};

// Safe hard and soft limits for desktop PBO tuning (unified with `crate::safety::bounds`).
pub const PPT_MIN_W: u32 = bounds::PPT_MIN_W;
pub const PPT_MAX_W: u32 = bounds::PPT_MAX_W;
pub const PPT_SOFT_MAX_W: u32 = bounds::PPT_SOFT_MAX_W;

pub const TDC_MIN_A: u32 = bounds::TDC_MIN_A;
pub const TDC_MAX_A: u32 = bounds::TDC_MAX_A;
pub const TDC_SOFT_MAX_A: u32 = bounds::TDC_SOFT_MAX_A;

pub const EDC_MIN_A: u32 = bounds::EDC_MIN_A;
pub const EDC_MAX_A: u32 = bounds::EDC_MAX_A;
pub const EDC_SOFT_MAX_A: u32 = bounds::EDC_SOFT_MAX_A;

pub const TCTL_MIN_C: u32 = bounds::TCTL_MIN_C;
pub const TCTL_MAX_C: u32 = bounds::TCTL_MAX_C;

pub const SCALAR_MIN: u32 = bounds::PBO_SCALAR_MIN;
pub const SCALAR_MAX: u32 = bounds::PBO_SCALAR_MAX;
pub const SCALAR_SOFT_MAX: u32 = bounds::PBO_SCALAR_SOFT_MAX;

pub const FMAX_MIN_MHZ: u32 = bounds::FMAX_MIN_MHZ;
pub const FMAX_MAX_MHZ: u32 = bounds::FMAX_MAX_MHZ;
pub const FMAX_SOFT_DELTA_MHZ: u32 = bounds::FMAX_SOFT_DELTA_MHZ;

/// Read-only fused factory power/current limits reported by the SMU (normalized to W / A).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusedLimits {
    pub ppt_w: Option<u32>,
    pub slow_w: Option<u32>,
    pub fast_w: Option<u32>,
    pub apu_slow_w: Option<u32>,
    pub vdd_tdc_a: Option<u32>,
    #[serde(default)]
    pub edc_a: Option<u32>,
    pub soc_tdc_a: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tdp_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tjmax_c: Option<u32>,
}

/// Read-only platform capability/mode flags reported by the SMU.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformFlags {
    pub overclockable: Option<bool>,
    pub expo_active: Option<bool>,
    pub ln2_mode: Option<bool>,
}

/// Aggregated read-only PBO status snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PboStatus {
    pub scalar: Option<f32>,
    pub fmax_mhz: Option<u32>,
    pub fused: FusedLimits,
    pub flags: PlatformFlags,
    pub core_perf_scores: Vec<(u32, u32)>,
}

/// Request to update one or more PBO parameters atomically (validated up front).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PboSetRequest {
    pub ppt_w: Option<u32>,
    pub tdc_a: Option<u32>,
    pub edc_a: Option<u32>,
    pub tctl_c: Option<u32>,
    pub scalar: Option<u32>,
    pub fmax_mhz: Option<u32>,
}

impl PboSetRequest {
    /// Returns true if no PBO parameters are set in this request.
    pub fn is_empty(&self) -> bool {
        self.ppt_w.is_none()
            && self.tdc_a.is_none()
            && self.edc_a.is_none()
            && self.tctl_c.is_none()
            && self.scalar.is_none()
            && self.fmax_mhz.is_none()
    }
}

fn query_optional(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    op: Option<OpcodeSpec>,
    args: SmuArgs,
) -> Result<Option<SmuArgs>> {
    let Some(spec) = op else {
        return Ok(None);
    };
    match send_verified(transport, hw, spec, args) {
        Ok(out) => Ok(Some(out)),
        Err(Error::SmuRejected { .. }) => Ok(None),
        Err(other) => Err(other),
    }
}

/// Query current PBO scalar, Fmax boost limit, fused factory limits, platform
/// flags, and per-core CPPC performance scores.
///
/// Any optional query that the BIOS locks or rejects is recorded as `None`
/// while preserving all values that succeeded.
pub fn get_pbo_status(transport: &dyn SmuTransport, hw: &HardwareContext) -> Result<PboStatus> {
    if hw.capabilities.test_message_rsmu.is_some() {
        check_mailbox_health(transport, hw, Mailbox::Rsmu)?;
    }

    let caps = &hw.capabilities;
    let scalar = query_optional(transport, hw, caps.get_pbo_scalar, [0; 6])?
        .map(|o| f32::from_bits(o[0]))
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v));

    let fmax_mhz = query_optional(transport, hw, caps.get_fmax, [0; 6])?
        .map(|o| o[0])
        .filter(|&mhz| mhz > 0);

    let norm_milli = |raw: u32| -> u32 {
        if raw >= 1000 {
            raw / 1000
        } else {
            raw
        }
    };

    let sustained = query_optional(transport, hw, caps.get_sustained_power_thm, [0; 6])?;
    let tdp_w = sustained.map(|o| (o[0] >> 16) & 0xFF).filter(|&w| w > 0);
    let tjmax_c = sustained
        .map(|o| o[0] & 0xFF)
        .filter(|c| (40..=115).contains(c));

    let fused = FusedLimits {
        ppt_w: query_optional(transport, hw, caps.get_fused_ppt, [0; 6])?.map(|o| norm_milli(o[0])),
        slow_w: query_optional(transport, hw, caps.get_fused_slow, [0; 6])?
            .map(|o| norm_milli(o[0])),
        fast_w: query_optional(transport, hw, caps.get_fused_fast, [0; 6])?
            .map(|o| norm_milli(o[0])),
        apu_slow_w: query_optional(transport, hw, caps.get_fused_apu_slow, [0; 6])?
            .map(|o| norm_milli(o[0])),
        vdd_tdc_a: query_optional(transport, hw, caps.get_fused_vdd_tdc, [0; 6])?
            .map(|o| norm_milli(o[0])),
        edc_a: query_optional(transport, hw, caps.get_fused_edc, [0; 6])?.map(|o| norm_milli(o[0])),
        soc_tdc_a: query_optional(transport, hw, caps.get_fused_soc_tdc, [0; 6])?
            .map(|o| norm_milli(o[0])),
        tdp_w,
        tjmax_c,
    };

    let flags = PlatformFlags {
        overclockable: query_optional(transport, hw, caps.get_is_overclockable, [0; 6])?
            .map(|o| o[0] != 0),
        expo_active: query_optional(transport, hw, caps.get_expo_active, [0; 6])?
            .map(|o| o[0] != 0),
        ln2_mode: query_optional(transport, hw, caps.get_ln2_mode, [0; 6])?.map(|o| o[0] != 0),
    };

    let mut core_perf_scores = Vec::new();
    if caps.get_perf_data.is_some() {
        for core in &hw.topology.cores {
            if let Some(out) = query_optional(
                transport,
                hw,
                caps.get_perf_data,
                [core.index, 0, 0, 0, 0, 0],
            )? {
                core_perf_scores.push((core.index, out[0]));
            }
        }
    }

    Ok(PboStatus {
        scalar,
        fmax_mhz,
        fused,
        flags,
        core_perf_scores,
    })
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

/// Apply requested PBO limits, scalar, and/or Fmax boost limit.
///
/// All fields are validated before any SMU command is sent. When `!dry_run`,
/// executes commands with verified RSP status and performs read-after-write
/// verification on scalar, Fmax, and (when PM table decoding is supported)
/// PPT/TDC/EDC/Tctl limits.
pub fn set_pbo(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    req: &PboSetRequest,
    allow_extreme: bool,
    dry_run: bool,
) -> Result<Vec<PlannedOp>> {
    let caps = &hw.capabilities;
    let codename = hw.codename.display_name().to_string();

    let mut planned = Vec::new();
    let mut exec_ops = Vec::new();

    if let Some(ppt_w) = req.ppt_w {
        let op = caps.set_ppt.ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_ppt",
            codename: codename.clone(),
        })?;
        bounds::validate_ppt_watts(ppt_w, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [ppt_w.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set PPT limit to {ppt_w} W"),
            format!("{ppt_w} W"),
        );
    }

    if let Some(tdc_a) = req.tdc_a {
        let op = caps.set_tdc.ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_tdc",
            codename: codename.clone(),
        })?;
        bounds::validate_tdc_amps(tdc_a, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [tdc_a.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set TDC limit to {tdc_a} A"),
            format!("{tdc_a} A"),
        );
    }

    if let Some(edc_a) = req.edc_a {
        let op = caps.set_edc.ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_edc",
            codename: codename.clone(),
        })?;
        bounds::validate_edc_amps(edc_a, allow_extreme)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [edc_a.saturating_mul(1000), 0, 0, 0, 0, 0],
            format!("Set EDC limit to {edc_a} A"),
            format!("{edc_a} A"),
        );
    }

    if let Some(tctl_c) = req.tctl_c {
        let op = caps.set_tctl.ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_tctl",
            codename: codename.clone(),
        })?;
        // Hard ceiling of 95 C (never overridden by allow_extreme).
        bounds::validate_tctl_celsius(tctl_c, false)?;
        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [tctl_c, 0, 0, 0, 0, 0],
            format!("Set Tctl thermal limit to {tctl_c} C"),
            format!("{tctl_c} C"),
        );
    }

    if let Some(scalar) = req.scalar {
        let op = caps
            .set_pbo_scalar
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "set_pbo_scalar",
                codename: codename.clone(),
            })?;
        bounds::validate_pbo_scalar_checked(scalar, allow_extreme)?;

        if let Some(en_op) = caps.set_pbo_en {
            let en_args = [1, 0, 0, 0, 0, 0];
            planned.push(PlannedOp {
                description: "Enable PBO scalar override".into(),
                mailbox: en_op.mailbox,
                opcode: en_op.opcode,
                args: en_args,
                current: None,
                target: "enabled".into(),
            });
            exec_ops.push(ExecOp::Single(en_op, en_args));
        }

        push_dual_op(
            &mut planned,
            &mut exec_ops,
            op,
            [scalar.saturating_mul(100), 0, 0, 0, 0, 0],
            format!("Set PBO scalar to {scalar}x"),
            format!("{scalar}x"),
        );
    }

    if let Some(fmax_mhz) = req.fmax_mhz {
        let op = caps.set_fmax.ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_fmax",
            codename: codename.clone(),
        })?;
        bounds::validate_fmax_hard_mhz(fmax_mhz)?;

        let mut current_fmax = None;
        if !dry_run {
            if let Some(get_fmax_op) = caps.get_fmax {
                if let Ok(out) = send_verified(transport, hw, get_fmax_op, [0; 6]) {
                    let cur = out[0];
                    if cur > 0 {
                        current_fmax = Some(cur);
                        if !allow_extreme && fmax_mhz > cur.saturating_add(FMAX_SOFT_DELTA_MHZ) {
                            return Err(Error::ExtremeValueBlocked {
                                field: "pbo.fmax_mhz".into(),
                                value: format!("{fmax_mhz} MHz"),
                                threshold: format!(
                                    "<= {} MHz (current {cur} + {FMAX_SOFT_DELTA_MHZ})",
                                    cur.saturating_add(FMAX_SOFT_DELTA_MHZ)
                                ),
                            });
                        }
                    }
                }
            }
        }

        let args = [fmax_mhz & 0xF_FFFF, 0, 0, 0, 0, 0];
        planned.push(PlannedOp {
            description: format!("Set Fmax boost limit to {fmax_mhz} MHz"),
            mailbox: op.mailbox,
            opcode: op.opcode,
            args,
            current: current_fmax.map(|c| format!("{c} MHz")),
            target: format!("{fmax_mhz} MHz"),
        });
        exec_ops.push(ExecOp::Single(op, args));
    }

    if dry_run || exec_ops.is_empty() {
        return Ok(planned);
    }

    execute_ops(transport, hw, exec_ops)?;

    // Read-after-write verification: scalar
    if let (Some(expected), Some(get_op)) = (req.scalar, caps.get_pbo_scalar) {
        let out = send_verified(transport, hw, get_op, [0; 6])?;
        let actual = f32::from_bits(out[0]);
        if !actual.is_finite() || (actual - expected as f32).abs() > 0.05 {
            return Err(Error::VerificationFailed {
                field: "pbo.scalar".into(),
                expected: format!("{expected}x"),
                actual: format!("{actual:.2}x"),
            });
        }
    }

    // Read-after-write verification: Fmax
    if let (Some(expected), Some(get_op)) = (req.fmax_mhz, caps.get_fmax) {
        let out = send_verified(transport, hw, get_op, [0; 6])?;
        let actual = out[0];
        if actual != expected {
            return Err(Error::VerificationFailed {
                field: "pbo.fmax_mhz".into(),
                expected: format!("{expected} MHz"),
                actual: format!("{actual} MHz"),
            });
        }
    }

    // Read-after-write verification: PPT/TDC/EDC/Tctl via PM table when supported & readable
    let need_pm_verify =
        req.ppt_w.is_some() || req.tdc_a.is_some() || req.edc_a.is_some() || req.tctl_c.is_some();
    if need_pm_verify && hw.driver.has_pm_table {
        if let Ok(telem) = pm_table::read_telemetry(transport, hw) {
            verify_pm_limits(&[
                ("pbo.ppt_w", req.ppt_w, telem.ppt_limit_w, "W"),
                ("pbo.tdc_a", req.tdc_a, telem.tdc_limit_a, "A"),
                ("pbo.edc_a", req.edc_a, telem.edc_limit_a, "A"),
                ("pbo.tctl_c", req.tctl_c, telem.tctl_limit_c, "C"),
            ])?;
        }
    }

    Ok(planned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SmuStatus;
    use crate::hw::profiles::capabilities_for;
    use crate::hw::topology::CpuTopology;
    use crate::transport::mock::MockTransport;
    use crate::types::Codename;

    fn mock_9700x_ctx() -> (MockTransport, HardwareContext) {
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

    #[test]
    fn get_pbo_status_reads_all_fields_and_tolerates_locked_optional() {
        let (transport, hw) = mock_9700x_ctx();
        let status = get_pbo_status(&transport, &hw).unwrap();
        assert_eq!(status.scalar, Some(1.0));
        assert_eq!(status.fmax_mhz, Some(5550));
        assert_eq!(status.fused.ppt_w, Some(88));
        assert_eq!(status.fused.vdd_tdc_a, Some(75));
        assert_eq!(status.fused.edc_a, Some(150));
        assert_eq!(status.fused.apu_slow_w, Some(65));
        assert_eq!(status.flags.overclockable, Some(true));
        assert_eq!(status.flags.expo_active, Some(true));
        assert_eq!(status.flags.ln2_mode, Some(false));
        assert_eq!(status.core_perf_scores.len(), 8);
        assert_eq!(status.core_perf_scores[0], (0, 200));

        // Lock EXPO query in BIOS -> gracefully returns None without failing
        transport.set_reject_opcode(0x35, SmuStatus::CmdRejectedPrereq);
        let status2 = get_pbo_status(&transport, &hw).unwrap();
        assert_eq!(status2.flags.expo_active, None);
        assert_eq!(status2.fmax_mhz, Some(5550));
    }

    #[test]
    fn set_pbo_validates_bounds_and_verifies_via_pm_table() {
        let (transport, hw) = mock_9700x_ctx();

        // Hard Tctl ceiling 95C cannot be bypassed even with allow_extreme
        let bad_tctl = PboSetRequest {
            tctl_c: Some(96),
            ..Default::default()
        };
        assert!(matches!(
            set_pbo(&transport, &hw, &bad_tctl, true, false),
            Err(Error::OutOfBounds { .. })
        ));
        assert!(transport.history().is_empty());

        // Soft PPT limit (> 230W) requires allow_extreme
        let high_ppt = PboSetRequest {
            ppt_w: Some(250),
            ..Default::default()
        };
        assert!(matches!(
            set_pbo(&transport, &hw, &high_ppt, false, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        assert!(transport.history().is_empty());

        // Fmax > current + 200 MHz (5550 + 200 = 5750) blocked without allow_extreme
        let high_fmax = PboSetRequest {
            fmax_mhz: Some(5800),
            ..Default::default()
        };
        assert!(matches!(
            set_pbo(&transport, &hw, &high_fmax, false, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));

        // Valid full PBO update
        let req = PboSetRequest {
            ppt_w: Some(142),
            tdc_a: Some(110),
            edc_a: Some(170),
            tctl_c: Some(85),
            scalar: Some(2),
            fmax_mhz: Some(5700),
        };
        let ops = set_pbo(&transport, &hw, &req, false, false).unwrap();
        assert_eq!(ops.len(), 6);

        let telem = pm_table::read_telemetry(&transport, &hw).unwrap();
        assert_eq!(telem.ppt_limit_w, Some(142.0));
        assert_eq!(telem.tdc_limit_a, Some(110.0));
        assert_eq!(telem.edc_limit_a, Some(170.0));
        assert_eq!(telem.tctl_limit_c, Some(85.0));

        let status = get_pbo_status(&transport, &hw).unwrap();
        assert_eq!(status.scalar, Some(2.0));
        assert_eq!(status.fmax_mhz, Some(5700));
    }

    #[test]
    fn set_pbo_dry_run_performs_zero_writes() {
        let (transport, hw) = mock_9700x_ctx();
        let req = PboSetRequest {
            ppt_w: Some(120),
            tdc_a: Some(90),
            edc_a: Some(150),
            tctl_c: Some(85),
            scalar: Some(3),
            fmax_mhz: Some(5600),
        };
        let ops = set_pbo(&transport, &hw, &req, false, true).unwrap();
        assert_eq!(ops.len(), 6);
        assert!(transport.history().is_empty());
    }

    #[test]
    fn set_pbo_catches_false_ok_bug() {
        let (transport, hw) = mock_9700x_ctx();
        transport.set_simulate_false_ok_bug(true);
        transport.set_reject_opcode(0x56, SmuStatus::CmdRejectedPrereq);
        transport.set_reject_opcode(0x3E, SmuStatus::CmdRejectedPrereq);

        let req = PboSetRequest {
            ppt_w: Some(120),
            ..Default::default()
        };
        let err = set_pbo(&transport, &hw, &req, false, false).unwrap_err();
        assert!(matches!(err, Error::SmuRejected { .. }));
    }

    #[test]
    fn get_pbo_status_decodes_sustained_power_and_thm_limit() {
        let (transport, hw) = mock_9700x_ctx();
        // Verified live on Vermeer 5900XT: MP1 0x23 -> 0x0069005A -> 105 W TDP, 90 C TjMax
        transport.set_sustained_power_thm_raw(0x0069_005A);
        let status = get_pbo_status(&transport, &hw).unwrap();
        assert_eq!(status.fused.tdp_w, Some(105));
        assert_eq!(status.fused.tjmax_c, Some(90));

        // Zero TDP and out-of-range TjMax are filtered to None
        transport.set_sustained_power_thm_raw(0x0000_0010);
        let status2 = get_pbo_status(&transport, &hw).unwrap();
        assert_eq!(status2.fused.tdp_w, None);
        assert_eq!(status2.fused.tjmax_c, None);
    }

    #[test]
    fn set_pbo_reports_all_clamped_vrm_limits() {
        let (transport, hw) = mock_9700x_ctx();
        transport.set_clamp_tdc_a(Some(100));
        transport.set_clamp_edc_a(Some(120));

        let req = PboSetRequest {
            ppt_w: Some(142),
            tdc_a: Some(140),
            edc_a: Some(220),
            ..Default::default()
        };
        let err = set_pbo(&transport, &hw, &req, false, false).unwrap_err();
        match err {
            Error::HardwareClamped { summary, clamped } => {
                assert_eq!(clamped.len(), 2);
                assert_eq!(clamped[0].field, "pbo.tdc_a");
                assert_eq!(clamped[0].requested, "140 A");
                assert_eq!(clamped[0].actual, "100 A");
                assert_eq!(clamped[0].actual_value, 100);
                assert_eq!(clamped[0].unit, "A");

                assert_eq!(clamped[1].field, "pbo.edc_a");
                assert_eq!(clamped[1].requested, "220 A");
                assert_eq!(clamped[1].actual, "120 A");
                assert_eq!(clamped[1].actual_value, 120);
                assert_eq!(clamped[1].unit, "A");

                assert_eq!(
                    summary,
                    "pbo.tdc_a (req 140 A -> cap 100 A), pbo.edc_a (req 220 A -> cap 120 A)"
                );
            }
            other => panic!("expected Error::HardwareClamped, got {other:?}"),
        }
    }
}
