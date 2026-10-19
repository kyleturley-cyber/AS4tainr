#!/usr/bin/env bash
# Import a public repository of IBM i source into a library and report what
# builds. The repository is cloned at test time and never added to ibmish
# (check its license before using it for anything else).
#
#   tools/corpus.sh [GIT_URL] [SUBDIR] [LIB]
#
# Default: Nick Litten's examples, folder IBMi, library NICK.
# Env: IBMISH_BIN (default target/debug/ibmish), IBMISH_DATABASE_URL.
set -euo pipefail
url="${1:-https://github.com/NickLitten/nick.litten.public.git}"
sub="${2:-IBMi}"
lib="${3:-NICK}"
bin="${IBMISH_BIN:-target/debug/ibmish}"
work="${CORPUS_DIR:-$(mktemp -d)}"
if [ ! -d "$work/repo/.git" ]; then
  git clone -q --depth 1 "$url" "$work/repo"
fi
"$bin" init >/dev/null
"$bin" import "$work/repo/$sub" --lib "$lib" || true
