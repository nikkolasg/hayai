#!/usr/bin/env bash
# Writes the Groth16 verifying keys of the Sapling Spend and Output circuits to
# crates/hayai-prepared/src/sapling_vk/. hayai embeds the two files in the binary.
#
# Usage:
#   scripts/fetch-params.sh DIR
#   scripts/extract-sapling-vk.sh DIR [OUT_DIR]
#
# DIR holds sapling-spend.params and sapling-output.params. The script checks the size and
# the BLAKE2b-512 hash of each file (the constants of zcash_proofs 0.30) before it reads
# the key.
#
# A parameter file is the encoding of bellman `groth16::Parameters`. The encoding starts
# with the verifying key: 3 G1 points and 3 G2 points (uncompressed, 96 and 192 bytes),
# the number of IC points (4 bytes, big-endian), and the IC points (G1). The key file is
# that prefix of the parameter file, without a change.
set -euo pipefail

usage() { sed -n '2,16p' "$0"; }

if [[ $# -lt 1 || $# -gt 2 || "$1" == "-h" || "$1" == "--help" ]]; then
  usage >&2
  exit 2
fi
DIR="$1"
OUT="${2:-$(cd "$(dirname "$0")/.." && pwd)/crates/hayai-prepared/src/sapling_vk}"

for tool in b2sum od head; do
  command -v "${tool}" >/dev/null || { echo "extract-sapling-vk: ${tool} is required" >&2; exit 1; }
done

# name bytes blake2b-512
PARAMS=(
  "sapling-spend 47958396 8270785a1a0d0bc77196f000ee6d221c9c9894f55307bd9357c3f0105d31ca63991ab91324160d8f53e2bbd3c2633a6eb8bdf5205d822e7f3f73edac51b2b70c"
  "sapling-output 3592860 657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028"
)

# Bytes before the IC count: alpha_g1, beta_g1, beta_g2, gamma_g2, delta_g1, delta_g2.
FIXED=$((96 + 96 + 192 + 192 + 96 + 192))

mkdir -p "${OUT}"
for entry in "${PARAMS[@]}"; do
  read -r name bytes hash <<<"${entry}"
  file="${DIR}/${name}.params"
  [[ -f "${file}" ]] || { echo "extract-sapling-vk: ${file} is absent" >&2; exit 1; }
  if [[ "$(stat -c %s "${file}")" != "${bytes}" ]] \
    || [[ "$(b2sum "${file}" | cut -d' ' -f1)" != "${hash}" ]]; then
    echo "extract-sapling-vk: ${file} has a wrong size or hash" >&2
    exit 1
  fi
  ic=$((16#$(od -An -tx1 -j "${FIXED}" -N 4 "${file}" | tr -d ' \n')))
  length=$((FIXED + 4 + 96 * ic))
  head -c "${length}" "${file}" >"${OUT}/${name}.vk"
  echo "extract-sapling-vk: ${OUT}/${name}.vk (${length} bytes, ${ic} IC points)"
done
