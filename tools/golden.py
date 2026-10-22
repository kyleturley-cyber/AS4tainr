#!/usr/bin/env python3
"""Compare as4tainr with captures from a real IBM i ("golden" outputs).

The captures are made on an IBM i with a list of commands, one per line:

    name|CL command            run as: system 'CL command' 2>&1
    name|!shell line           run as written; stderr kept apart ("2> " lines)

and saved as GOLDEN_DIR/<name>.txt: the output lines, then "rc=N", then
(for shell lines) the standard error lines prefixed "2> ". This tool runs the
same list against as4tainr, with `system` being `as4tainr system`, in a
fresh library of the capture's name and IFS directories under /home, and
compares what can be compared without copying IBM's texts:

  - the exit status (0, 255 for an escape message, 2 for a usage error);
  - the message identifiers written to standard output and standard error,
    in order;
  - whether printed output (a listing) was written.

Message texts are never compared for equality as a goal: as4tainr writes its
own texts (clean room). The opposite is checked instead: a message whose text
is the same as IBM's is reported as a clean-room problem to fix.

The captures hold IBM's texts, so they stay outside the repository (the
default place is target/ibmi/golden, which git ignores), and so does the
report this tool writes with --report.

usage: tools/golden.py GOLDEN_DIR COMMANDS_FILE... [--bin PATH] [--report FILE]
                      [--only NAME...] [--keep]
       tools/golden.py GOLDEN_DIR --audit SOURCE_DIR

--audit looks for IBM's message texts in as4tainr's source instead: each
captured message becomes a pattern (names and numbers taken out) and each
string literal of the source with "ID: text" the same way; a match is a
text that must be written again in as4tainr's own words.
environment: AS4TAINR_DATABASE_URL (a database this tool may change)
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile

MSG = re.compile(r"^([A-Z]{3}[0-9A-F]{4}): ?(.*)$")
# Lines a capture system adds to every printed page (PUB400's own footer).
FOOTER = re.compile(r"^\s*-=\*.*\*=-\s*$")


def read_commands(paths):
    items = []
    for p in paths:
        for line in open(p, encoding="utf-8"):
            line = line.rstrip("\n").rstrip("\r")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            name, cmd = line.split("|", 1)
            items.append((name.strip(), cmd.strip()))
    return items


def parse_result(lines, raw):
    """(stdout lines, rc, stderr lines) of one capture or run."""
    out, err, rc = [], [], None
    for line in lines:
        if line.startswith("rc=") and rc is None:
            rc = int(line[3:])
        elif line.startswith("2> "):
            err.append(line[3:])
        elif rc is None or not raw:
            out.append(line)
    out = [l for l in out if not FOOTER.match(l)]
    return out, rc, err


def messages(lines):
    """[(id, text)] of the message lines."""
    res = []
    for l in lines:
        m = MSG.match(l.strip())
        if m:
            res.append((m.group(1), m.group(2).strip()))
    return res


def printed(lines):
    """Whether there is printed output: lines that are not messages."""
    return any(l.strip() and not MSG.match(l.strip()) for l in lines)


def shell_for(cmd, sandbox):
    if cmd.startswith("!"):
        line = cmd[1:].replace("/home/", sandbox + "/home/")
        return (
            "{ %s ; } </dev/null 2>%s/err; echo \"rc=$?\"; sed 's/^/2> /' %s/err"
            % (line, sandbox, sandbox)
        )
    q = cmd.replace("'", "'\\''")
    return "system '%s' </dev/null 2>&1; echo \"rc=$?\"" % q


SHIM = """#!/bin/sh
# `system` for golden.py: the sandbox's /home goes to the IFS first, and
# paths into the sandbox in the command become IFS paths again.
if [ -d "{sb}/home" ]; then
  for d in "{sb}"/home/*; do
    [ -d "$d" ] && "{bin}" put "$d" "/home/$(basename "$d")" >/dev/null 2>&1
  done
fi
for a in "$@"; do
  set -- "$@" "$(printf '%s' "$a" | sed 's#{sb}/home/#/home/#g')"
  shift
done
exec "{bin}" system "$@"
"""


def skeleton(text):
    """A message text with the values taken out: names, numbers, quoted
    values and format placeholders become #."""
    t = re.sub(r"\{[^{}]*\}", "#", text)
    t = re.sub(r"'[^']*'", "#", t)
    t = re.sub(r"&\d+", "#", t)
    t = re.sub(r"(?<![A-Za-z])[*/]?[A-Z0-9][A-Z0-9_$#@./*-]*(?![a-z])", "#", t)
    t = re.sub(r"#(\s*#)+", "#", t)
    return re.sub(r"\s+", " ", t).strip().rstrip(".")


def audit(golden, src):
    ibm = {}
    for f in sorted(os.listdir(golden)):
        if not f.endswith(".txt"):
            continue
        for line in open(os.path.join(golden, f), encoding="utf-8"):
            line = line.rstrip("\n")
            if line.startswith("2> "):
                line = line[3:]
            m = MSG.match(line.strip())
            if m and len(skeleton(m.group(2))) > 8:
                ibm.setdefault((m.group(1), skeleton(m.group(2))), f[:-4])
    lit = re.compile(r'"([A-Z]{3}[0-9A-F]{4}): ((?:[^"\\]|\\.)*)"')
    found = 0
    for root, _, files in os.walk(src):
        for f in sorted(files):
            if not f.endswith(".rs"):
                continue
            path = os.path.join(root, f)
            for n, line in enumerate(open(path, encoding="utf-8"), 1):
                for m in lit.finditer(line):
                    key = (m.group(1), skeleton(m.group(2)))
                    if key in ibm:
                        found += 1
                        print("%s:%d: %s text is IBM's (capture %s)" % (path, n, m.group(1), ibm[key]))
    print("%d texts to write again (%d IBM texts known)" % (found, len(ibm)))
    return 1 if found else 0


def main():
    if "--audit" in sys.argv:
        i = sys.argv.index("--audit")
        return audit(sys.argv[1], sys.argv[i + 1])
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("golden")
    ap.add_argument("commands", nargs="+")
    ap.add_argument("--bin", default="target/debug/as4tainr")
    ap.add_argument("--lib", default="AS4TRAINR1", help="the capture's library, made here")
    ap.add_argument("--home", default="AS4TRAINR", help="the capture's /home directory")
    ap.add_argument("--report")
    ap.add_argument("--only", nargs="*")
    ap.add_argument("--keep", action="store_true", help="keep the sandbox directory")
    ap.add_argument("--save", help="write our output of each command to SAVE/<name>.txt")
    ap.add_argument("--files", help="files the commands use (golden.ps1 -Files), put in /home/HOME/gld")
    a = ap.parse_args()
    bin_ = os.path.abspath(a.bin)
    items = read_commands(a.commands)
    if a.only:
        items = [i for i in items if i[0] in a.only]

    sb = tempfile.mkdtemp(prefix="golden-")
    os.makedirs(os.path.join(sb, "bin"))
    os.makedirs(os.path.join(sb, "home", a.home))
    if a.files:
        gld = os.path.join(sb, "home", a.home, "gld")
        os.makedirs(gld)
        for f in os.listdir(a.files):
            src = os.path.join(a.files, f)
            if os.path.isfile(src):
                shutil.copy(src, gld)
    shim = os.path.join(sb, "bin", "system")
    with open(shim, "w") as f:
        f.write(SHIM.replace("{sb}", sb).replace("{bin}", bin_))
    os.chmod(shim, 0o755)
    env = dict(os.environ, PATH=os.path.join(sb, "bin") + ":" + os.environ["PATH"])
    env["AS4TAINR_LIBL"] = env.get("AS4TAINR_LIBL", "QGPL,QTEMP")
    # The capture's library and home directory.
    subprocess.run([bin_, "init"], env=env, capture_output=True)
    # A fresh library and home directory, as before the capture.
    subprocess.run([bin_, "system", "DLTLIB LIB(%s)" % a.lib], env=env, capture_output=True)
    subprocess.run(
        [bin_, "system", "RMVDIR DIR('/home/%s') SUBTREE(*ALL)" % a.home], env=env, capture_output=True
    )
    subprocess.run([bin_, "system", "CRTLIB LIB(%s)" % a.lib], env=env, capture_output=True)
    subprocess.run([bin_, "system", "MKDIR DIR('/home/%s')" % a.home], env=env, capture_output=True)
    env["AS4TAINR_LIBL"] = "%s,%s" % (a.lib, env["AS4TAINR_LIBL"])

    report = []
    counts = {"same": 0, "different": 0, "missing": 0, "clean-room": 0}
    for name, cmd in items:
        gpath = os.path.join(a.golden, name + ".txt")
        raw = cmd.startswith("!")
        r = subprocess.run(
            ["sh", "-c", shell_for(cmd, sb)], env=env, capture_output=True, text=True, cwd=sb
        )
        ours = parse_result(r.stdout.splitlines(), raw)
        if a.save:
            os.makedirs(a.save, exist_ok=True)
            with open(os.path.join(a.save, name + ".txt"), "w", encoding="utf-8") as f:
                f.write(r.stdout)
        if not os.path.exists(gpath):
            counts["missing"] += 1
            report.append("?  %-22s no capture" % name)
            continue
        gold = parse_result(open(gpath, encoding="utf-8").read().splitlines(), raw)
        g_out, g_rc, g_err = gold
        o_out, o_rc, o_err = ours
        diffs = []
        if g_rc != o_rc:
            diffs.append("rc %s, ours %s" % (g_rc, o_rc))
        gi = [m[0] for m in messages(g_out)], [m[0] for m in messages(g_err)]
        oi = [m[0] for m in messages(o_out)], [m[0] for m in messages(o_err)]
        if gi[0] != oi[0]:
            diffs.append("stdout ids %s, ours %s" % (gi[0], oi[0]))
        if gi[1] != oi[1]:
            diffs.append("stderr ids %s, ours %s" % (gi[1], oi[1]))
        if printed(g_out) != printed(o_out):
            diffs.append("printed output %s, ours %s" % (printed(g_out), printed(o_out)))
        # Clean room: our text must not be IBM's.
        # (Texts that are only message data, like CPF9898's, do not count.)
        ibm = {
            (i, re.sub(r"\s+", " ", t))
            for i, t in messages(g_out) + messages(g_err)
            if len(skeleton(t)) > 8
        }
        same = [
            i
            for i, t in messages(o_out) + messages(o_err)
            if (i, re.sub(r"\s+", " ", t)) in ibm
        ]
        if same:
            counts["clean-room"] += 1
        mark = "ok" if not diffs else "!!"
        counts["same" if not diffs else "different"] += 1
        line = "%s %-22s %s" % (mark, name, "; ".join(diffs) if diffs else "")
        if same:
            line += "  CLEAN-ROOM: our text for %s is IBM's" % ", ".join(sorted(set(same)))
        report.append(line.rstrip())
        if diffs or same:
            report.append("     ibm:  " + " | ".join(g_out + ["rc=%s" % g_rc] + ["2> " + e for e in g_err])[:400])
            report.append("     ours: " + " | ".join(o_out + ["rc=%s" % o_rc] + ["2> " + e for e in o_err])[:400])
    summary = "%(same)d same, %(different)d different, %(missing)d not captured, %(clean-room)d with IBM's text" % counts
    report.append(summary)
    text = "\n".join(report) + "\n"
    if a.report:
        with open(a.report, "w") as f:
            f.write(text)
        print(summary)
    else:
        sys.stdout.write(text)
    if not a.keep:
        shutil.rmtree(sb, ignore_errors=True)
    return 1 if counts["different"] or counts["clean-room"] else 0


if __name__ == "__main__":
    sys.exit(main())
