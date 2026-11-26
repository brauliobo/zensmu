//! Zen 5 Curve Shaper (5 frequency tiers x 3 temperature bands) SMU operations.
//!
//! Each frequency tier (`min`, `low`, `med`, `high`, `max`) packs three signed
//! 8-bit temperature-band margins (`low`, `med`, `high`) into a single 32-bit
//! RSMU argument.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::ops::co::{check_mailbox_health, send_verified};
use crate::transport::SmuTransport;
use crate::types::{CurveShaperState, FreqTier, PlannedOp, ShaperTierMargins};

/// Hard minimum Curve Shaper margin.
pub const SHAPER_MIN: i8 = -50;
/// Hard maximum Curve Shaper margin.
pub const SHAPER_MAX: i8 = 30;
/// Conservative soft minimum below which `--allow-extreme` is required.
pub const SHAPER_SOFT_MIN: i8 = -35;

/// Pack a frequency tier and its 3 signed temperature-band margins into the
/// 32-bit RSMU `SetCurveShaperMargin` argument format.
pub fn encode_shaper_tier(tier: FreqTier, m: ShaperTierMargins) -> u32 {
    ((m.high as u8 as u32) << 24)
        | ((m.med as u8 as u32) << 16)
        | ((m.low as u8 as u32) << 8)
        | (1u32 << 7)
        | (tier.as_u32() & 0x7F)
}

/// Unpack the 3 signed temperature-band margins from a raw 32-bit RSMU
/// `GetCurveShaperMargin` register word.
pub fn decode_shaper_tier(raw: u32) -> ShaperTierMargins {
    ShaperTierMargins {
        low: ((raw >> 8) & 0xFF) as u8 as i8,
        med: ((raw >> 16) & 0xFF) as u8 as i8,
        high: ((raw >> 24) & 0xFF) as u8 as i8,
    }
}

/// Validate a single Curve Shaper band margin against hard and soft limits.
pub fn validate_shaper_margin(margin: i32, allow_extreme: bool) -> Result<i8> {
    if margin < i32::from(SHAPER_MIN) || margin > i32::from(SHAPER_MAX) {
        return Err(Error::OutOfBounds {
            field: "shaper_margin".into(),
            value: margin.to_string(),
            bounds: format!("{SHAPER_MIN}..={SHAPER_MAX}"),
        });
    }
    if margin < i32::from(SHAPER_SOFT_MIN) && !allow_extreme {
        return Err(Error::ExtremeValueBlocked {
            field: "shaper_margin".into(),
            value: margin.to_string(),
            threshold: format!(">= {SHAPER_SOFT_MIN}"),
        });
    }
    Ok(margin as i8)
}

fn validate_tier_margins(margins: ShaperTierMargins, allow_extreme: bool) -> Result<()> {
    validate_shaper_margin(i32::from(margins.low), allow_extreme)?;
    validate_shaper_margin(i32::from(margins.med), allow_extreme)?;
    validate_shaper_margin(i32::from(margins.high), allow_extreme)?;
    Ok(())
}

fn format_tier_margins(m: ShaperTierMargins) -> String {
    format!("low={},med={},high={}", m.low, m.med, m.high)
}

/// Read the complete 5x3 Zen 5 Curve Shaper matrix from hardware.
pub fn get_curve_shaper(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
) -> Result<CurveShaperState> {
    let op = hw
        .capabilities
        .get_curve_shaper
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "get_curve_shaper",
            codename: hw.codename.display_name().to_string(),
        })?;

    check_mailbox_health(transport, hw, op.mailbox)?;
    let out = send_verified(transport, hw, op, [0; 6])?;

    Ok(CurveShaperState {
        min: decode_shaper_tier(out[0]),
        low: decode_shaper_tier(out[1]),
        med: decode_shaper_tier(out[2]),
        high: decode_shaper_tier(out[3]),
        max: decode_shaper_tier(out[4]),
    })
}

/// Write a single Curve Shaper frequency tier (all 3 temperature bands), with
/// bounds checking, dry-run support, and read-after-write verification.
pub fn set_curve_shaper_tier(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    tier: FreqTier,
    margins: ShaperTierMargins,
    allow_extreme: bool,
    dry_run: bool,
) -> Result<PlannedOp> {
    let op = hw
        .capabilities
        .set_curve_shaper
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_curve_shaper",
            codename: hw.codename.display_name().to_string(),
        })?;

    validate_tier_margins(margins, allow_extreme)?;

    let packed = encode_shaper_tier(tier, margins);
    let planned = PlannedOp {
        description: format!(
            "Set Curve Shaper tier '{}' to ({})",
            tier.label(),
            format_tier_margins(margins)
        ),
        mailbox: op.mailbox,
        opcode: op.opcode,
        args: [packed, 0, 0, 0, 0, 0],
        current: None,
        target: format_tier_margins(margins),
    };

    if dry_run {
        return Ok(planned);
    }

    check_mailbox_health(transport, hw, op.mailbox)?;
    send_verified(transport, hw, op, planned.args)?;

    if hw.capabilities.get_curve_shaper.is_some() {
        let after = get_curve_shaper(transport, hw)?;
        let actual = after.tier(tier);
        if actual != margins {
            return Err(Error::VerificationFailed {
                field: format!("shaper.{}", tier.label()),
                expected: format_tier_margins(margins),
                actual: format_tier_margins(actual),
            });
        }
    }

    Ok(planned)
}

/// Write all 5 frequency tiers of the Zen 5 Curve Shaper matrix.
///
/// Validates all 15 values BEFORE sending any SMU command, and performs a full
/// matrix read-after-write verification when `!dry_run`.
pub fn set_curve_shaper_all(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    state: &CurveShaperState,
    allow_extreme: bool,
    dry_run: bool,
) -> Result<Vec<PlannedOp>> {
    let op = hw
        .capabilities
        .set_curve_shaper
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "set_curve_shaper",
            codename: hw.codename.display_name().to_string(),
        })?;

    let mut planned = Vec::with_capacity(FreqTier::ALL.len());
    for tier in FreqTier::ALL {
        let margins = state.tier(tier);
        validate_tier_margins(margins, allow_extreme)?;
        let packed = encode_shaper_tier(tier, margins);
        planned.push(PlannedOp {
            description: format!(
                "Set Curve Shaper tier '{}' to ({})",
                tier.label(),
                format_tier_margins(margins)
            ),
            mailbox: op.mailbox,
            opcode: op.opcode,
            args: [packed, 0, 0, 0, 0, 0],
            current: None,
            target: format_tier_margins(margins),
        });
    }

    if dry_run {
        return Ok(planned);
    }

    check_mailbox_health(transport, hw, op.mailbox)?;
    for p in &planned {
        send_verified(transport, hw, op, p.args)?;
    }

    if hw.capabilities.get_curve_shaper.is_some() {
        let after = get_curve_shaper(transport, hw)?;
        for tier in FreqTier::ALL {
            let expected = state.tier(tier);
            let actual = after.tier(tier);
            if actual != expected {
                return Err(Error::VerificationFailed {
                    field: format!("shaper.{}", tier.label()),
                    expected: format_tier_margins(expected),
                    actual: format_tier_margins(actual),
                });
            }
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
    fn encode_decode_shaper_tier_roundtrip() {
        let m = ShaperTierMargins {
            low: -25,
            med: -10,
            high: 15,
        };
        for tier in FreqTier::ALL {
            let packed = encode_shaper_tier(tier, m);
            assert_eq!(packed & 0x7F, tier.as_u32());
            assert_ne!(packed & (1 << 7), 0);
            assert_eq!(decode_shaper_tier(packed), m);
        }
    }

    #[test]
    fn shaper_roundtrip_single_and_all_tiers() {
        let (transport, hw) = mock_9700x_ctx();
        let desired = CurveShaperState {
            min: ShaperTierMargins { low: -10, med: -5, high: 0 },
            low: ShaperTierMargins { low: -15, med: -10, high: -5 },
            med: ShaperTierMargins { low: -20, med: -15, high: -10 },
            high: ShaperTierMargins { low: -25, med: -20, high: -10 },
            max: ShaperTierMargins { low: -15, med: -10, high: 5 },
        };

        let ops = set_curve_shaper_all(&transport, &hw, &desired, false, false).unwrap();
        assert_eq!(ops.len(), 5);

        let readback = get_curve_shaper(&transport, &hw).unwrap();
        assert_eq!(readback, desired);

        // Update a single tier
        let new_high = ShaperTierMargins { low: -30, med: -25, high: -15 };
        set_curve_shaper_tier(&transport, &hw, FreqTier::High, new_high, false, false).unwrap();
        let readback2 = get_curve_shaper(&transport, &hw).unwrap();
        assert_eq!(readback2.high, new_high);
        assert_eq!(readback2.min, desired.min);
    }

    #[test]
    fn shaper_bounds_and_dry_run() {
        let (transport, hw) = mock_9700x_ctx();

        // Hard out of bounds (> 30 or < -50)
        assert!(matches!(
            validate_shaper_margin(31, true),
            Err(Error::OutOfBounds { .. })
        ));
        assert!(matches!(
            validate_shaper_margin(-51, true),
            Err(Error::OutOfBounds { .. })
        ));

        // Soft limit (-35) blocked without allow_extreme
        let extreme = ShaperTierMargins { low: -40, med: 0, high: 0 };
        assert!(matches!(
            set_curve_shaper_tier(&transport, &hw, FreqTier::Max, extreme, false, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        assert!(transport.history().is_empty());

        // Dry run performs zero writes
        let ops = set_curve_shaper_all(&transport, &hw, &CurveShaperState::default(), false, true)
            .unwrap();
        assert_eq!(ops.len(), 5);
        assert!(transport.history().is_empty());
    }

    #[test]
    fn shaper_rejected_on_zen4_raphael() {
        let (transport, mut hw) = mock_9700x_ctx();
        hw.codename = Codename::Raphael;
        hw.capabilities = capabilities_for(Codename::Raphael).unwrap();

        assert!(matches!(
            get_curve_shaper(&transport, &hw),
            Err(Error::UnsupportedCapability { capability: "get_curve_shaper", .. })
        ));
        assert!(matches!(
            set_curve_shaper_tier(
                &transport,
                &hw,
                FreqTier::Med,
                ShaperTierMargins::default(),
                false,
                false
            ),
            Err(Error::UnsupportedCapability { capability: "set_curve_shaper", .. })
        ));
    }

    #[test]
    fn shaper_catches_false_ok_bug() {
        let (transport, hw) = mock_9700x_ctx();
        transport.set_simulate_false_ok_bug(true);
        transport.set_reject_opcode(0xA6, SmuStatus::CmdRejectedPrereq);

        let err = set_curve_shaper_tier(
            &transport,
            &hw,
            FreqTier::Med,
            ShaperTierMargins { low: -10, med: -10, high: -10 },
            false,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, Error::SmuRejected { opcode: 0xA6, .. }));
    }
}
