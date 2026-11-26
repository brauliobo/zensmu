# zensmu

**Safety-first Rust suite for AMD Ryzen SMU control and live telemetry on Linux.**

`zensmu` provides three native interfaces in one workspace—a scriptable **CLI** (`zensmu`), a `btop`-inspired **live TUI** (`zensmu top`), and an **unprivileged desktop GUI** (`zensmu-gui`)—all running directly on top of the stock [`ryzen_smu`](https://gitlab.com/leogx9r/ryzen_smu) kernel module (`/sys/kernel/ryzen_smu_drv`). **No custom kernel fork or out-of-tree patches required.**

---

## Why `zensmu`?

Existing Linux SMU scripts often write raw opcodes directly to sysfs without locking, without checking hardware return codes, without validating CPU topology, and without any rollback or crash recovery. `zensmu` is engineered from the ground up around hardware safety:

### Safety Architecture

- **Defeats `ryzen_smu 0.1.7`'s `smu.c:213` false-OK bug**
  Upstream `ryzen_smu` (`0.1.5`–`0.1.7`) can report `SMU_Return_OK (0x01)` to userspace even when the SMU firmware rejected a command. `zensmu` neutralizes this with two independent checks:
  1. **Read-only SMN RSP verification**: While still holding the transaction lock, `zensmu` performs a strict 4-byte read-only SMN query against the platform's hardware response register (`0x03B10570` / `0x03B1057C` on desktop; `0x03B10A80` / `0x03B10564` / `0x03B10578` / `0x03B10978` on APUs) to read the true hardware `SmuStatus`.
  2. **Read-after-write verification**: For every register that supports hardware readback (per-core CO, iGPU CO, Curve Shaper, PBO scalar, Fmax), `zensmu` reads the value back after writing and fails immediately if hardware does not match.
- **Exclusive cross-process `flock` transaction locking**
  `ryzen_smu`'s sysfs interface shares stateful `smu_args`, `rsmu_cmd`, `mp1_smu_cmd`, and `smn` files across all processes. `zensmu` acquires an exclusive `flock` on `/run/lock/zensmu.lock` (falling back to `/tmp/zensmu.lock` when unprivileged) across the entire 5-step transaction (`write args` → `write opcode` → `read driver rsp` → `read out args` → `read hardware SMN rsp`), preventing concurrent corruption.
- **Hardware identity cross-checking & explicit capability tables**
  Cross-checks `/sys/kernel/ryzen_smu_drv/codename` against x86_64 `CPUID` family/model/stepping before touching any mailbox. Every codename maps to an explicit static capability table—unsupported CPUs or missing opcodes refuse execution rather than guessing across generations.
- **Topology-aware SMU core masks**
  Reads real CCD/CCX/physical-core topology from `/sys/devices/system/cpu/cpu*/topology/` and L3 `shared_cpu_list`, encoding exact `ZenStates-Core` core masks (`(ccd << 28) | ((core_in_ccd % 8) << 20)` on Zen 3/4/5, `(ccd << 28) | ((ccx % 2) << 24) | ((core_in_ccd % 4) << 20)` on Zen 2, linear core index on APUs). Dual-CCD CPUs (e.g. 9900X, 9950X, 7950X3D) address every physical core accurately.
- **Bounded inputs & conservative soft limits**
  Every parameter is strictly clamped to hardware-valid bounds (e.g. Curve Optimizer `-50..+50` on Zen 4/5 with a conservative soft floor of `-35`; `-30..+30` on Zen 3 with soft floor `-25`; Curve Shaper `-30..+30`). Crossing conservative thresholds requires explicit `--allow-extreme`.
- **`--dry-run` preview**
  Prints the exact mailbox, opcode, encoded `6 × u32` arguments, and `current → target` transition before writing anything.
- **Dead-man's switch (`--confirm-within <secs>`)**
  Applies a tuning change and automatically rolls back to the pre-change snapshot if you do not confirm before the timeout—safe for testing aggressive undervolts over SSH or on a desktop.
- **Snapshots & one-command rollback**
  Every mutating operation snapshots readable state first so `zensmu rollback` restores the previous configuration cleanly, and `zensmu reset` zeros offsets back to defaults.
- **Boot-loop crash guard (`--boot-guard --settle-secs <N>`)**
  Before applying a profile at boot or resume, `zensmu` writes and `fsync`s a persistent sentinel, applies the profile, and waits for the settle window to complete before disarming it. If an unstable undervolt causes a freeze, MCE, or sudden reboot during the settle window, the next boot sees the armed sentinel and refuses to auto-apply—breaking boot loops automatically.
- **Unprivileged telemetry & GUI**
  Live telemetry (`zensmu telemetry`, `zensmu top`, and `zensmu-gui`) reads sysfs PM tables and `hwmon` **without root**. `zensmu-gui` always runs as your normal user and invokes `pkexec /usr/bin/zensmu` via PolicyKit (`org.zensmu.pkexec.zensmu`) only when mailbox reads/writes are requested.

---

## Supported CPUs & Capabilities

Unsupported or unverified codenames (e.g. Zen 1 / Zen+, Van Gogh, EPYC Naples/Milan) are explicitly refused rather than guessed.

| Generation | Codenames | Curve Optimizer | iGPU CO | Curve Shaper (5×3) | PBO Limits (PPT/TDC/EDC/TjMax) | PBO Scalar | Fmax Boost | Fused Factory Limits | APU DPTC (STAPM/Slow/Skin/Modes) |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **Zen 5 Desktop** | Granite Ridge (Ryzen 9000) | R/W (`-50..+50`, soft `-35`) | R/W | **R/W** | W | R/W | R/W | R | — |
| **Zen 4 Desktop / HEDT** | Raphael (Ryzen 7000), Storm Peak (TR 7000) | R/W (`-50..+50`, soft `-35`) | R/W | — | W | R/W | R/W | R | — |
| **Zen 3 Desktop / HEDT** | Vermeer (Ryzen 5000), Chagall (TR 5000) | W (`-30..+30`, soft `-25`) | — | — | W | R/W | R | — | — |
| **Zen 2 Desktop / HEDT** | Matisse (Ryzen 3000), Castle Peak (TR 3000) | — | — | — | W | R/W | R | — | — |
| **Zen 5 Mobile APUs** | Strix Point, Strix Halo | R/W (`-50..+50`, soft `-35`) | R/W | — | W | R/W | R/W | R | W |
| **Zen 4 Mobile APUs** | Phoenix, Hawk Point | R/W (`-50..+50`, soft `-35`) | R/W | — | W | R/W | R/W | R | W |
| **Zen 3+ Mobile APUs** | Rembrandt | R/W (`-30..+30`, soft `-25`) | R/W | — | W | R/W | R/W | R | W |
| **Zen 3 Mobile APUs** | Cezanne | W (`-30..+30`, soft `-25`) | W | — | W | R/W | R | R | W |
| **Zen 2 Mobile APUs** | Renoir, Lucienne | — | — | — | W | R/W | R | R | W |

---

## Installation

### Prerequisites

Ensure the `ryzen_smu` kernel module is installed and loaded:

```bash
# On Arch / CachyOS (if not already provided by your kernel):
paru -S ryzen_smu-dkms-git

# Load now and persist across reboots:
sudo modprobe ryzen_smu
echo "ryzen_smu" | sudo tee /etc/modules-load.d/ryzen_smu.conf
```

### Arch Linux / CachyOS (AUR `zensmu-git` & `zensmu-bin`)

Two Arch `PKGBUILD`s are provided under [`packaging/arch/`](packaging/arch/):

- **From source (`zensmu-git`)**: [`packaging/arch/PKGBUILD`](packaging/arch/PKGBUILD)
  ```bash
  cd packaging/arch
  makepkg -si
  ```
- **Prebuilt binary (`zensmu-bin`)**: [`packaging/arch/zensmu-bin/PKGBUILD`](packaging/arch/zensmu-bin/PKGBUILD) (with `.SRCINFO` ready for AUR)
- **Local binary package & pacman repo helper**:
  ```bash
  # Build zensmu-bin-*.pkg.tar.zst locally into ./dist/
  ./packaging/arch/build-bin-pkg.sh

  # Build and publish to a local pacman repository (e.g. /srv/wb-archer)
  ./packaging/arch/build-bin-pkg.sh --publish /srv/wb-archer wb-archer.db.tar.gz
  ```

Both packages install `zensmu`, `zensmu-gui`, the `systemd` boot service and sleep/resume hook, the PolicyKit policy, shell completions (bash/zsh/fish), and the license.

### Build from Source

Requires Rust 1.78+ (`cargo`, `rustc`):

```bash
git clone https://github.com/brauliobo/zensmu.git
cd zensmu
cargo build --release --workspace

# Optional: install binaries locally
sudo install -Dm755 target/release/zensmu /usr/bin/zensmu
sudo install -Dm755 target/release/zensmu-gui /usr/bin/zensmu-gui
sudo install -Dm644 packaging/polkit/org.zensmu.policy /usr/share/polkit-1/actions/org.zensmu.policy
```

---

## Quickstart & CLI Reference

All read-only inspection and telemetry commands work cleanly; mailbox operations require `sudo` (or `pkexec`). Pass `--json` to commands for machine-readable output.

### Inspect Hardware & Diagnostics

```bash
# Show detected CPU, CPUID, topology (CCDs/cores/SMT), driver version, and capabilities
zensmu info

# Run full safety diagnostics (driver status, smu.c:213 false-OK detection,
# lock health, mailbox ping test, boot-guard state, PM table check)
sudo zensmu doctor

# Show current live SMU settings (CO margins, Curve Shaper, PBO scalar, Fmax, factory limits)
sudo zensmu status
```

### Curve Optimizer (`co`)

```bash
# Read current per-core Curve Optimizer offsets (Zen 4 / Zen 5 / supported APUs)
sudo zensmu co get

# Preview an all-core -20 undervolt without touching hardware
sudo zensmu co set --all -20 --dry-run

# Apply -20 all-core with a 30-second dead-man's switch (auto-rolls back if unconfirmed)
sudo zensmu co set --all -20 --confirm-within 30

# Set per-core offsets (e.g. weaker best cores at -15, others at -25)
sudo zensmu co set --core 0=-15 --core 1=-15 --core 2=-25 --core 3=-25

# Override conservative soft floor (e.g. -40 on Zen 4/5)
sudo zensmu co set --all -40 --allow-extreme --confirm-within 30

# Set iGPU Curve Optimizer offset (supported CPUs/APUs)
sudo zensmu co set --igpu -15

# Reset all Curve Optimizer offsets to 0
sudo zensmu co reset
```

### Curve Shaper (`shaper`, Zen 5)

```bash
# Read the full 5x3 (min/low/med/high/max freq x low/med/high temp) Curve Shaper matrix
sudo zensmu shaper get

# Set a specific frequency tier & temperature band(s) (range -50..+30)
sudo zensmu shaper set --freq max --temp high=-15
sudo zensmu shaper set --freq high --temp low=-10,med=-15,high=-5 --dry-run

# Reset all 15 Curve Shaper points to 0
sudo zensmu shaper reset
```

### PBO Limits, Scalar & Fmax (`pbo`)

```bash
# Read active PM table limits, PBO scalar, Fmax, fused factory limits, and CPPC ranking
sudo zensmu pbo get

# Preview and set PBO limits (accepts W/mW, A/mA, C/°C, MHz/GHz)
sudo zensmu pbo set --ppt 120W --tdc 95A --edc 150A --tctl 85C --dry-run
sudo zensmu pbo set --ppt 120W --tdc 95A --edc 150A --tctl 85C

# Set PBO scalar (1..=10) and Fmax boost ceiling (MHz)
sudo zensmu pbo set --scalar 1 --fmax 5550
sudo zensmu pbo reset
```

### Mobile APU DPTC Controls (`apu`)

```bash
# Set STAPM / slow / fast power limits, skin temperature target, and power mode
sudo zensmu apu set --stapm 28W --slow 35W --fast 45W --apu-skin 45C --mode power-saving
sudo zensmu apu set --mode max-performance
```

### Live Telemetry, TUI & Unprivileged GUI

```bash
# One-shot or JSON telemetry snapshot (no root needed)
zensmu telemetry
zensmu telemetry --json

# Launch btop-style live terminal dashboard (no root needed)
zensmu top

# Launch unprivileged native desktop GUI (uses PolicyKit pkexec only when applying writes)
zensmu-gui
```

### Rollback & Reset

```bash
# Roll back to the most recent automatic pre-change snapshot
sudo zensmu rollback

# Reset runtime CO / Curve Shaper offsets back to 0 (safe defaults)
sudo zensmu reset
```

### Shell Completions

```bash
zensmu completions bash > ~/.local/share/bash-completion/completions/zensmu
zensmu completions zsh  > ~/.zfunc/_zensmu
zensmu completions fish > ~/.config/fish/completions/zensmu.fish
```

---

## Profiles & `systemd` Persistence

AMD Ryzen SMU settings are volatile—they reset on every reboot and across suspend/hibernate (`S3`/`S0ix`/`S4`) cycles. `zensmu` supports declarative TOML profiles and ships hardened `systemd` persistence units.

### Profile TOML Format (`/etc/zensmu/active.toml`)

```toml
schema = 1

# Pin this profile to a specific CPU codename so it can never be applied to the wrong machine
[target]
codename = "granite-ridge"
physical_cores = 8

[curve_optimizer]
# Set an all-core baseline, and/or fine-tune specific physical cores
all = -20
# per_core = { "0" = -15, "1" = -15, "2" = -25, "3" = -25 }
# igpu = -10

# Zen 5 15-point Curve Shaper (optional; rejected on non-Zen 5 CPUs)
[curve_shaper]
min  = { low = 0,   med = 0,   high = 0   }
low  = { low = 0,   med = 0,   high = 0   }
med  = { low = -10, med = -15, high = -15 }
high = { low = -10, med = -15, high = -20 }
max  = { low = -5,  med = -10, high = -15 }

[pbo]
ppt_w = 120
tdc_a = 95
edc_a = 150
tctl_c = 85
# scalar = 1
# fmax_mhz = 5550

# Mobile APU DPTC settings (optional; APU platforms only)
# [apu]
# stapm_w = 28
# slow_w = 35
# fast_w = 45
# apu_skin_temp_c = 45
# power_mode = "power-saving"
```

### Managing Profiles

```bash
# Export current hardware settings to a profile file
sudo zensmu profile export /etc/zensmu/active.toml

# Validate & preview without writing
sudo zensmu profile apply /etc/zensmu/active.toml --dry-run

# Apply interactively with dead-man's switch before enabling at boot
sudo zensmu profile apply /etc/zensmu/active.toml --confirm-within 30
```

### Enabling Boot & Resume Persistence

```bash
# 1. Install the systemd unit and sleep/resume hook (already done if installed via PKGBUILD)
sudo install -Dm644 packaging/systemd/zensmu-apply.service /usr/lib/systemd/system/zensmu-apply.service
sudo install -Dm755 packaging/systemd/zensmu-resume /usr/lib/systemd/system-sleep/zensmu-resume

# 2. Ensure ryzen_smu loads at boot
echo "ryzen_smu" | sudo tee /etc/modules-load.d/ryzen_smu.conf

# 3. Enable the boot service
sudo systemctl daemon-reload
sudo systemctl enable zensmu-apply.service
```

#### Boot-Loop Crash Guard Recovery

`zensmu-apply.service` invokes:

```bash
/usr/bin/zensmu profile apply /etc/zensmu/active.toml --yes --boot-guard --settle-secs 15
```

And `zensmu-resume` re-applies on `post/*` resume with `--boot-guard --settle-secs 5`.

If an unstable undervolt crashes or reboots the system before the settle timer completes, the crash-guard sentinel stays armed on disk and future boots/resumes will **refuse to auto-apply** the profile, leaving the CPU at safe BIOS defaults. To recover:

1. Relax the unstable values in `/etc/zensmu/active.toml`.
2. Clear the tripped guard:
   ```bash
   sudo zensmu profile clear-guard
   ```
3. Verify the revised profile with `--dry-run` and `--confirm-within 30`.

---

## Disclaimer & What Is Intentionally Excluded

> **Warning:** Modifying CPU power limits, Curve Optimizer, or Curve Shaper offsets alters voltage/frequency operating points. While `zensmu` enforces strict bounds, SMN verification, snapshots, and boot-loop protection, aggressive undervolts can still cause system freezes, MCEs, or silent calculation errors until properly stress-tested (e.g. CoreCycler / Y-Cruncher / Prime95). Always preview with `--dry-run` and test interactively with `--confirm-within` before persisting a profile. **Use at your own risk.**

To keep `zensmu` safe by construction, the following hazardous features are **intentionally excluded**:

- **No manual OC VID / static Vcore overrides** — Static voltage overrides bypass AMD's silicon FIT protection and risk hardware degradation. `zensmu` sticks strictly to PBO, Curve Optimizer, Curve Shaper, and APU DPTC within safe bounds.
- **No arbitrary raw SMN register writes** — `zensmu` only ever writes 4-byte read addresses to `/sys/kernel/ryzen_smu_drv/smn` (triggering `smu_read_address`, read-only) and never performs 8-byte SMN writes.
- **No `-f` / force blind PM table parsing** — `zensmu` never guesses struct offsets for unrecognized `pm_table_version` layouts and refuses PM tables larger than Linux's `4096`-byte sysfs `PAGE_SIZE` limit.
- **No cross-generation opcode guessing** — Unknown or unverified codenames are rejected outright.
- **No running a GUI as root** — `zensmu-gui` runs completely unprivileged and delegates only explicit CLI operations through PolicyKit (`pkexec`).

---

## License

MIT — see [`LICENSE`](LICENSE).
