#!/usr/bin/env bash
# Installs hayaid as a systemd service on a Linux host.
#
# Usage:
#   scripts/install.sh [--network testnet|mainnet|regtest] [--binary PATH] [--backend upstream|zakura]
#   scripts/install.sh --uninstall [--purge]
#
# Install (run it again to upgrade; every step is idempotent):
#   1. Without --binary and as a normal user: build hayaid from this checkout
#      (cargo build --release --locked), then run the remaining steps with sudo.
#   2. Install the binary as /usr/local/bin/hayaid.
#   3. Create the system user hayai.
#   4. Write /etc/hayai/hayaid.toml from `hayaid config --network N`, with the data
#      directory /var/lib/hayai. An existing file is kept.
#   5. Install deploy/systemd/hayaid.service and enable it. The script does not start
#      the service: edit the configuration first (docs/install.md, Bare metal).
#
# Uninstall: stop and remove the service and the binary. The configuration, the data in
# /var/lib/hayai and the user stay. --purge removes them too.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN=/usr/local/bin/hayaid
CONFIG_DIR=/etc/hayai
CONFIG="${CONFIG_DIR}/hayaid.toml"
DATA_DIR=/var/lib/hayai
UNIT=/etc/systemd/system/hayaid.service
USER_NAME=hayai

NETWORK=testnet
BINARY=""
BACKEND=upstream
UNINSTALL=0
PURGE=0

usage() { sed -n '2,21p' "$0"; }
die() { echo "install.sh: $*" >&2; exit 1; }
log() { echo "install.sh: $*"; }

ARGS=("$@")
while [[ $# -gt 0 ]]; do
  case "$1" in
    --network) NETWORK="$2"; shift 2 ;;
    --binary) BINARY="$2"; shift 2 ;;
    --backend) BACKEND="$2"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    --purge) PURGE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument $1 (see --help)" ;;
  esac
done

case "${NETWORK}" in testnet|mainnet|regtest) ;; *) die "--network must be testnet, mainnet or regtest" ;; esac
case "${BACKEND}" in upstream|zakura) ;; *) die "--backend must be upstream or zakura" ;; esac
if [[ ${PURGE} -eq 1 && ${UNINSTALL} -eq 0 ]]; then
  die "--purge applies to --uninstall only"
fi

uninstall() {
  if [[ -f "${UNIT}" ]]; then
    systemctl disable --now hayaid.service
  fi
  rm -f "${UNIT}" "${BIN}"
  systemctl daemon-reload
  log "removed the service and ${BIN}"
  if [[ ${PURGE} -eq 1 ]]; then
    rm -rf "${CONFIG_DIR}" "${DATA_DIR}"
    if getent passwd "${USER_NAME}" >/dev/null; then
      userdel "${USER_NAME}"
    fi
    log "purged ${CONFIG_DIR}, ${DATA_DIR} and the user ${USER_NAME}"
  else
    log "kept ${CONFIG_DIR}, ${DATA_DIR} and the user ${USER_NAME} (--purge removes them)"
  fi
}

# Replaces the one line that matches PATTERN in FILE with LINE, or fails.
set_line() { # FILE PATTERN LINE
  local count
  count=$(grep -c -- "$2" "$1" || true)
  [[ "${count}" == 1 ]] || die "expected one line matching '$2' in the output of hayaid config, found ${count}"
  sed -i "s|$2|$3|" "$1"
}

write_config() {
  if [[ -f "${CONFIG}" ]]; then
    log "kept ${CONFIG}"
    return
  fi
  local tmp
  tmp=$(mktemp "${CONFIG_DIR}/hayaid.toml.XXXXXX")
  "${BIN}" config --network "${NETWORK}" >"${tmp}"
  set_line "${tmp}" '^data_dir = .*$' "data_dir = \"${DATA_DIR}\""
  chmod 0644 "${tmp}"
  mv "${tmp}" "${CONFIG}"
  log "wrote ${CONFIG} (${NETWORK})"
}

install_all() {
  command -v systemctl >/dev/null || die "systemd is required"
  [[ -x "${BINARY}" ]] || die "${BINARY} is not an executable file"

  install -m 0755 "${BINARY}" "${BIN}"
  log "installed ${BIN}"

  if ! getent passwd "${USER_NAME}" >/dev/null; then
    useradd --system --user-group --home-dir "${DATA_DIR}" --no-create-home \
      --shell /usr/sbin/nologin "${USER_NAME}"
    log "created the user ${USER_NAME}"
  fi
  install -d -m 0755 "${CONFIG_DIR}"
  install -d -o "${USER_NAME}" -g "${USER_NAME}" -m 0750 "${DATA_DIR}"

  write_config

  install -m 0644 "${REPO}/deploy/systemd/hayaid.service" "${UNIT}"
  systemctl daemon-reload
  systemctl enable hayaid.service
  log "enabled hayaid.service; edit ${CONFIG}, then run: sudo systemctl start hayaid"
}

if [[ ${EUID} -ne 0 ]]; then
  if [[ ${UNINSTALL} -eq 0 && -z "${BINARY}" ]]; then
    command -v cargo >/dev/null || die "cargo is required to build hayaid (or pass --binary)"
    log "building hayaid (${BACKEND} backend)"
    cargo build --manifest-path "${REPO}/Cargo.toml" --release --locked -p hayaid \
      --no-default-features --features "${BACKEND}"
    ARGS+=(--binary "${REPO}/target/release/hayaid")
  fi
  exec sudo "$0" "${ARGS[@]}"
fi

if [[ ${UNINSTALL} -eq 1 ]]; then
  uninstall
  exit 0
fi
[[ -n "${BINARY}" ]] || die "as root, pass --binary PATH (run as a normal user to build from source)"
install_all
