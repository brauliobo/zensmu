# zensmu

[![CI](https://img.shields.io/github/actions/workflow/status/brauliobo/zensmu/ci.yml?branch=main&label=CI)](https://github.com/brauliobo/zensmu/actions/workflows/ci.yml)
[![Latest Release](https://img.shields.io/github/v/release/brauliobo/zensmu?label=release)](https://github.com/brauliobo/zensmu/releases)
[![AUR zensmu-bin](https://img.shields.io/aur/version/zensmu-bin?label=AUR%20zensmu-bin)](https://aur.archlinux.org/packages/zensmu-bin)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Safety-first Rust CLI, live TUI (`zensmu top`), and unprivileged desktop GUI (`zensmu-gui`) for AMD Ryzen SMU control and telemetry on Linux.**

Uses the standard [`ryzen_smu`](https://github.com/amkillam/ryzen_smu) DKMS kernel module (`/sys/kernel/ryzen_smu_drv`) — no custom kernel required.

> [!WARNING]
> **Disclaimer:** Modifying CPU voltage, power, and thermal limits via SMU carries inherent risk of system instability, data loss, voided warranty, or hardware damage — use at your own risk.

### Live TUI (`zensmu top`)

<p align="center">
  <img src="docs/assets/zensmu-top.gif" alt="zensmu top live TUI monitoring and interactive tuning" width="820" />
</p>

### Desktop GUI (`zensmu-gui`)

<p align="center">
  <img src="docs/assets/zensmu-gui.gif" alt="zensmu-gui WCAG-validated desktop cockpit" width="820" />
</p>

---

## Highlights

- **CLI, TUI, and GUI in one workspace:** scriptable `zensmu` (with `--json`), `btop`-style `zensmu top`, and unprivileged `zensmu-gui` (elevates narrowly via `pkexec` only on demand).
- **Hardware-safe SMU transport:** defeats `ryzen_smu 0.1.7`'s `smu.c:213` false-OK bug via read-only SMN RSP + read-after-write verification, serializes mailbox calls with `flock`, cross-checks `CPUID`, discovers harvested cores automatically, and guards writes with `--dry-run`, `--confirm-within`, snapshots/rollback, and a boot-loop crash guard.
- **Full Desktop & Mobile APU tuning:** Curve Optimizer (per-core, all-core, iGPU), Zen 5 15-point Curve Shaper, PBO (`PPT`, `TDC`, `EDC`, `Tctl`, `Scalar`, `Fmax`), APU DPTC limits, and `systemd` boot/resume persistence.
- **Built-in testing & telemetry:** per-core CO stability tester (`zensmu test`), CSV/JSONL/Phoronix Test Suite recording (`zensmu record`), and unprivileged Prometheus exporter (`zensmu export`).

---

## Install

Every package bundles the `ryzen_smu` DKMS module and automatically builds/loads it on install (skipping safely if `ryzen_smu` is already installed) and removes it on uninstall.

### Arch Linux / CachyOS (AUR)

```bash
paru -S zensmu-bin     # prebuilt binary
paru -S zensmu-git     # build from main
```

### Debian / Ubuntu (APT Repo)

```bash
echo "deb [trusted=yes] https://brauliobo.github.io/zensmu/apt ./" | sudo tee /etc/apt/sources.list.d/zensmu.list
sudo apt update && sudo apt install linux-headers-$(uname -r) zensmu
```

### Fedora / RHEL / openSUSE (RPM Repo)

```bash
# Fedora / RHEL
curl -fsSL https://brauliobo.github.io/zensmu/rpm/zensmu.repo | sudo tee /etc/yum.repos.d/zensmu.repo
sudo dnf install kernel-devel-$(uname -r) zensmu

# openSUSE
sudo zypper addrepo --no-gpgcheck https://brauliobo.github.io/zensmu/rpm zensmu
sudo zypper refresh && sudo zypper install --allow-unsigned-rpm kernel-default-devel dkms zensmu
```

### Direct Packages, COPR & Source

Direct `.deb`, `.rpm`, `.pkg.tar.zst`, `.tar.gz`, and standalone `ryzen-smu-dkms` packages are on [GitHub Releases](https://github.com/brauliobo/zensmu/releases); a Fedora COPR spec is in [`packaging/copr/zensmu.spec`](packaging/copr/zensmu.spec).

```bash
git clone https://github.com/brauliobo/zensmu.git && cd zensmu
cargo build --release --workspace
sudo ./target/release/zensmu driver install   # installs headers/dkms if needed, builds & loads ryzen_smu
```

> **Driver & Secure Boot:** Manage the DKMS module anytime with `zensmu driver status`, `sudo zensmu driver install`, or `sudo zensmu driver uninstall`. Under UEFI Secure Boot, enroll the DKMS MOK (`sudo mokutil --import /var/lib/dkms/mok.pub` and reboot) or disable Secure Boot.

---

## Quick Usage

### Live TUI & Desktop GUI

```bash
zensmu top                # Unprivileged live telemetry
sudo zensmu top           # Full monitor + interactive [4:Tune] & [P] persistence
zensmu-gui                # Unprivileged desktop GUI
```

In `zensmu top`: `1`–`4` switch tabs, `←/→` adjust limits, `a`/`Enter` previews & applies, `w`/`W` records telemetry (`CSV`/`JSONL`/`PTS`), `P` persists to boot/resume, `R` rolls back, `?` shows help.

### CLI Essentials

```bash
# Diagnostics & driver
zensmu driver status
sudo zensmu doctor
zensmu info && sudo zensmu status
zensmu telemetry --json

# Curve Optimizer & Zen 5 Curve Shaper
sudo zensmu co get
sudo zensmu co set --all -20 --dry-run
sudo zensmu co set --all -20 --confirm-within 30
sudo zensmu co set --core 0=-15,1=-15,2..5=-25 --igpu -10
sudo zensmu shaper set --freq high --temp med=-15

# Desktop PBO & Mobile APU limits
sudo zensmu pbo set --ppt 105W --tdc 80A --edc 150A --tctl 85C --scalar 1 --fmax 5550
sudo zensmu apu set --stapm 25W --fast 35W --slow 28W --tdc-soc 15A --apu-skin 70C --mode max-performance

# Per-core stability test, recording (CSV/JSONL/PTS) & Prometheus exporter
sudo zensmu test --duration 30s --workload mixed --rollback-on-fail
zensmu record --interval 1s --duration 10m --pts-name ryzen-co20
zensmu export --listen 127.0.0.1:9452          # or: zensmu export service enable

# Rollback, reset, boot persistence & hardware reports
sudo zensmu rollback
sudo zensmu reset
sudo zensmu persist save --enable              # /etc/zensmu/active.toml + boot/resume guard
zensmu report --raw-pm-table --open            # privacy-safe CPU support / bug report
```

> Conservative soft bounds (e.g., CO `-35` on Zen 4/5, `-25` on Zen 3; PPT `230 W`) require `--allow-extreme`; hardware ceilings (e.g., `95 °C` Tctl) cannot be bypassed.

---

## Supported CPUs

Unverified codenames are refused rather than guessed. See [`docs/HARDWARE_MATRIX.md`](docs/HARDWARE_MATRIX.md) for CPUID mappings, PM tables, and validation status.

| Generation | Codenames | CO | iGPU CO | Shaper | PBO | Scalar | Fmax | Fused | APU DPTC |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **Zen 5 Desktop** | Granite Ridge (Ryzen 9000) | R/W (`±50`) | R/W | R/W* | W | R/W | R/W | R | — |
| **Zen 4 Desktop / HEDT** | Raphael (7000), Storm Peak (TR 7000) | R/W (`±50`) | R/W | — | W | R/W | R/W | R | — |
| **Zen 3 Desktop / HEDT** | Vermeer (5000), Chagall (TR 5000) | R/W (`±30`) | — | — | W | R/W | R | — | — |
| **Zen 2 Desktop / HEDT** | Matisse (3000), Castle Peak (TR 3000) | — | — | — | W | R/W | R | — | — |
| **Zen / Zen+ Desktop / HEDT** | Summit / Pinnacle Ridge, Colfax, TR | — | — | — | W | R/W | — | — | — |
| **Zen 5 Mobile APUs** | Strix Point, Strix Halo | R/W (`±50`) | R/W | — | W | R/W | R/W | R | W |
| **Zen 4 Mobile APUs** | Phoenix, Hawk Point | R/W (`±50`) | R/W | — | W | R/W | R/W | R | W |
| **Zen 3+ Mobile APUs** | Rembrandt | R/W (`±30`) | R/W | — | W | R/W | R/W | R | W |
| **Zen 3 Mobile APUs** | Cezanne (5000G/U/H) | R/W (`±30`) | R/W | — | W | R/W | R | R | W |
| **Zen 2 Mobile APUs** | Renoir, Lucienne, Van Gogh (Steam Deck) | W (Van Gogh) | — | — | W | R/W | R | R | W |
| **Zen / Zen+ Mobile APUs** | Raven Ridge, Raven Ridge 2, Dali, Picasso | — | — | — | W | R/W | R | R (Picasso) | W |

*\*Curve Shaper requires AGESA/SMU firmware exposing `0x84`/`0xA6`; when locked (`0xFE`), `zensmu` skips it cleanly.*

---

## Persistence & Safety

- **Boot & Resume Crash Guard:** `sudo zensmu persist save --enable` saves writable live settings to `/etc/zensmu/active.toml` and enables `zensmu-apply.service` + the `systemd-sleep` resume hook with a settle guard. If an unstable tune crashes during the settle window, auto-apply is blocked on the next boot until cleared with `sudo zensmu profile clear-guard`.
- **Non-goals:** No static Vcore/VID overrides, no arbitrary raw SMN writes, no blind PM table parsing, and no root GUI. Always preview with `--dry-run` and stress-test before persisting.

MIT License — see [`LICENSE`](LICENSE).
