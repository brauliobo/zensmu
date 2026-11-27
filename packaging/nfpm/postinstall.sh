#!/bin/sh
set -eu

# 1. Reload systemd units when systemd is active.
if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi

# 2. First-class DKMS lifecycle for the bundled ryzen_smu kernel module.
#    - Never conflicts with an externally installed ryzen_smu DKMS package (e.g.
#      ryzen_smu-dkms-git on Arch or standalone ryzen-smu-dkms) or a built-in module.
#    - Never fails the package transaction if kernel headers / dkms are missing or
#      Secure Boot blocks unsigned modules; prints an actionable next step instead.
BUNDLED_DIR="/usr/share/zensmu/ryzen_smu-dkms"
MODLOAD_CONF="/etc/modules-load.d/ryzen_smu.conf"

if [ ! -f "${BUNDLED_DIR}/dkms.conf" ] || [ ! -f "${BUNDLED_DIR}/VERSION" ]; then
    exit 0
fi

SMU_VER="$(tr -d ' \t\r\n' < "${BUNDLED_DIR}/VERSION")"
if [ -z "${SMU_VER}" ]; then
    exit 0
fi

# Check whether an external (non-zensmu-managed) ryzen_smu DKMS tree exists.
external_dkms=0
for dir in /usr/src/ryzen_smu-*; do
    [ -d "${dir}" ] || continue
    if [ ! -f "${dir}/.zensmu-managed" ]; then
        external_dkms=1
    fi
done

if [ "${external_dkms}" = "1" ]; then
    if command -v modprobe >/dev/null 2>&1; then
        modprobe ryzen_smu >/dev/null 2>&1 || true
    fi
    exit 0
fi

# Clean up any older zensmu-managed DKMS versions on package upgrade.
if command -v dkms >/dev/null 2>&1; then
    for dir in /usr/src/ryzen_smu-*; do
        [ -f "${dir}/.zensmu-managed" ] || continue
        old_ver="${dir#/usr/src/ryzen_smu-}"
        if [ "${old_ver}" != "${SMU_VER}" ]; then
            dkms remove -m ryzen_smu -v "${old_ver}" --all >/dev/null 2>&1 || true
            rm -rf "${dir}"
        fi
    done
fi

# If ryzen_smu is already provided directly by the running kernel (not via DKMS), just load it.
if command -v modinfo >/dev/null 2>&1 && modinfo -n ryzen_smu >/dev/null 2>&1; then
    mod_path="$(modinfo -n ryzen_smu 2>/dev/null || true)"
    case "${mod_path}" in
        */dkms/*|*/extra/*) ;;
        "") ;;
        *)
            if command -v modprobe >/dev/null 2>&1; then
                modprobe ryzen_smu >/dev/null 2>&1 || true
            fi
            exit 0
            ;;
    esac
fi

if ! command -v dkms >/dev/null 2>&1; then
    echo "zensmu: 'dkms' not found; install dkms + kernel headers and run 'sudo zensmu driver install' to build ryzen_smu." >&2
    exit 0
fi

TARGET_DIR="/usr/src/ryzen_smu-${SMU_VER}"
mkdir -p "${TARGET_DIR}"
for f in drv.c smu.c smu.h Makefile LICENSE dkms.conf VERSION; do
    if [ -f "${BUNDLED_DIR}/${f}" ]; then
        cp -f "${BUNDLED_DIR}/${f}" "${TARGET_DIR}/${f}"
    fi
done
printf '%s\n' "${SMU_VER}" > "${TARGET_DIR}/.zensmu-managed"

if [ ! -f "${MODLOAD_CONF}" ]; then
    mkdir -p /etc/modules-load.d
    printf '# Managed by zensmu\nryzen_smu\n' > "${MODLOAD_CONF}"
fi

dkms status -m ryzen_smu -v "${SMU_VER}" 2>/dev/null | grep -q "ryzen_smu" || \
    dkms add -m ryzen_smu -v "${SMU_VER}" >/dev/null 2>&1 || true

if dkms build -m ryzen_smu -v "${SMU_VER}" && dkms install -m ryzen_smu -v "${SMU_VER}" --force; then
    if command -v modprobe >/dev/null 2>&1; then
        if ! modprobe ryzen_smu >/dev/null 2>&1; then
            echo "zensmu: built & installed ryzen_smu/${SMU_VER} via DKMS, but 'modprobe ryzen_smu' did not load (check Secure Boot / MOK signing with 'zensmu driver status')." >&2
        fi
    fi
else
    echo "zensmu: ryzen_smu/${SMU_VER} DKMS build could not complete (missing kernel headers for $(uname -r)?). Run 'sudo zensmu driver install' to install headers and finish building." >&2
fi
