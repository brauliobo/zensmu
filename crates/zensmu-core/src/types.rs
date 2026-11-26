use serde::{Deserialize, Serialize};
use std::fmt;

/// Processor codenames matching `enum smu_processor_codename` in `ryzen_smu/smu.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Codename {
    Undefined = 0,
    Colfax = 1,
    Renoir = 2,
    Picasso = 3,
    Matisse = 4,
    ThreadRipper = 5,
    CastlePeak = 6,
    RavenRidge = 7,
    RavenRidge2 = 8,
    SummitRidge = 9,
    PinnacleRidge = 10,
    Rembrandt = 11,
    Vermeer = 12,
    VanGogh = 13,
    Cezanne = 14,
    Milan = 15,
    Dali = 16,
    Lucienne = 17,
    Naples = 18,
    Chagall = 19,
    Raphael = 20,
    Phoenix = 21,
    StrixPoint = 22,
    GraniteRidge = 23,
    HawkPoint = 24,
    StormPeak = 25,
    StrixHalo = 26,
}

impl Codename {
    pub fn from_driver_id(id: u32) -> Option<Self> {
        match id {
            0 => Some(Self::Undefined),
            1 => Some(Self::Colfax),
            2 => Some(Self::Renoir),
            3 => Some(Self::Picasso),
            4 => Some(Self::Matisse),
            5 => Some(Self::ThreadRipper),
            6 => Some(Self::CastlePeak),
            7 => Some(Self::RavenRidge),
            8 => Some(Self::RavenRidge2),
            9 => Some(Self::SummitRidge),
            10 => Some(Self::PinnacleRidge),
            11 => Some(Self::Rembrandt),
            12 => Some(Self::Vermeer),
            13 => Some(Self::VanGogh),
            14 => Some(Self::Cezanne),
            15 => Some(Self::Milan),
            16 => Some(Self::Dali),
            17 => Some(Self::Lucienne),
            18 => Some(Self::Naples),
            19 => Some(Self::Chagall),
            20 => Some(Self::Raphael),
            21 => Some(Self::Phoenix),
            22 => Some(Self::StrixPoint),
            23 => Some(Self::GraniteRidge),
            24 => Some(Self::HawkPoint),
            25 => Some(Self::StormPeak),
            26 => Some(Self::StrixHalo),
            _ => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Undefined => "undefined",
            Self::Colfax => "colfax",
            Self::Renoir => "renoir",
            Self::Picasso => "picasso",
            Self::Matisse => "matisse",
            Self::ThreadRipper => "threadripper",
            Self::CastlePeak => "castle-peak",
            Self::RavenRidge => "raven-ridge",
            Self::RavenRidge2 => "raven-ridge-2",
            Self::SummitRidge => "summit-ridge",
            Self::PinnacleRidge => "pinnacle-ridge",
            Self::Rembrandt => "rembrandt",
            Self::Vermeer => "vermeer",
            Self::VanGogh => "van-gogh",
            Self::Cezanne => "cezanne",
            Self::Milan => "milan",
            Self::Dali => "dali",
            Self::Lucienne => "lucienne",
            Self::Naples => "naples",
            Self::Chagall => "chagall",
            Self::Raphael => "raphael",
            Self::Phoenix => "phoenix",
            Self::StrixPoint => "strix-point",
            Self::GraniteRidge => "granite-ridge",
            Self::HawkPoint => "hawk-point",
            Self::StormPeak => "storm-peak",
            Self::StrixHalo => "strix-halo",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Undefined => "Undefined",
            Self::Colfax => "Colfax (Zen+ TR)",
            Self::Renoir => "Renoir (Zen 2 APU)",
            Self::Picasso => "Picasso (Zen+ APU)",
            Self::Matisse => "Matisse (Zen 2 Desktop)",
            Self::ThreadRipper => "Threadripper (Zen)",
            Self::CastlePeak => "Castle Peak (Zen 2 TR)",
            Self::RavenRidge => "Raven Ridge (Zen APU)",
            Self::RavenRidge2 => "Raven Ridge 2 (Zen APU)",
            Self::SummitRidge => "Summit Ridge (Zen)",
            Self::PinnacleRidge => "Pinnacle Ridge (Zen+)",
            Self::Rembrandt => "Rembrandt (Zen 3+ APU)",
            Self::Vermeer => "Vermeer (Zen 3 Desktop)",
            Self::VanGogh => "Van Gogh (Zen 2 APU)",
            Self::Cezanne => "Cezanne (Zen 3 APU)",
            Self::Milan => "Milan (Zen 3 EPYC)",
            Self::Dali => "Dali (Zen APU)",
            Self::Lucienne => "Lucienne (Zen 2 APU)",
            Self::Naples => "Naples (Zen EPYC)",
            Self::Chagall => "Chagall (Zen 3 TR)",
            Self::Raphael => "Raphael (Zen 4 Desktop)",
            Self::Phoenix => "Phoenix (Zen 4 APU)",
            Self::StrixPoint => "Strix Point (Zen 5 APU)",
            Self::GraniteRidge => "Granite Ridge (Zen 5 Desktop)",
            Self::HawkPoint => "Hawk Point (Zen 4 APU)",
            Self::StormPeak => "Storm Peak (Zen 4 TR)",
            Self::StrixHalo => "Strix Halo (Zen 5 APU)",
        }
    }

    pub fn is_apu(self) -> bool {
        matches!(
            self,
            Self::Renoir
                | Self::Picasso
                | Self::RavenRidge
                | Self::RavenRidge2
                | Self::Rembrandt
                | Self::VanGogh
                | Self::Cezanne
                | Self::Dali
                | Self::Lucienne
                | Self::Phoenix
                | Self::StrixPoint
                | Self::HawkPoint
                | Self::StrixHalo
        )
    }

    pub fn is_zen4_or_newer(self) -> bool {
        matches!(
            self,
            Self::Raphael
                | Self::Phoenix
                | Self::HawkPoint
                | Self::StormPeak
                | Self::GraniteRidge
                | Self::StrixPoint
                | Self::StrixHalo
        )
    }

    pub fn is_zen5(self) -> bool {
        matches!(self, Self::GraniteRidge | Self::StrixPoint | Self::StrixHalo)
    }
}

impl fmt::Display for Codename {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// SMU mailbox channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mailbox {
    Rsmu,
    Mp1,
}

impl Mailbox {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rsmu => "RSMU",
            Self::Mp1 => "MP1",
        }
    }

    pub fn sysfs_file(self) -> &'static str {
        match self {
            Self::Rsmu => "rsmu_cmd",
            Self::Mp1 => "mp1_smu_cmd",
        }
    }
}

/// 6 x 32-bit SMU command arguments.
pub type SmuArgs = [u32; 6];

/// A planned SMU write operation, used for `--dry-run` and audit logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedOp {
    pub description: String,
    pub mailbox: Mailbox,
    pub opcode: u32,
    pub args: SmuArgs,
    pub current: Option<String>,
    pub target: String,
}

/// Curve Shaper frequency tier (Zen 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FreqTier {
    Min = 0,
    Low = 1,
    Med = 2,
    High = 3,
    Max = 4,
}

impl FreqTier {
    pub const ALL: [Self; 5] = [Self::Min, Self::Low, Self::Med, Self::High, Self::Max];

    pub fn as_u32(self) -> u32 {
        self as u32
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Low => "low",
            Self::Med => "med",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "min" | "minimum" | "0" => Some(Self::Min),
            "low" | "1" => Some(Self::Low),
            "med" | "medium" | "2" => Some(Self::Med),
            "high" | "3" => Some(Self::High),
            "max" | "maximum" | "4" => Some(Self::Max),
            _ => None,
        }
    }
}

impl fmt::Display for FreqTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Curve Shaper temperature band (Zen 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TempBand {
    Low,
    Med,
    High,
}

impl TempBand {
    pub const ALL: [Self; 3] = [Self::Low, Self::Med, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Med => "med",
            Self::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "med" | "medium" => Some(Self::Med),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// Three temperature-band margins for a single Curve Shaper frequency tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShaperTierMargins {
    pub low: i8,
    pub med: i8,
    pub high: i8,
}

/// Full 5x3 Zen 5 Curve Shaper matrix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurveShaperState {
    pub min: ShaperTierMargins,
    pub low: ShaperTierMargins,
    pub med: ShaperTierMargins,
    pub high: ShaperTierMargins,
    pub max: ShaperTierMargins,
}

impl CurveShaperState {
    pub fn tier(&self, t: FreqTier) -> ShaperTierMargins {
        match t {
            FreqTier::Min => self.min,
            FreqTier::Low => self.low,
            FreqTier::Med => self.med,
            FreqTier::High => self.high,
            FreqTier::Max => self.max,
        }
    }

    pub fn tier_mut(&mut self, t: FreqTier) -> &mut ShaperTierMargins {
        match t {
            FreqTier::Min => &mut self.min,
            FreqTier::Low => &mut self.low,
            FreqTier::Med => &mut self.med,
            FreqTier::High => &mut self.high,
            FreqTier::Max => &mut self.max,
        }
    }
}

/// Physical core addressing for SMU core masks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreInfo {
    /// Zero-based sequential logical core index (0..physical_cores).
    pub index: u32,
    /// CCD index (0..).
    pub ccd: u32,
    /// CCX index within CCD (0 for Zen 3/4/5).
    pub ccx: u32,
    /// Physical core index within CCD/CCX.
    pub core_in_ccd: u32,
    /// Linux logical CPU IDs (SMT siblings) mapped to this physical core.
    pub linux_cpus: Vec<u32>,
}

impl CoreInfo {
    /// Compute the SMU core mask (`ZenStates-Core` `MakeCoreMask`).
    /// - Zen 3/4/5 (`family > 0x17`): `(ccd << 28) | ((core_in_ccd % 8) << 20)`
    /// - Zen 2 (`family == 0x17`): `(ccd << 28) | ((ccx % 2) << 24) | ((core_in_ccd % 4) << 20)`
    pub fn smu_mask(&self, is_family_17h: bool, is_apu: bool) -> u32 {
        if is_apu {
            self.index
        } else if is_family_17h {
            (self.ccd << 28) | ((self.ccx % 2) << 24) | ((self.core_in_ccd % 4) << 20)
        } else {
            (self.ccd << 28) | ((self.core_in_ccd % 8) << 20)
        }
    }
}

/// APU power/performance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApuPowerMode {
    MaxPerformance,
    PowerSaving,
}
