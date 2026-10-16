#!/usr/bin/env bash
# Container entrypoint: prepare the catalog (and, optionally, the sample
# files), then run the 5250 server in the foreground.
set -euo pipefail

ibmish init

if [ "${IBMISH_LOAD_SAMPLES:-1}" = "1" ]; then
  # Sample files (loaded the first time only), display files, CL programs.
  IBMISH_BIN=ibmish "$(dirname "$0")/samples.sh" || echo "ibmish: the sample objects could not all be created; starting anyway." >&2
fi

exec ibmish tn5250 --listen "${IBMISH_TN5250_LISTEN:-0.0.0.0:23}"
