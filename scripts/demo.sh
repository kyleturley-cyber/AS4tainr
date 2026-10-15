#!/usr/bin/env bash
# OpenEye spike demo: create the sample files, load data, run the sample
# programs and compare their output with the golden files.
#
# Env:
#   OPENEYE_DATABASE_URL  PostgreSQL URL (default: local openeye database)
#   OPENEYE_RUNNER        runner command (default: the Jariko jar)
#   OPENEYE_PGM_PATH      directory holding the .rpgle sources
#   OPENEYE_BIN           path to the openeye binary (default: openeye on PATH)
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
samples="${OPENEYE_SAMPLES:-$here/samples}"
bin="${OPENEYE_BIN:-openeye}"
export OPENEYE_PGM_PATH="${OPENEYE_PGM_PATH:-$samples/qrpglesrc}"

"$bin" init

for f in CUSTMAST ORDLIN; do
  if out=$("$bin" crtpf "QGPL/$f" --src "$samples/qddssrc/$f.pf" 2>&1); then
    echo "$out"
    "$bin" load "$f" --csv "$samples/data/$f.csv"
  elif grep -q CPF7302 <<<"$out"; then
    echo "File $f already exists; keeping its data."
  else
    echo "$out" >&2
    exit 1
  fi
done

status=0
run() {
  local pgm="$1" parm="$2" expected="$samples/expected/${1}_${2}.txt"
  echo "--- CALL $pgm PARM($parm)"
  local out
  out=$("$bin" call "$pgm" "$parm" | grep -v '^Picked up JAVA_TOOL_OPTIONS' || true)
  echo "$out"
  # Jariko prints DSPLY lines as-is; ignore surrounding blanks only.
  if diff <(sed 's/[[:space:]]*$//' <<<"$out") <(sed 's/[[:space:]]*$//' "$expected") >/dev/null; then
    echo "PASS: output matches $(basename "$expected")"
  else
    echo "FAIL: output differs from $(basename "$expected"):"
    diff <(sed 's/[[:space:]]*$//' <<<"$out") <(sed 's/[[:space:]]*$//' "$expected") || true
    status=1
  fi
}
run CUSTLIST 3
run ORDTOTAL 1002
exit $status
