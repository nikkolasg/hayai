#!/usr/bin/env bash
# Writes the Groth16 verifying key of the Sprout JoinSplit circuit to
# crates/hayai-prepared/src/sprout_vk/. hayai embeds the file in the binary.
#
# Usage:
#   scripts/fetch-params.sh DIR --sprout
#   scripts/extract-sprout-vk.sh DIR [OUT_DIR]
#
# DIR holds sprout-groth16.params. The script checks the size and the BLAKE2b-512 hash of
# the file (the constants of zcash_proofs 0.30) before it reads the key.
#
# The parameter file is the encoding of bellman `groth16::Parameters`. The encoding starts
# with the verifying key: 3 G1 points and 3 G2 points (uncompressed, 96 and 192 bytes),
# the number of IC points (4 bytes, big-endian), and the IC points (G1). The key file is
# that prefix of the parameter file, without a change.
set -euo pipefail

usage() { sed -n '2,15p' "$0"; }

if [[ $# -lt 1 || $# -gt 2 || "$1" == "-h" || "$1" == "--help" ]]; then
  usage >&2
  exit 2
fi
DIR="$1"
OUT="${2:-$(cd "$(dirname "$0")/.." && pwd)/crates/hayai-prepared/src/sprout_vk}"

for tool in b2sum od head; do
  command -v "${tool}" >/dev/null || { echo "extract-sprout-vk: ${tool} is required" >&2; exit 1; }
done

NAME="sprout-groth16"
BYTES=725523612
HASH="e9b238411bd6c0ec4791e9d04245ec350c9c5744f5610dfcce4365d5ca49dfefd5054e371842b3f88fa1b9d7e8e075249b3ebabd167fa8b0f3161292d36c180a"

# Bytes before the IC count: alpha_g1, beta_g1, beta_g2, gamma_g2, delta_g1, delta_g2.
FIXED=$((96 + 96 + 192 + 192 + 96 + 192))

file="${DIR}/${NAME}.params"
[[ -f "${file}" ]] || { echo "extract-sprout-vk: ${file} is absent" >&2; exit 1; }
if [[ "$(stat -c %s "${file}")" != "${BYTES}" ]] \
  || [[ "$(b2sum "${file}" | cut -d' ' -f1)" != "${HASH}" ]]; then
  echo "extract-sprout-vk: ${file} has a wrong size or hash" >&2
  exit 1
fi
mkdir -p "${OUT}"
ic=$((16#$(od -An -tx1 -j "${FIXED}" -N 4 "${file}" | tr -d ' \n')))
length=$((FIXED + 4 + 96 * ic))
head -c "${length}" "${file}" >"${OUT}/${NAME}.vk"
echo "extract-sprout-vk: ${OUT}/${NAME}.vk (${length} bytes, ${ic} IC points)"
