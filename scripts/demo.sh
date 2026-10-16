#!/usr/bin/env bash
# ibmish spike demo: create the sample files, load data, run the sample
# programs and compare their output with the golden files.
#
# Env:
#   IBMISH_DATABASE_URL  PostgreSQL URL (default: local ibmish database)
#   IBMISH_RUNNER        runner command (default: the Jariko jar)
#   IBMISH_PGM_PATH      directory holding the .rpgle sources
#   IBMISH_BIN           path to the ibmish binary (default: ibmish on PATH)
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
samples="${IBMISH_SAMPLES:-$here/samples}"
bin="${IBMISH_BIN:-ibmish}"
export IBMISH_PGM_PATH="${IBMISH_PGM_PATH:-$samples/qrpglesrc}"

"$bin" init

fresh=0
for f in CUSTMAST ORDLIN; do
  if out=$("$bin" crtpf "QGPL/$f" --src "$samples/qddssrc/$f.pf" 2>&1); then
    echo "$out"
    "$bin" load "$f" --csv "$samples/data/$f.csv"
    fresh=1
  elif grep -q CPF7302 <<<"$out"; then
    echo "File $f already exists; keeping its data."
  else
    echo "$out" >&2
    exit 1
  fi
done

# Logical files over CUSTMAST (created once, like the physical files).
for l in CUSTNAME CUSTOWE; do
  if out=$("$bin" crtlf "QGPL/$l" --src "$samples/qddssrc/$l.lf" 2>&1); then
    echo "$out"
  elif grep -q CPF7302 <<<"$out"; then
    echo "File $l already exists."
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
run OWING L
# CUSTUPD changes data, so it only runs right after the sample files were created.
if [ "$fresh" = 1 ]; then
  run CUSTUPD 5
else
  echo "--- skipping CUSTUPD (sample files already existed; drop them for a full run)"
fi
exit $status
