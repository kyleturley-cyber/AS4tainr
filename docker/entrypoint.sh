#!/usr/bin/env bash
# Container entrypoint: prepare the catalog (and, optionally, the sample
# files), then run the 5250 server in the foreground.
set -euo pipefail

ibmish init

if [ "${IBMISH_LOAD_SAMPLES:-1}" = "1" ]; then
  for f in CUSTMAST ORDLIN; do
    if ibmish crtpf "QGPL/$f" --src "${IBMISH_SAMPLES:-/opt/ibmish/samples}/qddssrc/$f.pf" >/dev/null 2>&1; then
      ibmish load "$f" --csv "${IBMISH_SAMPLES:-/opt/ibmish/samples}/data/$f.csv"
    fi
  done
  for l in CUSTNAME CUSTOWE; do
    ibmish crtlf "QGPL/$l" --src "${IBMISH_SAMPLES:-/opt/ibmish/samples}/qddssrc/$l.lf" >/dev/null 2>&1 || true
  done
  ibmish crtbndcl QGPL/CUSTRPT --src "${IBMISH_SAMPLES:-/opt/ibmish/samples}/qclsrc/CUSTRPT.clle" --text 'Customer report driver' >/dev/null 2>&1 || true
  for d in CUSTINQD CUSTWRKD; do
    ibmish crtdspf "QGPL/$d" --src "${IBMISH_SAMPLES:-/opt/ibmish/samples}/qddssrc/$d.dspf" >/dev/null 2>&1 || true
  done
fi

exec ibmish tn5250 --listen "${IBMISH_TN5250_LISTEN:-0.0.0.0:23}"
