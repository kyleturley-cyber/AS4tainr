#!/usr/bin/env bash
# Create the sample objects in QGPL: physical files (loaded from CSV the
# first time), logical files, display files and CL programs. Safe to run
# again: existing files keep their data, CL programs are replaced.
#
# Env:
#   IBMISH_BIN      path to the ibmish binary (default: ibmish on PATH)
#   IBMISH_SAMPLES  samples directory (default: ../samples next to this script)
# Prints "Loaded FILE" for each physical file created and loaded now.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
samples="${IBMISH_SAMPLES:-$here/samples}"
bin="${IBMISH_BIN:-ibmish}"

"$bin" init >/dev/null

# Creates an object; "already exists" (CPF7302) is fine, anything else fails.
create() {
  local out
  if out=$("$bin" "$@" 2>&1); then
    return 0
  elif grep -q CPF7302 <<<"$out"; then
    return 1
  else
    echo "$out" >&2
    exit 1
  fi
}

for f in CUSTMAST ORDLIN ITEMMAST ORDHDR; do
  if create crtpf "QGPL/$f" --src "$samples/qddssrc/$f.pf"; then
    "$bin" load "$f" --csv "$samples/data/$f.csv" >/dev/null
    echo "Loaded $f"
  fi
done
for l in CUSTNAME CUSTOWE; do
  create crtlf "QGPL/$l" --src "$samples/qddssrc/$l.lf" || true
done
for d in CUSTINQD CUSTWRKD ORDENTD CUSTMNTD; do
  create crtdspf "QGPL/$d" --src "$samples/qddssrc/$d.dspf" || true
done
# The order entry application's next order number (after the sample
# orders 1001-1003).
if ! out=$("$bin" system "CRTDTAARA DTAARA(QGPL/ORDNXT) TYPE(*DEC) LEN(7 0) VALUE(1004) TEXT('Next order number')" 2>&1) \
   && ! grep -q CPF1023 <<<"$out"; then
  echo "$out" >&2
  exit 1
fi
"$bin" crtbndcl QGPL/CUSTRPT --src "$samples/qclsrc/CUSTRPT.clle" --text 'Customer report driver' >/dev/null
"$bin" crtbndcl QGPL/ORDENTC --src "$samples/qclsrc/ORDENTC.clle" --text 'Order entry' >/dev/null
# A small legacy-style application, imported the way a customer's git
# repository of IBM i source would be (docs/import.md).
if ! out=$("$bin" import "$samples/apps/contacts" --lib CONTACTS 2>&1); then
  echo "$out" >&2
  exit 1
fi
echo "Sample objects ready in QGPL; the contacts application is in CONTACTS."
