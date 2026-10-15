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
fi

exec ibmish tn5250 --listen "${IBMISH_TN5250_LISTEN:-0.0.0.0:23}"
