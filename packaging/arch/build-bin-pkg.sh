#!/usr/bin/env bash
# Build release binaries, create the zensmu-bin Arch package (.pkg.tar.zst),
# and optionally publish it to a local pacman repository (e.g. /srv/wb-archer).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PKGVER="${PKGVER:-0.1.0}"
PKGREL="${PKGREL:-6}"
TARGET_TRIPLE="x86_64-unknown-linux-gnu"
STAGE_NAME="zensmu-v${PKGVER}-${TARGET_TRIPLE}"

cd "${REPO_ROOT}"

echo "==> Running workspace tests"
cargo test --workspace

echo "==> Building release binaries"
cargo build --release --workspace

TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

STAGE="${TMPDIR}/${STAGE_NAME}"
mkdir -p "${STAGE}/packaging/systemd" "${STAGE}/packaging/polkit"
cp "target/release/zensmu" "${STAGE}/zensmu"
cp "target/release/zensmu-gui" "${STAGE}/zensmu-gui"
cp "packaging/systemd/zensmu-apply.service" "${STAGE}/packaging/systemd/"
cp "packaging/systemd/zensmu-resume" "${STAGE}/packaging/systemd/"
cp "packaging/polkit/org.zensmu.policy" "${STAGE}/packaging/polkit/"
cp "README.md" "LICENSE" "${STAGE}/"

TARBALL="${TMPDIR}/zensmu-bin-${PKGVER}-x86_64.tar.gz"
tar -C "${TMPDIR}" -czf "${TARBALL}" "${STAGE_NAME}"

BUILD_DIR="${TMPDIR}/pkgbuild"
mkdir -p "${BUILD_DIR}"
cp "packaging/arch/zensmu-bin/PKGBUILD" "${BUILD_DIR}/PKGBUILD"
cp "${TARBALL}" "${BUILD_DIR}/"

# Point local PKGBUILD build at the freshly built local tarball
sed -i "s|^source=.*|source=(\"zensmu-bin-\${pkgver}-x86_64.tar.gz\")|" "${BUILD_DIR}/PKGBUILD"
sed -i "s|^pkgver=.*|pkgver=${PKGVER}|" "${BUILD_DIR}/PKGBUILD"
sed -i "s|^pkgrel=.*|pkgrel=${PKGREL}|" "${BUILD_DIR}/PKGBUILD"

echo "==> Running makepkg"
(cd "${BUILD_DIR}" && makepkg -f --noconfirm)

PKG_FILE="$(ls "${BUILD_DIR}"/zensmu-bin-*.pkg.tar.zst | head -n 1)"
OUT_DIR="${REPO_ROOT}/dist"
mkdir -p "${OUT_DIR}"
cp "${PKG_FILE}" "${OUT_DIR}/"
FINAL_PKG="${OUT_DIR}/$(basename "${PKG_FILE}")"
echo "==> Built package: ${FINAL_PKG}"

if [[ "${1:-}" == "--publish" ]]; then
    REPO_DIR="${2:-/srv/wb-archer}"
    DB_NAME="${3:-wb-archer.db.tar.gz}"
    echo "==> Publishing $(basename "${FINAL_PKG}") to ${REPO_DIR} (${DB_NAME})"
    sudo install -Dm644 -o root -g http "${FINAL_PKG}" "${REPO_DIR}/$(basename "${FINAL_PKG}")"
    sudo repo-add "${REPO_DIR}/${DB_NAME}" "${REPO_DIR}/$(basename "${FINAL_PKG}")"
    # Dereference .db / .files symlinks created by repo-add into regular files
    # (HTTP servers such as oxiroute reject symlinks with 403 Forbidden).
    DB_BASE="${DB_NAME%.db.tar.gz}"
    sudo cp -L --remove-destination "${REPO_DIR}/${DB_BASE}.db.tar.gz" "${REPO_DIR}/${DB_BASE}.db"
    sudo cp -L --remove-destination "${REPO_DIR}/${DB_BASE}.files.tar.gz" "${REPO_DIR}/${DB_BASE}.files"
    sudo chmod 644 "${REPO_DIR}/${DB_BASE}.db" "${REPO_DIR}/${DB_BASE}.files"
    echo "==> Published successfully to ${REPO_DIR}"
fi
