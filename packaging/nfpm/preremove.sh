#!/bin/sh
set -eu

# Only remove the zensmu-managed ryzen_smu DKMS module on full package removal,
# NOT during package upgrades across deb, rpm, or archlinux:
# - deb upgrade passes: upgrade | failed-upgrade | deconfigure
# - rpm upgrade passes: positive integer >= 1 (uninstall passes 0)
# - arch pre_remove:    only invoked on full removal (passes pkgver, e.g. 0.3.1-1)
case "${1:-}" in
    upgrade|failed-upgrade|deconfigure)
        exit 0
        ;;
esac

is_rpm_upgrade=0
case "${1:-}" in
    ""|*[!0-9]*) ;;
    0) ;;
    *) is_rpm_upgrade=1 ;;
esac
if [ "${is_rpm_upgrade}" = "1" ]; then
    exit 0
fi

# Only unload & remove DKMS trees explicitly managed by zensmu (.zensmu-managed),
# leaving externally installed ryzen_smu-dkms-git / ryzen-smu-dkms untouched.
found_managed=0
for dir in /usr/src/ryzen_smu-*; do
    if [ -f "${dir}/.zensmu-managed" ]; then
        found_managed=1
        break
    fi
done

if [ "${found_managed}" = "1" ]; then
    if command -v modprobe >/dev/null 2>&1; then
        modprobe -r ryzen_smu >/dev/null 2>&1 || true
    fi
    for dir in /usr/src/ryzen_smu-*; do
        [ -f "${dir}/.zensmu-managed" ] || continue
        ver="${dir#/usr/src/ryzen_smu-}"
        if command -v dkms >/dev/null 2>&1; then
            dkms remove -m ryzen_smu -v "${ver}" --all >/dev/null 2>&1 || true
        fi
        rm -rf "${dir}"
    done
fi

MODLOAD_CONF="/etc/modules-load.d/ryzen_smu.conf"
if [ -f "${MODLOAD_CONF}" ] && grep -q '^# Managed by zensmu' "${MODLOAD_CONF}" 2>/dev/null; then
    rm -f "${MODLOAD_CONF}"
fi
