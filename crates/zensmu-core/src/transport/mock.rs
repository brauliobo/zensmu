use crate::error::{Result, SmuStatus};
use crate::transport::{DriverSysfsInfo, MailboxResponse, SmuTransport};
use crate::types::{Codename, Mailbox, SmuArgs};
use std::collections::HashMap;
use std::sync::Mutex;

/// Recorded command for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCommand {
    pub mailbox: Mailbox,
    pub opcode: u32,
    pub args: SmuArgs,
}

/// Deterministic in-memory SMU simulator for testing all commands, edge cases,
/// and `ryzen_smu`'s `smu.c:213` false-OK bug without touching hardware.
pub struct MockTransport {
    pub info: DriverSysfsInfo,
    state: Mutex<MockState>,
}

struct MockState {
    /// Core mask -> signed 16-bit CO margin
    co_margins: HashMap<u32, i16>,
    igpu_co: i16,
    /// Freq tier (0..5) -> packed u32 (hi<<24 | med<<16 | lo<<8)
    shaper: [u32; 5],
    /// PBO scalar as f32 (1.0..=10.0)
    pbo_scalar: f32,
    /// Fmax in MHz
    fmax_mhz: u32,
    /// Limits
    ppt_mw: u32,
    tdc_ma: u32,
    edc_ma: u32,
    tctl_c: u32,
    /// Fused limits
    fused_ppt_w: u32,
    fused_vdd_tdc_a: u32,
    fused_edc_a: u32,
    fused_apu_slow_w: u32,
    /// Flags
    overclockable: bool,
    expo_active: bool,
    ln2_mode: bool,
    /// PM table bytes
    pm_table: Vec<u8>,
    /// Optional forced rejection status for a specific opcode
    reject_opcode: HashMap<u32, SmuStatus>,
    /// When true, simulates `ryzen_smu`'s `smu.c:213` bug where `driver_status`
    /// returns `Ok` even when `hw_status` is an error code!
    simulate_driver_false_ok_bug: bool,
    /// Command log
    history: Vec<RecordedCommand>,
}

impl MockTransport {
    /// Build a realistic mock Ryzen 7 9700X (Granite Ridge, PM table `0x620105`).
    pub fn granite_ridge_9700x() -> Self {
        let mut co_margins = HashMap::new();
        for c in 0..8u32 {
            co_margins.insert(c << 20, 0);
        }
        let mut pm_table = vec![0u8; 0x724];
        // Populate verified Granite Ridge 0x620105 floats (matching live Ryzen 7 9700X):
        write_f32(&mut pm_table, 0x008, 88.0); // PPT limit
        write_f32(&mut pm_table, 0x00C, 42.5); // PPT value
        write_f32(&mut pm_table, 0x020, 75.0); // TDC limit
        write_f32(&mut pm_table, 0x024, 21.0); // TDC value
        write_f32(&mut pm_table, 0x028, 95.0); // Tctl limit
        write_f32(&mut pm_table, 0x02C, 54.2); // Tctl value
        write_f32(&mut pm_table, 0x0D8, 1.24); // VDDCR_SOC
        write_f32(&mut pm_table, 0x0EC, 1.10); // VDDCR_MISC
        write_f32(&mut pm_table, 0x0FC, 150.0); // EDC limit
        write_f32(&mut pm_table, 0x100, 38.0); // EDC value
        write_f32(&mut pm_table, 0x11C, 2000.0); // FCLK
        write_f32(&mut pm_table, 0x12C, 3000.0); // UCLK
        write_f32(&mut pm_table, 0x13C, 3000.0); // MCLK
        write_f32(&mut pm_table, 0x40C, 0.95); // CLDO_VDDG_IOD
        write_f32(&mut pm_table, 0x414, 0.95); // CLDO_VDDG_CCD
        write_f32(&mut pm_table, 0x434, 1.05); // CLDO_VDDP

        Self {
            info: DriverSysfsInfo {
                drv_version: "0.1.7".into(),
                smu_version: "98.79.0".into(),
                codename_id: Codename::GraniteRidge as u32,
                mp1_if_version: 2,
                has_rsmu: true,
                has_pm_table: true,
                pm_table_version: Some(0x0062_0105),
                pm_table_size: Some(0x724),
            },
            state: Mutex::new(MockState {
                co_margins,
                igpu_co: 0,
                shaper: [0; 5],
                pbo_scalar: 1.0,
                fmax_mhz: 5550,
                ppt_mw: 88_000,
                tdc_ma: 75_000,
                edc_ma: 150_000,
                tctl_c: 95,
                fused_ppt_w: 88,
                fused_vdd_tdc_a: 75,
                fused_edc_a: 150,
                fused_apu_slow_w: 65,
                overclockable: true,
                expo_active: true,
                ln2_mode: false,
                pm_table,
                reject_opcode: HashMap::new(),
                simulate_driver_false_ok_bug: false,
                history: Vec::new(),
            }),
        }
    }

    /// Build a realistic mock Ryzen 7 7840U (Phoenix Zen 4 APU, PM table `0x004C_0008`).
    pub fn phoenix_apu() -> Self {
        let mut co_margins = HashMap::new();
        for c in 0..8u32 {
            co_margins.insert(c, 0);
        }
        let mut pm_table = vec![0u8; 0x778];
        write_f32(&mut pm_table, 0x000, 28.0); // STAPM limit
        write_f32(&mut pm_table, 0x004, 15.0); // STAPM value
        write_f32(&mut pm_table, 0x008, 45.0); // Fast PPT limit
        write_f32(&mut pm_table, 0x00C, 18.0); // Fast PPT value
        write_f32(&mut pm_table, 0x010, 35.0); // Slow PPT limit
        write_f32(&mut pm_table, 0x014, 16.0); // Slow PPT value
        write_f32(&mut pm_table, 0x018, 30.0); // APU Slow limit
        write_f32(&mut pm_table, 0x01C, 14.0); // APU Slow value
        write_f32(&mut pm_table, 0x020, 40.0); // TDC VDD limit
        write_f32(&mut pm_table, 0x024, 15.0); // TDC VDD value
        write_f32(&mut pm_table, 0x028, 20.0); // TDC SoC limit
        write_f32(&mut pm_table, 0x02C, 8.0); // TDC SoC value
        write_f32(&mut pm_table, 0x030, 75.0); // EDC VDD limit
        write_f32(&mut pm_table, 0x034, 24.0); // EDC VDD value
        write_f32(&mut pm_table, 0x038, 35.0); // EDC SoC limit
        write_f32(&mut pm_table, 0x03C, 12.0); // EDC SoC value
        write_f32(&mut pm_table, 0x040, 85.0); // Tctl limit
        write_f32(&mut pm_table, 0x044, 52.0); // Tctl value
        write_f32(&mut pm_table, 0x048, 70.0); // APU skin temp limit
        write_f32(&mut pm_table, 0x04C, 42.0); // APU skin temp value
        write_f32(&mut pm_table, 0x050, 65.0); // dGPU skin temp limit
        write_f32(&mut pm_table, 0x054, 40.0); // dGPU skin temp value
        write_f32(&mut pm_table, 0x164, 2000.0); // FCLK
        write_f32(&mut pm_table, 0x174, 3200.0); // UCLK
        write_f32(&mut pm_table, 0x184, 3200.0); // MCLK
        write_f32(&mut pm_table, 0x1B8, 1.15); // VDDCR_SOC (0x004C_0008)
        write_f32(&mut pm_table, 0x768, 0.95); // CLDO_VDDP (0x004C_0008)

        Self {
            info: DriverSysfsInfo {
                drv_version: "0.1.7".into(),
                smu_version: "76.70.0".into(),
                codename_id: Codename::Phoenix as u32,
                mp1_if_version: 2,
                has_rsmu: true,
                has_pm_table: true,
                pm_table_version: Some(0x004C_0008),
                pm_table_size: Some(0x778),
            },
            state: Mutex::new(MockState {
                co_margins,
                igpu_co: 0,
                shaper: [0; 5],
                pbo_scalar: 1.0,
                fmax_mhz: 5100,
                ppt_mw: 45_000,
                tdc_ma: 40_000,
                edc_ma: 75_000,
                tctl_c: 85,
                fused_ppt_w: 45,
                fused_vdd_tdc_a: 40,
                fused_edc_a: 75,
                fused_apu_slow_w: 30,
                overclockable: true,
                expo_active: false,
                ln2_mode: false,
                pm_table,
                reject_opcode: HashMap::new(),
                simulate_driver_false_ok_bug: false,
                history: Vec::new(),
            }),
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_reject_opcode(&self, opcode: u32, status: SmuStatus) {
        self.lock_state().reject_opcode.insert(opcode, status);
    }

    pub fn set_simulate_false_ok_bug(&self, enabled: bool) {
        self.lock_state().simulate_driver_false_ok_bug = enabled;
    }

    pub fn history(&self) -> Vec<RecordedCommand> {
        self.lock_state().history.clone()
    }
}

fn write_f32(buf: &mut [u8], offset: usize, val: f32) {
    if offset + 4 <= buf.len() {
        buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
    }
}

impl SmuTransport for MockTransport {
    fn read_info(&self) -> Result<DriverSysfsInfo> {
        Ok(self.info.clone())
    }

    fn read_pm_table(&self) -> Result<Vec<u8>> {
        Ok(self.lock_state().pm_table.clone())
    }

    fn send_command(
        &self,
        mailbox: Mailbox,
        opcode: u32,
        args: SmuArgs,
        verify_rsp_smn_addr: Option<u32>,
    ) -> Result<MailboxResponse> {
        let mut st = self.lock_state();
        st.history.push(RecordedCommand {
            mailbox,
            opcode,
            args,
        });

        if let Some(&forced) = st.reject_opcode.get(&opcode) {
            let drv = if st.simulate_driver_false_ok_bug {
                SmuStatus::Ok
            } else {
                forced
            };
            let hw = verify_rsp_smn_addr.map(|_| forced);
            return Ok(MailboxResponse {
                driver_status: drv,
                hw_status: hw,
                args,
            });
        }

        let mut out = args;
        let mut hw = SmuStatus::Ok;
        let is_phoenix = self.info.codename_id == Codename::Phoenix as u32;

        if opcode == 0x01 {
            // TestMessage on all platforms
            out[0] = args[0].wrapping_add(1);
        } else if is_phoenix {
            match (mailbox, opcode) {
                (Mailbox::Rsmu, 0xE1) => {
                    // GetDldoPsmMargin (APU core index)
                    if let Some(&m) = st.co_margins.get(&args[0]) {
                        out[0] = (m as u16) as u32;
                    } else {
                        hw = SmuStatus::InvalidArgument;
                    }
                }
                (Mailbox::Rsmu, 0x53) | (Mailbox::Mp1, 0x4B) => {
                    // SetDldoPsmMargin (APU: core_idx << 20 | margin)
                    let idx = (args[0] >> 20) & 0xFFF;
                    let margin = (args[0] & 0xFFFF) as u16 as i16;
                    if let Some(slot) = st.co_margins.get_mut(&idx) {
                        *slot = margin;
                    } else {
                        hw = SmuStatus::InvalidArgument;
                    }
                }
                (Mailbox::Rsmu, 0x5D) | (Mailbox::Mp1, 0x4C) => {
                    let margin = (args[0] & 0xFFFF) as u16 as i16;
                    for v in st.co_margins.values_mut() {
                        *v = margin;
                    }
                }
                (Mailbox::Rsmu, 0x20) => {
                    out[0] = (st.igpu_co as u16) as u32;
                }
                (Mailbox::Rsmu, 0x1F) => {
                    st.igpu_co = (args[0] & 0xFFFF) as u16 as i16;
                }
                (Mailbox::Mp1, 0x14) | (Mailbox::Rsmu, 0x31) => {
                    write_f32(&mut st.pm_table, 0x000, args[0] as f32 / 1000.0);
                }
                (Mailbox::Mp1, 0x15) | (Mailbox::Rsmu, 0x32) => {
                    st.ppt_mw = args[0];
                    write_f32(&mut st.pm_table, 0x008, args[0] as f32 / 1000.0);
                }
                (Mailbox::Mp1, 0x16) | (Mailbox::Rsmu, 0x33) => {
                    write_f32(&mut st.pm_table, 0x010, args[0] as f32 / 1000.0);
                }
                (Mailbox::Rsmu, 0x34) | (Mailbox::Mp1, 0x23) => {
                    write_f32(&mut st.pm_table, 0x018, args[0] as f32 / 1000.0);
                }
                (Mailbox::Mp1, 0x1A) | (Mailbox::Rsmu, 0x38) => {
                    st.tdc_ma = args[0];
                    write_f32(&mut st.pm_table, 0x020, args[0] as f32 / 1000.0);
                }
                (Mailbox::Mp1, 0x1B) | (Mailbox::Rsmu, 0x39) => {
                    write_f32(&mut st.pm_table, 0x028, args[0] as f32 / 1000.0);
                }
                (Mailbox::Mp1, 0x1C) | (Mailbox::Rsmu, 0x3A) => {
                    st.edc_ma = args[0];
                    write_f32(&mut st.pm_table, 0x030, args[0] as f32 / 1000.0);
                }
                (Mailbox::Mp1, 0x1D) | (Mailbox::Rsmu, 0x3B) => {
                    write_f32(&mut st.pm_table, 0x038, args[0] as f32 / 1000.0);
                }
                (Mailbox::Mp1, 0x19) | (Mailbox::Rsmu, 0x37) => {
                    st.tctl_c = args[0];
                    write_f32(&mut st.pm_table, 0x040, args[0] as f32);
                }
                (Mailbox::Rsmu, 0x91) | (Mailbox::Mp1, 0x33) => {
                    write_f32(&mut st.pm_table, 0x048, (args[0] >> 8) as f32);
                }
                (Mailbox::Rsmu, 0x92) | (Mailbox::Mp1, 0x34) => {
                    write_f32(&mut st.pm_table, 0x050, (args[0] >> 8) as f32);
                }
                (Mailbox::Mp1, 0x18)
                | (Mailbox::Rsmu, 0x36)
                | (Mailbox::Mp1, 0x17)
                | (Mailbox::Rsmu, 0x35)
                | (Mailbox::Mp1, 0x11)
                | (Mailbox::Mp1, 0x12) => {
                    // STAPM/Slow time constants & power mode accepted
                }
                (Mailbox::Rsmu, 0x0F) => {
                    out[0] = st.pbo_scalar.to_bits();
                }
                (Mailbox::Rsmu, 0x3E) | (Mailbox::Mp1, 0x63) => {
                    st.pbo_scalar = args[0] as f32 / 100.0;
                }
                (Mailbox::Rsmu, 0x42) => {
                    out[0] = st.fmax_mhz;
                }
                (Mailbox::Rsmu, 0x47) => {
                    st.fmax_mhz = args[0] & 0xF_FFFF;
                }
                (Mailbox::Rsmu, 0x82) => out[0] = u32::from(st.overclockable),
                (Mailbox::Rsmu, 0xDB) => out[0] = u32::from(st.expo_active),
                (Mailbox::Rsmu, 0xC4) => out[0] = u32::from(st.ln2_mode),
                (Mailbox::Rsmu, 0x11) => out[0] = st.fused_ppt_w * 1000,
                (Mailbox::Rsmu, 0x12) => out[0] = 35_000,
                (Mailbox::Rsmu, 0x13) => out[0] = st.fused_ppt_w * 1000,
                (Mailbox::Rsmu, 0x14) => out[0] = st.fused_apu_slow_w * 1000,
                (Mailbox::Rsmu, 0x15) => out[0] = st.fused_vdd_tdc_a * 1000,
                (Mailbox::Rsmu, 0x16) => out[0] = 20_000,
                (Mailbox::Rsmu, 0x0B) => {
                    let idx = args[0];
                    out[0] = 200u32.saturating_sub(idx * 5);
                }
                _ => {
                    hw = SmuStatus::UnknownCmd;
                }
            }
        } else {
            match (mailbox, opcode) {
                // Granite Ridge / Raphael RSMU
                (Mailbox::Rsmu, 0xD5) => {
                    // GetDldoPsmMargin
                    let mask = args[0] & 0xFFF0_0000;
                    if let Some(&m) = st.co_margins.get(&mask) {
                        out[0] = (m as u16) as u32;
                    } else {
                        hw = SmuStatus::InvalidArgument;
                    }
                }
                (Mailbox::Rsmu, 0x06) | (Mailbox::Mp1, 0x35) => {
                    // SetDldoPsmMargin
                    let mask = args[0] & 0xFFF0_0000;
                    let margin = (args[0] & 0xFFFF) as u16 as i16;
                    if let Some(slot) = st.co_margins.get_mut(&mask) {
                        *slot = margin;
                    } else {
                        hw = SmuStatus::InvalidArgument;
                    }
                }
                (Mailbox::Rsmu, 0x07) | (Mailbox::Mp1, 0x36) => {
                    // SetAllDldoPsmMargin
                    let margin = (args[0] & 0xFFFF) as u16 as i16;
                    for v in st.co_margins.values_mut() {
                        *v = margin;
                    }
                }
                (Mailbox::Rsmu, 0xD7) => {
                    out[0] = (st.igpu_co as u16) as u32;
                }
                (Mailbox::Rsmu, 0xA7) => {
                    st.igpu_co = (args[0] & 0xFFFF) as u16 as i16;
                }
                (Mailbox::Rsmu, 0x84) => {
                    // GetCurveShaperMargin
                    out[..5].copy_from_slice(&st.shaper);
                }
                (Mailbox::Rsmu, 0xA6) => {
                    // SetCurveShaperMargin
                    let tier = (args[0] & 0x7F) as usize;
                    if tier < 5 {
                        st.shaper[tier] = args[0] & 0xFFFF_FF00;
                    } else {
                        hw = SmuStatus::InvalidArgument;
                    }
                }
                (Mailbox::Rsmu, 0x56) | (Mailbox::Mp1, 0x3E) => {
                    st.ppt_mw = args[0];
                    let w = args[0] as f32 / 1000.0;
                    write_f32(&mut st.pm_table, 0x008, w);
                }
                (Mailbox::Rsmu, 0x57) | (Mailbox::Mp1, 0x3C) => {
                    st.tdc_ma = args[0];
                    let a = args[0] as f32 / 1000.0;
                    write_f32(&mut st.pm_table, 0x020, a);
                }
                (Mailbox::Rsmu, 0x58) | (Mailbox::Mp1, 0x3D) => {
                    st.edc_ma = args[0];
                    let a = args[0] as f32 / 1000.0;
                    write_f32(&mut st.pm_table, 0x0FC, a);
                }
                (Mailbox::Rsmu, 0x59) | (Mailbox::Mp1, 0x3F) => {
                    st.tctl_c = args[0];
                    let c = args[0] as f32;
                    write_f32(&mut st.pm_table, 0x028, c);
                }
                (Mailbox::Rsmu, 0x6D) => {
                    out[0] = st.pbo_scalar.to_bits();
                }
                (Mailbox::Rsmu, 0x5B) | (Mailbox::Mp1, 0x2F) => {
                    st.pbo_scalar = args[0] as f32 / 100.0;
                }
                (Mailbox::Rsmu, 0x6E) => {
                    out[0] = st.fmax_mhz;
                }
                (Mailbox::Rsmu, 0x70) => {
                    st.fmax_mhz = args[0] & 0xF_FFFF;
                }
                (Mailbox::Rsmu, 0x6F) => {
                    out[0] = u32::from(st.overclockable);
                }
                (Mailbox::Rsmu, 0x35) => {
                    out[0] = u32::from(st.expo_active);
                }
                (Mailbox::Rsmu, 0xDD) => {
                    out[0] = u32::from(st.ln2_mode);
                }
                (Mailbox::Rsmu, 0xDC) => {
                    out[0] = st.fused_edc_a * 1000; // Fused EDC (mA) on AM5
                }
                (Mailbox::Rsmu, 0xDB) => {
                    out[0] = st.fused_vdd_tdc_a * 1000;
                }
                (Mailbox::Rsmu, 0xD9) => {
                    out[0] = st.fused_ppt_w * 1000; // Fused PPT (mW) on AM5
                }
                (Mailbox::Rsmu, 0xDA) => {
                    out[0] = st.fused_apu_slow_w * 1000;
                }
                (Mailbox::Rsmu, 0x5C) => {
                    // CPPC perf score mock: higher on core 0 & 3
                    let idx = args[0];
                    out[0] = 200u32.saturating_sub(idx * 5);
                }
                _ => {
                    hw = SmuStatus::UnknownCmd;
                }
            }
        }

        let drv = if !hw.is_ok() && st.simulate_driver_false_ok_bug {
            SmuStatus::Ok
        } else {
            hw
        };

        Ok(MailboxResponse {
            driver_status: drv,
            hw_status: verify_rsp_smn_addr.map(|_| hw),
            args: out,
        })
    }

    fn read_smn(&self, _addr: u32) -> Result<u32> {
        Ok(SmuStatus::Ok as u32)
    }
}
