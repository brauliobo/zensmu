use super::{Profile, PROFILE_SCHEMA_VERSION};
use crate::error::{Error, Result};
use crate::hw::profiles::capabilities_for;
use crate::safety::bounds::{
    validate_apu_fast_watts, validate_apu_soc_edc_amps, validate_apu_soc_tdc_amps,
    validate_apu_time_secs, validate_apu_watts, validate_co_margin, validate_edc_amps,
    validate_fmax_mhz, validate_igpu_co_margin, validate_pbo_scalar_checked, validate_ppt_watts,
    validate_shaper_margin, validate_skin_temp_celsius, validate_tctl_celsius, validate_tdc_amps,
};
use crate::types::{ApuPowerMode, Codename};

impl Profile {
    /// Validate schema, target hardware match, capability support, core indices, and safety bounds.
    pub fn validate_for(
        &self,
        codename: Codename,
        physical_cores: usize,
        allow_extreme: bool,
    ) -> Result<()> {
        if self.schema != PROFILE_SCHEMA_VERSION {
            return Err(Error::InvalidInput(format!(
                "unsupported profile schema version {} (expected {})",
                self.schema, PROFILE_SCHEMA_VERSION
            )));
        }

        if self.target.codename != codename {
            return Err(Error::ProfileTargetMismatch {
                expected: self.target.codename.to_string(),
                actual: codename.to_string(),
            });
        }

        if let Some(expected_cores) = self.target.physical_cores {
            if expected_cores != physical_cores {
                return Err(Error::ProfileTargetMismatch {
                    expected: format!("{} ({} cores)", self.target.codename, expected_cores),
                    actual: format!("{} ({} cores)", codename, physical_cores),
                });
            }
        }

        let caps = capabilities_for(codename).ok_or_else(|| Error::UnsupportedCpu {
            codename: codename.to_string(),
            detail: "no verified capability profile for this CPU".into(),
        })?;

        if let Some(co) = &self.curve_optimizer {
            let has_any_co = co.all.is_some() || !co.per_core.is_empty() || co.igpu.is_some();
            if has_any_co {
                let bounds = caps.co_bounds.ok_or_else(|| Error::UnsupportedCapability {
                    capability: "curve_optimizer",
                    codename: codename.to_string(),
                })?;

                if let Some(all_margin) = co.all {
                    require_cap(
                        caps.set_co_all_cores.is_some() || caps.set_co_per_core.is_some(),
                        "curve_optimizer",
                        codename,
                    )?;
                    validate_co_margin("curve_optimizer.all", all_margin, bounds, allow_extreme)?;
                }

                if !co.per_core.is_empty() {
                    require_cap(
                        caps.set_co_per_core.is_some(),
                        "curve_optimizer_per_core",
                        codename,
                    )?;
                }

                for (&core_idx, &margin) in &co.per_core {
                    if (core_idx as usize) >= physical_cores {
                        return Err(Error::OutOfBounds {
                            field: "curve_optimizer.per_core".into(),
                            value: format!("core {core_idx}"),
                            bounds: format!("0..{physical_cores}"),
                        });
                    }
                    validate_co_margin(
                        &format!("curve_optimizer.per_core.{core_idx}"),
                        margin,
                        bounds,
                        allow_extreme,
                    )?;
                }

                if let Some(igpu_margin) = co.igpu {
                    require_cap(caps.set_igpu_co.is_some(), "igpu_co", codename)?;
                    validate_igpu_co_margin("curve_optimizer.igpu", igpu_margin, allow_extreme)?;
                }
            }
        }

        if let Some(shaper) = &self.curve_shaper {
            let has_any_shaper = shaper
                .values()
                .any(|t| t.low.is_some() || t.med.is_some() || t.high.is_some());
            if has_any_shaper {
                require_cap(caps.set_curve_shaper.is_some(), "curve_shaper", codename)?;
                for (&tier, margins) in shaper {
                    for (band, val) in [
                        ("low", margins.low),
                        ("med", margins.med),
                        ("high", margins.high),
                    ] {
                        if let Some(v) = val {
                            validate_shaper_margin(
                                &format!("curve_shaper.{}.{band}", tier.label()),
                                i32::from(v),
                                allow_extreme,
                            )?;
                        }
                    }
                }
            }
        }

        if let Some(pbo) = &self.pbo {
            if let Some(ppt) = pbo.ppt_w {
                require_cap(caps.set_ppt.is_some(), "pbo_ppt", codename)?;
                validate_ppt_watts(ppt, allow_extreme)?;
            }
            if let Some(tdc) = pbo.tdc_a {
                require_cap(caps.set_tdc.is_some(), "pbo_tdc", codename)?;
                validate_tdc_amps(tdc, allow_extreme)?;
            }
            if let Some(edc) = pbo.edc_a {
                require_cap(caps.set_edc.is_some(), "pbo_edc", codename)?;
                validate_edc_amps(edc, allow_extreme)?;
            }
            if let Some(tctl) = pbo.tctl_c {
                require_cap(caps.set_tctl.is_some(), "pbo_tctl", codename)?;
                validate_tctl_celsius(tctl, allow_extreme)?;
            }
            if let Some(scalar) = pbo.scalar {
                require_cap(caps.set_pbo_scalar.is_some(), "pbo_scalar", codename)?;
                validate_pbo_scalar_checked(scalar, allow_extreme)?;
            }
            if let Some(fmax) = pbo.fmax_mhz {
                require_cap(caps.set_fmax.is_some(), "fmax_boost_limit", codename)?;
                validate_fmax_mhz(fmax, allow_extreme)?;
            }
        }

        if let Some(apu) = &self.apu {
            if let Some(w) = apu.stapm_w {
                require_cap(caps.set_stapm_limit.is_some(), "apu_stapm", codename)?;
                validate_apu_watts("apu.stapm_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.fast_w {
                require_cap(
                    codename.is_apu() && caps.set_ppt.is_some(),
                    "apu_fast_limit",
                    codename,
                )?;
                validate_apu_fast_watts("apu.fast_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.slow_w {
                require_cap(
                    codename.is_apu() && caps.set_slow_limit.is_some(),
                    "apu_slow_limit",
                    codename,
                )?;
                validate_apu_watts("apu.slow_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.apu_slow_w {
                require_cap(
                    caps.set_apu_slow_limit.is_some(),
                    "apu_slow_limit",
                    codename,
                )?;
                validate_apu_watts("apu.apu_slow_w", w, allow_extreme)?;
            }
            if let Some(s) = apu.stapm_time_s {
                require_cap(caps.set_stapm_time.is_some(), "apu_stapm_time", codename)?;
                validate_apu_time_secs("apu.stapm_time_s", s)?;
            }
            if let Some(s) = apu.slow_time_s {
                require_cap(caps.set_slow_time.is_some(), "apu_slow_time", codename)?;
                validate_apu_time_secs("apu.slow_time_s", s)?;
            }
            if let Some(a) = apu.tdc_soc_a {
                require_cap(caps.set_tdc_soc.is_some(), "apu_tdc_soc", codename)?;
                validate_apu_soc_tdc_amps("apu.tdc_soc_a", a, allow_extreme)?;
            }
            if let Some(a) = apu.edc_soc_a {
                require_cap(caps.set_edc_soc.is_some(), "apu_edc_soc", codename)?;
                validate_apu_soc_edc_amps("apu.edc_soc_a", a, allow_extreme)?;
            }
            if let Some(c) = apu.apu_skin_temp_c {
                require_cap(caps.set_apu_skin_temp.is_some(), "apu_skin_temp", codename)?;
                validate_skin_temp_celsius("apu.apu_skin_temp_c", c, allow_extreme)?;
            }
            if let Some(c) = apu.dgpu_skin_temp_c {
                require_cap(
                    caps.set_dgpu_skin_temp.is_some(),
                    "dgpu_skin_temp",
                    codename,
                )?;
                validate_skin_temp_celsius("apu.dgpu_skin_temp_c", c, allow_extreme)?;
            }
            if let Some(mode) = apu.power_mode {
                let supported = match mode {
                    ApuPowerMode::MaxPerformance => caps.set_max_performance.is_some(),
                    ApuPowerMode::PowerSaving => caps.set_power_saving.is_some(),
                };
                require_cap(supported, "apu_power_mode", codename)?;
            }
        }

        Ok(())
    }
}

fn require_cap(supported: bool, capability: &'static str, codename: Codename) -> Result<()> {
    if !supported {
        return Err(Error::UnsupportedCapability {
            capability,
            codename: codename.to_string(),
        });
    }
    Ok(())
}
