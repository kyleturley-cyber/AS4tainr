#!/usr/bin/env python3
"""Compare as4tainr with output captured on a real IBM i.

Golden outputs are captured with tools/golden/golden.ps1 (PUB400 over SSH,
see docs/golden-outputs.md) from a commands file of `name|CL command` or
`name|!shell line` lines. They are kept out of the repository (they hold
IBM's message texts). This script runs the same commands through
`as4tainr system`, writes our output next to the golden files as
<name>.ours.txt, and reports where the two differ.

Compared after normalizing:
- report header lines: product, level, system name, date and time masked;
- PUB400's own page footer (`-=* https://pub400.com *=-`) dropped;
- messages: only the message ID, the stream (stdout or stderr "2> ") and
  their order count, never the text (ours is written fresh: clean room).

Usage:
  AS4TAINR_BIN=target/debug/as4tainr python3 tools/golden_compare.py \
      tools/golden/basics.txt target/ibmi/golden --setup "CRTLIB AS4TRAINR1"
"""

import os
import re
import subprocess
import sys

BIN = os.environ.get("AS4TAINR_BIN", "target/debug/as4tainr")
MSG = re.compile(r"^(2> )?([A-Z]{3}[0-9A-F]{4}): ")
STAMP = re.compile(r"\d\d[-/.,]\d\d[-/.,]\d\d\s+\d\d[:.]\d\d[:.]\d\d( [A-Z]{3})?")


def run_ours(cmd):
    """One commands-file entry through `as4tainr system`, in the golden
    capture's format (golden.ps1)."""
    if cmd.startswith("!"):
        # Raw shell line: `system` means our system utility.
        line = re.sub(r"(^|[;&|]\s*)system\b", r"\1%s system" % BIN, cmd[1:])
        p = subprocess.run(["bash", "-c", line], stdin=subprocess.DEVNULL,
                           capture_output=True, text=True)
        out = p.stdout.splitlines() + ["rc=%d" % p.returncode]
        out += ["2> " + l for l in p.stderr.splitlines()]
        return out
    p = subprocess.run([BIN, "system", cmd], stdin=subprocess.DEVNULL,
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return p.stdout.splitlines() + ["rc=%d" % p.returncode]


def normalize(lines):
    out = []
    for l in lines:
        if "-=* https://pub400.com *=-" in l:
            continue
        m = MSG.match(l)
        if m:
            out.append("%s%s: <text>" % (m.group(1) or "", m.group(2)))
            continue
        # Values that depend on when the object was made.
        l = re.sub(r"(File level identifier [. ]*:\s+)\d{13}", r"\1<level id>", l)
        l = re.sub(r"(Creation date [. ]*:\s+)\S+", r"\1<date>", l)
        if "Page" in l or STAMP.search(l):
            l = STAMP.sub("<date time>", l)
            l = re.sub(r"^ ?\S+ V\dR\dM\d\s+\d{6}", "<product>", l)
            l = re.sub(r"(<date time>|Page)", r"\1", l)
            l = re.sub(r"\s{2,}\S+\s{2,}(?=<date time>)", "  <system>  ", l)
        out.append(l.rstrip())
    return out


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    commands, golden = sys.argv[1], sys.argv[2]
    setup = []
    if "--setup" in sys.argv:
        setup = sys.argv[sys.argv.index("--setup") + 1:]
    for s in setup:
        subprocess.run([BIN, "system", s], capture_output=True)
    same = differ = missing = 0
    for raw in open(commands, encoding="utf-8"):
        raw = raw.strip()
        if not raw or raw.startswith("#"):
            continue
        name, cmd = [x.strip() for x in raw.split("|", 1)]
        gfile = os.path.join(golden, name + ".txt")
        ours = run_ours(cmd)
        with open(os.path.join(golden, name + ".ours.txt"), "w", encoding="utf-8") as f:
            f.write("\n".join(ours) + "\n")
        if not os.path.exists(gfile):
            missing += 1
            print("MISSING  %s (no golden file)" % name)
            continue
        g = normalize(open(gfile, encoding="utf-8").read().splitlines())
        o = normalize(ours)
        if g == o:
            same += 1
            print("SAME     %s" % name)
            continue
        differ += 1
        print("DIFFER   %s" % name)
        for i in range(max(len(g), len(o))):
            a = g[i] if i < len(g) else "<none>"
            b = o[i] if i < len(o) else "<none>"
            if a != b:
                print("   ibm : %s" % a)
                print("   ours: %s" % b)
    print("%d same, %d differ, %d without golden output" % (same, differ, missing))
    return 0 if differ == 0 and missing == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
