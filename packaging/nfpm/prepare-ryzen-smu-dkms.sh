#!/usr/bin/env bash
set -euo pipefail

# Clone upstream amkillam/ryzen_smu and prepare DKMS source + package staging files
# for both the bundled zensmu DKMS tree (/usr/share/zensmu/ryzen_smu-dkms) and the
# standalone packaging/nfpm/ryzen-smu-dkms.yaml package.
# Outputs RYZEN_SMU_VERSION to stdout (and GITHUB_OUTPUT when running under GitHub Actions).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

rm -rf build/ryzen_smu_git build/ryzen_smu_src
mkdir -p build/ryzen_smu_src dist

git clone https://github.com/amkillam/ryzen_smu.git build/ryzen_smu_git
REV="$(git -C build/ryzen_smu_git rev-list --count HEAD)"
SHA="$(git -C build/ryzen_smu_git rev-parse --short=7 HEAD)"
SMU_VER="0.1.7.r${REV}.g${SHA}"

cp build/ryzen_smu_git/drv.c \
   build/ryzen_smu_git/smu.c \
   build/ryzen_smu_git/smu.h \
   build/ryzen_smu_git/Makefile \
   build/ryzen_smu_git/LICENSE \
   build/ryzen_smu_src/

printf '%s\n' "${SMU_VER}" > build/ryzen_smu_src/VERSION

cat > build/ryzen_smu_src/dkms.conf <<EOF
PACKAGE_NAME="ryzen_smu"
PACKAGE_VERSION="${SMU_VER}"
MAKE[0]="make -C \$kernel_source_dir M=\$dkms_tree/\$PACKAGE_NAME/\$PACKAGE_VERSION/build modules"
BUILT_MODULE_NAME[0]="ryzen_smu"
DEST_MODULE_LOCATION[0]="/kernel/drivers/ryzen_smu"
AUTOINSTALL="yes"
EOF

cat > build/ryzen_smu_src/ryzen_smu.modules-load.conf <<'EOF'
# Autoload AMD Ryzen SMU driver for zensmu
ryzen_smu
EOF

cat > build/ryzen_smu_src/postinstall.sh <<EOF
#!/bin/sh
set -eu
VER="${SMU_VER}"
if command -v dkms >/dev/null 2>&1; then
  dkms status -m ryzen_smu -v "\${VER}" 2>/dev/null | grep -q "ryzen_smu" || \
    dkms add -m ryzen_smu -v "\${VER}" || true
  dkms build -m ryzen_smu -v "\${VER}" || true
  dkms install -m ryzen_smu -v "\${VER}" --force || true
fi
if command -v modprobe >/dev/null 2>&1; then
  modprobe ryzen_smu >/dev/null 2>&1 || true
fi
EOF

cat > build/ryzen_smu_src/preremove.sh <<EOF
#!/bin/sh
set -eu
# Skip removal during deb/rpm package upgrades; only remove on full uninstall.
case "\${1:-}" in
  upgrade|failed-upgrade|deconfigure)
    exit 0
    ;;
esac
is_rpm_upgrade=0
case "\${1:-}" in
  ""|*[!0-9]*) ;;
  0) ;;
  *) is_rpm_upgrade=1 ;;
esac
if [ "\${is_rpm_upgrade}" = "1" ]; then
  exit 0
fi

VER="${SMU_VER}"
if command -v modprobe >/dev/null 2>&1; then
  modprobe -r ryzen_smu >/dev/null 2>&1 || true
fi
if command -v dkms >/dev/null 2>&1; then
  dkms remove -m ryzen_smu -v "\${VER}" --all >/dev/null 2>&1 || true
fi
EOF

chmod 755 build/ryzen_smu_src/postinstall.sh build/ryzen_smu_src/preremove.sh

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "version=${SMU_VER}" >> "${GITHUB_OUTPUT}"
fi

echo "${SMU_VER}"
