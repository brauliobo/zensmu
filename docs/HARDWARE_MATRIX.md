# Supported AMD Ryzen Hardware Matrix

`zensmu` uses strict, per-generation static SMU capability tables and version-gated PM table decoders. Unknown codenames, mismatched CPUID family/model pairs, or unverified PM table versions (> 4096 B or unknown ID) are explicitly refused rather than guessed.

> **Upstream driver note:** For Zen 5 (`Granite Ridge`, `9000X3D`, `Fire Range`, `Strix Point`, `Krackan Point`, `Strix Halo`) and modern Linux kernels (6.12+), install the maintained [`amkillam/ryzen_smu`](https://github.com/amkillam/ryzen_smu) DKMS fork (automated by `sudo zensmu driver install` on Debian/Ubuntu/Fedora/openSUSE, or `paru -S ryzen_smu-dkms-git` on Arch/CachyOS).
>
> **Secure Boot / Kernel Lockdown:** Out-of-tree DKMS modules like `ryzen_smu` cannot load unsigned when UEFI Secure Boot is enabled or `/sys/kernel/security/lockdown` is set to `integrity`/`confidentiality`. Enroll your DKMS Machine Owner Key (`sudo mokutil --import /var/lib/dkms/mok.pub` and reboot) or disable Secure Boot in UEFI firmware. Check your current status with `zensmu driver status`.

---

## Validation Status Legend

| Status | Meaning |
| :--- | :--- |
| **Hardware-verified** | Tested directly on live silicon with verified SMU mailbox responses, SMN RSP verification, and decoded PM table telemetry. |
| **Spec-implemented — community report welcome** | Implemented from `ryzen_smu` / `ZenStates-Core` register specs and unit-tested against synthetic fixtures; live hardware reports are welcome to promote to Hardware-verified. |

---

## Desktop & HEDT Processors

| Generation | Codename (Driver ID) | Example SKUs | CPUID (Fam / Model) | Curve Optimizer | iGPU CO | Curve Shaper | PBO Limits / Scalar / Fmax | APU DPTC | PM Table Versions | Validation Status |
| :--- | :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: | :--- | :--- |
| **Zen 5 Desktop** | **Granite Ridge** (`23`) | Ryzen 5 9600X, Ryzen 7 9700X, Ryzen 9 9900X / 9950X | `0x1A` / `0x44` | R/W (`-50..+50`, soft `-35`) | R/W | R/W* | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | — (Slow PPT W) | 1-CCD (`0x724` B): `0x0062_0105`, `0x0062_1101`, `0x0062_1102`<br>2-CCD (`0x994` B): `0x0062_0205`, `0x0062_1201`, `0x0062_1202` | **Hardware-verified** (Ryzen 7 9700X, `0x0062_0105`)<br>2-CCD: **Spec-implemented — community report welcome** |
| **Zen 5 Desktop X3D** | **Granite Ridge** (`23`) | Ryzen 7 9800X3D, Ryzen 9 9900X3D / 9950X3D | `0x1A` / `0x44` | R/W (`-50..+50`, soft `-35`) | R/W | R/W* | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | — (Slow PPT W) | Same as Granite Ridge (`0x0062_0105`/`1101`/`1102` 1-CCD; `0x0062_0205`/`1201`/`1202` 2-CCD) | **Spec-implemented — community report welcome** |
| **Zen 5 Mobile Chiplet** | **Fire Range** (mapped to `GraniteRidge` `23`) | Ryzen 9 9850HX, 9955HX, 9955HX3D | `0x1A` / `0x44`, `0x47` | R/W (`-50..+50`, soft `-35`) | R/W | R/W* | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | — (Slow PPT W) | Granite Ridge layouts (`0x0062_*`) | **Spec-implemented — community report welcome** |
| **Zen 4 Desktop** | **Raphael** (`20`) | Ryzen 7000, 7000X3D, Dragon Range 7045HX | `0x19` / `0x61` | R/W (`-50..+50`, soft `-35`) | R/W | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | — (Slow PPT W) | `0x0054_0000..0005`, `0x0054_0100..0108`, `0x0054_0208` | **Spec-implemented — community report welcome** |
| **Zen 4 HEDT** | **Storm Peak** (`25`) | Threadripper 7000 / PRO 7000WX | `0x19` / `0x18` | R/W (`-50..+50`, soft `-35`) | R/W* | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | — | `0x005C_0302`, `0x005C_0303`, `0x005C_0402`, `0x005C_0403` (clocks & voltages) | **Spec-implemented — community report welcome** |
| **Zen 3 Desktop** | **Vermeer** (`12`) | Ryzen 5000, 5900XT, 5800X3D / 5700X3D | `0x19` / `0x21` | R/W (`-30..+30`, soft `-25`)** | — | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R | — | `0x002D_0803`, `0x002D_0903`, `0x0038_0505..0905` | **Hardware-verified** (Ryzen 9 5900XT, `0x0038_0805`)<br>5800X3D/5700X3D: **Spec-implemented — community report welcome** |
| **Zen 3 HEDT** | **Chagall** (`19`) | Threadripper PRO 5000WX | `0x19` / `0x08` | R/W (`-30..+30`, soft `-25`) | — | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R | — | Same as Vermeer (`0x002D_*`, `0x0038_*`) | **Spec-implemented — community report welcome** |
| **Zen 2 Desktop** | **Matisse** (`4`) | Ryzen 3000 Desktop | `0x17` / `0x71` | — | — | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R | — | `0x0024_0802`, `0x0024_0902`, `0x0024_0503..0903` | **Spec-implemented — community report welcome** |
| **Zen 2 HEDT** | **Castle Peak** (`6`) | Threadripper 3000 | `0x17` / `0x31` | — | — | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R | — | Same as Matisse (`0x0024_*`) | **Spec-implemented — community report welcome** |
| **Zen+ Desktop** | **Pinnacle Ridge** (`10`) | Ryzen 2000 Desktop | `0x17` / `0x08` | — | — | — | PPT, TDC, EDC, Tctl, Scalar R/W | — | Not exposed | **Spec-implemented — community report welcome** |
| **Zen+ HEDT** | **Colfax** (`1`) | Threadripper 2000 | `0x17` / `0x08` | — | — | — | TDC, EDC, Tctl, Scalar R/W | — | Not exposed | **Spec-implemented — community report welcome** |
| **Zen 1 Desktop** | **Summit Ridge** (`9`) | Ryzen 1000 Desktop | `0x17` / `0x01` | — | — | — | PPT, EDC, TDP/TjMax R | — | Not exposed | **Spec-implemented — community report welcome** |
| **Zen 1 HEDT** | **Threadripper** (`5`) | Threadripper 1000 (Whitehaven) | `0x17` / `0x01` | — | — | — | PPT, EDC, TDP/TjMax R | — | Not exposed | **Spec-implemented — community report welcome** |

*\*Curve Shaper (`0x84`/`0xA6`) requires AGESA/SMU firmware that unlocks the opcode; if the SMU returns `0xFE` (`UnknownCmd`), `zensmu` detects and skips it cleanly. On Storm Peak / HEDT without integrated graphics, iGPU CO opcodes are ignored or rejected by SMU.*  
*\*\*On Ryzen 5800X3D/5700X3D, Curve Optimizer support depends on motherboard AGESA/BIOS exposing CO via SMU.*

---

## Mobile, Handheld & Desktop APUs

| Generation | Codename (Driver ID) | Example SKUs | CPUID (Fam / Model) | Curve Optimizer | iGPU CO | Curve Shaper | PBO Limits / Scalar / Fmax | APU DPTC | PM Table Versions | Validation Status |
| :--- | :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: | :--- | :--- |
| **Zen 5 Mobile APU** | **Strix Point** (`22`) | Ryzen AI 9 HX 370 / 365 | `0x1A` / `0x24` | R/W (`-50..+50`, soft `-35`) | R/W | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | Full (`STAPM`, `Slow`, `APU Slow`, `SoC TDC/EDC`, `Skin Temp`, `MaxPerf`/`PowerSaving`) | `0x005D_0008..000B` (`0xD54` B: APU header + FCLK/UCLK/MCLK) | **Spec-implemented — community report welcome** |
| **Zen 5 Mobile APU** | **Krackan Point** (mapped to `StrixPoint` `22`) | Ryzen AI 7 350, AI 5 340 | `0x1A` / `0x60` | R/W (`-50..+50`, soft `-35`) | R/W | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | Full | Strix Point layouts (`0x005D_*`) or new table (report welcome) | **Spec-implemented — community report welcome** |
| **Zen 5 Halo APU** | **Strix Halo** (`26`) | Ryzen AI Max / Max+ 395 / 390 / 385 | `0x1A` / `0x70` | W (`-50..+50`, soft `-35`, MP1/RSMU) | R/W | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | Full | 1-CCD (`0xDC0`/`0xE50` B): `0x0064_0107..010C`<br>2-CCD (`0xE50` B): `0x0064_0207..020C` (STAPM, Fast/Slow/APU Slow PPT, VDD TDC, Tctl) | **Hardware-verified** (Ryzen AI Max+ 395, `0x0064_020C` telemetry & SMU probes)<br>1-CCD (`0x0064_010C`): **Spec-implemented — community report welcome** |
| **Zen 4 Mobile / Desktop APU** | **Phoenix** (`21`) / **Hawk Point** (`24`) | Ryzen 7040U/HS, 8040U/HS, 8000G | `0x19` / `0x74`, `0x75`, `0x78`, `0x7C` | R/W (`-50..+50`, soft `-35`) | R/W | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | Full | `0x004C_0001..0009` (`0x76C` / `0x778` B) | **Spec-implemented — community report welcome** |
| **Zen 3+ Mobile APU** | **Rembrandt** (`11`) | Ryzen 6000U/H, 7035U/H | `0x19` / `0x40`, `0x44` | R/W (`-30..+30`, soft `-25`) | R/W | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R/W, Fused R | Full | `0x0045_0004`, `0x0045_0005` (`0xAA4` / `0xAB0` B) | **Spec-implemented — community report welcome** |
| **Zen 3 Mobile / Desktop APU** | **Cezanne** (`14`) | Ryzen 5000G/U/H, Barcelo 5x25U | `0x19` / `0x50` | R/W (`-30..+30`, soft `-25`) | R/W | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R, Fused R | Full | `0x0040_0001..0005` (`0x8D0`–`0x94C` B) | **Spec-implemented — community report welcome** |
| **Zen 2 Mobile APU** | **Lucienne** (`17`) | Ryzen 5300U, 5500U, 5700U | `0x17` / `0x68` | — | — | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R, Fused R | Full | `0x0037_0000..0005` (`0x794`–`0x8C8` B) | **Hardware-verified** (Ryzen 7 5700U, `0x0037_0005`) |
| **Zen 2 Mobile / Desktop APU** | **Renoir** (`2`) | Ryzen 4000U/H/G | `0x17` / `0x60` | — | — | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R, Fused R | Full | `0x0037_0000..0005` (`0x794`–`0x8C8` B) | **Spec-implemented — community report welcome** |
| **Zen 2 Handheld APU** | **Van Gogh** (`13`) | Steam Deck (Aerith / Sephiroth) | `0x17` / `0x90`, `0x91` | W (`-30..+30`, MP1) | — | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R, Fused R | Full | `0x003F_0000` (`0x7AC` B) | **Spec-implemented — community report welcome** |
| **Zen+ Mobile / Desktop APU** | **Picasso** (`3`) | Ryzen 3000U/H/G | `0x17` / `0x18` | — | — | — | PPT, TDC, EDC, Tctl, Scalar R/W, Fmax R, Fused R | Full | `0x001E_0001..0005`, `0x001E_000A`, `0x001E_0101` | **Spec-implemented — community report welcome** |
| **Zen 1 Mobile / Desktop APU** | **Raven Ridge** (`7`), **Raven Ridge 2** (`8`), **Dali** (`16`) | Ryzen 2000U/G, Athlon 3000G / 3050U | `0x17` / `0x11`, `0x20` | — | — | — | PPT, TDC, EDC, Tctl, Scalar R | Full (except APU Slow) | `0x001E_0001..0005`, `0x001E_000A`, `0x001E_0101` | **Spec-implemented — community report welcome** |

---

## Explicitly Unsupported Platforms (Safely Refused)

- **EPYC Server CPUs (`Naples` ID `18`, `Milan` ID `15`, Genoa / Turin)**: Server SMU mailbox tables and multi-socket addressing differ significantly from consumer Ryzen. `zensmu` refuses mutating operations rather than risking wrong opcodes.
- **Unrecognized PM table versions or sizes > 4096 B**: `zensmu telemetry` refuses blind float parsing and recommends submitting a diagnostic report instead.

---

## Contributing a Hardware Verification Report

If your CPU is listed as **Spec-implemented — community report welcome**, or if you hit an unrecognized PM table version (for example on a new AGESA release, Ryzen 9000X3D, Strix Point, Krackan Point, Strix Halo, or Fire Range laptop), you can submit a privacy-safe diagnostic report in one command:

```bash
# Unprivileged report: collects CPUID, topology, Secure Boot/lockdown status,
# driver/PM table version, hwmon reference sensors, and raw non-zero PM table floats,
# then opens a prefilled GitHub issue in your browser:
zensmu report --raw-pm-table --open

# Optional (privileged, still strictly read-only): also includes RSMU/MP1 ping,
# factory fused limits, current CO, and Curve Shaper readouts:
sudo zensmu report --raw-pm-table --open
```

To inspect the report locally first or file via GitHub CLI (`gh`):

```bash
zensmu report --raw-pm-table              # print Markdown to stdout
zensmu report --raw-pm-table --create     # open issue directly via `gh`
```

**Privacy guarantee:** `zensmu report` is strictly read-only and never sends mutating SMU commands. It never collects hostnames, usernames, IP/MAC addresses, or hardware serial numbers.
