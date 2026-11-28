use super::{DistroFamily, RYZEN_SMU_UPSTREAM_GIT};
use std::fs;
use std::process::Command;

/// Parse `/etc/os-release` contents into `(DistroFamily, pretty_name)`.
pub fn parse_os_release(content: &str) -> (DistroFamily, String) {
    let mut id = String::new();
    let mut id_like = String::new();
    let mut pretty = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            "ID" => id = val.to_ascii_lowercase(),
            "ID_LIKE" => id_like = val.to_ascii_lowercase(),
            "PRETTY_NAME" => pretty = val,
            _ => {}
        }
    }

    let tokens: Vec<&str> = id
        .split_whitespace()
        .chain(id_like.split_whitespace())
        .collect();

    let has = |names: &[&str]| tokens.iter().any(|t| names.contains(t));

    let family = if has(&[
        "arch",
        "cachyos",
        "endeavouros",
        "manjaro",
        "garuda",
        "artix",
    ]) {
        DistroFamily::Arch
    } else if has(&[
        "debian",
        "ubuntu",
        "pop",
        "linuxmint",
        "elementary",
        "zorin",
        "kali",
        "neon",
        "raspbian",
    ]) {
        DistroFamily::Debian
    } else if has(&[
        "fedora",
        "rhel",
        "centos",
        "rocky",
        "almalinux",
        "nobara",
        "bazzite",
        "ol",
    ]) {
        DistroFamily::Fedora
    } else if has(&[
        "opensuse",
        "opensuse-tumbleweed",
        "opensuse-leap",
        "suse",
        "sles",
    ]) {
        DistroFamily::OpenSuse
    } else {
        DistroFamily::Other
    };

    let display = if pretty.is_empty() {
        if id.is_empty() {
            "Linux".to_string()
        } else {
            id
        }
    } else {
        pretty
    };

    (family, display)
}

/// Detect Linux distribution from `/etc/os-release`.
pub fn detect_distro() -> (DistroFamily, String) {
    match fs::read_to_string("/etc/os-release") {
        Ok(content) => parse_os_release(&content),
        Err(_) => (DistroFamily::Other, "Linux".to_string()),
    }
}

/// Read the running kernel release (`uname -r` / `/proc/sys/kernel/osrelease`).
pub fn read_kernel_release() -> String {
    if let Ok(s) = fs::read_to_string("/proc/sys/kernel/osrelease") {
        let t = s.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Build distro-specific installation commands for `ryzen_smu` (DKMS build first-class).
pub fn manual_install_commands(family: DistroFamily) -> Vec<String> {
    let mut cmds = vec![
        "sudo zensmu driver install  # recommended: builds & registers ryzen_smu via DKMS (auto-installs headers/dkms)".to_string(),
    ];
    match family {
        DistroFamily::Arch => {
            cmds.push("# Or install DKMS package from AUR (note underscore '_'):".to_string());
            cmds.push("paru -S ryzen_smu-dkms-git && sudo modprobe ryzen_smu".to_string());
        }
        DistroFamily::Debian => {
            cmds.push("# Or install prerequisites + build manually via DKMS:".to_string());
            cmds.push(
                "sudo apt install dkms build-essential git linux-headers-$(uname -r)".to_string(),
            );
            cmds.push(format!(
                "git clone {RYZEN_SMU_UPSTREAM_GIT} /tmp/ryzen_smu && sudo make -C /tmp/ryzen_smu dkms-install && sudo modprobe ryzen_smu"
            ));
        }
        DistroFamily::Fedora => {
            cmds.push("# Or install prerequisites + build manually via DKMS:".to_string());
            cmds.push("sudo dnf install dkms gcc make git kernel-devel-$(uname -r)".to_string());
            cmds.push(format!(
                "git clone {RYZEN_SMU_UPSTREAM_GIT} /tmp/ryzen_smu && sudo make -C /tmp/ryzen_smu dkms-install && sudo modprobe ryzen_smu"
            ));
        }
        DistroFamily::OpenSuse => {
            cmds.push("# Or install prerequisites + build manually via DKMS:".to_string());
            cmds.push("sudo zypper install dkms gcc make git kernel-default-devel".to_string());
            cmds.push(format!(
                "git clone {RYZEN_SMU_UPSTREAM_GIT} /tmp/ryzen_smu && sudo make -C /tmp/ryzen_smu dkms-install && sudo modprobe ryzen_smu"
            ));
        }
        DistroFamily::Other => {
            cmds.push("# Or build manually via DKMS:".to_string());
            cmds.push(format!(
                "git clone {RYZEN_SMU_UPSTREAM_GIT} /tmp/ryzen_smu && sudo make -C /tmp/ryzen_smu dkms-install && sudo modprobe ryzen_smu"
            ));
        }
    }
    cmds
}

/// Recommended single command for a given driver state + distro.
///
/// First-class DKMS build via `sudo zensmu driver install` is the standard recommendation
/// across all distros because it handles bundled/upstream DKMS sources, kernel headers,
/// autoload, and module loading in one step.
pub fn recommended_driver_command(
    loaded: bool,
    module_installed: bool,
    _family: DistroFamily,
) -> String {
    if loaded {
        return "zensmu doctor".to_string();
    }
    if module_installed {
        return "sudo zensmu driver load".to_string();
    }
    "sudo zensmu driver install".to_string()
}
