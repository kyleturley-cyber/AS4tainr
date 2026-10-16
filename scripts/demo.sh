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
run CUSTRPT L
# CUSTUPD changes data, so it only runs right after the sample files were created.
if [ "$fresh" = 1 ]; then
  run CUSTUPD 5
else
  echo "--- skipping CUSTUPD (sample files already existed; drop them for a full run)"
fi
exit $status
