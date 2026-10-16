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

# The sample files, display files and CL programs (scripts/samples.sh).
setup=$(IBMISH_BIN="$bin" IBMISH_SAMPLES="$samples" "$(dirname "$0")/samples.sh")
echo "$setup"
fresh=0
if grep -qx "Loaded CUSTMAST" <<<"$setup"; then
  fresh=1
else
  echo "File CUSTMAST already existed; keeping its data."
fi

status=0
# Trailing blanks and today's date (MM/DD/YY) do not count.
normalize() {
  sed -e 's/[[:space:]]*$//' -e 's#[ 0-9][0-9]/[0-9][0-9]/[0-9][0-9]#MM/DD/YY#g'
}
run() {
  local pgm="$1" parm="${2:-}" expected out
  if [ -n "$parm" ]; then
    expected="$samples/expected/${pgm}_${parm}.txt"
    echo "--- CALL $pgm PARM($parm)"
    out=$("$bin" call "$pgm" "$parm" | grep -v '^Picked up JAVA_TOOL_OPTIONS' || true)
  else
    expected="$samples/expected/${pgm}.txt"
    echo "--- CALL $pgm"
    out=$("$bin" call "$pgm" | grep -v '^Picked up JAVA_TOOL_OPTIONS' || true)
  fi
  echo "$out"
  if diff <(normalize <<<"$out") <(normalize <"$expected") >/dev/null; then
    echo "PASS: output matches $(basename "$expected")"
  else
    echo "FAIL: output differs from $(basename "$expected"):"
    diff <(normalize <<<"$out") <(normalize <"$expected") || true
    status=1
  fi
}
run CUSTLIST 3
run ORDTOTAL 1002
run OWING L
run CUSTRPT L
# A printed report (O specs, EXCEPT). The conformance runner replays file
# operations only; the page layout is the Jariko runner's.
if [[ "${IBMISH_RUNNER:-}" != *BridgeConformance* ]]; then
  run CUSTPRT
fi
# CUSTUPD changes data, so it only runs right after the sample files were created.
if [ "$fresh" = 1 ]; then
  run CUSTUPD 5
else
  echo "--- skipping CUSTUPD (sample files already existed; drop them for a full run)"
fi
exit $status
