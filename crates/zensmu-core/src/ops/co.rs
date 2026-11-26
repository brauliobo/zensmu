//! Curve Optimizer (per-core, all-core, and iGPU) SMU operations.
//!
//! All write operations validate generation-specific bounds, check mailbox health,
//! verify true hardware RSP status via read-only SMN reads, and perform
//! read-after-write verification whenever readback is supported.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::hw::profiles::{DualOpcode, OpcodeSpec};
use crate::transport::SmuTransport;
use crate::types::{Mailbox, PlannedOp, SmuArgs};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Sentinel payload sent to SMU `TestMessage` (`0x01`) for mailbox health checks.
pub const HEALTH_CHECK_ARG: u32 = 0x1337;
/// Expected response from SMU `TestMessage` (`0x01`) (`arg0 + 1`).
pub const HEALTH_CHECK_EXPECTED: u32 = 0x1338;

/// Conservative iGPU Curve Optimizer bounds.
pub const IGPU_CO_MIN: i32 = -30;
pub const IGPU_CO_MAX: i32 = 30;
pub const IGPU_CO_SOFT_MIN: i32 = -25;

/// Curve Optimizer margin for a single physical core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreCoMargin {
    pub core_index: u32,
    pub ccd: u32,
    pub core_in_ccd: u32,
    pub linux_cpus: Vec<u32>,
    pub margin: i16,
}

/// Snapshot of current Curve Optimizer margins across all physical cores (and optional iGPU).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoSnapshot {
    pub cores: Vec<CoreCoMargin>,
    pub igpu: Option<i16>,
}

/// Verify that an SMU mailbox is alive and responding accurately by sending
/// `TestMessage` (`0x01`) with `arg0 = 0x1337` and asserting `args[0] == 0x1338`.
pub fn check_mailbox_health(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    mb: Mailbox,
) -> Result<()> {
    let (op, cap_name) = match mb {
        Mailbox::Rsmu => (hw.capabilities.test_message_rsmu, "test_message_rsmu"),
        Mailbox::Mp1 => (hw.capabilities.test_message_mp1, "test_message_mp1"),
    };
    let op = op.ok_or_else(|| Error::UnsupportedCapability {
        capability: cap_name,
        codename: hw.codename.display_name().to_string(),
    })?;

    let out = send_verified(transport, hw, op, [HEALTH_CHECK_ARG, 0, 0, 0, 0, 0])?;
    if out[0] != HEALTH_CHECK_EXPECTED {
        return Err(Error::MailboxUnhealthy {
            mailbox: mb.as_str(),
            expected: HEALTH_CHECK_EXPECTED,
            actual: out[0],
        });
    }
    Ok(())
}

/// Execute a single SMU opcode, passing the platform's read-only SMN response
/// register address so `ryzen_smu`'s `smu.c:213` false-OK bug cannot hide rejections.
pub fn send_verified(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    op: OpcodeSpec,
    args: SmuArgs,
) -> Result<SmuArgs> {
    let rsp_addr = hw.capabilities.rsp_addr(op.mailbox);
    let resp = transport.send_command(op.mailbox, op.opcode, args, rsp_addr)?;
    let status = resp.effective_status();
    if !status.is_ok() {
        return Err(Error::SmuRejected {
            mailbox: op.mailbox.as_str(),
            opcode: op.opcode,
            status: status.label().to_string(),
        });
    }
    Ok(resp.args)
}

/// Execute a primary SMU opcode and, if the SMU rejects it and a fallback opcode
/// is configured, retry on the fallback mailbox/opcode.
pub fn send_dual_verified(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    op: DualOpcode,
    args: SmuArgs,
) -> Result<SmuArgs> {
    match send_verified(transport, hw, op.primary, args) {
        Ok(out) => Ok(out),
        Err(Error::SmuRejected { .. }) if op.fallback.is_some() => {
            let fallback = op.fallback.expect("checked is_some");
            send_verified(transport, hw, fallback, args)
        }
        Err(err) => Err(err),
    }
}

/// Encode a signed Curve Optimizer margin as two's complement in the lower 16 bits.
pub fn encode_psm_margin(margin: i32) -> u32 {
    (margin as i16 as u16) as u32
}

/// Decode a signed 16-bit Curve Optimizer margin from the lower 16 bits of an SMU register.
pub fn decode_psm_margin(raw: u32) -> i16 {
    (raw & 0xFFFF) as u16 as i16
}

/// Validate a CPU core Curve Optimizer margin against the CPU's generation-specific
/// hard and soft bounds.
pub fn validate_co_margin(
    hw: &HardwareContext,
    margin: i32,
    allow_extreme: bool,
) -> Result<i16> {
    let bounds = hw
        .capabilities
        .co_bounds
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "curve_optimizer",
            codename: hw.codename.display_name().to_string(),
        })?;

    if margin < bounds.min || margin > bounds.max {
        return Err(Error::OutOfBounds {
            field: "co_margin".into(),
            value: margin.to_string(),
            bounds: format!("{}..={}", bounds.min, bounds.max),
        });
    }

    if margin < bounds.soft_min && !allow_extreme {
        return Err(Error::ExtremeValueBlocked {
            field: "co_margin".into(),
            value: margin.to_string(),
            threshold: format!(">= {}", bounds.soft_min),
        });
    }

    Ok(margin as i16)
}

fn validate_igpu_co_margin(margin: i32, allow_extreme: bool) -> Result<i16> {
    if !(IGPU_CO_MIN..=IGPU_CO_MAX).contains(&margin) {
        return Err(Error::OutOfBounds {
            field: "igpu_co".into(),
            value: margin.to_string(),
            bounds: format!("{IGPU_CO_MIN}..={IGPU_CO_MAX}"),
        });
    }
    if margin < IGPU_CO_SOFT_MIN && !allow_extreme {
        return Err(Error::ExtremeValueBlocked {
            field: "igpu_co".into(),
            value: margin.to_string(),
            threshold: format!(">= {IGPU_CO_SOFT_MIN}"),
        });
    }
    Ok(margin as i16)
}

fn encode_per_core_arg(mask: u32, is_apu: bool, margin: i32) -> u32 {
    let encoded = encode_psm_margin(margin);
    if is_apu {
        ((mask & 0xFFF) << 20) | encoded
    } else {
        (mask & 0xFFF0_0000) | encoded
    }
}

/// Read current Curve Optimizer margins for all physical cores (and iGPU if available).
pub fn get_co(transport: &dyn SmuTransport, hw: &HardwareContext) -> Result<CoSnapshot> {
    let get_op = hw
        .capabilities
        .get_co_per_core
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "get_co_per_core",
            codename: hw.codename.display_name().to_string(),
        })?;

    check_mailbox_health(transport, hw, get_op.mailbox)?;

    let is_f17 = hw.capabilities.is_family_17h;
    let is_apu = hw.codename.is_apu();
    let mut cores = Vec::with_capacity(hw.topology.cores.len());

    for core in &hw.topology.cores {
        let mask = core.smu_mask(is_f17, is_apu);
        let out = send_verified(transport, hw, get_op, [mask, 0, 0, 0, 0, 0])?;

        // Detect arg-echo anomaly: when SMN RSP verification is unavailable and
        // ryzen_smu returns a false-OK, the input core mask is echoed back unchanged.
        if (mask & 0xFFFF_0000) != 0 && out[0] == mask {
            return Err(Error::SmuRejected {
                mailbox: get_op.mailbox.as_str(),
                opcode: get_op.opcode,
                status: format!(
                    "arg-echo anomaly: SMU echoed input mask 0x{mask:08X} unchanged"
                ),
            });
        }

        cores.push(CoreCoMargin {
            core_index: core.index,
            ccd: core.ccd,
            core_in_ccd: core.core_in_ccd,
            linux_cpus: core.linux_cpus.clone(),
            margin: decode_psm_margin(out[0]),
        });
    }

    let igpu = if let Some(igpu_op) = hw.capabilities.get_igpu_co {
        match send_verified(transport, hw, igpu_op, [0; 6]) {
            Ok(out) => Some(decode_psm_margin(out[0])),
            Err(Error::SmuRejected { .. }) => None,
            Err(err) => return Err(err),
        }
    } else {
        None
    };

    Ok(CoSnapshot { cores, igpu })
}

/// Build a validated [`PlannedOp`] for setting an all-core Curve Optimizer margin.
pub fn plan_set_co_all(
    hw: &HardwareContext,
    margin: i32,
    allow_extreme: bool,
    current: Option<&CoSnapshot>,
) -> Result<PlannedOp> {
    let validated = validate_co_margin(hw, margin, allow_extreme)?;
    let op = hw
        .capabilities
        .set_co_all_cores
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_co_all_cores",
            codename: hw.codename.display_name().to_string(),
        })?;

    let current_summary = current.and_then(|snap| {
        let first = snap.cores.first()?;
        if snap.cores.iter().all(|c| c.margin == first.margin) {
            Some(first.margin.to_string())
        } else {
            Some(
                snap.cores
                    .iter()
                    .map(|c| format!("c{}:{}", c.core_index, c.margin))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }
    });

    Ok(PlannedOp {
        description: format!("Set all-core Curve Optimizer margin to {validated}"),
        mailbox: op.primary.mailbox,
        opcode: op.primary.opcode,
        args: [encode_psm_margin(i32::from(validated)), 0, 0, 0, 0, 0],
        current: current_summary,
        target: validated.to_string(),
    })
}

/// Set an all-core Curve Optimizer margin, with bounds validation, health check,
/// dry-run support, and read-after-write verification across all cores.
pub fn set_co_all(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    margin: i32,
    allow_extreme: bool,
    dry_run: bool,
) -> Result<Vec<PlannedOp>> {
    let validated = validate_co_margin(hw, margin, allow_extreme)?;
    let op = hw
        .capabilities
        .set_co_all_cores
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_co_all_cores",
            codename: hw.codename.display_name().to_string(),
        })?;

    let planned = plan_set_co_all(hw, margin, allow_extreme, None)?;
    if dry_run {
        return Ok(vec![planned]);
    }

    check_mailbox_health(transport, hw, op.primary.mailbox)?;
    send_dual_verified(transport, hw, op, planned.args)?;

    if hw.capabilities.get_co_per_core.is_some() {
        let after = get_co(transport, hw)?;
        for core in &after.cores {
            if core.margin != validated {
                return Err(Error::VerificationFailed {
                    field: format!("co.core{}", core.core_index),
                    expected: validated.to_string(),
                    actual: core.margin.to_string(),
                });
            }
        }
    }

    Ok(vec![planned])
}

/// Set per-core Curve Optimizer margins for the specified `(core_index, margin)` pairs.
///
/// Validates every core index and margin BEFORE sending any command to hardware.
/// When `!dry_run`, performs a mailbox health check, writes all updates, and
/// read-after-write verifies every updated core when readback is supported.
pub fn set_co_cores(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    updates: &[(u32, i32)],
    allow_extreme: bool,
    dry_run: bool,
) -> Result<Vec<PlannedOp>> {
    let op = hw
        .capabilities
        .set_co_per_core
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_co_per_core",
            codename: hw.codename.display_name().to_string(),
        })?;

    let is_f17 = hw.capabilities.is_family_17h;
    let is_apu = hw.codename.is_apu();

    // Validate all inputs up front before touching hardware.
    let mut planned = Vec::with_capacity(updates.len());
    let mut expected_final: BTreeMap<u32, i16> = BTreeMap::new();

    for &(core_index, margin) in updates {
        let core = hw.topology.core(core_index).ok_or_else(|| Error::OutOfBounds {
            field: "core_index".into(),
            value: core_index.to_string(),
            bounds: format!("0..{}", hw.topology.physical_cores),
        })?;
        let validated = validate_co_margin(hw, margin, allow_extreme)?;
        let mask = core.smu_mask(is_f17, is_apu);
        let arg0 = encode_per_core_arg(mask, is_apu, i32::from(validated));

        planned.push(PlannedOp {
            description: format!(
                "Set core {core_index} (CCD {}, local {}) Curve Optimizer margin to {validated}",
                core.ccd, core.core_in_ccd
            ),
            mailbox: op.primary.mailbox,
            opcode: op.primary.opcode,
            args: [arg0, 0, 0, 0, 0, 0],
            current: None,
            target: validated.to_string(),
        });
        expected_final.insert(core_index, validated);
    }

    if dry_run || planned.is_empty() {
        return Ok(planned);
    }

    check_mailbox_health(transport, hw, op.primary.mailbox)?;
    for p in &planned {
        send_dual_verified(transport, hw, op, p.args)?;
    }

    if hw.capabilities.get_co_per_core.is_some() {
        let after = get_co(transport, hw)?;
        for (core_index, expected) in expected_final {
            let actual = after
                .cores
                .iter()
                .find(|c| c.core_index == core_index)
                .map(|c| c.margin);
            if actual != Some(expected) {
                return Err(Error::VerificationFailed {
                    field: format!("co.core{core_index}"),
                    expected: expected.to_string(),
                    actual: actual
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "missing".into()),
                });
            }
        }
    }

    Ok(planned)
}

/// Set the iGPU Curve Optimizer margin (bounded `-30..=30`, soft `-25`), with
/// read-after-write verification when `get_igpu_co` is supported.
pub fn set_igpu_co(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    margin: i32,
    allow_extreme: bool,
    dry_run: bool,
) -> Result<PlannedOp> {
    let op = hw
        .capabilities
        .set_igpu_co
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_igpu_co",
            codename: hw.codename.display_name().to_string(),
        })?;
    let validated = validate_igpu_co_margin(margin, allow_extreme)?;

    let planned = PlannedOp {
        description: format!("Set iGPU Curve Optimizer margin to {validated}"),
        mailbox: op.mailbox,
        opcode: op.opcode,
        args: [encode_psm_margin(i32::from(validated)), 0, 0, 0, 0, 0],
        current: None,
        target: validated.to_string(),
    };

    if dry_run {
        return Ok(planned);
    }

    check_mailbox_health(transport, hw, op.mailbox)?;
    send_verified(transport, hw, op, planned.args)?;

    if let Some(get_op) = hw.capabilities.get_igpu_co {
        let out = send_verified(transport, hw, get_op, [0; 6])?;
        let actual = decode_psm_margin(out[0]);
        if actual != validated {
            return Err(Error::VerificationFailed {
                field: "co.igpu".into(),
                expected: validated.to_string(),
                actual: actual.to_string(),
            });
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
    fn psm_margin_roundtrip() {
        for m in [-50, -35, -20, -1, 0, 1, 15, 50] {
            let enc = encode_psm_margin(m);
            assert_eq!(decode_psm_margin(enc), m as i16);
        }
        assert_eq!(encode_psm_margin(-20), 0xFFEC);
        assert_eq!(encode_psm_margin(0), 0x0000);
        assert_eq!(encode_psm_margin(10), 0x000A);
    }

    #[test]
    fn health_check_passes_and_detects_rejection() {
        let (transport, hw) = mock_9700x_ctx();
        assert!(check_mailbox_health(&transport, &hw, Mailbox::Rsmu).is_ok());

        transport.set_reject_opcode(0x01, SmuStatus::CmdRejectedBusy);
        let err = check_mailbox_health(&transport, &hw, Mailbox::Rsmu).unwrap_err();
        assert!(matches!(err, Error::SmuRejected { opcode: 0x01, .. }));
    }

    #[test]
    fn detects_false_ok_bug_via_smn_rsp_verification() {
        let (transport, hw) = mock_9700x_ctx();
        transport.set_simulate_false_ok_bug(true);
        // Reject primary RSMU set_co_all_cores (0x07) and MP1 fallback (0x36)
        transport.set_reject_opcode(0x07, SmuStatus::CmdRejectedPrereq);
        transport.set_reject_opcode(0x36, SmuStatus::CmdRejectedPrereq);

        // Even though driver_status returns Ok due to smu.c:213 bug,
        // hw_status via SMN RSP addr catches the rejection.
        let err = set_co_all(&transport, &hw, -15, false, false).unwrap_err();
        assert!(matches!(err, Error::SmuRejected { .. }));
    }

    #[test]
    fn detects_arg_echo_anomaly_when_smn_unavailable() {
        let (transport, mut hw) = mock_9700x_ctx();
        // Disable SMN RSP verification so driver false-OK leaks through
        hw.capabilities.rsp_addrs.rsmu_rsp = None;
        transport.set_simulate_false_ok_bug(true);
        transport.set_reject_opcode(0xD5, SmuStatus::UnknownCmd);

        let err = get_co(&transport, &hw).unwrap_err();
        assert!(matches!(err, Error::SmuRejected { opcode: 0xD5, .. }));
    }

    #[test]
    fn bounds_and_extreme_value_enforcement() {
        let (transport, hw) = mock_9700x_ctx();

        // Out of hard bounds (-50..=50)
        assert!(matches!(
            set_co_all(&transport, &hw, -51, true, false),
            Err(Error::OutOfBounds { .. })
        ));
        assert!(matches!(
            set_co_all(&transport, &hw, 51, true, false),
            Err(Error::OutOfBounds { .. })
        ));

        // Below soft_min (-35) blocked without allow_extreme
        assert!(matches!(
            set_co_all(&transport, &hw, -40, false, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        // Zero writes must have happened
        assert!(transport.history().is_empty());

        // Allowed with allow_extreme
        let ops = set_co_all(&transport, &hw, -40, true, false).unwrap();
        assert_eq!(ops.len(), 1);
        let snap = get_co(&transport, &hw).unwrap();
        assert!(snap.cores.iter().all(|c| c.margin == -40));
    }

    #[test]
    fn dry_run_performs_zero_writes() {
        let (transport, hw) = mock_9700x_ctx();
        let ops = set_co_all(&transport, &hw, -20, false, true).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].args[0], encode_psm_margin(-20));
        assert!(transport.history().is_empty());

        let ops = set_co_cores(&transport, &hw, &[(0, -15), (3, -25)], false, true).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(transport.history().is_empty());

        let op = set_igpu_co(&transport, &hw, -10, false, true).unwrap();
        assert_eq!(op.args[0], encode_psm_margin(-10));
        assert!(transport.history().is_empty());
    }

    #[test]
    fn per_core_validates_all_before_writing_any() {
        let (transport, hw) = mock_9700x_ctx();
        // Second entry is invalid core index 99
        let err = set_co_cores(&transport, &hw, &[(0, -15), (99, -10)], false, false).unwrap_err();
        assert!(matches!(err, Error::OutOfBounds { .. }));
        assert!(transport.history().is_empty());

        // Second entry violates soft_min
        let err = set_co_cores(&transport, &hw, &[(0, -15), (1, -45)], false, false).unwrap_err();
        assert!(matches!(err, Error::ExtremeValueBlocked { .. }));
        assert!(transport.history().is_empty());

        // Valid batch writes and verifies cleanly
        let ops = set_co_cores(&transport, &hw, &[(0, -15), (3, -25), (7, -10)], false, false)
            .unwrap();
        assert_eq!(ops.len(), 3);
        let snap = get_co(&transport, &hw).unwrap();
        assert_eq!(snap.cores[0].margin, -15);
        assert_eq!(snap.cores[1].margin, 0);
        assert_eq!(snap.cores[3].margin, -25);
        assert_eq!(snap.cores[7].margin, -10);
    }

    #[test]
    fn dual_opcode_falls_back_to_mp1_when_rsmu_rejected() {
        let (transport, hw) = mock_9700x_ctx();
        // Reject RSMU 0x07; MP1 0x36 succeeds
        transport.set_reject_opcode(0x07, SmuStatus::CmdRejectedPrereq);
        let ops = set_co_all(&transport, &hw, -18, false, false).unwrap();
        assert_eq!(ops.len(), 1);
        let snap = get_co(&transport, &hw).unwrap();
        assert!(snap.cores.iter().all(|c| c.margin == -18));
    }

    #[test]
    fn igpu_co_set_and_bounds() {
        let (transport, hw) = mock_9700x_ctx();
        assert!(matches!(
            set_igpu_co(&transport, &hw, -31, true, false),
            Err(Error::OutOfBounds { .. })
        ));
        assert!(matches!(
            set_igpu_co(&transport, &hw, -28, false, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        set_igpu_co(&transport, &hw, -20, false, false).unwrap();
        let snap = get_co(&transport, &hw).unwrap();
        assert_eq!(snap.igpu, Some(-20));
    }

    #[test]
    fn plan_set_co_all_includes_current_summary() {
        let (transport, hw) = mock_9700x_ctx();
        let snap = get_co(&transport, &hw).unwrap();
        let plan = plan_set_co_all(&hw, -15, false, Some(&snap)).unwrap();
        assert_eq!(plan.current.as_deref(), Some("0"));
        assert_eq!(plan.target, "-15");
    }
}
