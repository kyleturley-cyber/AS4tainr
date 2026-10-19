#!/usr/bin/env bash
# Container entrypoint: prepare the catalog (and, optionally, the sample
# files), then run the 5250 server in the foreground.
set -euo pipefail

as4tainr init

if [ "${AS4TAINR_LOAD_SAMPLES:-1}" = "1" ]; then
  # Sample files (loaded the first time only), display files, CL programs.
  AS4TAINR_BIN=as4tainr "$(dirname "$0")/samples.sh" || echo "as4tainr: the sample objects could not all be created; starting anyway." >&2
fi

exec as4tainr tn5250 --listen "${AS4TAINR_TN5250_LISTEN:-0.0.0.0:23}"
