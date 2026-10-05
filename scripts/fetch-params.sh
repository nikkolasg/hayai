#!/usr/bin/env bash
# Downloads the Sapling parameters (sapling-spend.params, sapling-output.params) into a
# directory and checks the size and the BLAKE2b-512 hash of each file. A file that is
# already present with the correct hash is kept.
#
# Usage:
#   scripts/fetch-params.sh DIR [--url BASE_URL] [--sprout]
#
# --sprout also downloads sprout-groth16.params (725,523,612 bytes), the source of the
# Sprout verifying key (scripts/extract-sprout-vk.sh).
#
# BASE_URL defaults to the download location of zcash_proofs (DOWNLOAD_URL in
# zcash_proofs 0.30). Each file is served in parts (<name>.part.1, <name>.part.2), as
# zcash_proofs::download_sapling_parameters reads them. The sizes and hashes are the
# constants of zcash_proofs 0.30 (SAPLING_*_BYTES, SAPLING_*_HASH, SPROUT_BYTES, SPROUT_HASH).
set -euo pipefail

usage() { sed -n '2,15p' "$0"; }

DIR=""
BASE_URL="https://download.z.cash/downloads"
SPROUT=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --url) BASE_URL="$2"; shift 2 ;;
    --sprout) SPROUT=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "fetch-params: unknown option $1" >&2; exit 2 ;;
    *) DIR="$1"; shift ;;
  esac
done
if [[ -z "${DIR}" ]]; then
  usage >&2
  exit 2
fi

for tool in curl b2sum; do
  command -v "${tool}" >/dev/null || { echo "fetch-params: ${tool} is required" >&2; exit 1; }
done

mkdir -p "${DIR}"

# name bytes blake2b-512
PARAMS=(
  "sapling-spend.params 47958396 8270785a1a0d0bc77196f000ee6d221c9c9894f55307bd9357c3f0105d31ca63991ab91324160d8f53e2bbd3c2633a6eb8bdf5205d822e7f3f73edac51b2b70c"
  "sapling-output.params 3592860 657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028"
)
if [[ "${SPROUT}" == 1 ]]; then
  PARAMS+=("sprout-groth16.params 725523612 e9b238411bd6c0ec4791e9d04245ec350c9c5744f5610dfcce4365d5ca49dfefd5054e371842b3f88fa1b9d7e8e075249b3ebabd167fa8b0f3161292d36c180a")
fi

valid() { # FILE BYTES HASH
  [[ -f "$1" ]] && [[ "$(stat -c %s "$1")" == "$2" ]] && [[ "$(b2sum "$1" | cut -d' ' -f1)" == "$3" ]]
}

for entry in "${PARAMS[@]}"; do
  read -r name bytes hash <<<"${entry}"
  file="${DIR}/${name}"
  if valid "${file}" "${bytes}" "${hash}"; then
    echo "fetch-params: ${name} is present"
    continue
  fi
  part="${file}.download"
  rm -f "${part}"
  curl -fsSL --retry 3 -o "${part}" "${BASE_URL}/${name}.part.1"
  if [[ "$(stat -c %s "${part}")" -lt "${bytes}" ]]; then
    curl -fsSL --retry 3 "${BASE_URL}/${name}.part.2" >>"${part}"
  fi
  if ! valid "${part}" "${bytes}" "${hash}"; then
    rm -f "${part}"
    echo "fetch-params: ${name} has a wrong size or hash" >&2
    exit 1
  fi
  mv "${part}" "${file}"
  echo "fetch-params: ${name} downloaded"
done
