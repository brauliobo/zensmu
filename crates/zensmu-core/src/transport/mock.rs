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
    fused_soc_tdc_a: u32,
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
        // Populate realistic verified Granite Ridge 0x620105 floats:
        // FCLK=2000, UCLK=3000, MCLK=3000, VDDCR_SOC=1.24V, CLDO_VDDP=1.05V,
        // CLDO_VDDG_IOD=0.95V, CLDO_VDDG_CCD=0.95V, MISC=1.10V
        write_f32(&mut pm_table, 0x000, 88.0); // PPT limit
        write_f32(&mut pm_table, 0x004, 42.5); // PPT value
        write_f32(&mut pm_table, 0x008, 75.0); // TDC limit
        write_f32(&mut pm_table, 0x00C, 21.0); // TDC value
        write_f32(&mut pm_table, 0x010, 95.0); // Tctl limit
        write_f32(&mut pm_table, 0x014, 54.2); // Tctl value
        write_f32(&mut pm_table, 0x020, 150.0); // EDC limit
        write_f32(&mut pm_table, 0x024, 38.0); // EDC value
        write_f32(&mut pm_table, 0x0D8, 1.24); // VDDCR_SOC
        write_f32(&mut pm_table, 0x0EC, 1.10); // VDDCR_MISC
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
                fused_soc_tdc_a: 45,
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

    pub fn set_reject_opcode(&self, opcode: u32, status: SmuStatus) {
        self.state.lock().unwrap().reject_opcode.insert(opcode, status);
    }

    pub fn set_simulate_false_ok_bug(&self, enabled: bool) {
        self.state.lock().unwrap().simulate_driver_false_ok_bug = enabled;
    }

    pub fn history(&self) -> Vec<RecordedCommand> {
        self.state.lock().unwrap().history.clone()
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
        Ok(self.state.lock().unwrap().pm_table.clone())
    }

    fn send_command(
        &self,
        mailbox: Mailbox,
        opcode: u32,
        args: SmuArgs,
        verify_rsp_smn_addr: Option<u32>,
    ) -> Result<MailboxResponse> {
        let mut st = self.state.lock().unwrap();
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

        match (mailbox, opcode) {
            // TestMessage
            (_, 0x01) => {
                out[0] = args[0].wrapping_add(1);
            }
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
                if st.co_margins.contains_key(&mask) {
                    st.co_margins.insert(mask, margin);
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
                for i in 0..5 {
                    out[i] = st.shaper[i];
                }
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
                write_f32(&mut st.pm_table, 0x000, w);
            }
            (Mailbox::Rsmu, 0x57) | (Mailbox::Mp1, 0x3C) => {
                st.tdc_ma = args[0];
                let a = args[0] as f32 / 1000.0;
                write_f32(&mut st.pm_table, 0x008, a);
            }
            (Mailbox::Rsmu, 0x58) | (Mailbox::Mp1, 0x3D) => {
                st.edc_ma = args[0];
                let a = args[0] as f32 / 1000.0;
                write_f32(&mut st.pm_table, 0x020, a);
            }
            (Mailbox::Rsmu, 0x59) | (Mailbox::Mp1, 0x3F) => {
                st.tctl_c = args[0];
                let c = args[0] as f32;
                write_f32(&mut st.pm_table, 0x010, c);
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
                out[0] = st.fused_ppt_w;
            }
            (Mailbox::Rsmu, 0xDB) => {
                out[0] = st.fused_vdd_tdc_a;
            }
            (Mailbox::Rsmu, 0xD9) => {
                out[0] = st.fused_soc_tdc_a;
            }
            (Mailbox::Rsmu, 0xDA) => {
                out[0] = st.fused_apu_slow_w;
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
