use super::*;
use std::fs;

#[test]
fn parses_major_linux_distros_from_os_release() {
    let (fam, name) = parse_os_release(
        "NAME=\"CachyOS Linux\"\nID=cachyos\nID_LIKE=arch\nPRETTY_NAME=\"CachyOS\"\n",
    );
    assert_eq!(fam, DistroFamily::Arch);
    assert_eq!(name, "CachyOS");

    let (fam, name) =
        parse_os_release("ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n");
    assert_eq!(fam, DistroFamily::Debian);
    assert_eq!(name, "Ubuntu 24.04.1 LTS");

    let (fam, _) = parse_os_release("ID=fedora\nPRETTY_NAME=\"Fedora Linux 41\"\n");
    assert_eq!(fam, DistroFamily::Fedora);

    let (fam, _) = parse_os_release(
        "ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\nPRETTY_NAME=\"openSUSE Tumbleweed\"\n",
    );
    assert_eq!(fam, DistroFamily::OpenSuse);

    let (fam, _) = parse_os_release("ID=nixos\nPRETTY_NAME=\"NixOS 25.05\"\n");
    assert_eq!(fam, DistroFamily::Other);
}

#[test]
fn selects_deb_and_rpm_assets_from_release_json() {
    let json = r#"{
        "tag_name": "v0.2.1",
        "assets": [
            {"name": "zensmu_0.2.1_amd64.deb", "browser_download_url": "https://example.invalid/zensmu.deb"},
            {"name": "ryzen-smu-dkms_0.1.7.r191.gd298366_all.deb", "browser_download_url": "https://example.invalid/ryzen-smu-dkms.deb"},
            {"name": "ryzen-smu-dkms-0.1.7.r191.gd298366-1.noarch.rpm", "browser_download_url": "https://example.invalid/ryzen-smu-dkms.rpm"},
            {"name": "ryzen-smu-dkms.SHA256SUMS.txt", "browser_download_url": "https://example.invalid/sums.txt"}
        ]
    }"#;

    let (deb, sums) = select_release_assets(json, ".deb").unwrap();
    assert_eq!(deb, "https://example.invalid/ryzen-smu-dkms.deb");
    assert_eq!(sums.as_deref(), Some("https://example.invalid/sums.txt"));

    let (rpm, sums) = select_release_assets(json, ".rpm").unwrap();
    assert_eq!(rpm, "https://example.invalid/ryzen-smu-dkms.rpm");
    assert_eq!(sums.as_deref(), Some("https://example.invalid/sums.txt"));
}

#[test]
fn parses_sha256sum_entries_accurately() {
    let h1 = "a".repeat(64);
    let h2 = "b".repeat(64);
    let txt =
        format!("{h1}  ryzen-smu-dkms_0.1.7_all.deb\n{h2} *ryzen-smu-dkms-0.1.7-1.noarch.rpm\n");
    assert_eq!(
        parse_sha256_for_filename(&txt, "ryzen-smu-dkms_0.1.7_all.deb").as_deref(),
        Some(h1.as_str())
    );
    assert_eq!(
        parse_sha256_for_filename(&txt, "ryzen-smu-dkms-0.1.7-1.noarch.rpm").as_deref(),
        Some(h2.as_str())
    );
    assert!(parse_sha256_for_filename(&txt, "other.deb").is_none());
}

#[test]
fn recommended_command_adapts_to_driver_state_and_distro() {
    assert_eq!(
        recommended_driver_command(true, true, DistroFamily::Debian),
        "zensmu doctor"
    );
    assert_eq!(
        recommended_driver_command(false, true, DistroFamily::Debian),
        "sudo zensmu driver load"
    );
    for fam in [
        DistroFamily::Arch,
        DistroFamily::Debian,
        DistroFamily::Fedora,
        DistroFamily::OpenSuse,
        DistroFamily::Other,
    ] {
        assert_eq!(
            recommended_driver_command(false, false, fam),
            "sudo zensmu driver install"
        );
    }
}

#[test]
fn dkms_prereq_command_targets_distro_headers_and_pkgbase() {
    let (bin, args) =
        dkms_prereq_command(DistroFamily::Debian, "6.8.0-45-generic", None, false).unwrap();
    assert_eq!(bin, "apt-get");
    assert!(args.contains(&"dkms".to_string()));
    assert!(args.contains(&"linux-headers-6.8.0-45-generic".to_string()));
    assert!(!args.contains(&"git".to_string()));

    let (bin, args) =
        dkms_prereq_command(DistroFamily::Fedora, "6.12.5-200.fc41.x86_64", None, true).unwrap();
    assert_eq!(bin, "dnf");
    assert!(args.contains(&"kernel-devel-6.12.5-200.fc41.x86_64".to_string()));
    assert!(args.contains(&"git".to_string()));

    let (bin, args) = dkms_prereq_command(
        DistroFamily::Arch,
        "7.3.6-1-cachyos-bore",
        Some("linux-cachyos-bore"),
        false,
    )
    .unwrap();
    assert_eq!(bin, "pacman");
    assert!(args.contains(&"linux-cachyos-bore-headers".to_string()));
    assert!(args.contains(&"dkms".to_string()));

    let (bin, args) =
        dkms_prereq_command(DistroFamily::OpenSuse, "6.12.0-default", None, false).unwrap();
    assert_eq!(bin, "zypper");
    assert!(args.contains(&"kernel-default-devel".to_string()));

    assert!(dkms_prereq_command(DistroFamily::Other, "6.12.0", None, true).is_none());
}

#[test]
fn finds_only_zensmu_managed_dkms_trees_unless_all_requested() {
    let dir = tempfile::tempdir().unwrap();
    let ext = dir.path().join("ryzen_smu-191.d298366");
    let managed = dir.path().join("ryzen_smu-0.1.7.r191.gd298366");
    let other = dir.path().join("other_mod-1.0");
    fs::create_dir_all(&ext).unwrap();
    fs::create_dir_all(&managed).unwrap();
    fs::create_dir_all(&other).unwrap();
    fs::write(managed.join(ZENSMU_MANAGED_MARKER), "0.1.7.r191.gd298366\n").unwrap();

    let only_managed = find_managed_dkms_versions(dir.path());
    assert_eq!(only_managed.len(), 1);
    assert_eq!(only_managed[0].0, "0.1.7.r191.gd298366");

    let all = find_all_ryzen_smu_dkms_versions(dir.path());
    assert_eq!(all.len(), 2);
}

#[test]
fn renders_valid_dkms_conf() {
    let conf = render_dkms_conf("0.1.7.r191.gd298366");
    assert!(conf.contains("PACKAGE_NAME=\"ryzen_smu\""));
    assert!(conf.contains("PACKAGE_VERSION=\"0.1.7.r191.gd298366\""));
    assert!(conf.contains("BUILT_MODULE_NAME[0]=\"ryzen_smu\""));
    assert!(conf.contains("AUTOINSTALL=\"yes\""));
}

#[test]
fn parses_secure_boot_efivar_and_mokutil_output() {
    // 4-byte EFI attribute header + payload byte
    assert_eq!(
        parse_secure_boot_efivar(&[0x06, 0x00, 0x00, 0x00, 0x01]),
        Some(true)
    );
    assert_eq!(
        parse_secure_boot_efivar(&[0x06, 0x00, 0x00, 0x00, 0x00]),
        Some(false)
    );
    assert_eq!(
        parse_secure_boot_efivar(&[0x06, 0x00, 0x00, 0x00, 0x02]),
        None
    );
    assert_eq!(parse_secure_boot_efivar(&[0x06, 0x00, 0x00, 0x00]), None);
    assert_eq!(parse_secure_boot_efivar(&[]), None);

    assert_eq!(parse_mokutil_sb_state("SecureBoot enabled\n"), Some(true));
    assert_eq!(
        parse_mokutil_sb_state("SecureBoot disabled\nPlatform is in Setup Mode\n"),
        Some(false)
    );
    assert_eq!(
        parse_mokutil_sb_state("EFI variables are not supported on this system\n"),
        None
    );
}

#[test]
fn parses_kernel_lockdown_modes() {
    assert_eq!(
        parse_lockdown_mode("[none] integrity confidentiality\n").as_deref(),
        Some("none")
    );
    assert_eq!(
        parse_lockdown_mode("none [integrity] confidentiality\n").as_deref(),
        Some("integrity")
    );
    assert_eq!(
        parse_lockdown_mode("none integrity [confidentiality]").as_deref(),
        Some("confidentiality")
    );
    assert_eq!(parse_lockdown_mode("none integrity confidentiality"), None);
    assert_eq!(parse_lockdown_mode("[]"), None);
    assert_eq!(parse_lockdown_mode("[unclosed"), None);
    assert_eq!(parse_lockdown_mode(""), None);
}

#[test]
fn computes_blocks_unsigned_modules_and_remediation_hint() {
    let disabled = secure_boot_status_from(Some(false), Some("none".into()));
    assert!(!disabled.blocks_unsigned_modules);
    assert!(disabled.hint.is_none());

    let unknown = secure_boot_status_from(None, None);
    assert!(!unknown.blocks_unsigned_modules);
    assert!(unknown.hint.is_none());

    let sb_only = secure_boot_status_from(Some(true), Some("none".into()));
    assert!(sb_only.blocks_unsigned_modules);
    assert!(sb_only
        .hint
        .as_deref()
        .unwrap()
        .contains("mokutil --import"));

    let integrity_only = secure_boot_status_from(Some(false), Some("integrity".into()));
    assert!(integrity_only.blocks_unsigned_modules);
    assert!(integrity_only
        .hint
        .as_deref()
        .unwrap()
        .contains("integrity"));

    let confidentiality = secure_boot_status_from(None, Some("confidentiality".into()));
    assert!(confidentiality.blocks_unsigned_modules);
    assert!(confidentiality
        .hint
        .as_deref()
        .unwrap()
        .contains("confidentiality"));

    let both = secure_boot_status_from(Some(true), Some("integrity".into()));
    assert!(both.blocks_unsigned_modules);
    let hint = both.hint.as_deref().unwrap();
    assert!(hint.contains("Secure Boot"));
    assert!(hint.contains("integrity"));
    assert!(hint.contains("mokutil --import"));

    // Verify DriverStatus JSON backwards compatibility when `secure_boot` is omitted
    let legacy_json = r#"{
        "sysfs_path": "/sys/kernel/ryzen_smu_drv",
        "loaded": false,
        "module_installed": true,
        "dkms_installed": true,
        "autoload_configured": false,
        "kernel_release": "6.12.0",
        "distro_family": "fedora",
        "distro_name": "Fedora Linux 41",
        "summary": "not loaded",
        "recommended_command": "sudo zensmu driver load",
        "manual_commands": []
    }"#;
    let mut parsed: DriverStatus = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(parsed.secure_boot, SecureBootStatus::default());
    assert!(!format_driver_remediation_hint(&parsed).contains("mokutil"));

    parsed.secure_boot = both;
    let rem = format_driver_remediation_hint(&parsed);
    assert!(rem.contains("Secure Boot"));
    assert!(rem.contains("mokutil --import"));
}
