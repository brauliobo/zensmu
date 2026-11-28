use super::*;
use crate::hw::profiles::capabilities_for;
use crate::hw::topology::CpuTopology;
use crate::transport::mock::MockTransport;

fn write_f32(buf: &mut [u8], offset: usize, val: f32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

#[test]
fn decodes_granite_ridge_9700x_pm_table() {
    let transport = MockTransport::granite_ridge_9700x();
    let driver = transport.read_info().unwrap();
    let hw = HardwareContext {
        driver,
        codename: Codename::GraniteRidge,
        cpuid: None,
        topology: CpuTopology::synthetic(1, 8, 2),
        capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
    };

    let snap = read_telemetry(&transport, &hw).unwrap();
    assert_eq!(snap.codename, Codename::GraniteRidge);
    assert_eq!(snap.table_version, 0x0062_0105);
    assert_eq!(snap.table_size, 0x724);
    assert_eq!(snap.ppt_limit_w, Some(88.0));
    assert_eq!(snap.ppt_value_w, Some(42.5));
    assert_eq!(snap.tdc_limit_a, Some(75.0));
    assert_eq!(snap.tdc_value_a, Some(21.0));
    assert_eq!(snap.edc_limit_a, Some(150.0));
    assert_eq!(snap.edc_value_a, Some(38.0));
    assert_eq!(snap.tctl_limit_c, Some(95.0));
    assert_eq!(snap.tctl_value_c, Some(54.2));
    assert_eq!(snap.fclk_mhz, Some(2000.0));
    assert_eq!(snap.uclk_mhz, Some(3000.0));
    assert_eq!(snap.mclk_mhz, Some(3000.0));
    assert_eq!(snap.vddcr_soc_v, Some(1.24));
    assert_eq!(snap.vddcr_misc_v, Some(1.10));
    assert_eq!(snap.cldo_vddp_v, Some(1.05));
    assert_eq!(snap.cldo_vddg_iod_v, Some(0.95));
    assert_eq!(snap.cldo_vddg_ccd_v, Some(0.95));
}

#[test]
fn refuses_unknown_version_and_oversized_table() {
    let buf = vec![0u8; 0x724];
    assert!(matches!(
        decode_pm_table(Codename::GraniteRidge, 0xDEAD_BEEF, &buf),
        Err(Error::UnsupportedPmTable {
            version: 0xDEAD_BEEF,
            ..
        })
    ));

    // Valid version for GraniteRidge rejected when paired with Raphael
    assert!(matches!(
        decode_pm_table(Codename::Raphael, 0x0062_0105, &buf),
        Err(Error::UnsupportedPmTable { .. })
    ));

    // Oversized (> 4096 bytes) refused
    let huge = vec![0u8; 4097];
    assert!(matches!(
        decode_pm_table(Codename::GraniteRidge, 0x0062_0105, &huge),
        Err(Error::PmTableTooLarge { size: 4097 })
    ));

    // Truncated buffer refused
    let tiny = vec![0u8; 0x100];
    assert!(matches!(
        decode_pm_table(Codename::GraniteRidge, 0x0062_0105, &tiny),
        Err(Error::UnsupportedPmTable { .. })
    ));
}

#[test]
fn filters_nan_inf_and_absurd_values() {
    let mut raw = vec![0u8; 0x724];
    write_f32(&mut raw, 0x008, f32::NAN);
    write_f32(&mut raw, 0x00C, f32::INFINITY);
    write_f32(&mut raw, 0x0D8, 99.0); // absurd voltage
    write_f32(&mut raw, 0x11C, 2000.0); // valid FCLK
    write_f32(&mut raw, 0x12C, -50.0); // negative clock

    let snap = decode_pm_table(Codename::GraniteRidge, 0x0062_0105, &raw).unwrap();
    assert_eq!(snap.ppt_limit_w, None);
    assert_eq!(snap.ppt_value_w, None);
    assert_eq!(snap.vddcr_soc_v, None);
    assert_eq!(snap.fclk_mhz, Some(2000.0));
    assert_eq!(snap.uclk_mhz, None);
}

#[test]
fn decodes_vermeer_and_phoenix_layouts() {
    let mut vermeer = vec![0u8; 0x300];
    write_f32(&mut vermeer, 0x000, 142.0);
    write_f32(&mut vermeer, 0x0B4, 1.12);
    write_f32(&mut vermeer, 0x0C0, 1800.0);
    write_f32(&mut vermeer, 0x224, 0.90);
    let vsnap = decode_pm_table(Codename::Vermeer, 0x0038_0805, &vermeer).unwrap();
    assert_eq!(vsnap.ppt_limit_w, Some(142.0));
    assert_eq!(vsnap.vddcr_soc_v, Some(1.12));
    assert_eq!(vsnap.fclk_mhz, Some(1800.0));
    assert_eq!(vsnap.cldo_vddp_v, Some(0.90));

    let mut phoenix = vec![0u8; 0x800];
    write_f32(&mut phoenix, 0x008, 45.0);
    write_f32(&mut phoenix, 0x164, 2000.0);
    write_f32(&mut phoenix, 0x1B8, 1.15);
    write_f32(&mut phoenix, 0x768, 0.95);
    let psnap = decode_pm_table(Codename::Phoenix, 0x004C_0008, &phoenix).unwrap();
    assert_eq!(psnap.ppt_limit_w, Some(45.0));
    assert_eq!(psnap.fclk_mhz, Some(2000.0));
    assert_eq!(psnap.vddcr_soc_v, Some(1.15));
    assert_eq!(psnap.cldo_vddp_v, Some(0.95));

    let mut strix = vec![0u8; 0xD54];
    write_f32(&mut strix, 0x000, 28.0);
    write_f32(&mut strix, 0x4E0, 2000.0);
    let ssnap = decode_pm_table(Codename::StrixPoint, 0x005D_0008, &strix).unwrap();
    assert_eq!(ssnap.stapm_limit_w, Some(28.0));
    assert_eq!(ssnap.fclk_mhz, Some(2000.0));

    let mut picasso = vec![0u8; 0x608];
    write_f32(&mut picasso, 0x000, 15.0);
    write_f32(&mut picasso, 0x298, 1200.0);
    write_f32(&mut picasso, 0x104, 0.92);
    let picsnap = decode_pm_table(Codename::Picasso, 0x001E_0004, &picasso).unwrap();
    assert_eq!(picsnap.stapm_limit_w, Some(15.0));
    assert_eq!(picsnap.fclk_mhz, Some(1200.0));
    assert_eq!(picsnap.vddcr_soc_v, Some(0.92));

    let mut lucienne = vec![0u8; 0x8C8];
    write_f32(&mut lucienne, 0x008, 30.0);
    write_f32(&mut lucienne, 0x00C, 4.6);
    write_f32(&mut lucienne, 0x020, 33.0);
    write_f32(&mut lucienne, 0x030, 70.0);
    write_f32(&mut lucienne, 0x040, 85.0);
    write_f32(&mut lucienne, 0x044, 44.4);
    write_f32(&mut lucienne, 0x198, 0.725);
    write_f32(&mut lucienne, 0x5E8, 1200.0);
    let lsnap = decode_pm_table(Codename::Lucienne, 0x0037_0005, &lucienne).unwrap();
    assert_eq!(lsnap.ppt_limit_w, Some(30.0));
    assert_eq!(lsnap.tdc_limit_a, Some(33.0));
    assert_eq!(lsnap.edc_limit_a, Some(70.0));
    assert_eq!(lsnap.tctl_limit_c, Some(85.0));
    assert_eq!(lsnap.tctl_value_c, Some(44.4));
    assert_eq!(lsnap.vddcr_soc_v, Some(0.725));
    assert_eq!(lsnap.fclk_mhz, Some(1200.0));
}

#[test]
fn decodes_zen5_x3d_dual_ccd_and_strix_halo_pm_tables() {
    // 1-CCD 9800X3D / updated AGESA (0x0062_1102, 0x724 bytes)
    let mut x3d_1ccd = vec![0u8; 0x724];
    write_f32(&mut x3d_1ccd, 0x008, 162.0);
    write_f32(&mut x3d_1ccd, 0x00C, 92.5);
    write_f32(&mut x3d_1ccd, 0x0FC, 180.0);
    write_f32(&mut x3d_1ccd, 0x11C, 2000.0);
    let snap_1ccd = decode_pm_table(Codename::GraniteRidge, 0x0062_1102, &x3d_1ccd).unwrap();
    assert_eq!(snap_1ccd.ppt_limit_w, Some(162.0));
    assert_eq!(snap_1ccd.ppt_value_w, Some(92.5));
    assert_eq!(snap_1ccd.edc_limit_a, Some(180.0));
    assert_eq!(snap_1ccd.fclk_mhz, Some(2000.0));

    // 2-CCD 9950X / 9950X3D (0x0062_0205 / 0x0062_1202, 0x994 bytes)
    assert!(matches!(
        decode_pm_table(Codename::GraniteRidge, 0x0062_1202, &x3d_1ccd),
        Err(Error::UnsupportedPmTable { .. })
    ));
    let mut x3d_2ccd = vec![0u8; 0x994];
    write_f32(&mut x3d_2ccd, 0x008, 200.0);
    write_f32(&mut x3d_2ccd, 0x020, 160.0);
    write_f32(&mut x3d_2ccd, 0x0FC, 225.0);
    write_f32(&mut x3d_2ccd, 0x434, 1.05);
    let snap_2ccd = decode_pm_table(Codename::GraniteRidge, 0x0062_1202, &x3d_2ccd).unwrap();
    assert_eq!(snap_2ccd.ppt_limit_w, Some(200.0));
    assert_eq!(snap_2ccd.tdc_limit_a, Some(160.0));
    assert_eq!(snap_2ccd.edc_limit_a, Some(225.0));
    assert_eq!(snap_2ccd.cldo_vddp_v, Some(1.05));

    // Strix Point 0x005D_000B (0xD54 bytes)
    let mut strix_p = vec![0u8; 0xD54];
    write_f32(&mut strix_p, 0x000, 54.0);
    write_f32(&mut strix_p, 0x040, 95.0);
    write_f32(&mut strix_p, 0x4E0, 2000.0);
    write_f32(&mut strix_p, 0x4E4, 2800.0);
    write_f32(&mut strix_p, 0x4EC, 3750.0);
    let snap_sp = decode_pm_table(Codename::StrixPoint, 0x005D_000B, &strix_p).unwrap();
    assert_eq!(snap_sp.stapm_limit_w, Some(54.0));
    assert_eq!(snap_sp.tctl_limit_c, Some(95.0));
    assert_eq!(snap_sp.fclk_mhz, Some(2000.0));
    assert_eq!(snap_sp.uclk_mhz, Some(2800.0));
    assert_eq!(snap_sp.mclk_mhz, Some(3750.0));

    // Strix Halo 1-CCD 0x0064_010C (min 0xDC0 bytes) + truncated buffer refusal
    assert!(matches!(
        decode_pm_table(Codename::StrixHalo, 0x0064_010C, &strix_p),
        Err(Error::UnsupportedPmTable { .. })
    ));
    let mut strix_h = vec![0u8; 0xDC0];
    write_f32(&mut strix_h, 0x000, 120.0);
    write_f32(&mut strix_h, 0x008, 140.0);
    write_f32(&mut strix_h, 0x030, 160.0);
    write_f32(&mut strix_h, 0x058, 98.0);
    let snap_sh = decode_pm_table(Codename::StrixHalo, 0x0064_010C, &strix_h).unwrap();
    assert_eq!(snap_sh.stapm_limit_w, Some(120.0));
    assert_eq!(snap_sh.ppt_limit_w, Some(140.0));
    assert_eq!(snap_sh.tdc_limit_a, Some(160.0));
    assert_eq!(snap_sh.tctl_limit_c, Some(98.0));
}

#[test]
fn decodes_strix_halo_0x0064020c_ai_max_395_pm_table() {
    // Synthetic 3664-byte (0xE50) PM table populated with real float values
    // captured from AMD Ryzen AI Max+ 395 (GitHub issue #1, SMU 10.100.7.0).
    let mut raw = vec![0u8; 0xE50];
    write_f32(&mut raw, 0x000, 85.000); // STAPM limit
    write_f32(&mut raw, 0x004, 21.725); // STAPM value
    write_f32(&mut raw, 0x008, 120.000); // Fast PPT limit
    write_f32(&mut raw, 0x00C, 6.546); // Fast PPT value
    write_f32(&mut raw, 0x010, 85.000); // Slow PPT limit
    write_f32(&mut raw, 0x014, 6.658); // Slow PPT value
    write_f32(&mut raw, 0x018, 70.000); // APU Slow limit
    write_f32(&mut raw, 0x01C, 0.000); // APU Slow value
    write_f32(&mut raw, 0x030, 160.000); // VDD TDC limit
    write_f32(&mut raw, 0x034, 3.541); // VDD TDC value
                                       // Unverified secondary rails & thermal sensors from issue #1 dump — must NOT
                                       // be mis-decoded into unverified TelemetrySnapshot fields.
    write_f32(&mut raw, 0x038, 80.000);
    write_f32(&mut raw, 0x03C, 1.061);
    write_f32(&mut raw, 0x040, 40.000);
    write_f32(&mut raw, 0x044, 1.778);
    write_f32(&mut raw, 0x048, 98.000);
    write_f32(&mut raw, 0x04C, 36.011);
    write_f32(&mut raw, 0x050, 98.000);
    write_f32(&mut raw, 0x054, 37.073);
    write_f32(&mut raw, 0x058, 98.000); // Tctl limit (matches MP1 0x5F TjMax=98C)
    write_f32(&mut raw, 0x05C, 39.670); // Tctl value (matches k10temp Tctl=39.6C)
    write_f32(&mut raw, 0x060, 98.000);
    write_f32(&mut raw, 0x064, 40.249);
    write_f32(&mut raw, 0x090, 270.000);
    write_f32(&mut raw, 0x0A8, 79.500);
    write_f32(&mut raw, 0x104, 6.547);
    write_f32(&mut raw, 0x2A8, 2000.000);

    let snap = decode_pm_table(Codename::StrixHalo, 0x0064_020C, &raw).unwrap();
    assert_eq!(snap.codename, Codename::StrixHalo);
    assert_eq!(snap.table_version, 0x0064_020C);
    assert_eq!(snap.table_size, 3664);

    // Verified decoded fields
    assert_eq!(snap.stapm_limit_w, Some(85.000));
    assert_eq!(snap.stapm_value_w, Some(21.725));
    assert_eq!(snap.ppt_limit_w, Some(120.000));
    assert_eq!(snap.ppt_value_w, Some(6.546));
    assert_eq!(snap.slow_limit_w, Some(85.000));
    assert_eq!(snap.slow_value_w, Some(6.658));
    assert_eq!(snap.apu_slow_limit_w, Some(70.000));
    assert_eq!(snap.apu_slow_value_w, Some(0.000));
    assert_eq!(snap.tdc_limit_a, Some(160.000));
    assert_eq!(snap.tdc_value_a, Some(3.541));
    assert_eq!(snap.tctl_limit_c, Some(98.000));
    assert_eq!(snap.tctl_value_c, Some(39.670));

    // Ambiguous / unverified fields must remain None
    assert_eq!(snap.tdc_soc_limit_a, None);
    assert_eq!(snap.tdc_soc_value_a, None);
    assert_eq!(snap.edc_limit_a, None);
    assert_eq!(snap.edc_value_a, None);
    assert_eq!(snap.edc_soc_limit_a, None);
    assert_eq!(snap.edc_soc_value_a, None);
    assert_eq!(snap.apu_skin_temp_limit_c, None);
    assert_eq!(snap.apu_skin_temp_value_c, None);
    assert_eq!(snap.dgpu_skin_temp_limit_c, None);
    assert_eq!(snap.dgpu_skin_temp_value_c, None);
    assert_eq!(snap.fclk_mhz, None);
    assert_eq!(snap.uclk_mhz, None);
    assert_eq!(snap.mclk_mhz, None);
    assert_eq!(snap.vddcr_soc_v, None);
    assert_eq!(snap.vddcr_misc_v, None);
    assert_eq!(snap.cldo_vddp_v, None);
    assert_eq!(snap.cldo_vddg_iod_v, None);
    assert_eq!(snap.cldo_vddg_ccd_v, None);

    // Truncated buffer (< 0xE50) refused for 2-CCD 0x0064_020C
    let short = vec![0u8; 0xDC0];
    assert!(matches!(
        decode_pm_table(Codename::StrixHalo, 0x0064_020C, &short),
        Err(Error::UnsupportedPmTable {
            version: 0x0064_020C,
            size: 0xDC0,
            ..
        })
    ));

    // Wrong codename refused even with valid Strix Halo version & size
    assert!(matches!(
        decode_pm_table(Codename::StrixPoint, 0x0064_020C, &raw),
        Err(Error::UnsupportedPmTable { .. })
    ));
}

#[test]
fn decodes_raphael_0x00540004_7950x3d_and_variants() {
    // Synthetic 2236-byte (0x8BC) PM table populated with real float values
    // captured from AMD Ryzen 9 7950X3D (GitHub issue #3, SMU 4.84.4.0).
    let mut raw = vec![0u8; 0x8BC];
    write_f32(&mut raw, 0x008, 1000.000); // PPT limit
    write_f32(&mut raw, 0x00C, 61.358); // PPT value
    write_f32(&mut raw, 0x020, 1000.000); // TDC limit
    write_f32(&mut raw, 0x024, 22.042); // TDC value
    write_f32(&mut raw, 0x028, 89.000); // Tctl limit (matches MP1 0x23 TjMax=89C)
    write_f32(&mut raw, 0x02C, 66.678); // Tctl value (matches k10temp Tctl=66.9C)
    write_f32(&mut raw, 0x0D0, 1.234); // VDDCR_SOC
    write_f32(&mut raw, 0x0E4, 1.092); // VDDCR_MISC
    write_f32(&mut raw, 0x0F4, 1000.000); // EDC limit (float 61)
    write_f32(&mut raw, 0x0F8, 46.500); // EDC value (float 62)
                                        // Adjacent floats from issue #3 dump that must NOT be misread as EDC
    write_f32(&mut raw, 0x0FC, 19.714);
    write_f32(&mut raw, 0x100, 348.000);
    write_f32(&mut raw, 0x118, 2000.000); // FCLK
    write_f32(&mut raw, 0x128, 2800.000); // UCLK
    write_f32(&mut raw, 0x138, 2800.000); // MCLK
    write_f32(&mut raw, 0x430, 0.997); // CLDO_VDDP

    let snap = decode_pm_table(Codename::Raphael, 0x0054_0004, &raw).unwrap();
    assert_eq!(snap.codename, Codename::Raphael);
    assert_eq!(snap.table_version, 0x0054_0004);
    assert_eq!(snap.table_size, 2236);
    assert_eq!(snap.ppt_limit_w, Some(1000.000));
    assert_eq!(snap.ppt_value_w, Some(61.358));
    assert_eq!(snap.tdc_limit_a, Some(1000.000));
    assert_eq!(snap.tdc_value_a, Some(22.042));
    assert_eq!(snap.edc_limit_a, Some(1000.000));
    assert_eq!(snap.edc_value_a, Some(46.500));
    assert_eq!(snap.tctl_limit_c, Some(89.000));
    assert_eq!(snap.tctl_value_c, Some(66.678));
    assert_eq!(snap.fclk_mhz, Some(2000.000));
    assert_eq!(snap.uclk_mhz, Some(2800.000));
    assert_eq!(snap.mclk_mhz, Some(2800.000));
    assert_eq!(snap.vddcr_soc_v, Some(1.234));
    assert_eq!(snap.vddcr_misc_v, Some(1.092));
    assert_eq!(snap.cldo_vddp_v, Some(0.997));
    assert_eq!(snap.cldo_vddg_iod_v, None);
    assert_eq!(snap.cldo_vddg_ccd_v, None);

    // Truncated buffer (< 0x8BC) refused for 2-CCD 0x0054_0004
    let short = vec![0u8; 0x6A8];
    assert!(matches!(
        decode_pm_table(Codename::Raphael, 0x0054_0004, &short),
        Err(Error::UnsupportedPmTable {
            version: 0x0054_0004,
            size: 0x6A8,
            ..
        })
    ));

    // 1-CCD 0x0054_0104 (0x6A8 bytes) shares 0x0F4/0x0F8 EDC layout
    let mut ccd1 = vec![0u8; 0x6A8];
    write_f32(&mut ccd1, 0x008, 142.0);
    write_f32(&mut ccd1, 0x0F4, 180.0);
    write_f32(&mut ccd1, 0x0F8, 52.0);
    write_f32(&mut ccd1, 0x430, 1.05);
    let snap_1ccd = decode_pm_table(Codename::Raphael, 0x0054_0104, &ccd1).unwrap();
    assert_eq!(snap_1ccd.ppt_limit_w, Some(142.0));
    assert_eq!(snap_1ccd.edc_limit_a, Some(180.0));
    assert_eq!(snap_1ccd.edc_value_a, Some(52.0));
    assert_eq!(snap_1ccd.cldo_vddp_v, Some(1.05));

    // 0x0054_0208 (0x8D0 bytes) shifts SVI3/EDC/clocks/VDDP by +4 bytes (EDC @ 0x0F8/0x0FC)
    let mut r208 = vec![0u8; 0x8D0];
    write_f32(&mut r208, 0x008, 230.0);
    write_f32(&mut r208, 0x00C, 85.0);
    write_f32(&mut r208, 0x020, 160.0);
    write_f32(&mut r208, 0x024, 40.0);
    write_f32(&mut r208, 0x028, 95.0);
    write_f32(&mut r208, 0x02C, 62.5);
    write_f32(&mut r208, 0x0D4, 1.20);
    write_f32(&mut r208, 0x0E8, 1.10);
    write_f32(&mut r208, 0x0F8, 225.0);
    write_f32(&mut r208, 0x0FC, 71.0);
    write_f32(&mut r208, 0x11C, 2000.0);
    write_f32(&mut r208, 0x12C, 3000.0);
    write_f32(&mut r208, 0x13C, 3000.0);
    write_f32(&mut r208, 0x434, 1.00);
    let snap_208 = decode_pm_table(Codename::Raphael, 0x0054_0208, &r208).unwrap();
    assert_eq!(snap_208.ppt_limit_w, Some(230.0));
    assert_eq!(snap_208.tdc_limit_a, Some(160.0));
    assert_eq!(snap_208.edc_limit_a, Some(225.0));
    assert_eq!(snap_208.edc_value_a, Some(71.0));
    assert_eq!(snap_208.tctl_limit_c, Some(95.0));
    assert_eq!(snap_208.tctl_value_c, Some(62.5));
    assert_eq!(snap_208.vddcr_soc_v, Some(1.20));
    assert_eq!(snap_208.vddcr_misc_v, Some(1.10));
    assert_eq!(snap_208.fclk_mhz, Some(2000.0));
    assert_eq!(snap_208.uclk_mhz, Some(3000.0));
    assert_eq!(snap_208.mclk_mhz, Some(3000.0));
    assert_eq!(snap_208.cldo_vddp_v, Some(1.00));

    // Early 0x0054_0002 (EDC @ 0x0DC/0x0E0) and 0x0054_0000 (PPT 0x000/0x004, TDC 0x008/0x00C, EDC 0x0A8/0x0AC)
    let mut r002 = vec![0u8; 0x87C];
    write_f32(&mut r002, 0x0DC, 200.0);
    write_f32(&mut r002, 0x0E0, 65.0);
    let snap_002 = decode_pm_table(Codename::Raphael, 0x0054_0002, &r002).unwrap();
    assert_eq!(snap_002.edc_limit_a, Some(200.0));
    assert_eq!(snap_002.edc_value_a, Some(65.0));

    let mut r000 = vec![0u8; 0x828];
    write_f32(&mut r000, 0x000, 142.0);
    write_f32(&mut r000, 0x004, 50.0);
    write_f32(&mut r000, 0x008, 95.0);
    write_f32(&mut r000, 0x00C, 30.0);
    write_f32(&mut r000, 0x0A8, 160.0);
    write_f32(&mut r000, 0x0AC, 42.0);
    let snap_000 = decode_pm_table(Codename::Raphael, 0x0054_0000, &r000).unwrap();
    assert_eq!(snap_000.ppt_limit_w, Some(142.0));
    assert_eq!(snap_000.ppt_value_w, Some(50.0));
    assert_eq!(snap_000.tdc_limit_a, Some(95.0));
    assert_eq!(snap_000.tdc_value_a, Some(30.0));
    assert_eq!(snap_000.edc_limit_a, Some(160.0));
    assert_eq!(snap_000.edc_value_a, Some(42.0));
    assert_eq!(snap_000.tctl_limit_c, None);
    assert_eq!(snap_000.tctl_value_c, None);

    // Unverified 0x0054_0106 (not in ryzen_smu/ZenStates-Core) is refused
    assert!(matches!(
        decode_pm_table(Codename::Raphael, 0x0054_0106, &r208),
        Err(Error::UnsupportedPmTable {
            version: 0x0054_0106,
            ..
        })
    ));
}

#[test]
fn dump_raw_pm_table_writes_file() {
    let transport = MockTransport::granite_ridge_9700x();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pm_table.bin");
    let n = dump_raw_pm_table(&transport, &out).unwrap();
    assert_eq!(n, 0x724);
    assert_eq!(std::fs::read(&out).unwrap().len(), 0x724);
}
