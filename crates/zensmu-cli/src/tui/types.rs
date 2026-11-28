//! View state, interactive tuning model, confirmation modal, and mailbox load tracking.

use ratatui::style::Color;
use zensmu_core::error::Error;
use zensmu_core::ops::apu::ApuSetRequest;
use zensmu_core::ops::pbo::PboSetRequest;
use zensmu_core::safety::snapshot::SmuStateSnapshot;
use zensmu_core::types::{ApuPowerMode, FreqTier, PlannedOp, ShaperTierMargins, TempBand};

// ---------------------------------------------------------------------------
// View state, interactive tuning model & mailbox load tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewTab {
    Overview,
    ClocksVolts,
    CoShaper,
    Tune,
}

impl ViewTab {
    pub(super) fn next(self) -> Self {
        match self {
            Self::Overview => Self::ClocksVolts,
            Self::ClocksVolts => Self::CoShaper,
            Self::CoShaper => Self::Tune,
            Self::Tune => Self::Overview,
        }
    }

    pub(super) fn prev(self) -> Self {
        match self {
            Self::Overview => Self::Tune,
            Self::ClocksVolts => Self::Overview,
            Self::CoShaper => Self::ClocksVolts,
            Self::Tune => Self::CoShaper,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlashKind {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TuneSection {
    CurveOptimizer,
    CurveShaper,
    Pbo,
    Apu,
}

impl TuneSection {
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::CurveOptimizer => "Curve Optimizer",
            Self::CurveShaper => "Curve Shaper (Zen 5)",
            Self::Pbo => "PBO Limits & Boost",
            Self::Apu => "APU DPTC Controls",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum TuneParam {
    CoAll,
    CoCore(u32),
    CoIgpu,
    Shaper { tier: FreqTier, band: TempBand },
    PboPpt,
    PboTdc,
    PboEdc,
    PboTctl,
    PboScalar,
    PboFmax,
    ApuStapm,
    ApuStapmTime,
    ApuFast,
    ApuSlow,
    ApuSlowTime,
    ApuApuSlow,
    ApuTdcSoc,
    ApuEdcSoc,
    ApuSkinTemp,
    ApuDgpuSkinTemp,
    ApuPowerMode,
}

impl TuneParam {
    pub(super) fn section(self) -> TuneSection {
        match self {
            Self::CoAll | Self::CoCore(_) | Self::CoIgpu => TuneSection::CurveOptimizer,
            Self::Shaper { .. } => TuneSection::CurveShaper,
            Self::PboPpt
            | Self::PboTdc
            | Self::PboEdc
            | Self::PboTctl
            | Self::PboScalar
            | Self::PboFmax => TuneSection::Pbo,
            Self::ApuStapm
            | Self::ApuStapmTime
            | Self::ApuFast
            | Self::ApuSlow
            | Self::ApuSlowTime
            | Self::ApuApuSlow
            | Self::ApuTdcSoc
            | Self::ApuEdcSoc
            | Self::ApuSkinTemp
            | Self::ApuDgpuSkinTemp
            | Self::ApuPowerMode => TuneSection::Apu,
        }
    }

    pub(super) fn is_co_or_shaper(self) -> bool {
        matches!(
            self,
            Self::CoAll | Self::CoCore(_) | Self::CoIgpu | Self::Shaper { .. }
        )
    }

    pub(super) fn sort_key(self) -> (u8, u32, u8) {
        match self {
            Self::CoAll => (0, 0, 0),
            Self::CoCore(idx) => (1, idx, 0),
            Self::CoIgpu => (2, 0, 0),
            Self::Shaper { tier, band } => {
                let b = match band {
                    TempBand::Low => 0,
                    TempBand::Med => 1,
                    TempBand::High => 2,
                };
                (3, tier.as_u32(), b)
            }
            Self::PboPpt => (4, 0, 0),
            Self::PboTdc => (5, 0, 0),
            Self::PboEdc => (6, 0, 0),
            Self::PboTctl => (7, 0, 0),
            Self::PboScalar => (8, 0, 0),
            Self::PboFmax => (9, 0, 0),
            Self::ApuStapm => (10, 0, 0),
            Self::ApuStapmTime => (11, 0, 0),
            Self::ApuFast => (12, 0, 0),
            Self::ApuSlow => (13, 0, 0),
            Self::ApuSlowTime => (14, 0, 0),
            Self::ApuApuSlow => (15, 0, 0),
            Self::ApuTdcSoc => (16, 0, 0),
            Self::ApuEdcSoc => (17, 0, 0),
            Self::ApuSkinTemp => (18, 0, 0),
            Self::ApuDgpuSkinTemp => (19, 0, 0),
            Self::ApuPowerMode => (20, 0, 0),
        }
    }
}

impl PartialOrd for TuneParam {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TuneParam {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

pub(super) fn tune_param_from_clamped_field(field: &str) -> Option<TuneParam> {
    match field {
        "pbo.ppt_w" => Some(TuneParam::PboPpt),
        "pbo.tdc_a" => Some(TuneParam::PboTdc),
        "pbo.edc_a" => Some(TuneParam::PboEdc),
        "pbo.tctl_c" => Some(TuneParam::PboTctl),
        "apu.stapm_w" => Some(TuneParam::ApuStapm),
        "apu.fast_w" => Some(TuneParam::ApuFast),
        "apu.slow_w" => Some(TuneParam::ApuSlow),
        "apu.apu_slow_w" => Some(TuneParam::ApuApuSlow),
        "apu.tdc_soc_a" => Some(TuneParam::ApuTdcSoc),
        "apu.edc_soc_a" => Some(TuneParam::ApuEdcSoc),
        "apu.apu_skin_temp_c" => Some(TuneParam::ApuSkinTemp),
        "apu.dgpu_skin_temp_c" => Some(TuneParam::ApuDgpuSkinTemp),
        _ => None,
    }
}

pub(super) fn param_unit(param: TuneParam) -> &'static str {
    match param {
        TuneParam::PboPpt
        | TuneParam::ApuStapm
        | TuneParam::ApuFast
        | TuneParam::ApuSlow
        | TuneParam::ApuApuSlow => "W",
        TuneParam::PboTdc | TuneParam::PboEdc | TuneParam::ApuTdcSoc | TuneParam::ApuEdcSoc => "A",
        TuneParam::PboTctl | TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => "°C",
        TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => "s",
        TuneParam::PboScalar => "x",
        TuneParam::PboFmax => "MHz",
        TuneParam::CoAll
        | TuneParam::CoCore(_)
        | TuneParam::CoIgpu
        | TuneParam::Shaper { .. }
        | TuneParam::ApuPowerMode => "",
    }
}

pub(super) fn format_board_cap(param: TuneParam, cap: u32) -> String {
    let unit = param_unit(param);
    if unit.is_empty() {
        format!("board cap ≤{cap}")
    } else {
        format!("board cap ≤{cap} {unit}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TuneValue {
    Int(i32),
    PowerMode(Option<ApuPowerMode>),
}

impl TuneValue {
    pub(super) fn as_int(self) -> Option<i32> {
        match self {
            Self::Int(v) => Some(v),
            Self::PowerMode(_) => None,
        }
    }

    pub(super) fn as_power_mode(self) -> Option<ApuPowerMode> {
        match self {
            Self::PowerMode(m) => m,
            Self::Int(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum TuneRow {
    SectionHeader(String, String),
    InfoBanner(String, Color),
    Param(TuneParam),
}

#[derive(Debug, Clone)]
pub(super) enum ConfirmAction {
    ApplyStaged {
        co_all: Option<i32>,
        co_cores: Vec<(u32, i32)>,
        co_igpu: Option<i32>,
        shaper_tiers: Vec<(FreqTier, ShaperTierMargins)>,
        pbo: Option<PboSetRequest>,
        apu: Option<ApuSetRequest>,
    },
    Rollback(SmuStateSnapshot),
    ResetBootBaseline(SmuStateSnapshot),
    ResetCoZero,
    PersistLiveAndEnable,
    ToggleBootService {
        enable: bool,
    },
    ClearBootGuard,
}

impl ConfirmAction {
    pub(super) fn is_persist_action(&self) -> bool {
        matches!(
            self,
            Self::PersistLiveAndEnable | Self::ToggleBootService { .. } | Self::ClearBootGuard
        )
    }
}

#[derive(Debug, Clone)]
pub(super) struct ConfirmModal {
    pub(super) title: String,
    pub(super) subtitle: String,
    pub(super) action: ConfirmAction,
    pub(super) ops: Vec<PlannedOp>,
    pub(super) steps: Vec<(String, String)>,
    pub(super) scroll: usize,
    pub(super) allow_extreme: bool,
}

impl ConfirmModal {
    pub(super) fn ops_modal(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        action: ConfirmAction,
        ops: Vec<PlannedOp>,
        allow_extreme: bool,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            action,
            ops,
            steps: Vec::new(),
            scroll: 0,
            allow_extreme,
        }
    }

    pub(super) fn steps_modal(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        action: ConfirmAction,
        steps: Vec<(String, String)>,
        allow_extreme: bool,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            action,
            ops: Vec::new(),
            steps,
            scroll: 0,
            allow_extreme,
        }
    }

    pub(super) fn item_count(&self) -> usize {
        if self.steps.is_empty() {
            self.ops.len()
        } else {
            self.steps.len()
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum MailboxState<T> {
    Loaded(T),
    PermissionDenied,
    Unsupported,
    FirmwareUnsupported(String),
    Error(String),
}

impl<T> MailboxState<T> {
    pub(super) fn from_result(res: zensmu_core::Result<T>) -> Self {
        match res {
            Ok(v) => Self::Loaded(v),
            Err(Error::PermissionDenied { .. }) => Self::PermissionDenied,
            Err(Error::UnsupportedCapability { .. }) => Self::Unsupported,
            Err(Error::SmuRejected { status, .. })
                if status.contains("0xFE") || status.contains("0xFD") =>
            {
                let code = if status.contains("0xFE") {
                    "0xFE"
                } else {
                    "0xFD"
                };
                Self::FirmwareUnsupported(code.to_string())
            }
            Err(err) => Self::Error(err.to_string()),
        }
    }

    pub(super) fn as_loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(v) => Some(v),
            _ => None,
        }
    }

    pub(super) fn is_permission_denied(&self) -> bool {
        matches!(self, Self::PermissionDenied)
    }
}
