use crate::types::Codename;

/// Verified byte offsets for a specific `(Codename, pm_table_version)` pair
/// (cross-referenced against `ZenStates-Core/Hardware/PowerTable.cs`).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PmTableLayout {
    pub(super) min_size: usize,
    pub(super) ppt_limit_w: Option<usize>,
    pub(super) ppt_value_w: Option<usize>,
    pub(super) tdc_limit_a: Option<usize>,
    pub(super) tdc_value_a: Option<usize>,
    pub(super) tctl_limit_c: Option<usize>,
    pub(super) tctl_value_c: Option<usize>,
    pub(super) edc_limit_a: Option<usize>,
    pub(super) edc_value_a: Option<usize>,
    pub(super) stapm_limit_w: Option<usize>,
    pub(super) stapm_value_w: Option<usize>,
    pub(super) slow_limit_w: Option<usize>,
    pub(super) slow_value_w: Option<usize>,
    pub(super) apu_slow_limit_w: Option<usize>,
    pub(super) apu_slow_value_w: Option<usize>,
    pub(super) tdc_soc_limit_a: Option<usize>,
    pub(super) tdc_soc_value_a: Option<usize>,
    pub(super) edc_soc_limit_a: Option<usize>,
    pub(super) edc_soc_value_a: Option<usize>,
    pub(super) apu_skin_temp_limit_c: Option<usize>,
    pub(super) apu_skin_temp_value_c: Option<usize>,
    pub(super) dgpu_skin_temp_limit_c: Option<usize>,
    pub(super) dgpu_skin_temp_value_c: Option<usize>,
    pub(super) vddcr_soc_v: Option<usize>,
    pub(super) vddcr_misc_v: Option<usize>,
    pub(super) fclk_mhz: Option<usize>,
    pub(super) uclk_mhz: Option<usize>,
    pub(super) mclk_mhz: Option<usize>,
    pub(super) cldo_vddp_v: Option<usize>,
    pub(super) cldo_vddg_iod_v: Option<usize>,
    pub(super) cldo_vddg_ccd_v: Option<usize>,
}

/// Standard mobile APU PM table header offsets (`0x000..=0x054`), shared by
/// Renoir/Lucienne, Cezanne, Rembrandt, and Phoenix/HawkPoint (`0x004C_0006+`).
fn apu_std_header(min_size: usize) -> PmTableLayout {
    PmTableLayout {
        min_size,
        stapm_limit_w: Some(0x000),
        stapm_value_w: Some(0x004),
        ppt_limit_w: Some(0x008),
        ppt_value_w: Some(0x00C),
        slow_limit_w: Some(0x010),
        slow_value_w: Some(0x014),
        apu_slow_limit_w: Some(0x018),
        apu_slow_value_w: Some(0x01C),
        tdc_limit_a: Some(0x020),
        tdc_value_a: Some(0x024),
        tdc_soc_limit_a: Some(0x028),
        tdc_soc_value_a: Some(0x02C),
        edc_limit_a: Some(0x030),
        edc_value_a: Some(0x034),
        edc_soc_limit_a: Some(0x038),
        edc_soc_value_a: Some(0x03C),
        tctl_limit_c: Some(0x040),
        tctl_value_c: Some(0x044),
        apu_skin_temp_limit_c: Some(0x048),
        apu_skin_temp_value_c: Some(0x04C),
        dgpu_skin_temp_limit_c: Some(0x050),
        dgpu_skin_temp_value_c: Some(0x054),
        ..PmTableLayout::default()
    }
}

pub(super) fn layout_for(codename: Codename, version: u32) -> Option<PmTableLayout> {
    match codename {
        Codename::GraniteRidge => {
            // 1-CCD (9600X/9700X/9800X3D): 0x724 bytes — 0x0062_0105 hardware-verified on Ryzen 7 9700X;
            // 0x0062_1101/0x0062_1102 and 2-CCD (9900X/9950X/9900X3D/9950X3D/Fire Range, 0x994 bytes)
            // are spec-implemented (community reports welcome via `zensmu report --raw-pm-table --open`).
            let min_size = match version {
                0x0062_0105 | 0x0062_1101 | 0x0062_1102 => 0x724,
                0x0062_0205 | 0x0062_1201 | 0x0062_1202 => 0x994,
                _ => return None,
            };
            Some(PmTableLayout {
                min_size,
                // Verified live on Ryzen 7 9700X (Granite Ridge, PM table 0x620105, 0x724 bytes):
                // 0x000..0x004 STAPM (0 on desktop), 0x008/0x00C PPT, 0x020/0x024 TDC VDD,
                // 0x028/0x02C THM/Tctl, 0x0FC/0x100 EDC VDD.
                ppt_limit_w: Some(0x008),
                ppt_value_w: Some(0x00C),
                tdc_limit_a: Some(0x020),
                tdc_value_a: Some(0x024),
                tctl_limit_c: Some(0x028),
                tctl_value_c: Some(0x02C),
                edc_limit_a: Some(0x0FC),
                edc_value_a: Some(0x100),
                vddcr_soc_v: Some(0x0D8),
                vddcr_misc_v: Some(0x0EC),
                fclk_mhz: Some(0x11C),
                uclk_mhz: Some(0x12C),
                mclk_mhz: Some(0x13C),
                cldo_vddg_iod_v: Some(0x40C),
                cldo_vddg_ccd_v: Some(0x414),
                cldo_vddp_v: Some(0x434),
                ..PmTableLayout::default()
            })
        }
        Codename::Raphael => {
            // Verified live on Ryzen 9 7950X3D (Raphael, PM table 0x00540004, 0x8BC = 2236 B,
            // GitHub issue #3) and cross-referenced against SMUDebugTool PowerLimitMap,
            // ZenStates-Core PowerTable.cs, RyzenAdj, and amkillam/ryzen_smu:
            // - SVI3 block is 3 rails x 0x14 B (VDD, SoC, MISC); EDC sits immediately after SVI3:
            //   - 0x0054_0004/0005 & 0x0054_0104/0105: SVI3 @ 0xB8..0xF0 -> EDC @ 0x0F4/0x0F8
            //   - 0x0054_0108 & 0x0054_0208: EDC @ 0x0F8/0x0FC (0x0054_0208 SVI3 @ 0xBC..0xF4, +4 B)
            //   - 0x0054_0002/0003 & 0x0054_0102/0103: EDC @ 0x0DC/0x0E0
            //   - 0x0054_0000/0001 & 0x0054_0100/0101: early header (PPT 0x000/0x004, TDC 0x008/0x00C,
            //     EDC 0x0A8/0x0AC, Tctl unverified -> None)
            // - Note: Granite Ridge (0x620105) shifts SVI3 to 0xC0..0xF8 and EDC to 0x0FC/0x100.
            let min_size = match version {
                0x0054_0000 => 0x828,
                0x0054_0001 => 0x82C,
                0x0054_0002 => 0x87C,
                0x0054_0003 => 0x89C,
                0x0054_0004 => 0x8BC,
                0x0054_0005 => 0x8C8,
                0x0054_0100 => 0x618,
                0x0054_0101 => 0x61C,
                0x0054_0102 => 0x66C,
                0x0054_0103 => 0x68C,
                0x0054_0104 => 0x6A8,
                0x0054_0105 => 0x6B4,
                0x0054_0108 => 0x6BC,
                0x0054_0208 => 0x8D0,
                _ => return None,
            };
            let (ppt_lim, ppt_val, tdc_lim, tdc_val, tctl, edc_lim, edc_val) = match version {
                0x0054_0000 | 0x0054_0001 | 0x0054_0100 | 0x0054_0101 => {
                    (0x000, 0x004, 0x008, 0x00C, None, 0x0A8, 0x0AC)
                }
                0x0054_0002 | 0x0054_0003 | 0x0054_0102 | 0x0054_0103 => (
                    0x008,
                    0x00C,
                    0x020,
                    0x024,
                    Some((0x028, 0x02C)),
                    0x0DC,
                    0x0E0,
                ),
                0x0054_0004 | 0x0054_0005 | 0x0054_0104 | 0x0054_0105 => (
                    0x008,
                    0x00C,
                    0x020,
                    0x024,
                    Some((0x028, 0x02C)),
                    0x0F4,
                    0x0F8,
                ),
                0x0054_0108 | 0x0054_0208 => (
                    0x008,
                    0x00C,
                    0x020,
                    0x024,
                    Some((0x028, 0x02C)),
                    0x0F8,
                    0x0FC,
                ),
                _ => return None,
            };
            let (vsoc, misc, fclk, uclk, mclk, vddp) = match version {
                0x0054_0208 => (0x0D4, 0x0E8, 0x11C, 0x12C, 0x13C, 0x434),
                _ => (0x0D0, 0x0E4, 0x118, 0x128, 0x138, 0x430),
            };
            Some(PmTableLayout {
                min_size,
                ppt_limit_w: Some(ppt_lim),
                ppt_value_w: Some(ppt_val),
                tdc_limit_a: Some(tdc_lim),
                tdc_value_a: Some(tdc_val),
                tctl_limit_c: tctl.map(|(l, _)| l),
                tctl_value_c: tctl.map(|(_, v)| v),
                edc_limit_a: Some(edc_lim),
                edc_value_a: Some(edc_val),
                vddcr_soc_v: Some(vsoc),
                vddcr_misc_v: Some(misc),
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                cldo_vddp_v: Some(vddp),
                ..PmTableLayout::default()
            })
        }
        Codename::StormPeak => {
            let (min_size, fclk, uclk, mclk, vsoc, misc) = match version {
                0x005C_0302 => (0xD9C, 0x194, 0x1A8, 0x1BC, 0x120, 0x134),
                0x005C_0303 | 0x005C_0402 | 0x005C_0403 => {
                    (0x974, 0x19C, 0x1B0, 0x1C4, 0x128, 0x13C)
                }
                _ => return None,
            };
            Some(PmTableLayout {
                min_size,
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                vddcr_misc_v: Some(misc),
                ..PmTableLayout::default()
            })
        }
        Codename::Vermeer | Codename::Chagall => match version {
            0x002D_0803 | 0x002D_0903 => Some(PmTableLayout {
                min_size: 0x228,
                ppt_limit_w: Some(0x000),
                ppt_value_w: Some(0x004),
                tdc_limit_a: Some(0x008),
                tdc_value_a: Some(0x00C),
                tctl_limit_c: Some(0x010),
                tctl_value_c: Some(0x014),
                edc_limit_a: Some(0x020),
                edc_value_a: Some(0x024),
                vddcr_soc_v: Some(0x0B0),
                fclk_mhz: Some(0x0BC),
                uclk_mhz: Some(0x0C4),
                mclk_mhz: Some(0x0C8),
                cldo_vddp_v: Some(0x220),
                cldo_vddg_iod_v: Some(0x224),
                ..PmTableLayout::default()
            }),
            0x0038_0505 | 0x0038_0605 | 0x0038_0705 | 0x0038_0804 | 0x0038_0805 | 0x0038_0904
            | 0x0038_0905 => Some(PmTableLayout {
                min_size: 0x230,
                ppt_limit_w: Some(0x000),
                ppt_value_w: Some(0x004),
                tdc_limit_a: Some(0x008),
                tdc_value_a: Some(0x00C),
                tctl_limit_c: Some(0x010),
                tctl_value_c: Some(0x014),
                edc_limit_a: Some(0x020),
                edc_value_a: Some(0x024),
                vddcr_soc_v: Some(0x0B4),
                fclk_mhz: Some(0x0C0),
                uclk_mhz: Some(0x0C8),
                mclk_mhz: Some(0x0CC),
                cldo_vddp_v: Some(0x224),
                cldo_vddg_iod_v: Some(0x228),
                cldo_vddg_ccd_v: Some(0x22C),
                ..PmTableLayout::default()
            }),
            _ => None,
        },
        Codename::Matisse | Codename::CastlePeak => {
            let (min_size, vsoc, fclk, uclk, mclk, vddp, vddg_iod) = match version {
                0x0024_0802 | 0x0024_0902 => (0x1F8, 0x0B0, 0x0BC, 0x0C4, 0x0C8, 0x1F0, 0x1F4),
                0x0024_0503 | 0x0024_0603 | 0x0024_0703 | 0x0024_0803 | 0x0024_0903 => {
                    (0x1FC, 0x0B4, 0x0C0, 0x0C8, 0x0CC, 0x1F4, 0x1F8)
                }
                _ => return None,
            };
            Some(PmTableLayout {
                min_size,
                ppt_limit_w: Some(0x000),
                ppt_value_w: Some(0x004),
                tdc_limit_a: Some(0x008),
                tdc_value_a: Some(0x00C),
                tctl_limit_c: Some(0x010),
                tctl_value_c: Some(0x014),
                edc_limit_a: Some(0x020),
                edc_value_a: Some(0x024),
                vddcr_soc_v: Some(vsoc),
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                cldo_vddp_v: Some(vddp),
                cldo_vddg_iod_v: Some(vddg_iod),
                ..PmTableLayout::default()
            })
        }
        Codename::Renoir | Codename::Lucienne => {
            // Verified live on Ryzen 7 5700U (Lucienne, PM table 0x00370005, 0x8C8 bytes)
            // and cross-checked with RyzenAdj + ZenStates-Core PowerTable.cs.
            let (min_size, fclk, uclk, mclk, vsoc) = match version {
                0x0037_0000 => (0x794, 0x4B4, 0x4B8, 0x4BC, 0x190),
                0x0037_0001 => (0x884, 0x5A4, 0x5A8, 0x5AC, 0x190),
                0x0037_0002 => (0x88C, 0x5AC, 0x5B0, 0x5B4, 0x198),
                0x0037_0003 | 0x0037_0004 => (0x8AC, 0x5CC, 0x5D0, 0x5D4, 0x198),
                0x0037_0005 => (0x8C8, 0x5E8, 0x5EC, 0x5F0, 0x198),
                _ => return None,
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                ..apu_std_header(min_size)
            })
        }
        Codename::Cezanne => {
            let (min_size, fclk, uclk, mclk) = match version {
                0x0040_0001 => (0x8D0, 0x624, 0x628, 0x62C),
                0x0040_0002 => (0x928, 0x63C, 0x640, 0x644),
                0x0040_0003 => (0x94C, 0x660, 0x664, 0x668),
                0x0040_0004 | 0x0040_0005 => (0x944, 0x664, 0x668, 0x66C),
                _ => return None,
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(0x19C),
                ..apu_std_header(min_size)
            })
        }
        Codename::Rembrandt => {
            let (min_size, fclk, uclk, mclk, vsoc) = match version {
                0x0045_0004 => (0xAA4, 0x664, 0x668, 0x66C, 0x19C),
                0x0045_0005 => (0xAB0, 0x6B0, 0x6B4, 0x6B8, 0x1C8),
                _ => return None,
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                ..apu_std_header(min_size)
            })
        }
        Codename::Phoenix | Codename::HawkPoint => {
            let (min_size, fclk, uclk, mclk, vsoc, vddp) = match version {
                0x004C_0001..=0x004C_0007 => (0x76C, 0x174, 0x184, 0x194, 0x1C8, 0x768),
                0x004C_0008 => (0x76C, 0x164, 0x174, 0x184, 0x1B8, 0x768),
                0x004C_0009 => (0x778, 0x164, 0x174, 0x184, 0x1BC, 0x774),
                _ => return None,
            };
            let base = if matches!(version, 0x004C_0006..=0x004C_0009) {
                apu_std_header(min_size)
            } else {
                PmTableLayout {
                    min_size,
                    ..PmTableLayout::default()
                }
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                cldo_vddp_v: Some(vddp),
                ..base
            })
        }
        Codename::StrixPoint => match version {
            // Spec-implemented for Strix Point / Krackan Point (0xD54 bytes) — community report welcome.
            0x005D_0008..=0x005D_000B => Some(PmTableLayout {
                fclk_mhz: Some(0x4E0),
                uclk_mhz: Some(0x4E4),
                mclk_mhz: Some(0x4EC),
                ..apu_std_header(0xD54)
            }),
            _ => None,
        },
        Codename::StrixHalo => {
            // Verified live on Ryzen AI Max+ 395 (Strix Halo, PM table 0x0064020C, 0xE50 = 3664 B,
            // GitHub issues #1 & #2) and cross-referenced against RyzenAdj, amkillam/ryzen_smu PR #50,
            // and SMUDebugTool PowerLimitMap:
            // - 0x000/0x004 STAPM, 0x008/0x00C Fast PPT, 0x010/0x014 Slow PPT, 0x018/0x01C APU Slow
            // - 0x020..0x02C are zeroed on Strix Halo; VDD TDC sits at 0x030/0x034
            // - 0x058/0x05C Tctl thermal limit/value (matches k10temp Tctl & MP1 0x5F TjMax)
            // - 1-CCD (0x0064_0107..=0x0064_010C, min 0xDC0 B) and 2-CCD (0x0064_0207..=0x0064_020C,
            //   0xE50 B) share this header layout.
            // - Ambiguous/unverified fields (SoC TDC, EDC, skin temps, clocks, voltages) stay None.
            let min_size = match version {
                0x0064_0107..=0x0064_010C => 0xDC0,
                0x0064_0207..=0x0064_020C => 0xE50,
                _ => return None,
            };
            Some(PmTableLayout {
                min_size,
                stapm_limit_w: Some(0x000),
                stapm_value_w: Some(0x004),
                ppt_limit_w: Some(0x008),
                ppt_value_w: Some(0x00C),
                slow_limit_w: Some(0x010),
                slow_value_w: Some(0x014),
                apu_slow_limit_w: Some(0x018),
                apu_slow_value_w: Some(0x01C),
                tdc_limit_a: Some(0x030),
                tdc_value_a: Some(0x034),
                tctl_limit_c: Some(0x058),
                tctl_value_c: Some(0x05C),
                ..PmTableLayout::default()
            })
        }
        Codename::VanGogh => match version {
            0x003F_0000 => Some(PmTableLayout {
                fclk_mhz: Some(0x3BC),
                uclk_mhz: Some(0x3C0),
                mclk_mhz: Some(0x3C4),
                vddcr_soc_v: Some(0x1A0),
                ..apu_std_header(0x7AC)
            }),
            _ => None,
        },
        Codename::RavenRidge | Codename::RavenRidge2 | Codename::Picasso | Codename::Dali => {
            let (min_size, fclk, uclk, mclk, vsoc, vddp) = match version {
                0x001E_0001 => (0x570, 0x460, 0x464, 0x468, 0x10C, 0x0F8),
                0x001E_0002 => (0x580, 0x474, 0x478, 0x47C, 0x10C, 0x0F8),
                0x001E_0003..=0x001E_0005 | 0x001E_000A | 0x001E_0101 => {
                    (0x608, 0x298, 0x29C, 0x2A0, 0x104, 0x0F0)
                }
                _ => return None,
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                cldo_vddp_v: Some(vddp),
                ..apu_std_header(min_size)
            })
        }
        _ => None,
    }
}
