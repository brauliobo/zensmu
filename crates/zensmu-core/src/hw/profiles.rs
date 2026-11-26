use crate::types::{Codename, Mailbox};
use serde::{Deserialize, Serialize};

/// A specific SMU mailbox + opcode pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpcodeSpec {
    pub mailbox: Mailbox,
    pub opcode: u32,
}

impl OpcodeSpec {
    pub const fn rsmu(opcode: u32) -> Self {
        Self {
            mailbox: Mailbox::Rsmu,
            opcode,
        }
    }

    pub const fn mp1(opcode: u32) -> Self {
        Self {
            mailbox: Mailbox::Mp1,
            opcode,
        }
    }
}

/// Primary + optional fallback opcode (e.g. RSMU primary with MP1 fallback on desktop,
/// or MP1 primary on older APUs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DualOpcode {
    pub primary: OpcodeSpec,
    pub fallback: Option<OpcodeSpec>,
}

impl DualOpcode {
    pub const fn rsmu_only(op: u32) -> Self {
        Self {
            primary: OpcodeSpec::rsmu(op),
            fallback: None,
        }
    }

    pub const fn mp1_only(op: u32) -> Self {
        Self {
            primary: OpcodeSpec::mp1(op),
            fallback: None,
        }
    }

    pub const fn rsmu_then_mp1(rsmu: u32, mp1: u32) -> Self {
        Self {
            primary: OpcodeSpec::rsmu(rsmu),
            fallback: Some(OpcodeSpec::mp1(mp1)),
        }
    }

    pub const fn mp1_then_rsmu(mp1: u32, rsmu: u32) -> Self {
        Self {
            primary: OpcodeSpec::mp1(mp1),
            fallback: Some(OpcodeSpec::rsmu(rsmu)),
        }
    }
}

/// Read-only SMN addresses for the SMU response registers (from `ryzen_smu/smu.c`),
/// used to read the true hardware RSP and defeat `ryzen_smu`'s `smu.c:213` false-OK bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxRspAddrs {
    pub rsmu_rsp: Option<u32>,
    pub mp1_rsp: Option<u32>,
}

/// Safe Curve Optimizer bounds for a CPU generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoBounds {
    pub min: i32,
    pub max: i32,
    /// Conservative soft limit below which `--allow-extreme` is required.
    pub soft_min: i32,
}

/// Static capability table for a CPU codename.
///
/// Every capability is explicitly `Option<...>` — missing entries refuse execution
/// instead of guessing or falling back across generations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuCapabilities {
    pub codename: Codename,
    pub is_family_17h: bool,
    pub rsp_addrs: MailboxRspAddrs,
    pub co_bounds: Option<CoBounds>,

    // Mailbox health test (TestMessage = 0x01 on all Ryzen platforms)
    pub test_message_rsmu: Option<OpcodeSpec>,
    pub test_message_mp1: Option<OpcodeSpec>,

    // Curve Optimizer
    pub get_co_per_core: Option<OpcodeSpec>,
    pub set_co_per_core: Option<DualOpcode>,
    pub set_co_all_cores: Option<DualOpcode>,
    pub get_igpu_co: Option<OpcodeSpec>,
    pub set_igpu_co: Option<OpcodeSpec>,

    // Curve Shaper (Zen 5)
    pub get_curve_shaper: Option<OpcodeSpec>,
    pub set_curve_shaper: Option<OpcodeSpec>,

    // PBO Limits & Boost
    pub set_ppt: Option<DualOpcode>,
    pub set_tdc: Option<DualOpcode>,
    pub set_edc: Option<DualOpcode>,
    pub set_tctl: Option<DualOpcode>,
    pub get_pbo_scalar: Option<OpcodeSpec>,
    pub set_pbo_scalar: Option<DualOpcode>,
    pub set_pbo_en: Option<OpcodeSpec>,
    pub get_fmax: Option<OpcodeSpec>,
    pub set_fmax: Option<OpcodeSpec>,

    // Read-only factory / status queries
    pub get_is_overclockable: Option<OpcodeSpec>,
    pub get_expo_active: Option<OpcodeSpec>,
    pub get_ln2_mode: Option<OpcodeSpec>,
    pub get_perf_data: Option<OpcodeSpec>,
    pub get_fused_ppt: Option<OpcodeSpec>,
    pub get_fused_slow: Option<OpcodeSpec>,
    pub get_fused_fast: Option<OpcodeSpec>,
    pub get_fused_apu_slow: Option<OpcodeSpec>,
    pub get_fused_vdd_tdc: Option<OpcodeSpec>,
    pub get_fused_edc: Option<OpcodeSpec>,
    pub get_fused_soc_tdc: Option<OpcodeSpec>,
    pub get_sustained_power_thm: Option<OpcodeSpec>,

    // APU-specific DPTC controls
    pub set_stapm_limit: Option<DualOpcode>,
    pub set_stapm_time: Option<DualOpcode>,
    pub set_slow_limit: Option<DualOpcode>,
    pub set_slow_time: Option<DualOpcode>,
    pub set_apu_slow_limit: Option<DualOpcode>,
    pub set_tdc_soc: Option<DualOpcode>,
    pub set_edc_soc: Option<DualOpcode>,
    pub set_apu_skin_temp: Option<DualOpcode>,
    pub set_dgpu_skin_temp: Option<DualOpcode>,
    pub set_max_performance: Option<OpcodeSpec>,
    pub set_power_saving: Option<OpcodeSpec>,
}

impl CpuCapabilities {
    pub fn rsp_addr(&self, mb: Mailbox) -> Option<u32> {
        match mb {
            Mailbox::Rsmu => self.rsp_addrs.rsmu_rsp,
            Mailbox::Mp1 => self.rsp_addrs.mp1_rsp,
        }
    }

    /// Summary list of supported high-level features for `zensmu info`.
    pub fn feature_summary(&self) -> Vec<(&'static str, bool)> {
        vec![
            ("curve_optimizer_read", self.get_co_per_core.is_some()),
            (
                "curve_optimizer_write",
                self.set_co_per_core.is_some() || self.set_co_all_cores.is_some(),
            ),
            ("igpu_co", self.set_igpu_co.is_some()),
            (
                "curve_shaper",
                self.get_curve_shaper.is_some() && self.set_curve_shaper.is_some(),
            ),
            (
                "pbo_limits",
                self.set_ppt.is_some()
                    && self.set_tdc.is_some()
                    && self.set_edc.is_some()
                    && self.set_tctl.is_some(),
            ),
            ("pbo_scalar", self.set_pbo_scalar.is_some()),
            ("fmax_boost_limit", self.set_fmax.is_some()),
            ("fused_factory_limits", self.get_fused_ppt.is_some()),
            ("apu_dptc", self.set_stapm_limit.is_some()),
        ]
    }
}

/// Resolve verified static capabilities for a detected codename.
/// Returns `None` for unsupported/unverified codenames (never guesses).
pub fn capabilities_for(codename: Codename) -> Option<CpuCapabilities> {
    match codename {
        Codename::GraniteRidge => Some(zen5_desktop(codename)),
        Codename::Raphael | Codename::StormPeak => Some(zen4_desktop(codename)),
        Codename::Vermeer | Codename::Chagall => Some(zen3_desktop(codename)),
        Codename::Matisse | Codename::CastlePeak => Some(zen2_desktop(codename)),
        Codename::PinnacleRidge => Some(zen_plus_desktop(codename)),
        Codename::Colfax => Some(zen_plus_colfax(codename)),
        Codename::SummitRidge | Codename::ThreadRipper => Some(zen1_desktop(codename)),
        Codename::Renoir | Codename::Lucienne | Codename::Cezanne => Some(apu1_base(codename)),
        Codename::VanGogh => Some(apu1_vangogh(codename)),
        Codename::Rembrandt => Some(apu1_rembrandt(codename)),
        Codename::Phoenix | Codename::HawkPoint => Some(apu1_phoenix(codename)),
        Codename::StrixPoint | Codename::StrixHalo => Some(apu1_strix(codename)),
        Codename::RavenRidge | Codename::RavenRidge2 | Codename::Dali => Some(apu0_base(codename)),
        Codename::Picasso => Some(apu0_picasso(codename)),
        _ => None,
    }
}

fn zen4_desktop(codename: Codename) -> CpuCapabilities {
    CpuCapabilities {
        codename,
        is_family_17h: false,
        rsp_addrs: MailboxRspAddrs {
            rsmu_rsp: Some(0x03B1_0570),
            mp1_rsp: Some(0x03B1_057C),
        },
        co_bounds: Some(CoBounds {
            min: -50,
            max: 50,
            soft_min: -35,
        }),
        test_message_rsmu: Some(OpcodeSpec::rsmu(0x01)),
        test_message_mp1: Some(OpcodeSpec::mp1(0x01)),

        // Curve Optimizer (Zen4Settings.cs)
        get_co_per_core: Some(OpcodeSpec::rsmu(0xD5)),
        set_co_per_core: Some(DualOpcode::rsmu_then_mp1(0x06, 0x35)),
        set_co_all_cores: Some(DualOpcode::rsmu_then_mp1(0x07, 0x36)),
        get_igpu_co: Some(OpcodeSpec::rsmu(0xD7)),
        set_igpu_co: Some(OpcodeSpec::rsmu(0xA7)),

        // Curve Shaper is Zen 5 only
        get_curve_shaper: None,
        set_curve_shaper: None,

        // PBO Limits (Zen4Settings.cs)
        set_ppt: Some(DualOpcode::rsmu_then_mp1(0x56, 0x3E)),
        set_tdc: Some(DualOpcode::rsmu_then_mp1(0x57, 0x3C)),
        set_edc: Some(DualOpcode::rsmu_then_mp1(0x58, 0x3D)),
        set_tctl: Some(DualOpcode::rsmu_then_mp1(0x59, 0x3F)),
        get_pbo_scalar: Some(OpcodeSpec::rsmu(0x6D)),
        set_pbo_scalar: Some(DualOpcode::rsmu_then_mp1(0x5B, 0x2F)),
        set_pbo_en: None,
        get_fmax: Some(OpcodeSpec::rsmu(0x6E)),
        set_fmax: Some(OpcodeSpec::rsmu(0x70)),

        // Read-only info/debug
        get_is_overclockable: Some(OpcodeSpec::rsmu(0x6F)),
        get_expo_active: Some(OpcodeSpec::rsmu(0x35)),
        get_ln2_mode: Some(OpcodeSpec::rsmu(0xDD)),
        get_perf_data: Some(OpcodeSpec::rsmu(0x5C)),
        // Verified live on Ryzen 7 9700X (Granite Ridge):
        // 0xD9 = 88000 mW (PPT), 0xDB = 75000 mA (TDC), 0xDC = 150000 mA (EDC)
        get_fused_ppt: Some(OpcodeSpec::rsmu(0xD9)),
        get_fused_slow: None,
        get_fused_fast: None,
        get_fused_apu_slow: Some(OpcodeSpec::rsmu(0xDA)),
        get_fused_vdd_tdc: Some(OpcodeSpec::rsmu(0xDB)),
        get_fused_edc: Some(OpcodeSpec::rsmu(0xDC)),
        get_fused_soc_tdc: None,
        get_sustained_power_thm: Some(OpcodeSpec::mp1(0x23)),

        set_stapm_limit: None,
        set_stapm_time: None,
        set_slow_limit: Some(DualOpcode::rsmu_then_mp1(0xCB, 0x5F)),
        set_slow_time: None,
        set_apu_slow_limit: None,
        set_tdc_soc: None,
        set_edc_soc: None,
        set_apu_skin_temp: None,
        set_dgpu_skin_temp: None,
        set_max_performance: None,
        set_power_saving: None,
    }
}

/// Zen 5 Desktop / 9000X3D (Granite Ridge, including 9800X3D/9900X3D/9950X3D)
/// and chiplet mobile (Fire Range, mapped to GraniteRidge by `ryzen_smu`).
///
/// Hardware-verified on Ryzen 7 9700X (Granite Ridge, SMU 98.79.0); 9000X3D and
/// Fire Range share the Granite Ridge SMU command table (community reports welcome).
fn zen5_desktop(codename: Codename) -> CpuCapabilities {
    let mut caps = zen4_desktop(codename);
    // Zen 5 adds 15-point Curve Shaper on RSMU (AGESA/SMU dependent; 0xFE skipped cleanly)
    caps.get_curve_shaper = Some(OpcodeSpec::rsmu(0x84));
    caps.set_curve_shaper = Some(OpcodeSpec::rsmu(0xA6));
    caps
}

fn zen3_desktop(codename: Codename) -> CpuCapabilities {
    CpuCapabilities {
        codename,
        is_family_17h: false,
        rsp_addrs: MailboxRspAddrs {
            rsmu_rsp: Some(0x03B1_0570),
            mp1_rsp: Some(0x03B1_057C),
        },
        co_bounds: Some(CoBounds {
            min: -30,
            max: 30,
            soft_min: -25,
        }),
        test_message_rsmu: Some(OpcodeSpec::rsmu(0x01)),
        test_message_mp1: Some(OpcodeSpec::mp1(0x01)),

        // Zen 3 (Vermeer/Chagall): verified live on Ryzen 9 5900XT (SMU 56.76.0):
        // RSMU 0x7C reads per-core CO across CCD0/CCD1; MP1 0x35/0x36 (with RSMU 0x0A/0x0B fallback) writes CO.
        get_co_per_core: Some(OpcodeSpec::rsmu(0x7C)),
        set_co_per_core: Some(DualOpcode::mp1_then_rsmu(0x35, 0x0A)),
        set_co_all_cores: Some(DualOpcode::mp1_then_rsmu(0x36, 0x0B)),
        get_igpu_co: None,
        set_igpu_co: None,

        get_curve_shaper: None,
        set_curve_shaper: None,

        set_ppt: Some(DualOpcode::rsmu_then_mp1(0x53, 0x3D)),
        set_tdc: Some(DualOpcode::rsmu_then_mp1(0x54, 0x3B)),
        set_edc: Some(DualOpcode::rsmu_then_mp1(0x55, 0x3C)),
        set_tctl: Some(DualOpcode::rsmu_then_mp1(0x56, 0x3E)),
        get_pbo_scalar: Some(OpcodeSpec::rsmu(0x6C)),
        set_pbo_scalar: Some(DualOpcode::rsmu_then_mp1(0x58, 0x2F)),
        set_pbo_en: Some(OpcodeSpec::mp1(0x33)),
        get_fmax: Some(OpcodeSpec::rsmu(0x6E)),
        set_fmax: None,

        get_is_overclockable: Some(OpcodeSpec::rsmu(0x6F)),
        get_expo_active: None,
        get_ln2_mode: None,
        get_perf_data: None,
        get_fused_ppt: None,
        get_fused_slow: None,
        get_fused_fast: None,
        get_fused_apu_slow: None,
        get_fused_vdd_tdc: None,
        get_fused_edc: None,
        get_fused_soc_tdc: None,
        get_sustained_power_thm: Some(OpcodeSpec::mp1(0x23)),

        set_stapm_limit: None,
        set_stapm_time: None,
        set_slow_limit: None,
        set_slow_time: None,
        set_apu_slow_limit: None,
        set_tdc_soc: None,
        set_edc_soc: None,
        set_apu_skin_temp: None,
        set_dgpu_skin_temp: None,
        set_max_performance: None,
        set_power_saving: None,
    }
}

fn zen2_desktop(codename: Codename) -> CpuCapabilities {
    CpuCapabilities {
        codename,
        is_family_17h: true,
        rsp_addrs: MailboxRspAddrs {
            rsmu_rsp: Some(0x03B1_0570),
            mp1_rsp: Some(0x03B1_057C),
        },
        co_bounds: None, // Zen 2 has no Curve Optimizer
        test_message_rsmu: Some(OpcodeSpec::rsmu(0x01)),
        test_message_mp1: Some(OpcodeSpec::mp1(0x01)),

        get_co_per_core: None,
        set_co_per_core: None,
        set_co_all_cores: None,
        get_igpu_co: None,
        set_igpu_co: None,
        get_curve_shaper: None,
        set_curve_shaper: None,

        set_ppt: Some(DualOpcode::rsmu_then_mp1(0x53, 0x3C)),
        set_tdc: Some(DualOpcode::rsmu_then_mp1(0x54, 0x3A)),
        set_edc: Some(DualOpcode::rsmu_then_mp1(0x55, 0x3B)),
        set_tctl: Some(DualOpcode::rsmu_then_mp1(0x56, 0x3D)),
        get_pbo_scalar: Some(OpcodeSpec::rsmu(0x6C)),
        set_pbo_scalar: Some(DualOpcode::rsmu_then_mp1(0x58, 0x2F)),
        set_pbo_en: Some(OpcodeSpec::mp1(0x33)),
        get_fmax: Some(OpcodeSpec::rsmu(0x6E)),
        set_fmax: None,

        get_is_overclockable: Some(OpcodeSpec::rsmu(0x6F)),
        get_expo_active: None,
        get_ln2_mode: None,
        get_perf_data: None,
        get_fused_ppt: None,
        get_fused_slow: None,
        get_fused_fast: None,
        get_fused_apu_slow: None,
        get_fused_vdd_tdc: None,
        get_fused_edc: None,
        get_fused_soc_tdc: None,
        get_sustained_power_thm: Some(OpcodeSpec::mp1(0x23)),

        set_stapm_limit: None,
        set_stapm_time: None,
        set_slow_limit: None,
        set_slow_time: None,
        set_apu_slow_limit: None,
        set_tdc_soc: None,
        set_edc_soc: None,
        set_apu_skin_temp: None,
        set_dgpu_skin_temp: None,
        set_max_performance: None,
        set_power_saving: None,
    }
}

fn zen_empty_f17(codename: Codename, rsmu_rsp: u32, mp1_rsp: u32) -> CpuCapabilities {
    CpuCapabilities {
        codename,
        is_family_17h: true,
        rsp_addrs: MailboxRspAddrs {
            rsmu_rsp: Some(rsmu_rsp),
            mp1_rsp: Some(mp1_rsp),
        },
        co_bounds: None,
        test_message_rsmu: Some(OpcodeSpec::rsmu(0x01)),
        test_message_mp1: Some(OpcodeSpec::mp1(0x01)),
        get_co_per_core: None,
        set_co_per_core: None,
        set_co_all_cores: None,
        get_igpu_co: None,
        set_igpu_co: None,
        get_curve_shaper: None,
        set_curve_shaper: None,
        set_ppt: None,
        set_tdc: None,
        set_edc: None,
        set_tctl: None,
        get_pbo_scalar: None,
        set_pbo_scalar: None,
        set_pbo_en: None,
        get_fmax: None,
        set_fmax: None,
        get_is_overclockable: None,
        get_expo_active: None,
        get_ln2_mode: None,
        get_perf_data: None,
        get_fused_ppt: None,
        get_fused_slow: None,
        get_fused_fast: None,
        get_fused_apu_slow: None,
        get_fused_vdd_tdc: None,
        get_fused_edc: None,
        get_fused_soc_tdc: None,
        get_sustained_power_thm: None,
        set_stapm_limit: None,
        set_stapm_time: None,
        set_slow_limit: None,
        set_slow_time: None,
        set_apu_slow_limit: None,
        set_tdc_soc: None,
        set_edc_soc: None,
        set_apu_skin_temp: None,
        set_dgpu_skin_temp: None,
        set_max_performance: None,
        set_power_saving: None,
    }
}

fn zen1_desktop(codename: Codename) -> CpuCapabilities {
    let mut caps = zen_empty_f17(codename, 0x03B1_0568, 0x03B1_0564);
    caps.set_ppt = Some(DualOpcode::rsmu_then_mp1(0x58, 0x31));
    caps.set_edc = Some(DualOpcode::mp1_only(0x2A));
    caps.get_sustained_power_thm = Some(OpcodeSpec::rsmu(0x5F));
    caps
}

fn zen_plus_desktop(codename: Codename) -> CpuCapabilities {
    let mut caps = zen_empty_f17(codename, 0x03B1_0568, 0x03B1_0564);
    caps.set_ppt = Some(DualOpcode::rsmu_only(0x64));
    caps.set_tdc = Some(DualOpcode::rsmu_only(0x65));
    caps.set_edc = Some(DualOpcode::rsmu_only(0x66));
    caps.set_tctl = Some(DualOpcode::rsmu_only(0x68));
    caps.set_pbo_scalar = Some(DualOpcode::rsmu_only(0x6A));
    caps.get_pbo_scalar = Some(OpcodeSpec::rsmu(0x6F));
    caps
}

fn zen_plus_colfax(codename: Codename) -> CpuCapabilities {
    let mut caps = zen_empty_f17(codename, 0x03B1_0568, 0x03B1_0564);
    caps.set_tdc = Some(DualOpcode::rsmu_only(0x6B));
    caps.set_edc = Some(DualOpcode::rsmu_only(0x6C));
    caps.set_tctl = Some(DualOpcode::rsmu_only(0x6E));
    caps.set_pbo_scalar = Some(DualOpcode::rsmu_only(0x6F));
    caps.get_pbo_scalar = Some(OpcodeSpec::rsmu(0x70));
    caps
}

fn apu0_base(codename: Codename) -> CpuCapabilities {
    let mut caps = zen_empty_f17(codename, 0x03B1_0A80, 0x03B1_0564);
    caps.set_stapm_limit = Some(DualOpcode::mp1_then_rsmu(0x1A, 0x2E));
    caps.set_ppt = Some(DualOpcode::mp1_then_rsmu(0x1B, 0x2F));
    caps.set_slow_limit = Some(DualOpcode::mp1_then_rsmu(0x1C, 0x30));
    caps.set_slow_time = Some(DualOpcode::mp1_then_rsmu(0x1D, 0x31));
    caps.set_stapm_time = Some(DualOpcode::mp1_then_rsmu(0x1E, 0x32));
    caps.set_tctl = Some(DualOpcode::mp1_then_rsmu(0x1F, 0x33));
    caps.set_tdc = Some(DualOpcode::mp1_then_rsmu(0x20, 0x34));
    caps.set_tdc_soc = Some(DualOpcode::mp1_then_rsmu(0x21, 0x35));
    caps.set_edc = Some(DualOpcode::mp1_then_rsmu(0x22, 0x36));
    caps.set_edc_soc = Some(DualOpcode::mp1_then_rsmu(0x23, 0x37));
    caps.set_max_performance = Some(OpcodeSpec::mp1(0x18));
    caps.set_power_saving = Some(OpcodeSpec::mp1(0x19));
    caps.get_pbo_scalar = Some(OpcodeSpec::rsmu(0x62));
    caps.get_is_overclockable = Some(OpcodeSpec::rsmu(0x4C));
    caps.get_sustained_power_thm = Some(OpcodeSpec::mp1(0x43));
    caps
}

fn apu0_picasso(codename: Codename) -> CpuCapabilities {
    let mut caps = apu0_base(codename);
    caps.set_apu_slow_limit = Some(DualOpcode::mp1_then_rsmu(0x54, 0x75));
    caps.set_pbo_scalar = Some(DualOpcode::mp1_then_rsmu(0x57, 0x7C));
    caps.get_is_overclockable = Some(OpcodeSpec::rsmu(0x87));
    caps.get_fmax = Some(OpcodeSpec::rsmu(0x86));
    caps.get_fused_ppt = Some(OpcodeSpec::rsmu(0x7F));
    caps.get_fused_slow = Some(OpcodeSpec::rsmu(0x80));
    caps.get_fused_fast = Some(OpcodeSpec::rsmu(0x81));
    caps.get_fused_apu_slow = Some(OpcodeSpec::rsmu(0x82));
    caps.get_fused_vdd_tdc = Some(OpcodeSpec::rsmu(0x83));
    caps.get_fused_soc_tdc = Some(OpcodeSpec::rsmu(0x84));
    caps
}

fn apu1_base(codename: Codename) -> CpuCapabilities {
    let is_family_17h = matches!(
        codename,
        Codename::Renoir | Codename::Lucienne | Codename::VanGogh
    );
    CpuCapabilities {
        codename,
        is_family_17h,
        rsp_addrs: MailboxRspAddrs {
            rsmu_rsp: Some(0x03B1_0A80),
            mp1_rsp: Some(0x03B1_0564),
        },
        co_bounds: if codename == Codename::Cezanne {
            Some(CoBounds {
                min: -30,
                max: 30,
                soft_min: -25,
            })
        } else {
            None
        },
        test_message_rsmu: Some(OpcodeSpec::rsmu(0x01)),
        test_message_mp1: Some(OpcodeSpec::mp1(0x01)),

        get_co_per_core: if codename == Codename::Cezanne {
            Some(OpcodeSpec::rsmu(0xC3))
        } else {
            None
        },
        set_co_per_core: if codename == Codename::Cezanne {
            Some(DualOpcode::mp1_then_rsmu(0x54, 0x52))
        } else {
            None
        },
        set_co_all_cores: if codename == Codename::Cezanne {
            Some(DualOpcode::mp1_then_rsmu(0x55, 0xB1))
        } else {
            None
        },
        get_igpu_co: if codename == Codename::Cezanne {
            Some(OpcodeSpec::rsmu(0xC6))
        } else {
            None
        },
        set_igpu_co: if codename == Codename::Cezanne {
            Some(OpcodeSpec::rsmu(0x53))
        } else {
            None
        },
        get_curve_shaper: None,
        set_curve_shaper: None,

        set_ppt: Some(DualOpcode::mp1_then_rsmu(0x15, 0x32)),
        set_tdc: Some(DualOpcode::mp1_then_rsmu(0x1A, 0x38)),
        set_edc: Some(DualOpcode::mp1_then_rsmu(0x1C, 0x3A)),
        set_tctl: Some(DualOpcode::mp1_then_rsmu(0x19, 0x37)),
        get_pbo_scalar: Some(OpcodeSpec::rsmu(0x0F)),
        set_pbo_scalar: Some(DualOpcode::mp1_then_rsmu(0x49, 0x3F)),
        set_pbo_en: None,
        get_fmax: Some(OpcodeSpec::rsmu(0x42)),
        set_fmax: None,

        get_is_overclockable: Some(OpcodeSpec::rsmu(0x82)),
        get_expo_active: None,
        get_ln2_mode: None,
        get_perf_data: None,
        get_fused_ppt: Some(OpcodeSpec::rsmu(0x11)),
        get_fused_slow: Some(OpcodeSpec::rsmu(0x12)),
        get_fused_fast: Some(OpcodeSpec::rsmu(0x13)),
        get_fused_apu_slow: Some(OpcodeSpec::rsmu(0x14)),
        get_fused_vdd_tdc: Some(OpcodeSpec::rsmu(0x15)),
        get_fused_edc: None,
        get_fused_soc_tdc: Some(OpcodeSpec::rsmu(0x16)),
        get_sustained_power_thm: Some(OpcodeSpec::mp1(0x5B)),

        set_stapm_limit: Some(DualOpcode::mp1_then_rsmu(0x14, 0x31)),
        set_stapm_time: Some(DualOpcode::mp1_then_rsmu(0x18, 0x36)),
        set_slow_limit: Some(DualOpcode::mp1_then_rsmu(0x16, 0x33)),
        set_slow_time: Some(DualOpcode::mp1_then_rsmu(0x17, 0x35)),
        set_apu_slow_limit: Some(DualOpcode::mp1_then_rsmu(0x21, 0x34)),
        set_tdc_soc: Some(DualOpcode::mp1_then_rsmu(0x1B, 0x39)),
        set_edc_soc: Some(DualOpcode::mp1_then_rsmu(0x1D, 0x3B)),
        set_apu_skin_temp: Some(DualOpcode::mp1_then_rsmu(0x38, 0x91)),
        set_dgpu_skin_temp: Some(DualOpcode::mp1_then_rsmu(0x39, 0x92)),
        set_max_performance: Some(OpcodeSpec::mp1(0x11)),
        set_power_saving: Some(OpcodeSpec::mp1(0x12)),
    }
}

fn apu1_vangogh(codename: Codename) -> CpuCapabilities {
    let mut caps = apu1_base(codename);
    caps.is_family_17h = true;
    caps.rsp_addrs.mp1_rsp = Some(0x03B1_0578);
    caps.co_bounds = Some(CoBounds {
        min: -30,
        max: 30,
        soft_min: -25,
    });
    caps.set_co_per_core = Some(DualOpcode::mp1_only(0x4B));
    caps.set_co_all_cores = Some(DualOpcode::mp1_only(0x4C));
    caps.set_apu_slow_limit = Some(DualOpcode::mp1_then_rsmu(0x23, 0x34));
    caps.set_apu_skin_temp = Some(DualOpcode::mp1_then_rsmu(0x33, 0x91));
    caps.set_dgpu_skin_temp = Some(DualOpcode::mp1_then_rsmu(0x34, 0x92));
    caps.get_sustained_power_thm = Some(OpcodeSpec::mp1(0x54));
    caps
}

fn apu1_phoenix(codename: Codename) -> CpuCapabilities {
    let mut caps = apu1_base(codename);
    caps.is_family_17h = false;
    caps.rsp_addrs.mp1_rsp = Some(0x03B1_0578);
    caps.co_bounds = Some(CoBounds {
        min: -50,
        max: 50,
        soft_min: -35,
    });
    caps.get_co_per_core = Some(OpcodeSpec::rsmu(0xE1));
    caps.set_co_per_core = Some(DualOpcode::rsmu_then_mp1(0x53, 0x4B));
    caps.set_co_all_cores = Some(DualOpcode::rsmu_then_mp1(0x5D, 0x4C));
    caps.get_igpu_co = Some(OpcodeSpec::rsmu(0x20));
    caps.set_igpu_co = Some(OpcodeSpec::rsmu(0x1F));
    caps.set_pbo_scalar = Some(DualOpcode::rsmu_then_mp1(0x3E, 0x63));
    caps.set_fmax = Some(OpcodeSpec::rsmu(0x47));
    caps.get_expo_active = Some(OpcodeSpec::rsmu(0xDB));
    caps.get_ln2_mode = Some(OpcodeSpec::rsmu(0xC4));
    caps.get_perf_data = Some(OpcodeSpec::rsmu(0x0B));
    caps.set_apu_slow_limit = Some(DualOpcode::rsmu_then_mp1(0x34, 0x23));
    caps.set_apu_skin_temp = Some(DualOpcode::rsmu_then_mp1(0x91, 0x33));
    caps.set_dgpu_skin_temp = Some(DualOpcode::rsmu_then_mp1(0x92, 0x34));
    caps.get_sustained_power_thm = Some(OpcodeSpec::mp1(0x5F));
    caps
}

fn apu1_rembrandt(codename: Codename) -> CpuCapabilities {
    let mut caps = apu1_phoenix(codename);
    caps.co_bounds = Some(CoBounds {
        min: -30,
        max: 30,
        soft_min: -25,
    });
    caps.get_co_per_core = Some(OpcodeSpec::rsmu(0x2F));
    caps.get_igpu_co = Some(OpcodeSpec::rsmu(0x30));
    caps.set_igpu_co = Some(OpcodeSpec::rsmu(0xB7));
    caps
}

/// Zen 5 Mobile APUs: Strix Point, Krackan Point (mapped to StrixPoint by `ryzen_smu`),
/// and Strix Halo.
///
/// Spec-implemented from `ryzen_smu` / `ZenStates-Core` (MP1 RSP `0x03B1_0978`,
/// RSMU `GetDldoPsmMargin = 0xAF`, CO `-50..+50`, full APU DPTC) — community report
/// welcome via `zensmu report --raw-pm-table --open`.
fn apu1_strix(codename: Codename) -> CpuCapabilities {
    let mut caps = apu1_phoenix(codename);
    caps.rsp_addrs.mp1_rsp = Some(0x03B1_0978);
    caps.get_co_per_core = Some(OpcodeSpec::rsmu(0xAF));
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granite_ridge_opcodes_match_zen5_spec() {
        let caps = capabilities_for(Codename::GraniteRidge).unwrap();
        assert_eq!(caps.get_co_per_core, Some(OpcodeSpec::rsmu(0xD5)));
        assert_eq!(
            caps.set_co_per_core,
            Some(DualOpcode::rsmu_then_mp1(0x06, 0x35))
        );
        assert_eq!(
            caps.set_co_all_cores,
            Some(DualOpcode::rsmu_then_mp1(0x07, 0x36))
        );
        assert_eq!(caps.get_curve_shaper, Some(OpcodeSpec::rsmu(0x84)));
        assert_eq!(caps.set_curve_shaper, Some(OpcodeSpec::rsmu(0xA6)));
        assert_eq!(caps.set_ppt, Some(DualOpcode::rsmu_then_mp1(0x56, 0x3E)));
        assert_eq!(caps.set_tdc, Some(DualOpcode::rsmu_then_mp1(0x57, 0x3C)));
        assert_eq!(caps.set_edc, Some(DualOpcode::rsmu_then_mp1(0x58, 0x3D)));
        assert_eq!(caps.set_tctl, Some(DualOpcode::rsmu_then_mp1(0x59, 0x3F)));
        assert_eq!(caps.rsp_addrs.rsmu_rsp, Some(0x03B1_0570));
    }

    #[test]
    fn raphael_has_no_curve_shaper() {
        let caps = capabilities_for(Codename::Raphael).unwrap();
        assert!(caps.get_curve_shaper.is_none());
        assert!(caps.set_curve_shaper.is_none());
        assert_eq!(caps.get_co_per_core, Some(OpcodeSpec::rsmu(0xD5)));
    }

    #[test]
    fn unknown_codename_returns_none() {
        assert!(capabilities_for(Codename::Undefined).is_none());
        assert!(capabilities_for(Codename::Naples).is_none());
    }

    #[test]
    fn cezanne_and_extended_zen_profiles_have_verified_opcodes() {
        let cez = capabilities_for(Codename::Cezanne).unwrap();
        assert_eq!(cez.get_igpu_co, Some(OpcodeSpec::rsmu(0xC6)));

        let vg = capabilities_for(Codename::VanGogh).unwrap();
        assert!(vg.is_family_17h);
        assert_eq!(vg.rsp_addrs.mp1_rsp, Some(0x03B1_0578));
        assert_eq!(vg.set_co_per_core, Some(DualOpcode::mp1_only(0x4B)));
        assert_eq!(vg.set_co_all_cores, Some(DualOpcode::mp1_only(0x4C)));

        let pic = capabilities_for(Codename::Picasso).unwrap();
        assert_eq!(
            pic.set_stapm_limit,
            Some(DualOpcode::mp1_then_rsmu(0x1A, 0x2E))
        );
        assert_eq!(
            pic.set_apu_slow_limit,
            Some(DualOpcode::mp1_then_rsmu(0x54, 0x75))
        );
        assert_eq!(pic.get_fmax, Some(OpcodeSpec::rsmu(0x86)));

        let pr = capabilities_for(Codename::PinnacleRidge).unwrap();
        assert_eq!(pr.rsp_addrs.rsmu_rsp, Some(0x03B1_0568));
        assert_eq!(pr.set_ppt, Some(DualOpcode::rsmu_only(0x64)));

        let cf = capabilities_for(Codename::Colfax).unwrap();
        assert_eq!(cf.set_tdc, Some(DualOpcode::rsmu_only(0x6B)));
        assert_eq!(cf.get_pbo_scalar, Some(OpcodeSpec::rsmu(0x70)));

        let sr = capabilities_for(Codename::SummitRidge).unwrap();
        assert_eq!(sr.set_ppt, Some(DualOpcode::rsmu_then_mp1(0x58, 0x31)));
        assert_eq!(sr.set_edc, Some(DualOpcode::mp1_only(0x2A)));
        assert_eq!(sr.get_sustained_power_thm, Some(OpcodeSpec::rsmu(0x5F)));
    }

    #[test]
    fn sustained_power_and_thm_opcodes_match_platform_profiles() {
        assert_eq!(
            capabilities_for(Codename::SummitRidge)
                .unwrap()
                .get_sustained_power_thm,
            Some(OpcodeSpec::rsmu(0x5F))
        );
        assert_eq!(
            capabilities_for(Codename::ThreadRipper)
                .unwrap()
                .get_sustained_power_thm,
            Some(OpcodeSpec::rsmu(0x5F))
        );
        for codename in [
            Codename::Matisse,
            Codename::CastlePeak,
            Codename::Vermeer,
            Codename::Chagall,
            Codename::Raphael,
            Codename::StormPeak,
            Codename::GraniteRidge,
        ] {
            assert_eq!(
                capabilities_for(codename).unwrap().get_sustained_power_thm,
                Some(OpcodeSpec::mp1(0x23)),
                "{codename:?}"
            );
        }
        for codename in [
            Codename::RavenRidge,
            Codename::RavenRidge2,
            Codename::Dali,
            Codename::Picasso,
        ] {
            assert_eq!(
                capabilities_for(codename).unwrap().get_sustained_power_thm,
                Some(OpcodeSpec::mp1(0x43)),
                "{codename:?}"
            );
        }
        for codename in [Codename::Renoir, Codename::Lucienne, Codename::Cezanne] {
            assert_eq!(
                capabilities_for(codename).unwrap().get_sustained_power_thm,
                Some(OpcodeSpec::mp1(0x5B)),
                "{codename:?}"
            );
        }
        assert_eq!(
            capabilities_for(Codename::VanGogh)
                .unwrap()
                .get_sustained_power_thm,
            Some(OpcodeSpec::mp1(0x54))
        );
        for codename in [
            Codename::Phoenix,
            Codename::HawkPoint,
            Codename::Rembrandt,
            Codename::StrixPoint,
            Codename::StrixHalo,
        ] {
            assert_eq!(
                capabilities_for(codename).unwrap().get_sustained_power_thm,
                Some(OpcodeSpec::mp1(0x5F)),
                "{codename:?}"
            );
        }
        assert_eq!(
            capabilities_for(Codename::PinnacleRidge)
                .unwrap()
                .get_sustained_power_thm,
            None
        );
        assert_eq!(
            capabilities_for(Codename::Colfax)
                .unwrap()
                .get_sustained_power_thm,
            None
        );
    }

    #[test]
    fn strix_point_and_strix_halo_profiles_match_zen5_apu_spec() {
        for codename in [Codename::StrixPoint, Codename::StrixHalo] {
            let caps = capabilities_for(codename).unwrap();
            assert!(!caps.is_family_17h);
            assert_eq!(caps.rsp_addrs.rsmu_rsp, Some(0x03B1_0A80));
            assert_eq!(caps.rsp_addrs.mp1_rsp, Some(0x03B1_0978));
            assert_eq!(
                caps.co_bounds,
                Some(CoBounds {
                    min: -50,
                    max: 50,
                    soft_min: -35,
                })
            );
            assert_eq!(caps.get_co_per_core, Some(OpcodeSpec::rsmu(0xAF)));
            assert_eq!(
                caps.set_co_per_core,
                Some(DualOpcode::rsmu_then_mp1(0x53, 0x4B))
            );
            assert_eq!(
                caps.set_co_all_cores,
                Some(DualOpcode::rsmu_then_mp1(0x5D, 0x4C))
            );
            assert_eq!(caps.get_igpu_co, Some(OpcodeSpec::rsmu(0x20)));
            assert_eq!(caps.set_igpu_co, Some(OpcodeSpec::rsmu(0x1F)));
            assert!(caps.get_curve_shaper.is_none());
            assert!(caps.set_curve_shaper.is_none());
            assert!(caps.set_stapm_limit.is_some());
            assert!(caps.set_apu_skin_temp.is_some());
        }
    }
}
